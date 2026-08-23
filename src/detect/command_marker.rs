//! Finding the lines on screen that mean "a shell command ran".
//!
//! Separate from [`super::manifest`] on purpose: a manifest rule resolves the
//! single winning [`super::AgentState`] for a screen, and a command marker is a
//! different kind of answer — *every* line on screen that looks like a
//! completed shell command, so a caller can diff two scans and see which ones
//! are new. Bolting a second output shape onto the state-priority rule engine
//! would have complicated the one thing it is good at; this stays a small,
//! separate pass over the same evidence.
//!
//! v1 covers Claude Code only — the report this shipped from named it the
//! cheapest agent to start with, and its tool-call bullet is the one
//! documented, stable marker of a tool call actually being a shell command
//! rather than a read, an edit, or anything else Claude's bullet glyph also
//! introduces.
//!
//! # Two shapes, because Claude Code draws two
//!
//! Captured live from Claude Code v2.1.241 at 120x34, both forms confirmed on
//! the same session:
//!
//! * `● Bash(npm test)` — the *expanded* transcript (`ctrl+o`, "verbose"),
//!   and every Claude Code old enough to have printed tool calls unfolded.
//!   The bullet is U+25CF; releases up to and including the one this module
//!   first shipped against used U+23FA, so both are accepted — a glyph swap
//!   in the agent must not silently empty a pane's command log.
//! * `  ⎿  $ npm test` — the *collapsed* default view, which is what a pane
//!   actually shows unless its user turned verbose on. Here the bullet line
//!   carries a prose description ("Sleeping 20s in Python then printing ok")
//!   and the command itself only appears on the `⎿` result line, prefixed
//!   `$ `. Non-command results use U+00A0 after the same `⎿`, never `$ `,
//!   which is what keeps this from matching a `Read`'s output.
//!
//! The collapsed form is transient: once the call finishes, Claude Code folds
//! the whole block down to `Ran 1 shell command` and the `⎿  $ ` line leaves
//! the screen. That is precisely why
//! [`crate::app::pane_command_log::PaneCommandLog`] keeps its own copy — the
//! scan sees the line while it is up, and the zone outlives it.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

use super::Agent;

/// The expanded transcript's own shape: a tool bullet introducing `Bash(`.
/// Both bullet glyphs Claude Code has used are accepted — see this module's
/// header on why neither may be the only one.
fn bash_bullet_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*[\x{23FA}\x{25CF}]\s+Bash\(").expect("bash bullet pattern is valid")
    })
}

/// The collapsed default view's shape: a `⎿` result line whose content is a
/// `$ `-prefixed shell command. Deliberately requires the literal `$` and a
/// following space — every other `⎿` result Claude Code draws puts U+00A0
/// there instead, so a `Read`'s or a `Grep`'s output cannot match.
fn shell_echo_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\x{23BF}\s+\$\s+\S").expect("shell echo pattern is valid"))
}

/// Whether `line` is either shape of "Claude Code ran this shell command".
fn is_command_marker(line: &str) -> bool {
    bash_bullet_regex().is_match(line) || shell_echo_regex().is_match(line)
}

/// Every line in `screen` that reads as Claude Code reporting a Bash tool
/// call, in on-screen order.
///
/// Scoped to the agent's own transcript region ([`super::transcript_line_range`])
/// so a command *named* in the still-unsent composer line — someone typing
/// about `Bash(...)` rather than Claude having run it — can never match: the
/// composer is exactly what that region excludes.
///
/// Returns the raw lines rather than a count. A caller diffing scans needs to
/// tell *which* commands are already acknowledged, and a bullet's own text is
/// the only identity this pass has to offer — there is no per-command id
/// anywhere in the screen.
pub(crate) fn command_markers(agent: Option<Agent>, screen: &str) -> Vec<String> {
    if agent != Some(Agent::Claude) {
        return Vec::new();
    }
    let range = super::transcript_line_range(agent, screen).unwrap_or(0..screen.lines().count());
    let region = screen.lines().skip(range.start).take(range.len());
    region
        .filter(|line| is_command_marker(line))
        .map(str::trim_end)
        .map(str::to_string)
        .collect()
}

/// Pulls the command text out of one marker line — either shape — for a
/// caller that wants to show the command itself rather than the raw bullet.
///
/// For the expanded `● Bash(...)` form, takes everything between the first
/// `Bash(` and the last `)`, so a command that itself contains parentheses is
/// not truncated at the first one. For the collapsed `⎿  $ ...` form, takes
/// everything after the `$ `. Falls back to the whole trimmed line if neither
/// shape matches — this is a display helper, not a second parser with its own
/// failure mode.
pub(crate) fn bash_command_text(marker_line: &str) -> String {
    let trimmed = marker_line.trim();
    if let Some(start) = trimmed.find("Bash(") {
        let after = &trimmed[start + "Bash(".len()..];
        return match after.rfind(')') {
            Some(end) => after[..end].trim().to_string(),
            None => after.trim().to_string(),
        };
    }
    // `⎿  $ cmd`: strip the result marker and the shell sigil, keeping the
    // command exactly as the agent echoed it.
    if let Some(rest) = trimmed.strip_prefix('\u{23BF}') {
        let rest = rest.trim_start();
        if let Some(command) = rest.strip_prefix('$') {
            return command.trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Diffs one scan's markers against what a caller has already acknowledged,
/// and reports only the ones genuinely new since the last scan.
///
/// `acknowledged` is `None` before the first scan under the pane's current
/// agent identity. That first scan seeds it without reporting anything new —
/// the markers already on screen at that moment are history the agent printed
/// before this task started watching, not a command that just ran. A caller
/// resets `acknowledged` back to `None` whenever the pane's agent identity
/// changes, for the same reason.
///
/// A marker line that scrolls out of the scanned region and is pruned from
/// `acknowledged` reports as new again if it reappears — a known, accepted
/// limit of identifying a command by its own line text rather than a real id.
pub(crate) fn diff_new_markers(
    markers: Vec<String>,
    acknowledged: &mut Option<HashSet<String>>,
) -> Vec<String> {
    let Some(seen) = acknowledged else {
        *acknowledged = Some(markers.into_iter().collect());
        return Vec::new();
    };
    let mut fresh = Vec::new();
    for marker in &markers {
        if seen.insert(marker.clone()) {
            fresh.push(marker.clone());
        }
    }
    seen.retain(|line| markers.contains(line));
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_agent_finds_nothing() {
        assert!(command_markers(None, "⏺ Bash(npm test)").is_empty());
    }

    #[test]
    fn a_non_claude_agent_finds_nothing() {
        assert!(command_markers(Some(Agent::Codex), "⏺ Bash(npm test)").is_empty());
    }

    #[test]
    fn a_bash_bullet_is_found() {
        let screen = "Some earlier text\n⏺ Bash(npm test)\n  ⎿  Running…\n";
        assert_eq!(
            command_markers(Some(Agent::Claude), screen),
            vec!["⏺ Bash(npm test)".to_string()]
        );
    }

    #[test]
    fn a_non_bash_bullet_is_ignored() {
        let screen = "⏺ Read(src/main.rs)\n⏺ Edit(src/main.rs)\n";
        assert!(command_markers(Some(Agent::Claude), screen).is_empty());
    }

    #[test]
    fn every_bash_bullet_in_a_burst_is_returned_independently() {
        let screen = "⏺ Bash(npm test)\n⏺ Bash(npm build)\n⏺ Bash(npm lint)\n";
        assert_eq!(
            command_markers(Some(Agent::Claude), screen),
            vec![
                "⏺ Bash(npm test)".to_string(),
                "⏺ Bash(npm build)".to_string(),
                "⏺ Bash(npm lint)".to_string(),
            ]
        );
    }

    #[test]
    fn a_bullet_typed_unsent_in_the_composer_is_not_a_command_that_ran() {
        // Claude's transcript region excludes the composer/prompt box, so a
        // user typing about a bullet rather than Claude drawing one must not
        // match — this asserts the exclusion actually reaches this function
        // and not only the manifest's own state rules. The two `───` rules
        // are the composer's top and bottom border, exactly what
        // `above_prompt_box` looks for.
        let screen = "⏺ Bash(npm test)\n\
────────────────────\n\
❯ Bash(rm -rf /)\n\
────────────────────\n";
        assert_eq!(
            command_markers(Some(Agent::Claude), screen),
            vec!["⏺ Bash(npm test)".to_string()]
        );
    }

    /// Claude Code v2.1.241's expanded transcript, captured live: the bullet
    /// is U+25CF, not the U+23FA this module first shipped against. A pane's
    /// command log went permanently empty when the agent swapped the glyph,
    /// so both are accepted rather than tracking whichever is current.
    #[test]
    fn the_current_black_circle_bullet_is_found() {
        let screen = "\u{25CF} Bash(pwd)\n  \u{23BF} \u{a0}/tmp\n";
        assert_eq!(
            command_markers(Some(Agent::Claude), screen),
            vec!["\u{25CF} Bash(pwd)".to_string()]
        );
    }

    /// The collapsed default view — what a pane shows unless verbose is on.
    /// The bullet line carries a prose description and only the `\u{23BF}` result
    /// line has the command, prefixed `$ `.
    #[test]
    fn the_collapsed_views_dollar_echo_is_a_command() {
        let screen =
            "\u{25CF} Sleeping 20s in Python then printing ok\n  \u{23BF}  $ python3 -c \"pass\"\n";
        assert_eq!(
            command_markers(Some(Agent::Claude), screen),
            vec!["  \u{23BF}  $ python3 -c \"pass\"".to_string()]
        );
    }

    /// Every non-command `\u{23BF}` result Claude Code draws puts U+00A0 where a
    /// shell command puts `$ `. Matching the marker alone would turn every
    /// file read and every grep hit into a logged "command".
    #[test]
    fn a_non_command_result_line_is_not_a_command() {
        let screen =
            "\u{25CF} Read(src/main.rs)\n  \u{23BF} \u{a0}total 4\n  \u{23BF} \u{a0}/tmp/x\n";
        assert!(command_markers(Some(Agent::Claude), screen).is_empty());
    }

    #[test]
    fn bash_command_text_extracts_the_collapsed_echo() {
        assert_eq!(
            bash_command_text("  \u{23BF}  $ cargo nextest run"),
            "cargo nextest run"
        );
    }

    #[test]
    fn bash_command_text_extracts_the_current_bullet_form() {
        assert_eq!(bash_command_text("\u{25CF} Bash(npm test)"), "npm test");
    }

    #[test]
    fn bash_command_text_extracts_the_command() {
        assert_eq!(bash_command_text("⏺ Bash(npm test)"), "npm test");
    }

    #[test]
    fn bash_command_text_keeps_inner_parens() {
        assert_eq!(bash_command_text("⏺ Bash(echo $(date))"), "echo $(date)");
    }

    #[test]
    fn bash_command_text_falls_back_to_the_whole_line_when_unshaped() {
        assert_eq!(bash_command_text("not a bash bullet"), "not a bash bullet");
    }

    #[test]
    fn the_first_scan_seeds_without_reporting_anything_new() {
        let mut acknowledged = None;
        let fresh = diff_new_markers(vec!["⏺ Bash(npm test)".to_string()], &mut acknowledged);
        assert!(
            fresh.is_empty(),
            "the transcript a scan first sees is history, not an event"
        );
        assert!(acknowledged.is_some());
    }

    #[test]
    fn a_later_scan_reports_only_the_genuinely_new_lines() {
        let mut acknowledged = None;
        diff_new_markers(vec!["⏺ Bash(npm test)".to_string()], &mut acknowledged);
        let fresh = diff_new_markers(
            vec![
                "⏺ Bash(npm test)".to_string(),
                "⏺ Bash(npm build)".to_string(),
            ],
            &mut acknowledged,
        );
        assert_eq!(fresh, vec!["⏺ Bash(npm build)".to_string()]);
    }

    #[test]
    fn a_burst_reports_every_new_line_independently() {
        let mut acknowledged = None;
        diff_new_markers(Vec::new(), &mut acknowledged);
        let fresh = diff_new_markers(
            vec![
                "⏺ Bash(npm test)".to_string(),
                "⏺ Bash(npm build)".to_string(),
                "⏺ Bash(npm lint)".to_string(),
            ],
            &mut acknowledged,
        );
        assert_eq!(
            fresh,
            vec![
                "⏺ Bash(npm test)".to_string(),
                "⏺ Bash(npm build)".to_string(),
                "⏺ Bash(npm lint)".to_string(),
            ]
        );
    }

    #[test]
    fn a_line_that_scrolled_away_reports_new_again_if_it_reappears() {
        let mut acknowledged = None;
        diff_new_markers(vec!["⏺ Bash(npm test)".to_string()], &mut acknowledged);
        // Scrolled out of the scanned region.
        diff_new_markers(Vec::new(), &mut acknowledged);
        let fresh = diff_new_markers(vec!["⏺ Bash(npm test)".to_string()], &mut acknowledged);
        assert_eq!(
            fresh,
            vec!["⏺ Bash(npm test)".to_string()],
            "a known, accepted limit of identifying a command by its own text"
        );
    }
}
