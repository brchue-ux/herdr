pub(super) fn tab_attention_priority(state: crate::detect::AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (crate::detect::AgentState::Blocked, _) => 4,
        (crate::detect::AgentState::Idle, false) => 3,
        (crate::detect::AgentState::Working, _) => 2,
        (crate::detect::AgentState::Idle, true) => 1,
        (crate::detect::AgentState::Unknown, _) => 0,
    }
}

fn parse_api_key(key: &str) -> Option<crossterm::event::KeyEvent> {
    let normalized = normalize_api_key_alias(key.trim());
    let (code, modifiers) = crate::config::parse_key_combo(normalized)?;
    Some(crossterm::event::KeyEvent::new(code, modifiers))
}

fn normalize_api_key_alias(key: &str) -> &str {
    match key {
        "C-c" | "c-c" => "ctrl+c",
        "+" => "plus",
        _ => key,
    }
}

pub(super) fn encode_api_text(runtime: &crate::terminal::TerminalRuntime, text: &str) -> Vec<u8> {
    let bracketed = runtime
        .input_state()
        .map(|state| state.bracketed_paste)
        .unwrap_or(false);
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}

pub(crate) fn encode_api_keys(
    runtime: &crate::terminal::TerminalRuntime,
    keys: &[String],
) -> Result<Vec<Vec<u8>>, String> {
    let mut encoded_keys = Vec::with_capacity(keys.len());
    for key in keys {
        let Some(key_event) = parse_api_key(key) else {
            return Err(key.clone());
        };
        encoded_keys.push(runtime.encode_terminal_key(key_event.into()));
    }
    Ok(encoded_keys)
}

pub(super) fn encode_api_submission_parts(
    runtime: &crate::terminal::TerminalRuntime,
    text: &str,
) -> (Vec<u8>, Vec<u8>) {
    let text = encode_api_text(runtime, text);
    let enter = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    );
    (text, runtime.encode_terminal_key(enter.into()))
}

pub(super) fn encode_api_submission(
    runtime: &crate::terminal::TerminalRuntime,
    text: &str,
) -> Vec<u8> {
    let (mut text, enter) = encode_api_submission_parts(runtime, text);
    text.extend_from_slice(&enter);
    text
}

pub(super) fn encode_api_input(
    runtime: &crate::terminal::TerminalRuntime,
    text: &str,
    keys: &[String],
) -> Result<Vec<u8>, String> {
    let mut bytes = if text.is_empty() {
        Vec::new()
    } else {
        encode_api_text(runtime, text)
    };
    for encoded in encode_api_keys(runtime, keys)? {
        bytes.extend_from_slice(&encoded);
    }
    Ok(bytes)
}

pub(super) fn detect_state_from_api(
    state: crate::api::schema::PaneAgentState,
) -> crate::detect::AgentState {
    match state {
        crate::api::schema::PaneAgentState::Idle => crate::detect::AgentState::Idle,
        crate::api::schema::PaneAgentState::Working => crate::detect::AgentState::Working,
        crate::api::schema::PaneAgentState::Blocked => crate::detect::AgentState::Blocked,
        crate::api::schema::PaneAgentState::Unknown => crate::detect::AgentState::Unknown,
    }
}

pub(super) fn pane_agent_status(
    state: crate::detect::AgentState,
    seen: bool,
) -> crate::api::schema::AgentStatus {
    match (state, seen) {
        (crate::detect::AgentState::Idle, false) => crate::api::schema::AgentStatus::Done,
        (crate::detect::AgentState::Idle, true) => crate::api::schema::AgentStatus::Idle,
        (crate::detect::AgentState::Working, _) => crate::api::schema::AgentStatus::Working,
        (crate::detect::AgentState::Blocked, _) => crate::api::schema::AgentStatus::Blocked,
        (crate::detect::AgentState::Unknown, _) => crate::api::schema::AgentStatus::Unknown,
    }
}

/// A pane read plus the transcript-slicing outcome. `transcript_applied` is
/// `None` for every source other than `transcript`.
pub(super) struct PaneReadOutcome {
    pub(super) snapshot: crate::pane::TerminalReadSnapshot,
    pub(super) transcript_applied: Option<bool>,
}

impl From<crate::pane::TerminalReadSnapshot> for PaneReadOutcome {
    fn from(snapshot: crate::pane::TerminalReadSnapshot) -> Self {
        Self {
            snapshot,
            transcript_applied: None,
        }
    }
}

pub(super) fn read_terminal_snapshot(
    terminal: &crate::terminal::TerminalRuntime,
    agent: Option<crate::detect::Agent>,
    source: crate::api::schema::ReadSource,
    format: crate::api::schema::ReadFormat,
    lines: Option<u32>,
) -> PaneReadOutcome {
    use crate::api::schema::{ReadFormat, ReadSource};

    let line_limit = lines.map(|lines| lines.min(1000) as usize);
    let recent_lines = line_limit.unwrap_or(80);
    if matches!(source, ReadSource::Transcript) {
        let snapshot = match format {
            ReadFormat::Text => terminal.recent_text_snapshot(recent_lines),
            ReadFormat::Ansi => terminal.recent_ansi_snapshot(recent_lines),
        };
        return transcript_outcome(agent, snapshot, format);
    }
    PaneReadOutcome::from(match (format, source) {
        (ReadFormat::Text, ReadSource::Visible) => {
            limit_snapshot_lines(terminal.visible_text(), line_limit)
        }
        (ReadFormat::Text, ReadSource::Recent) => terminal.recent_text_snapshot(recent_lines),
        (ReadFormat::Text, ReadSource::RecentUnwrapped) => {
            terminal.recent_unwrapped_text_snapshot(recent_lines)
        }
        (ReadFormat::Text, ReadSource::Detection) => {
            limit_snapshot_lines(terminal.detection_text(), line_limit)
        }
        (ReadFormat::Ansi, ReadSource::Visible) => {
            limit_snapshot_lines(terminal.visible_ansi(), line_limit)
        }
        (ReadFormat::Ansi, ReadSource::Recent) => terminal.recent_ansi_snapshot(recent_lines),
        (ReadFormat::Ansi, ReadSource::RecentUnwrapped) => {
            terminal.recent_unwrapped_ansi_snapshot(recent_lines)
        }
        (ReadFormat::Ansi, ReadSource::Detection) => {
            limit_snapshot_lines(terminal.detection_text(), line_limit)
        }
        // Handled above, before the recent snapshot is taken.
        (_, ReadSource::Transcript) => crate::pane::TerminalReadSnapshot::default(),
    })
}

/// Cut the composer out of a recent snapshot using the agent manifest's
/// `transcript_region`. When no region resolves, the snapshot is returned
/// unchanged — identical to `recent` — and reported as not applied.
fn transcript_outcome(
    agent: Option<crate::detect::Agent>,
    snapshot: crate::pane::TerminalReadSnapshot,
    format: crate::api::schema::ReadFormat,
) -> PaneReadOutcome {
    let lines: Vec<&str> = snapshot.text.lines().collect();
    // Regions are matched on plain text; an ANSI read is stripped only to
    // locate the cut, so both formats slice at the same line indices.
    let plain = match format {
        crate::api::schema::ReadFormat::Text => None,
        crate::api::schema::ReadFormat::Ansi => Some(
            lines
                .iter()
                .map(|line| strip_ansi_sequences(line))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
    };
    let screen = match &plain {
        Some(plain) => plain.as_str(),
        None => snapshot.text.as_str(),
    };
    let Some(range) = crate::detect::transcript_line_range(agent, screen) else {
        return PaneReadOutcome {
            snapshot,
            transcript_applied: Some(false),
        };
    };
    let end = range.end.min(lines.len());
    let start = range.start.min(end);
    let mut text = lines[start..end].join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    PaneReadOutcome {
        snapshot: crate::pane::TerminalReadSnapshot {
            text,
            truncated: snapshot.truncated || start > 0,
        },
        transcript_applied: Some(true),
    }
}

/// Remove ANSI escape sequences from a single line. Only used to align a
/// styled read with the plain-text region matchers; the returned text is never
/// handed to a caller.
fn strip_ansi_sequences(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            // CSI: parameters/intermediates until a final byte in @..~
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            // OSC / other string sequences: run to BEL or ST.
            Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-byte escapes consume just the second byte.
            Some(_) | None => {}
        }
    }
    out
}

fn limit_snapshot_lines(text: String, limit: Option<usize>) -> crate::pane::TerminalReadSnapshot {
    let Some(limit) = limit else {
        return crate::pane::TerminalReadSnapshot {
            text,
            truncated: false,
        };
    };
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    crate::pane::TerminalReadSnapshot {
        text: lines[lines.len().saturating_sub(limit)..].concat(),
        truncated: lines.len() > limit,
    }
}

#[cfg(test)]
mod read_snapshot_tests {
    use super::{limit_snapshot_lines, strip_ansi_sequences, transcript_outcome};
    use crate::api::schema::ReadFormat;
    use crate::detect::Agent;
    use crate::pane::TerminalReadSnapshot;

    fn snapshot(text: &str) -> TerminalReadSnapshot {
        TerminalReadSnapshot {
            text: text.to_string(),
            truncated: false,
        }
    }

    const CLAUDE_SCREEN: &str = concat!(
        "  Applied the patch.\n",
        "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n",
        " \u{276f} rm -rf /tmp/scratch\n",
        "\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n",
        "  ? for shortcuts\n",
    );

    #[test]
    fn transcript_read_drops_the_composer_and_reports_it_applied() {
        let outcome = transcript_outcome(
            Some(Agent::Claude),
            snapshot(CLAUDE_SCREEN),
            ReadFormat::Text,
        );

        assert_eq!(outcome.transcript_applied, Some(true));
        assert_eq!(outcome.snapshot.text, "  Applied the patch.\n");
    }

    #[test]
    fn transcript_read_without_a_known_agent_falls_back_to_recent_and_says_so() {
        let outcome = transcript_outcome(None, snapshot(CLAUDE_SCREEN), ReadFormat::Text);

        assert_eq!(outcome.transcript_applied, Some(false));
        assert_eq!(outcome.snapshot.text, CLAUDE_SCREEN);
    }

    #[test]
    fn transcript_read_keeps_styling_while_cutting_at_the_plain_text_boundary() {
        let styled = CLAUDE_SCREEN
            .lines()
            .map(|line| format!("\u{1b}[38;5;42m{line}\u{1b}[0m"))
            .collect::<Vec<_>>()
            .join("\n");
        let outcome = transcript_outcome(Some(Agent::Claude), snapshot(&styled), ReadFormat::Ansi);

        assert_eq!(outcome.transcript_applied, Some(true));
        assert_eq!(
            outcome.snapshot.text,
            "\u{1b}[38;5;42m  Applied the patch.\u{1b}[0m\n"
        );
    }

    #[test]
    fn ansi_stripping_removes_csi_and_osc_sequences_only() {
        assert_eq!(strip_ansi_sequences("\u{1b}[1;31mred\u{1b}[0m"), "red");
        assert_eq!(strip_ansi_sequences("\u{1b}]0;title\u{7}after"), "after");
        assert_eq!(
            strip_ansi_sequences("\u{1b}]8;;https://x\u{1b}\\link"),
            "link"
        );
        assert_eq!(
            strip_ansi_sequences("plain \u{2500} text"),
            "plain \u{2500} text"
        );
    }

    #[test]
    fn line_limit_preserves_endings_and_reports_omitted_lines() {
        let snapshot = limit_snapshot_lines("one\ntwø\n三\n".into(), Some(2));
        assert_eq!(snapshot.text, "twø\n三\n");
        assert!(snapshot.truncated);

        let snapshot = limit_snapshot_lines("one\ntwo\nthree".into(), Some(1));
        assert_eq!(snapshot.text, "three");
        assert!(snapshot.truncated);

        let snapshot = limit_snapshot_lines("one\ntwo".into(), Some(0));
        assert_eq!(snapshot.text, "");
        assert!(snapshot.truncated);

        let snapshot = limit_snapshot_lines("".into(), Some(2));
        assert_eq!(snapshot.text, "");
        assert!(!snapshot.truncated);
    }

    #[test]
    fn omitted_line_limit_returns_the_complete_snapshot() {
        let snapshot = limit_snapshot_lines("one\ntwo\n".into(), None);
        assert_eq!(snapshot.text, "one\ntwo\n");
        assert!(!snapshot.truncated);
    }
}

pub(super) fn normalize_reported_agent_label(agent: &str) -> Option<String> {
    let trimmed = agent.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(agent) = crate::detect::parse_agent_label(trimmed) {
        return Some(crate::detect::agent_label(agent).to_string());
    }
    Some(trimmed.to_string())
}

pub(super) const METADATA_TTL_MAX_MS: u64 = 86_400_000;
pub(super) const METADATA_SOURCE_MAX_CHARS: usize = 80;
const METADATA_TTL_MIN_MS: u64 = 1;
const MAX_METADATA_TOKEN_KEYS_PER_REQUEST: usize = 16;
pub(super) const MAX_METADATA_TOKEN_KEYS_PER_RESOURCE: usize = 32;
const MAX_METADATA_TOKEN_KEY_LEN: usize = 32;
const MAX_METADATA_TOKEN_VALUE_LEN: usize = 80;

pub(super) fn normalize_metadata_source(value: String) -> Result<String, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Err("metadata source must not be empty");
    }
    if value.chars().count() > METADATA_SOURCE_MAX_CHARS {
        return Err("metadata source must be 80 characters or fewer");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
    {
        return Err(
            "metadata source may contain only ASCII letters, digits, colon, dot, underscore, and hyphen",
        );
    }
    Ok(value.to_string())
}

pub(super) fn normalize_metadata_ttl(
    ttl_ms: Option<u64>,
) -> Result<Option<std::time::Duration>, &'static str> {
    let Some(ttl_ms) = ttl_ms else {
        return Ok(None);
    };
    if ttl_ms < METADATA_TTL_MIN_MS {
        return Err("metadata ttl_ms must be at least 1");
    }
    if ttl_ms > METADATA_TTL_MAX_MS {
        return Err("metadata ttl_ms must be 86400000 or less");
    }
    Ok(Some(std::time::Duration::from_millis(ttl_ms)))
}

pub(super) fn normalize_metadata_tokens(
    tokens: std::collections::HashMap<String, Option<String>>,
) -> Result<std::collections::HashMap<String, Option<String>>, String> {
    if tokens.is_empty() {
        return Err("missing token to set or clear".into());
    }
    if tokens.len() > MAX_METADATA_TOKEN_KEYS_PER_REQUEST {
        return Err(format!(
            "a metadata report may update at most {MAX_METADATA_TOKEN_KEYS_PER_REQUEST} tokens"
        ));
    }

    tokens
        .into_iter()
        .map(|(key, value)| {
            if key.is_empty()
                || key.len() > MAX_METADATA_TOKEN_KEY_LEN
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
            {
                return Err(format!("invalid metadata token key: {key}"));
            }
            let value = value.and_then(|value| {
                let normalized = value
                    .trim()
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(MAX_METADATA_TOKEN_VALUE_LEN)
                    .collect::<String>();
                (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
            });
            Ok((key, value))
        })
        .collect()
}

#[cfg(test)]
mod metadata_token_tests {
    use super::*;

    #[test]
    fn token_normalization_sanitizes_values_and_turns_empty_into_clear() {
        let tokens = normalize_metadata_tokens(std::collections::HashMap::from([
            ("summary".into(), Some("  review\nready  ".into())),
            ("empty".into(), Some(" \n ".into())),
            ("clear".into(), None),
        ]))
        .unwrap();

        assert_eq!(tokens["summary"].as_deref(), Some("reviewready"));
        assert_eq!(tokens["empty"], None);
        assert_eq!(tokens["clear"], None);
    }

    #[test]
    fn token_normalization_rejects_invalid_or_unbounded_keys() {
        for key in [
            "bad.name".to_string(),
            "x".repeat(MAX_METADATA_TOKEN_KEY_LEN + 1),
        ] {
            assert!(normalize_metadata_tokens(std::collections::HashMap::from([(
                key,
                Some("value".into()),
            )]))
            .is_err());
        }
        let too_many = (0..=MAX_METADATA_TOKEN_KEYS_PER_REQUEST)
            .map(|index| (format!("key{index}"), Some("value".into())))
            .collect();
        assert!(normalize_metadata_tokens(too_many).is_err());
    }
}
