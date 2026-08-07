//! Animated resize-reflow for a pane's own terminal grid.
//!
//! A pane's PTY winsize and the ghostty-vt grid behind it already reflow the
//! instant they are told a new row/column count — that mechanism is not
//! ours to build, it is the terminal's own. What is missing is *pacing*: today
//! a layout change hands the runtime its final size in one step, so the
//! reflow happens in a single invisible frame. This module's whole job is to
//! feed the runtime a sequence of intermediate row/column counts instead of
//! the final one directly, so the same real reflow plays out over several
//! frames and reads as a transition rather than a snap.
//!
//! Deliberately not folded into [`crate::anim::Animator`]: that engine
//! resolves a *fixed* lifecycle (mount, settle, dismount) into per-cell paint,
//! and a pane's target size is a value that can retarget mid-flight — a
//! divider drag reports a new target every frame it moves. [`RelationSignals`]
//! is the closer precedent already in the fork: its own small hand-rolled
//! `advance`/`next_deadline` pair, held in [`crate::app::AppState`] and fed
//! into the same loop-deadline computation [`crate::anim::Animator`] feeds,
//! without going through it.
//!
//! Growing and shrinking are deliberately not symmetric, for a reason proven
//! the hard way rather than guessed: the frame this module resolves a size
//! for is drawn into a buffer already sized to the *target* rect for this
//! frame (the layout tree's own geometry is final immediately — nothing here
//! delays that), so an eased value can never exceed the target without
//! writing past that buffer. Growing is safe to ease because every
//! intermediate size sits between the old (smaller) size and the target, so
//! it is always `<=` the target. Shrinking has no equivalent safe range —
//! the old size is the *larger* one — so a shrinking axis resolves straight
//! to its target, the same way it did before this module existed. This is
//! the same asymmetry the fork's pane-materialise research already found:
//! opening is nearly free, closing needs a mechanism (a snapshot of content
//! that is about to go out of frame) this module does not have.
//!
//! [`RelationSignals`]: crate::app::relation_signal::RelationSignals

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::terminal::TerminalId;

/// How long a growing axis takes to ease from its old size to a newly
/// requested one.
///
/// Short enough that a fast succession of retargets (a divider drag) never
/// feels like it is lagging behind the mouse by more than a beat, long enough
/// that a single discrete resize — a split created or closed, a keybinding
/// step — reads as a transition rather than a flicker.
const RESIZE_REFLOW_DURATION: Duration = Duration::from_millis(220);

/// Finest a resize-reflow is ever redrawn at.
///
/// Matches the app's render floor rather than the coarser
/// [`crate::app::ANIMATION_INTERVAL`]: at that interval a reflow spanning more
/// than a couple of rows would visibly step rather than glide, the same
/// finding the pane-materialise spike made for its own reveal.
pub(crate) const RESIZE_REFLOW_FRAME_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy)]
struct GrowingTo {
    from: u16,
    to: u16,
    started_at: Instant,
}

impl GrowingTo {
    fn eased_progress(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.started_at).as_secs_f32()
            / RESIZE_REFLOW_DURATION.as_secs_f32();
        let t = t.clamp(0.0, 1.0);
        // Ease-out: fast, then settling — the pacing the fork already uses
        // for anything arriving (`Curve::EaseOut` in `crate::anim::behaviour`,
        // reproduced here rather than reached for across a `pub(crate)`
        // boundary that would only exist to serve this one call site).
        1.0 - (1.0 - t) * (1.0 - t)
    }

    /// The eased value, always in `self.from..=self.to` — never past `to`,
    /// which is what makes a growing axis safe to draw into a buffer already
    /// sized to `to` for this frame.
    fn value_at(&self, now: Instant) -> u16 {
        let t = self.eased_progress(now);
        let value = f32::from(self.from) + (f32::from(self.to) - f32::from(self.from)) * t;
        value.round().clamp(1.0, f32::from(self.to.max(1))) as u16
    }

    fn finished(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= RESIZE_REFLOW_DURATION
    }
}

#[derive(Debug, Clone, Copy)]
struct AxisReflow {
    /// Last value actually handed to the runtime on this axis — the ground
    /// truth this module resolves against, since it is also the only thing
    /// that decides what the runtime is told next.
    current: u16,
    /// Set while easing toward a larger value than `current`. Absent for a
    /// settled axis and never used for a shrink, which resolves in one step.
    growing: Option<GrowingTo>,
}

impl AxisReflow {
    fn new(current: u16) -> Self {
        Self {
            current,
            growing: None,
        }
    }

    fn resolve(&mut self, target: u16, now: Instant) -> u16 {
        if self.growing.is_none() && self.current == target {
            return target;
        }

        if let Some(growing) = self.growing {
            if growing.to == target {
                if growing.finished(now) {
                    self.current = target;
                    self.growing = None;
                    return target;
                }
                let visible = growing.value_at(now);
                self.current = visible;
                return visible;
            }
        }

        // Either settled at a value that is no longer the target, or
        // mid-flight toward a target that just moved: resolve fresh from
        // wherever this axis is visibly sitting right now, never from
        // history.
        let from = self.current;
        if target <= from {
            // Shrinking (or exactly arrived): no safe range to ease through,
            // since the frame this resolves for is already drawn into a
            // buffer sized to `target`. Snap, the same as before this module
            // existed.
            self.current = target;
            self.growing = None;
            return target;
        }
        self.growing = Some(GrowingTo {
            from,
            to: target,
            started_at: now,
        });
        from
    }

    fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.growing
            .is_some_and(|growing| !growing.finished(now))
            .then(|| now + RESIZE_REFLOW_FRAME_INTERVAL)
    }
}

#[derive(Debug, Clone, Copy)]
struct TrackedTerminal {
    rows: AxisReflow,
    cols: AxisReflow,
}

/// Per-terminal resize-reflow state.
///
/// Pull-based rather than ticked: [`Self::resolve`] is the only way anything
/// here changes, and it is called exactly where a pane's target size is
/// already being computed for this frame — so there is nothing to advance
/// independently of a render pass actually asking.
#[derive(Debug, Default)]
pub(crate) struct PaneResizeReflow {
    tracked: HashMap<TerminalId, TrackedTerminal>,
}

impl PaneResizeReflow {
    /// The size a terminal's grid should actually be resized to *this frame*,
    /// given the layout wants `target_rows`/`target_cols`.
    ///
    /// Rows and columns resolve independently, because one axis can grow
    /// while the other shrinks — a pane getting taller and narrower at once
    /// is one layout change, not two. The first time a terminal is seen there
    /// is nothing to ease from, so both axes resolve straight to their
    /// target, exactly as they did before this mechanism existed. Only a
    /// growing axis whose target has changed animates; an unchanged target —
    /// the overwhelmingly common case, most frames — is a couple of
    /// comparisons and nothing more.
    pub(crate) fn resolve(
        &mut self,
        terminal_id: TerminalId,
        target_rows: u16,
        target_cols: u16,
        now: Instant,
    ) -> (u16, u16) {
        let tracked = self.tracked.entry(terminal_id).or_insert(TrackedTerminal {
            rows: AxisReflow::new(target_rows),
            cols: AxisReflow::new(target_cols),
        });
        (
            tracked.rows.resolve(target_rows, now),
            tracked.cols.resolve(target_cols, now),
        )
    }

    /// Earliest moment any tracked terminal's resolved size would change, or
    /// `None` when nothing is easing.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.tracked
            .values()
            .filter_map(|tracked| {
                tracked
                    .rows
                    .next_deadline(now)
                    .into_iter()
                    .chain(tracked.cols.next_deadline(now))
                    .min()
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> TerminalId {
        // `TerminalId` deliberately has no public test constructor beyond
        // `alloc()`; equality here only needs two calls to disagree, which
        // `alloc()` already guarantees, so it is used directly rather than
        // adding a parsing/`From<&str>` surface that would only exist for
        // this test module.
        TerminalId::alloc()
    }

    #[test]
    fn a_terminal_seen_for_the_first_time_resolves_straight_to_target() {
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        assert_eq!(reflow.resolve(id(), 40, 100, now), (40, 100));
    }

    #[test]
    fn growing_eases_rather_than_snapping() {
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        let terminal = id();
        reflow.resolve(terminal.clone(), 20, 100, now);

        // The very frame the target changes still resolves to the old size:
        // the ease begins on the *next* frame, not this one.
        let first = reflow.resolve(terminal.clone(), 40, 100, now);
        assert_eq!(first, (20, 100));

        let mid = reflow.resolve(terminal.clone(), 40, 100, now + RESIZE_REFLOW_DURATION / 2);
        assert!(
            mid.0 > 20 && mid.0 < 40,
            "half-way through should sit strictly between old and new: {mid:?}"
        );

        let done = reflow.resolve(terminal, 40, 100, now + RESIZE_REFLOW_DURATION);
        assert_eq!(done, (40, 100));
    }

    #[test]
    fn shrinking_snaps_immediately_never_overshooting_the_new_smaller_bound() {
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        let terminal = id();
        reflow.resolve(terminal.clone(), 40, 100, now);

        // A shrink resolves to its target on the very frame it is issued —
        // never a size larger than the target, which is the one thing that
        // would draw past a buffer already sized to it.
        let shrunk = reflow.resolve(terminal.clone(), 20, 100, now);
        assert_eq!(shrunk, (20, 100));
        assert_eq!(reflow.next_deadline(now), None);
    }

    #[test]
    fn one_axis_can_grow_while_the_other_shrinks() {
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        let terminal = id();
        reflow.resolve(terminal.clone(), 20, 100, now);

        // Rows grow (20 -> 40), columns shrink (100 -> 50): each axis must
        // follow its own rule independently.
        let first = reflow.resolve(terminal, 40, 50, now);
        assert_eq!(first, (20, 50), "rows still easing, cols already snapped");
    }

    #[test]
    fn a_retarget_mid_growth_restarts_from_where_it_visibly_is() {
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        let terminal = id();
        reflow.resolve(terminal.clone(), 20, 100, now);
        reflow.resolve(terminal.clone(), 40, 100, now);

        let partway = now + RESIZE_REFLOW_DURATION / 2;
        let visible_before_retarget = reflow.resolve(terminal.clone(), 40, 100, partway);
        assert!(visible_before_retarget.0 < 40);

        // The target grows again before the first ease finished. The very
        // next resolve must not jump back to 20 (the original start) or snap
        // straight to the new target — it has to continue from what was just
        // visible.
        let just_after_retarget = reflow.resolve(terminal, 50, 100, partway);
        assert_eq!(
            just_after_retarget, visible_before_retarget,
            "a retarget must not move the pane on the frame it is issued"
        );
    }

    #[test]
    fn next_deadline_is_armed_only_while_something_is_growing() {
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        assert_eq!(reflow.next_deadline(now), None);

        let terminal = id();
        reflow.resolve(terminal.clone(), 20, 100, now);
        assert_eq!(
            reflow.next_deadline(now),
            None,
            "first sight of a terminal settles immediately; nothing to wake for"
        );

        reflow.resolve(terminal.clone(), 40, 100, now);
        assert_eq!(
            reflow.next_deadline(now),
            Some(now + RESIZE_REFLOW_FRAME_INTERVAL)
        );

        reflow.resolve(terminal, 40, 100, now + RESIZE_REFLOW_DURATION);
        assert_eq!(
            reflow.next_deadline(now + RESIZE_REFLOW_DURATION),
            None,
            "settled again; the loop should stop waking for this terminal"
        );
    }

    #[test]
    fn an_eased_value_never_exceeds_its_target() {
        // The property the buffer-overflow this module exists to prevent
        // actually depends on: at no point in a growth ease may the
        // resolved value be larger than the target it is easing toward.
        let now = Instant::now();
        let mut reflow = PaneResizeReflow::default();
        let terminal = id();
        reflow.resolve(terminal.clone(), 10, 10, now);
        reflow.resolve(terminal.clone(), 90, 90, now);

        let mut t = Duration::ZERO;
        while t <= RESIZE_REFLOW_DURATION {
            let (rows, cols) = reflow.resolve(terminal.clone(), 90, 90, now + t);
            assert!(
                rows <= 90 && cols <= 90,
                "overshot at {t:?}: ({rows}, {cols})"
            );
            t += RESIZE_REFLOW_FRAME_INTERVAL;
        }
    }
}
