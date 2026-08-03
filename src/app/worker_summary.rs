//! What a finished worker says it did, and which second mate it belongs to.
//!
//! Herdr does not author summaries. The text is published onto a worker's pane
//! the same way every other display fact is — `pane report-metadata --token
//! summary=...` — so it is already server-owned state on the JSON API
//! (`panes[].tokens`), already persisted, and already carried across a handoff.
//! Nothing here adds a second channel; it only reads the tokens a worker has
//! published and groups them the way the tree already groups their rows.
//!
//! A token value is capped at 80 characters with control characters stripped
//! ([`crate::app::api_helpers`]), so one token is exactly one line. A summary
//! longer than that arrives as an ordered family — `summary`, then
//! `summary_2`, `summary_3`, … — which is why [`summary_lines`] exists rather
//! than a single lookup. That keeps a real multi-line report inside the
//! existing token contract instead of widening the wire format for it.
//!
//! Scoping is [`summaries_for_owner`]: a worker belongs to the second mate its
//! `owner` token names, which is the same fact
//! [`crate::app::agent_tree`] uses to decide where the worker's row is drawn.
//! There is one source of truth for "whose worker is this", so the summary view
//! can never disagree with the tree it was opened from.

use crate::detect::AgentState;
use crate::ui::AgentPanelEntry;

/// The token carrying a worker's summary, and the stem its continuations use.
pub(crate) const SUMMARY_TOKEN: &str = "summary";

/// The most continuation lines one worker can publish.
///
/// A pane holds at most 32 tokens in total
/// (`MAX_METADATA_TOKEN_KEYS_PER_RESOURCE`), and `owner` plus whatever else the
/// fleet publishes has to fit beside them, so the scan stops well before a
/// pathological key set can make it walk forever.
const MAX_SUMMARY_LINES: usize = 32;

/// The key holding continuation line `n`, where line 1 is [`SUMMARY_TOKEN`].
fn continuation_key(n: usize) -> String {
    format!("{SUMMARY_TOKEN}_{n}")
}

/// A worker's published summary, in the order it was published.
///
/// Empty when the worker published nothing. Continuations stop at the first
/// gap: `summary` + `summary_3` with no `summary_2` yields one line, because a
/// missing middle line means the publisher is still mid-report and showing
/// line 3 under line 1 would silently reorder its own words.
pub(crate) fn summary_lines(tokens: &std::collections::HashMap<String, String>) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(first) = tokens.get(SUMMARY_TOKEN).map(|line| line.trim()) else {
        return lines;
    };
    if first.is_empty() {
        return lines;
    }
    lines.push(first.to_string());

    for n in 2..=MAX_SUMMARY_LINES {
        match tokens.get(&continuation_key(n)).map(|line| line.trim()) {
            Some(line) if !line.is_empty() => lines.push(line.to_string()),
            _ => break,
        }
    }
    lines
}

/// Whether this pane has published anything for the summary view to show.
pub(crate) fn has_summary(tokens: &std::collections::HashMap<String, String>) -> bool {
    tokens
        .get(SUMMARY_TOKEN)
        .is_some_and(|line| !line.trim().is_empty())
}

/// One worker as the summary view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerSummary {
    pub ws_idx: usize,
    pub tab_idx: usize,
    pub pane_id: crate::layout::PaneId,
    /// The worker's own handle, falling back to whatever label the row shows.
    pub name: String,
    pub state: AgentState,
    pub seen: bool,
    pub lines: Vec<String>,
}

impl WorkerSummary {
    /// Whether this worker is finished and not yet looked at — the `done` row
    /// state the sidebar already paints, and the case this whole view exists
    /// for.
    pub(crate) fn is_unseen_finish(&self) -> bool {
        matches!(self.state, AgentState::Idle) && !self.seen
    }
}

/// What to call this worker in the list.
///
/// The handle first, because that is the name its mate already knows it by and
/// the one an `owner` token would spell. A pane label comes next; the agent
/// label is the last resort on purpose, since every worker running the same
/// tool shares it and a list of identical rows tells the reader nothing.
fn worker_display_name(entry: &AgentPanelEntry) -> String {
    [
        entry.agent_name.as_deref(),
        entry.pane_label.as_deref(),
        entry.agent_label.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|name| !name.is_empty())
    .unwrap_or("worker")
    .to_string()
}

/// Whether `owner` names this entry, matched the way the tree matches it.
fn names(entry_name: Option<&str>, owner: &str) -> bool {
    entry_name.is_some_and(|name| name.trim() == owner.trim() && !owner.trim().is_empty())
}

/// Every worker owned by `owner` that has published a summary, in row order.
///
/// This is the scoping contract: one second mate's workers, never the fleet.
/// `entries` is the same slice the sidebar draws, so the view lists exactly the
/// rows nested under the row the user clicked.
pub(crate) fn summaries_for_owner(entries: &[AgentPanelEntry], owner: &str) -> Vec<WorkerSummary> {
    entries
        .iter()
        .filter(|entry| names(entry.owner.as_deref(), owner))
        .filter(|entry| has_summary(&entry.tokens))
        .map(|entry| WorkerSummary {
            ws_idx: entry.ws_idx,
            tab_idx: entry.tab_idx,
            pane_id: entry.pane_id,
            name: worker_display_name(entry),
            state: entry.state,
            seen: entry.seen,
            lines: summary_lines(&entry.tokens),
        })
        .collect()
}

/// How many of `owner`'s workers have published a summary.
///
/// The badge count. Kept beside [`summaries_for_owner`] so the number on the
/// row and the list behind it are produced by one filter.
pub(crate) fn summary_count_for_owner(entries: &[AgentPanelEntry], owner: &str) -> usize {
    entries
        .iter()
        .filter(|entry| names(entry.owner.as_deref(), owner))
        .filter(|entry| has_summary(&entry.tokens))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn worker(name: &str, owner: &str, pairs: &[(&str, &str)]) -> AgentPanelEntry {
        let mut entry = AgentPanelEntry::test_new(name);
        entry.owner = Some(owner.to_string());
        entry.tokens = tokens(pairs);
        entry
    }

    #[test]
    fn a_single_token_is_a_one_line_summary() {
        assert_eq!(
            summary_lines(&tokens(&[("summary", "rebased and green")])),
            vec!["rebased and green".to_string()]
        );
    }

    #[test]
    fn continuations_extend_the_summary_in_published_order() {
        let lines = summary_lines(&tokens(&[
            ("summary_3", "third"),
            ("summary", "first"),
            ("summary_2", "second"),
        ]));
        assert_eq!(lines, vec!["first", "second", "third"]);
    }

    #[test]
    fn a_gap_stops_the_continuation_rather_than_reordering_it() {
        // summary_2 missing: showing line 3 under line 1 would put the
        // publisher's words in an order it never wrote.
        let lines = summary_lines(&tokens(&[("summary", "first"), ("summary_3", "third")]));
        assert_eq!(lines, vec!["first"]);
    }

    #[test]
    fn no_summary_token_means_no_summary() {
        assert!(summary_lines(&tokens(&[("owner", "mate")])).is_empty());
        assert!(!has_summary(&tokens(&[("owner", "mate")])));
        // A whitespace-only value is a cleared token in every other reader too.
        assert!(!has_summary(&tokens(&[("summary", "   ")])));
        assert!(summary_lines(&tokens(&[("summary", "   ")])).is_empty());
    }

    #[test]
    fn scoping_returns_one_mates_workers_and_never_the_fleet() {
        let entries = vec![
            worker("worker-a1", "mate-alpha", &[("summary", "alpha one")]),
            worker("worker-a2", "mate-alpha", &[("summary", "alpha two")]),
            worker("worker-b1", "mate-beta", &[("summary", "beta one")]),
        ];

        let alpha = summaries_for_owner(&entries, "mate-alpha");
        assert_eq!(
            alpha.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["worker-a1", "worker-a2"]
        );
        assert_eq!(summary_count_for_owner(&entries, "mate-alpha"), 2);
        assert_eq!(summary_count_for_owner(&entries, "mate-beta"), 1);
        assert_eq!(summary_count_for_owner(&entries, "nobody"), 0);
    }

    #[test]
    fn a_worker_with_no_summary_is_neither_counted_nor_listed() {
        let entries = vec![
            worker("worker-a1", "mate-alpha", &[("summary", "did a thing")]),
            worker("worker-a2", "mate-alpha", &[]),
        ];
        assert_eq!(summary_count_for_owner(&entries, "mate-alpha"), 1);
        assert_eq!(summaries_for_owner(&entries, "mate-alpha").len(), 1);
    }

    #[test]
    fn an_empty_owner_never_matches_an_unowned_pane() {
        let mut orphan = AgentPanelEntry::test_new("stray");
        orphan.owner = None;
        orphan.tokens = tokens(&[("summary", "did a thing")]);
        assert_eq!(summary_count_for_owner(&[orphan], ""), 0);
    }

    #[test]
    fn owner_matching_ignores_surrounding_whitespace_like_the_tree_does() {
        let entries = vec![worker("worker-a1", "  mate-alpha  ", &[("summary", "x")])];
        assert_eq!(summary_count_for_owner(&entries, "mate-alpha"), 1);
    }

    #[test]
    fn the_handle_names_a_worker_ahead_of_the_tool_it_runs() {
        let mut entry = worker("worker-a1", "mate-alpha", &[("summary", "x")]);
        entry.agent_label = Some("claude".into());
        entry.pane_label = Some("pane 2".into());
        assert_eq!(worker_display_name(&entry), "worker-a1");

        // No handle: the pane's own label still tells two workers apart, which
        // the shared tool label never would.
        entry.agent_name = None;
        assert_eq!(worker_display_name(&entry), "pane 2");

        entry.pane_label = None;
        assert_eq!(worker_display_name(&entry), "claude");

        entry.agent_label = None;
        assert_eq!(worker_display_name(&entry), "worker");
    }

    #[test]
    fn a_blank_handle_falls_through_rather_than_naming_a_worker_nothing() {
        let mut entry = worker("worker-a1", "mate-alpha", &[("summary", "x")]);
        entry.agent_name = Some("   ".into());
        entry.pane_label = None;
        entry.agent_label = Some("claude".into());
        assert_eq!(worker_display_name(&entry), "claude");
    }

    #[test]
    fn an_unseen_idle_worker_reads_as_a_fresh_finish() {
        let mut entry = worker("worker-a1", "mate-alpha", &[("summary", "done")]);
        entry.state = AgentState::Idle;
        entry.seen = false;
        let summaries = summaries_for_owner(&[entry], "mate-alpha");
        assert!(summaries[0].is_unseen_finish());
    }

    #[test]
    fn continuation_scan_stops_at_the_token_ceiling() {
        // 40 published continuations, but a pane cannot hold that many tokens;
        // the scan must terminate at the ceiling rather than walk forever.
        let mut pairs = vec![("summary".to_string(), "first".to_string())];
        for n in 2..=40 {
            pairs.push((format!("summary_{n}"), format!("line {n}")));
        }
        let map: std::collections::HashMap<String, String> = pairs.into_iter().collect();
        assert_eq!(summary_lines(&map).len(), MAX_SUMMARY_LINES);
    }
}
