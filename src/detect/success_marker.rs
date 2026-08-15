//! Finding Claude's on-screen marker for a successfully completed ask.
//!
//! State detection and event detection answer different questions. A manifest resolves one
//! current [`super::AgentState`], while this pass reports every newly visible success marker so
//! the caller can diff scans and turn an event into a comet. The marker's colour is evidence:
//! Claude uses the same solid circle glyph for other prose, so plain text alone cannot establish
//! a win.

use super::Agent;

#[derive(Clone, Copy, Default)]
enum Foreground {
    #[default]
    Default,
    Basic(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Foreground {
    fn is_green(self) -> bool {
        match self {
            Self::Basic(code) => matches!(code, 32 | 92),
            Self::Indexed(index) => {
                let (r, g, b) = indexed_rgb(index);
                rgb_is_green(r, g, b)
            }
            Self::Rgb(r, g, b) => rgb_is_green(r, g, b),
            Self::Default => false,
        }
    }
}

fn rgb_is_green(r: u8, g: u8, b: u8) -> bool {
    g >= 96 && g >= r.saturating_add(24) && g >= b.saturating_add(8)
}

fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    const BASIC: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    if index < 16 {
        return BASIC[usize::from(index)];
    }
    if index < 232 {
        let cube = index - 16;
        let channel = |value: u8| if value == 0 { 0 } else { 55 + value * 40 };
        return (
            channel(cube / 36),
            channel((cube % 36) / 6),
            channel(cube % 6),
        );
    }
    let grey = 8 + (index - 232) * 10;
    (grey, grey, grey)
}

fn apply_sgr(params: &str, foreground: &mut Foreground) {
    let values: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params
            .split([';', ':'])
            // ISO-8613-6's colon form may contain an empty colour-space field (`38:2::r:g:b`).
            .filter(|value| !value.is_empty())
            .map(|value| value.parse::<u16>().unwrap_or(0))
            .collect()
    };
    let mut index = 0;
    while index < values.len() {
        match values[index] {
            0 | 39 => *foreground = Foreground::Default,
            code @ (30..=37 | 90..=97) => *foreground = Foreground::Basic(code as u8),
            38 if values.get(index + 1) == Some(&2) && index + 4 < values.len() => {
                *foreground = Foreground::Rgb(
                    values[index + 2].min(255) as u8,
                    values[index + 3].min(255) as u8,
                    values[index + 4].min(255) as u8,
                );
                index += 4;
            }
            38 if values.get(index + 1) == Some(&5) && index + 2 < values.len() => {
                *foreground = Foreground::Indexed(values[index + 2].min(255) as u8);
                index += 2;
            }
            _ => {}
        }
        index += 1;
    }
}

fn leading_circle_is_green(line: &str, foreground: &mut Foreground) -> bool {
    let mut rest = line;
    let mut leading_is_green = None;
    loop {
        if let Some(after_escape) = rest.strip_prefix("\x1b[") {
            let Some(end) = after_escape.find('m') else {
                break;
            };
            apply_sgr(&after_escape[..end], foreground);
            rest = &after_escape[end + 1..];
            continue;
        }
        let Some(ch) = rest.chars().next() else {
            break;
        };
        if leading_is_green.is_none() && !ch.is_whitespace() {
            leading_is_green = Some(ch == '●' && foreground.is_green());
        }
        rest = &rest[ch.len_utf8()..];
    }
    leading_is_green.unwrap_or(false)
}

/// Every line in Claude's transcript whose leading solid circle is green.
pub(crate) fn success_markers(
    agent: Option<Agent>,
    screen: &str,
    ansi_screen: &str,
) -> Vec<String> {
    if agent != Some(Agent::Claude) {
        return Vec::new();
    }
    let range = super::transcript_line_range(agent, screen).unwrap_or(0..screen.lines().count());
    let plain_lines: Vec<_> = screen.lines().collect();
    let ansi_lines: Vec<_> = ansi_screen.lines().collect();
    let mut foreground = Foreground::Default;
    let green_by_line: Vec<_> = ansi_lines
        .iter()
        .map(|line| leading_circle_is_green(line, &mut foreground))
        .collect();
    range
        .filter_map(|index| {
            let plain = *plain_lines.get(index)?;
            let green = *green_by_line.get(index)?;
            (plain.trim_start().starts_with('●') && green).then(|| plain.trim_end().to_string())
        })
        .collect()
}

/// Count newly visible occurrences without collapsing repeated success text.
///
/// Claude often prints the same short completion more than once. A set would treat two visible
/// `● Done` rows as one marker and silently lose the later win.
pub(crate) fn diff_new_success_markers(
    markers: Vec<String>,
    acknowledged: &mut Option<std::collections::HashMap<String, usize>>,
) -> usize {
    let mut current = std::collections::HashMap::new();
    for marker in markers {
        *current.entry(marker).or_insert(0) += 1;
    }
    let Some(previous) = acknowledged.as_ref() else {
        *acknowledged = Some(current);
        return 0;
    };
    let fresh = current
        .iter()
        .map(|(marker, count)| count.saturating_sub(previous.get(marker).copied().unwrap_or(0)))
        .sum();
    *acknowledged = Some(current);
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_green_success_circle_is_a_win() {
        let screen = "● Completed the requested edit\n";
        let ansi = "\x1b[38;2;86;180;112m●\x1b[0m Completed the requested edit\n";

        assert_eq!(
            success_markers(Some(Agent::Claude), screen, ansi),
            vec!["● Completed the requested edit".to_string()]
        );
    }

    #[test]
    fn colour_and_agent_identity_are_required() {
        let screen = "● Finished\n";
        let blue = "\x1b[38;2;51;153;255m●\x1b[0m Finished\n";
        let white = "\x1b[97m●\x1b[0m Finished\n";
        let green_tool_ring = "\x1b[32m⏺\x1b[0m Bash(just test)\n";

        assert!(success_markers(Some(Agent::Claude), screen, blue).is_empty());
        assert!(success_markers(Some(Agent::Claude), screen, white).is_empty());
        assert!(
            success_markers(Some(Agent::Claude), "⏺ Bash(just test)\n", green_tool_ring).is_empty()
        );
        assert!(
            success_markers(Some(Agent::Codex), screen, "\x1b[32m●\x1b[0m Finished\n").is_empty()
        );
    }

    #[test]
    fn a_green_circle_typed_in_the_composer_is_not_a_win() {
        let screen = "● Finished the real ask\n\
────────────────────\n\
❯ ● Please discuss green circles\n\
────────────────────\n";
        let ansi = "\x1b[32m●\x1b[0m Finished the real ask\n\
────────────────────\n\
❯ \x1b[32m●\x1b[0m Please discuss green circles\n\
────────────────────\n";

        assert_eq!(
            success_markers(Some(Agent::Claude), screen, ansi),
            vec!["● Finished the real ask".to_string()]
        );
    }

    #[test]
    fn named_and_indexed_ansi_greens_are_supported() {
        let screen = "● first\n● second\n● third\n";
        let ansi = "\x1b[92m●\x1b[0m first\n\x1b[38;5;34m●\x1b[0m second\n\x1b[38:2::86:180:112m●\x1b[0m third\n";
        assert_eq!(success_markers(Some(Agent::Claude), screen, ansi).len(), 3);
    }

    #[test]
    fn colour_state_can_span_lines() {
        let screen = "● first\n● second\n● white\n";
        let ansi = "\x1b[32m● first\n● second\n\x1b[0m● white\n";
        assert_eq!(success_markers(Some(Agent::Claude), screen, ansi).len(), 2);
    }

    #[test]
    fn identical_visible_successes_are_counted_as_separate_wins() {
        let mut acknowledged = None;
        assert_eq!(
            diff_new_success_markers(vec!["● Done".to_string()], &mut acknowledged),
            0
        );
        assert_eq!(
            diff_new_success_markers(
                vec!["● Done".to_string(), "● Done".to_string()],
                &mut acknowledged,
            ),
            1
        );
        assert_eq!(
            diff_new_success_markers(
                vec!["● Done".to_string(), "● Done".to_string()],
                &mut acknowledged,
            ),
            0
        );
    }
}
