//! Output-scoped unread latch.
//!
//! Whether a pane's `seen` bit should flip to unread is decided purely by
//! whether new PTY content has arrived since it was last polled while its tab
//! was not the active one — never by `AgentState` transitions. That keeps a
//! plain shell (no agent) and a `Working`/`Blocked` pane able to go unread the
//! same way an `Idle` one always could.
//!
//! This is a **leading-edge latch**, not a debounce: the moment new content is
//! observed on a backgrounded, currently-seen pane, `seen` flips to `false`
//! and stays there until the pane is viewed again. A trailing-edge debounce
//! (wait for quiet, then mark unread) was considered and rejected — it would
//! never mark a continuously-streaming background pane unread until it
//! paused, which just re-derives completion-scoping in a fuzzier disguise.
//!
//! [`PaneUnreadTracker`] only tracks the baseline `detection_content_seq`
//! reading per terminal, polled on the existing per-pane detection task's
//! ~300ms cadence rather than a new interval. The actual `seen` bit lives on
//! `PaneState` and is mutated by the caller: this module has no access to
//! tab/workspace visibility, so it cannot decide the flip on its own.
//!
//! The baseline is refreshed on every poll regardless of a pane's current
//! `seen` value, rather than frozen while unread and reset on view by
//! `mark_active_tab_seen`. Keeping the baseline continuously fresh gets the
//! same correctness a freeze-and-reset scheme would — re-viewing a pane never
//! re-latches unread against a stale backlog, only against content that
//! arrived in the one poll window since — without needing every call site
//! that clears `seen` to also remember to reset this tracker. The cost is a
//! per-tick atomic load and hashmap update for panes that are already unread,
//! which this codebase already treats as cheap (see `output_bytes`/
//! `PaneActivityMap` doing the same per-pane, per-tick work). The
//! side-effecting half of the latch — actually flipping `seen` and whatever
//! redraw/resort that implies — still only fires once: the caller
//! (`AppState::observe_pane_unread`) gates the write on `pane.seen` already
//! being `true`, so an already-unread pane's repeated "changed" readings are
//! no-ops.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::terminal::TerminalId;

/// How often the latch compares `detection_content_seq` against its last
/// reading. Matches the base cadence of the per-pane detection task
/// (`spawn_basic_detection_task`, `src/pane.rs`) rather than inventing a new
/// interval for a purely state-side concern.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Tracks each terminal's last-observed `detection_content_seq` for the
/// unread latch.
#[derive(Debug, Default)]
pub(crate) struct PaneUnreadTracker {
    last_seq: HashMap<TerminalId, u64>,
    next_poll: Option<Instant>,
}

impl PaneUnreadTracker {
    /// Whether enough time has passed since the last poll to check again.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        self.next_poll.is_none_or(|next| now >= next)
    }

    /// Record that a poll happened at `now` and arm the next one.
    pub(crate) fn mark_polled(&mut self, now: Instant) {
        self.next_poll = Some(now + POLL_INTERVAL);
    }

    /// Compare `current_seq` against the last reading for `terminal_id`,
    /// update the baseline, and report whether it moved.
    ///
    /// The first observation of a terminal only establishes the baseline —
    /// it is never itself "new content," or every pane would latch unread on
    /// the poll right after it was created.
    pub(crate) fn observe(&mut self, terminal_id: &TerminalId, current_seq: u64) -> bool {
        match self.last_seq.insert(terminal_id.clone(), current_seq) {
            Some(last) => last != current_seq,
            None => false,
        }
    }

    /// Drop a terminal this tracker no longer needs to watch, e.g. because
    /// its pane closed. Not required for correctness — a stale entry for a
    /// gone terminal id is simply never read again — but keeps the map from
    /// growing across a long session's worth of closed panes.
    pub(crate) fn remove(&mut self, terminal_id: &TerminalId) {
        self.last_seq.remove(terminal_id);
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.last_seq.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> TerminalId {
        TerminalId::alloc()
    }

    #[test]
    fn first_observation_establishes_baseline_without_flagging_content_changed() {
        let mut tracker = PaneUnreadTracker::default();
        let id = terminal();
        assert!(!tracker.observe(&id, 7));
    }

    #[test]
    fn a_later_seq_reads_as_changed_exactly_once() {
        let mut tracker = PaneUnreadTracker::default();
        let id = terminal();
        tracker.observe(&id, 1);
        assert!(tracker.observe(&id, 2));
        assert!(!tracker.observe(&id, 2), "unchanged seq must not re-flag");
    }

    #[test]
    fn removing_a_terminal_drops_its_baseline() {
        let mut tracker = PaneUnreadTracker::default();
        let id = terminal();
        tracker.observe(&id, 1);
        tracker.remove(&id);
        assert!(tracker.is_empty());
    }

    #[test]
    fn polling_is_gated_by_the_interval() {
        let mut tracker = PaneUnreadTracker::default();
        let now = Instant::now();
        assert!(tracker.is_due(now), "never polled must be due immediately");
        tracker.mark_polled(now);
        assert!(!tracker.is_due(now + Duration::from_millis(50)));
        assert!(tracker.is_due(now + POLL_INTERVAL));
    }
}
