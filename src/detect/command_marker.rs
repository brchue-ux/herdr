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
//! cheapest agent to start with, and its `⏺ Bash(...)` bullet is the one
//! documented, stable marker of a tool call actually being a shell command
//! rather than a read, an edit, or anything else Claude's bullet glyph also
//! introduces.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

use super::Agent;

fn bash_bullet_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\x{23FA}\s+Bash\(").expect("bash bullet pattern is valid"))
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
    let pattern = bash_bullet_regex();
    region
        .filter(|line| pattern.is_match(line))
        .map(str::trim_end)
        .map(str::to_string)
        .collect()
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
