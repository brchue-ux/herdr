//! Turning "a shell command just ran on this card" into one or more
//! independent floating acknowledgement markers.
//!
//! Deciding *whether* a command is genuinely new is not this module's job —
//! that evidence-gathering (reading the pane's screen, remembering which
//! command lines were already there, telling a burst of commands apart) lives
//! on the background detection task that already reads a pane's screen on its
//! own clock (`spawn_basic_detection_task` in [`crate::pane`]), the same place
//! [`crate::detect::AgentState`] itself is decided. This module starts once
//! that task has already decided "yes, this one is new" and published it as
//! an [`crate::events::AppEvent::CommandAcknowledged`]: from here it is purely
//! about giving each acknowledgement its own bounded, independently-animating
//! life and forgetting it once that life is over.
//!
//! **Multiplicity is the one place this deliberately does not mirror
//! [`crate::app::card_wash::CardWashes`].** A wash is "the" state a card is
//! in, so a second change correctly replaces the first mid-sweep. A command
//! ack is an event, not a state, and the captain's own call was that a burst
//! of N commands is N independent markers, each ticking its own slot-machine
//! settle — never a single marker with a counter on it. So where `CardWashes`
//! remembers one thing per card, this remembers a *set*, keyed by a sequence
//! number nothing else shares, so two acknowledgements arriving close
//! together mount as two elements instead of one restarting the other.
//!
//! **Memory cannot outlive the tree.** Rows absent from a pass are dropped,
//! the same rule `CardWashes` follows and for the same reason: a fleet that
//! churns panes must not accumulate a memory per pane that ever existed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::anim::behaviour::{names, DriveInputs};
use crate::anim::{CardRow, CmdAck, ElementId, Lifecycle, Stage};

/// Every drawn card's live command-acknowledgement instances, each a sequence
/// number and when it was recorded.
#[derive(Debug, Clone, Default)]
pub(crate) struct CmdAcks {
    rows: HashMap<CardRow, Vec<(u64, Instant)>>,
    next_seq: u64,
}

impl CmdAcks {
    /// The life one marker instance is given: a snap-in mount and a fade-out
    /// dismount, nothing declared in between.
    ///
    /// No idle behaviour, and that is deliberate rather than an omission: once
    /// the mount ends there is nothing left for the *engine* to animate,
    /// because the hold is not a phase this module asks it to play — it is
    /// simply how long [`Self::observe`] keeps publishing the marker as live
    /// before letting it fall out of membership. The renderer draws the held
    /// glyph at its own settled style, which is exactly what an element with
    /// no idle behaviour paints as: nothing on top of it, so the glyph reads
    /// as holding right where the snap left it.
    pub(crate) fn lifecycle(mount: Duration, dismount: Duration) -> Lifecycle {
        Lifecycle::still()
            .with_mount(Stage::new(names::CMD_ACK, mount))
            .with_dismount(Stage::new(names::FADE, dismount))
    }

    /// Record one freshly detected command as a new, independent marker on
    /// `row`.
    ///
    /// Never restarts or replaces an existing marker on the same row — see
    /// this module's own header on why that is the one place it departs from
    /// [`crate::app::card_wash::CardWashes`]. A caller recording several
    /// commands in the same pass simply calls this once per command.
    pub(crate) fn record(&mut self, row: CardRow, now: Instant) {
        self.next_seq += 1;
        self.rows.entry(row).or_default().push((self.next_seq, now));
    }

    /// True when at least one card has a marker still live.
    ///
    /// The cheap check a caller uses to decide whether it is worth paying for
    /// [`Self::observe`]'s own pass at all — see
    /// [`crate::app::runtime::App::advance_animations`], which folds this into
    /// the same "is anything actually happening" gate every other sidebar
    /// animation family already answers.
    pub(crate) fn any_live(&self) -> bool {
        self.rows.values().any(|instances| !instances.is_empty())
    }

    /// Prune markers this module no longer has any reason to remember and
    /// rows no longer in the tree, and return the marker instances still
    /// worth asking the animation engine to keep alive.
    ///
    /// `live_rows` is every agent card the tree is drawing right now.
    /// `active_window` is how long a recorded marker stays in the returned
    /// membership — once it ages out, this module simply stops re-asserting
    /// it, which is what lets [`crate::anim::Animator::admit`] read the
    /// marker as departed and start its dismount stage. `retain_window` is
    /// how long *this module's own memory* holds onto the marker after that:
    /// it must outlast `active_window` by at least the dismount's own
    /// duration, or [`Self::live`] would stop mentioning a marker to the
    /// renderer while it is still visibly fading out — the engine plays the
    /// exit, but only a renderer that keeps asking for a frame ever draws it.
    ///
    /// The returned membership is what [`crate::anim::Animator::observe`] is
    /// handed for [`crate::anim::Family::CmdAck`].
    pub(crate) fn observe(
        &mut self,
        now: Instant,
        active_window: Duration,
        retain_window: Duration,
        live_rows: impl IntoIterator<Item = CardRow>,
    ) -> Vec<(ElementId, DriveInputs)> {
        let seen: Vec<CardRow> = live_rows.into_iter().collect();
        self.rows.retain(|row, _| seen.contains(row));
        let mut members = Vec::new();
        for (row, instances) in &mut self.rows {
            instances
                .retain(|(_, started)| now.saturating_duration_since(*started) < retain_window);
            for (seq, started) in instances.iter() {
                if now.saturating_duration_since(*started) < active_window {
                    members.push((
                        ElementId::CmdAck(CmdAck {
                            row: row.clone(),
                            seq: *seq,
                        }),
                        DriveInputs::default(),
                    ));
                }
            }
        }
        members
    }

    /// The marker instances this module still remembers on this row, oldest
    /// first — including one whose active window has closed and is now
    /// playing its dismount, so a renderer must not assume every id here is
    /// still snapped fully in.
    ///
    /// The renderer's half: it knows which card it is drawing and asks this
    /// for the sequence numbers to resolve against the animation engine.
    pub(crate) fn live(&self, row: &CardRow) -> impl Iterator<Item = u64> + '_ {
        self.rows
            .get(row)
            .into_iter()
            .flat_map(|instances| instances.iter().map(|(seq, _)| *seq))
    }

    /// Drop every card's memory.
    ///
    /// For the same moment [`crate::anim::Animator::forget_all`] exists for: a
    /// host that has stopped drawing. The engine holds presentation state
    /// only, so forgetting it loses nothing true — a marker nobody was
    /// watching for is not owed a repaint once a client reattaches.
    ///
    /// Returns whether there was anything to forget.
    pub(crate) fn forget_all(&mut self) -> bool {
        let had_any = self.any_live();
        self.rows.clear();
        had_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> CardRow {
        CardRow::Agent(crate::layout::PaneId::alloc())
    }

    const ACTIVE: Duration = Duration::from_millis(500);
    // ACTIVE plus a dismount stage's own duration — see `Self::observe`'s doc
    // on why `retain_window` must outlast `active_window`.
    const RETAIN: Duration = Duration::from_millis(700);

    #[test]
    fn a_recorded_marker_is_published_until_its_active_window_closes() {
        let mut acks = CmdAcks::default();
        let now = Instant::now();
        let mine = row();
        acks.record(mine.clone(), now);
        assert!(acks.any_live());
        let members = acks.observe(now, ACTIVE, RETAIN, [mine.clone()]);
        assert_eq!(members.len(), 1);
        assert_eq!(acks.live(&mine).count(), 1);

        // Past the active window it stops being republished — which is what
        // lets the engine read it as departed and start its own dismount —
        // but this module still remembers it.
        let members = acks.observe(now + ACTIVE, ACTIVE, RETAIN, [mine.clone()]);
        assert!(members.is_empty());
        assert_eq!(
            acks.live(&mine).count(),
            1,
            "still remembered while its dismount stage would be playing"
        );

        // Only once the dismount itself would have finished does this module
        // let it go.
        acks.observe(now + RETAIN, ACTIVE, RETAIN, [mine.clone()]);
        assert_eq!(acks.live(&mine).count(), 0);
        assert!(!acks.any_live());
    }

    #[test]
    fn a_burst_of_commands_is_one_instance_each_not_a_counter() {
        let mut acks = CmdAcks::default();
        let now = Instant::now();
        let mine = row();
        acks.record(mine.clone(), now);
        acks.record(mine.clone(), now);
        acks.record(mine.clone(), now);
        let members = acks.observe(now, ACTIVE, RETAIN, [mine.clone()]);
        assert_eq!(members.len(), 3, "three commands is three instances");
        let live: Vec<u64> = acks.live(&mine).collect();
        assert_eq!(live.len(), 3);
        // Independent identities: no two share a sequence number.
        assert_eq!(
            live.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
    }

    #[test]
    fn a_row_that_leaves_the_tree_takes_its_memory_with_it() {
        let mut acks = CmdAcks::default();
        let now = Instant::now();
        let mine = row();
        acks.record(mine.clone(), now);
        acks.observe(now, ACTIVE, RETAIN, []);
        assert_eq!(acks.live(&mine).count(), 0);
    }

    #[test]
    fn two_cards_ack_independently() {
        let mut acks = CmdAcks::default();
        let now = Instant::now();
        let mine = row();
        let other = row();
        acks.record(mine.clone(), now);
        let members = acks.observe(now, ACTIVE, RETAIN, [mine.clone(), other.clone()]);
        assert_eq!(members.len(), 1);
        assert_eq!(acks.live(&mine).count(), 1);
        assert_eq!(acks.live(&other).count(), 0);
    }

    #[test]
    fn forget_all_reports_whether_there_was_anything_to_forget() {
        let mut acks = CmdAcks::default();
        assert!(!acks.forget_all());
        acks.record(row(), Instant::now());
        assert!(acks.forget_all());
        assert!(!acks.any_live());
    }
}
