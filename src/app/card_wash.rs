//! Turning "this card's state changed" into one bounded sweep across it.
//!
//! A state change is a *transition*, and a transition is the one thing a pure
//! render pass cannot see: it is handed the state a card is in now, never the
//! state it was in a moment ago. So the previous state has to be remembered,
//! exactly the way [`crate::app::signal_tray`] remembers the previous
//! magnitudes it needs to tell an escalation from a standing count.
//!
//! What is remembered is deliberately small: the state each drawn card was last
//! seen in, and — while a sweep is still running — the state it left and when it
//! left it. Everything else about the wash lives in the animation engine, which
//! already owns bounded lifetimes: the sweep is a mount stage playing
//! [`crate::anim::behaviour::names::CARD_WASH`], and it ends because the mount
//! ends, not because anything here counts frames.
//!
//! Three properties this module is responsible for holding:
//!
//! - **A card that has just arrived does not wash.** The first time a row is
//!   seen its state is recorded with no wash behind it. Otherwise every card in
//!   the tree would sweep on the frame Herdr started, and on every frame a
//!   sidebar was reopened.
//! - **A second change restarts the sweep rather than being absorbed.** The
//!   change is part of the element's *name*
//!   ([`crate::anim::ElementId::CardWash`]), so a card that changes again while
//!   washing publishes a different element: the one in flight falls out of
//!   membership and retires, and the new one mounts. Nothing here has to reach
//!   into the engine to restart anything, which is what keeps the engine's
//!   "membership is the whole lifecycle" contract intact.
//! - **Memory cannot outlive the tree.** Rows absent from a pass are dropped, so
//!   a fleet that churns panes does not accumulate a state per pane that ever
//!   existed.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::anim::behaviour::{names, DriveInputs};
use crate::anim::{CardRow, CardWash, ElementId, Lifecycle, Stage};
use crate::detect::AgentState;

/// What one card's state was last seen to be, and the sweep it is still in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Remembered {
    state: AgentState,
    /// The state this card left, and when — only while the sweep from it is
    /// still due to be running.
    washing: Option<(AgentState, Instant)>,
}

/// Every drawn card's last seen state, and the washes running right now.
#[derive(Debug, Clone, Default)]
pub(crate) struct CardWashes {
    rows: HashMap<CardRow, Remembered>,
}

impl CardWashes {
    /// The life a wash is given: one bounded sweep and nothing else.
    ///
    /// No idle behaviour on purpose. A wash is an *event*, so when its mount
    /// finishes there is nothing left for it to be — the element settles still,
    /// and the same pass that let its deadline expire stops publishing it, so it
    /// retires on the next reconcile. A consumer therefore reads a live wash as
    /// "mounting with a behaviour", exactly the way the view switch does.
    pub(crate) fn lifecycle(duration: Duration) -> Lifecycle {
        Lifecycle::still().with_mount(Stage::new(names::CARD_WASH, duration))
    }

    /// Fold this pass's card states in and return the washes that are live.
    ///
    /// `live` is every card the tree is drawing right now, in any order. The
    /// returned membership is what [`crate::anim::Animator::observe`] is handed
    /// for [`crate::anim::Family::CardWash`], so a wash whose window has closed
    /// simply stops being published and the engine retires it.
    pub(crate) fn observe(
        &mut self,
        now: Instant,
        window: Duration,
        live: impl IntoIterator<Item = (CardRow, AgentState)>,
    ) -> Vec<(ElementId, DriveInputs)> {
        let mut members = Vec::new();
        let mut seen: Vec<CardRow> = Vec::new();
        for (row, state) in live {
            let remembered = self.rows.entry(row.clone()).or_insert(Remembered {
                state,
                washing: None,
            });
            if remembered.state != state {
                remembered.washing = Some((remembered.state, now));
                remembered.state = state;
            }
            // Expired sweeps are cleared here rather than left to be filtered on
            // read, so a tree that stops changing stops holding anything.
            if remembered
                .washing
                .is_some_and(|(_, started)| now.saturating_duration_since(started) >= window)
            {
                remembered.washing = None;
            }
            if let Some((from, _)) = remembered.washing {
                members.push((
                    ElementId::CardWash(CardWash {
                        row: row.clone(),
                        from,
                        into: state,
                    }),
                    DriveInputs::default(),
                ));
            }
            seen.push(row);
        }
        self.rows.retain(|row, _| seen.contains(row));
        members
    }

    /// The wash crossing this row right now, as the element that plays it.
    ///
    /// The renderer's half: it knows which card it is drawing and what state
    /// that card is in, and this is the only thing it cannot derive — which
    /// state the card came *from*, and therefore which element to read.
    pub(crate) fn live(&self, row: &CardRow) -> Option<CardWash> {
        let remembered = self.rows.get(row)?;
        let (from, _) = remembered.washing?;
        Some(CardWash {
            row: row.clone(),
            from,
            into: remembered.state,
        })
    }

    /// Drop every card's memory.
    ///
    /// For the same moment [`crate::anim::Animator::forget_all`] exists for: a
    /// host that has stopped drawing. Keeping the states across that would make
    /// the first frame after a client reattaches wash every card whose state
    /// moved while nobody was looking, which is animating history.
    ///
    /// Returns whether there was anything to forget.
    pub(crate) fn forget_all(&mut self) -> bool {
        let had_any = !self.rows.is_empty();
        self.rows.clear();
        had_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> CardRow {
        CardRow::Space("one".to_string())
    }

    fn ids(members: &[(ElementId, DriveInputs)]) -> Vec<&ElementId> {
        members.iter().map(|(id, _)| id).collect()
    }

    const WINDOW: Duration = Duration::from_millis(500);

    #[test]
    fn a_card_seen_for_the_first_time_does_not_wash() {
        let mut washes = CardWashes::default();
        let now = Instant::now();
        let members = washes.observe(now, WINDOW, [(row(), AgentState::Working)]);
        assert!(
            members.is_empty(),
            "a card arriving is not a card changing, and washing every row on \
             the first frame is what that would look like"
        );
        assert_eq!(washes.live(&row()), None);
    }

    #[test]
    fn a_state_change_publishes_one_wash_carrying_both_states() {
        let mut washes = CardWashes::default();
        let now = Instant::now();
        washes.observe(now, WINDOW, [(row(), AgentState::Idle)]);
        let members = washes.observe(now, WINDOW, [(row(), AgentState::Working)]);
        assert_eq!(
            ids(&members),
            vec![&ElementId::CardWash(CardWash {
                row: row(),
                from: AgentState::Idle,
                into: AgentState::Working,
            })]
        );
        assert_eq!(
            washes.live(&row()),
            Some(CardWash {
                row: row(),
                from: AgentState::Idle,
                into: AgentState::Working,
            })
        );
    }

    #[test]
    fn a_wash_stops_being_published_once_its_window_has_closed() {
        let mut washes = CardWashes::default();
        let now = Instant::now();
        washes.observe(now, WINDOW, [(row(), AgentState::Idle)]);
        washes.observe(now, WINDOW, [(row(), AgentState::Working)]);
        let members = washes.observe(now + WINDOW, WINDOW, [(row(), AgentState::Working)]);
        assert!(members.is_empty());
        assert_eq!(washes.live(&row()), None);
    }

    /// The restart contract. A second change mid-sweep has to become a
    /// *different* element, because the engine deliberately never restarts one
    /// that is still in its membership set.
    #[test]
    fn a_change_mid_wash_names_a_different_element() {
        let mut washes = CardWashes::default();
        let now = Instant::now();
        washes.observe(now, WINDOW, [(row(), AgentState::Idle)]);
        let first = washes.observe(now, WINDOW, [(row(), AgentState::Working)]);
        let second = washes.observe(
            now + Duration::from_millis(100),
            WINDOW,
            [(row(), AgentState::Blocked)],
        );
        assert_ne!(ids(&first), ids(&second));
        assert_eq!(
            washes.live(&row()),
            Some(CardWash {
                row: row(),
                from: AgentState::Working,
                into: AgentState::Blocked,
            }),
            "the second sweep carries the state the card actually left, not the \
             one the first sweep started from"
        );
    }

    #[test]
    fn a_row_that_leaves_the_tree_takes_its_memory_with_it() {
        let mut washes = CardWashes::default();
        let now = Instant::now();
        washes.observe(now, WINDOW, [(row(), AgentState::Idle)]);
        washes.observe(now, WINDOW, []);
        // Back again, in a different state: it is a new card as far as this is
        // concerned, so it arrives rather than washing.
        let members = washes.observe(now, WINDOW, [(row(), AgentState::Working)]);
        assert!(members.is_empty());
    }

    #[test]
    fn two_rows_wash_independently() {
        let mut washes = CardWashes::default();
        let now = Instant::now();
        let other = CardRow::Agent(crate::layout::PaneId::alloc());
        let live = |a, b| [(row(), a), (other.clone(), b)];
        washes.observe(now, WINDOW, live(AgentState::Idle, AgentState::Idle));
        let members = washes.observe(now, WINDOW, live(AgentState::Idle, AgentState::Working));
        assert_eq!(members.len(), 1);
        assert_eq!(washes.live(&row()), None);
        assert!(washes.live(&other).is_some());
    }
}
