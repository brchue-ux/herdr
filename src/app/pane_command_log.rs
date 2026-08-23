//! A per-pane, capped log of the shell commands a Claude Code pane has run.
//!
//! # Why this is not [`crate::app::status_feed::StatusFeed`]
//!
//! `StatusFeed` already carries agent-observed commands, but as one global
//! eight-line stream shared by every pane in the session, labeled per line so
//! a reader can tell panes apart. This is the opposite shape on purpose: the
//! terminal triview's bottom zone lives *inside* one specific pane, so its
//! log only ever needs that pane's own commands, unlabeled — the pane it is
//! drawn in already says whose activity this is.
//!
//! # Why this is a session fact, not TUI presentation state
//!
//! Per this project's runtime/client boundary rule: what a pane's wrapped
//! agent actually did is a **session fact** — the same reasoning
//! `StatusFeed`'s own header gives — so it lives here in
//! [`crate::app::state::AppState`], keyed by [`crate::layout::PaneId`], not in
//! the client-only render path that happens to draw it.
//!
//! # Memory cannot outlive the pane
//!
//! Mirrors [`crate::app::pane_unread::PaneUnreadTracker`]'s own rule: a
//! closed pane's entry is dropped via [`Self::remove`], though a stale entry
//! left behind (a pane closed through some path that does not call it) is
//! never read again and costs at most [`PANE_COMMAND_LOG_MAX`] short strings.
//!
//! # Retention is not the same number as the zone's own height
//!
//! The terminal triview's bottom zone is a fixed
//! `CLAUDE_TRIVIEW_LOG_ROWS` rows regardless of how many
//! commands this log holds, and scrolls internally to reach the rest — see
//! that constant's own doc. [`PANE_COMMAND_LOG_MAX`] is only how much history
//! is worth keeping around for that scroll to reach; it is deliberately far
//! bigger than the zone's own height.

use std::collections::{HashMap, VecDeque};

use crate::layout::PaneId;

/// How many commands one pane's log retains, oldest evicted first. Sized for
/// a long session's worth of scrollable history rather than for the visible
/// zone height — at a few dozen bytes per command this is a trivial memory
/// cost per pane.
pub(crate) const PANE_COMMAND_LOG_MAX: usize = 500;

/// Every pane's own capped command history, oldest evicted first.
#[derive(Debug, Clone, Default)]
pub(crate) struct PaneCommandLog {
    lines: HashMap<PaneId, VecDeque<String>>,
}

impl PaneCommandLog {
    /// Appends `command` to `pane_id`'s log, evicting the oldest line once
    /// the cap is reached. Empty text is dropped rather than stored — never
    /// observed today ([`crate::detect::command_marker::bash_command_text`]
    /// always has content when a marker matched), but an empty row would
    /// still be a blank line the bottom zone has to draw.
    pub(crate) fn record(&mut self, pane_id: PaneId, command: String) {
        if command.is_empty() {
            return;
        }
        let lines = self.lines.entry(pane_id).or_default();
        if lines.len() == PANE_COMMAND_LOG_MAX {
            lines.pop_front();
        }
        lines.push_back(command);
    }

    /// `pane_id`'s log, oldest first — the order the bottom zone draws
    /// top-to-bottom so the newest command lands on the zone's own bottom
    /// row.
    pub(crate) fn lines(&self, pane_id: PaneId) -> impl Iterator<Item = &str> {
        self.lines
            .get(&pane_id)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// Drops a closed pane's log. See this module's own header on why a
    /// caller skipping this is not a correctness problem, only a hygiene one.
    pub(crate) fn remove(&mut self, pane_id: PaneId) {
        self.lines.remove(&pane_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(n: u32) -> PaneId {
        PaneId::from_raw(n)
    }

    #[test]
    fn records_are_returned_oldest_first() {
        let mut log = PaneCommandLog::default();
        log.record(pane(1), "npm test".to_string());
        log.record(pane(1), "git status".to_string());
        assert_eq!(
            log.lines(pane(1)).collect::<Vec<_>>(),
            vec!["npm test", "git status"]
        );
    }

    #[test]
    fn caps_retention_and_evicts_the_oldest() {
        let mut log = PaneCommandLog::default();
        let overflow = PANE_COMMAND_LOG_MAX + 2;
        for i in 0..overflow {
            log.record(pane(1), format!("cmd {i}"));
        }
        let lines: Vec<&str> = log.lines(pane(1)).collect();
        assert_eq!(lines.len(), PANE_COMMAND_LOG_MAX);
        assert_eq!(lines.first(), Some(&"cmd 2"));
        let expected_last = format!("cmd {}", overflow - 1);
        assert_eq!(lines.last(), Some(&expected_last.as_str()));
    }

    #[test]
    fn panes_do_not_share_a_log() {
        let mut log = PaneCommandLog::default();
        log.record(pane(1), "npm test".to_string());
        assert!(log.lines(pane(2)).next().is_none());
    }

    #[test]
    fn empty_commands_are_not_recorded() {
        let mut log = PaneCommandLog::default();
        log.record(pane(1), String::new());
        assert!(log.lines(pane(1)).next().is_none());
    }

    #[test]
    fn removing_a_pane_drops_its_log() {
        let mut log = PaneCommandLog::default();
        log.record(pane(1), "npm test".to_string());
        log.remove(pane(1));
        assert!(log.lines(pane(1)).next().is_none());
    }
}
