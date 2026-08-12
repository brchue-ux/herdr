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
//! Growth pacing is currently **off** — see [`GROW_EASE_ENABLED`] for why and
//! for how to put it back. With it off this module still owns the same call
//! site and still resolves both axes; it simply resolves a growing one straight
//! to its target, so a pane reaches its final size on the frame the layout
//! changes.
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

/// Whether a growing axis is paced at all.
///
/// Off. A terminal grid is drawn top-aligned into its pane's rect while a
/// terminal's content is bottom-anchored, so every intermediate size lifts the
/// newest output off the bottom of the pane and parks it near the top with dead
/// space below, then walks it back down as the ease completes — most visibly
/// when a sibling pane closes and the survivor grows into the freed space. That
/// artifact is the transition, not a flaw in it, so the pacing is disabled
/// rather than reshaped: a growing axis resolves straight to its target the
/// same way a shrinking one always has, and nothing is drawn at an intermediate
/// size.
///
/// The mechanism below is kept whole behind this one flag. Flipping it back to
/// `true` restores the eased behaviour exactly, and the tests covering that
/// regime run against it directly through
/// [`PaneResizeReflow::with_grow_ease_for_test`].
const GROW_EASE_ENABLED: bool = false;

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
    /// Last value this module handed the runtime on this axis.
    ///
    /// Deliberately *not* treated as the grid's real size: this module is not
    /// the only thing that resizes a pane, so the two agree only until
    /// something else writes the runtime. [`Self::resolve`] takes the grid's
    /// own size alongside the target and plans from that instead; `current` is
    /// only what tells it whether a flight in progress is still the one the
    /// grid is actually flying.
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

    /// The size this axis's grid should be this frame, given the layout wants
    /// `target` and the grid is currently `actual`.
    fn resolve(&mut self, target: u16, actual: u16, now: Instant, ease_growth: bool) -> u16 {
        // Settled where the layout still wants it. `actual` is deliberately
        // ignored here: a grid that has drifted off a target that never moved
        // was taken somewhere by something else — a direct terminal attach
        // resizes the runtime to the attaching terminal while the layout
        // stands still — and putting it back is a restoration, not a
        // transition to pace.
        if self.growing.is_none() && self.current == target {
            return target;
        }

        // A flight is worth continuing only while the grid is still where the
        // flight left it. If it is not, something resized the pane underneath
        // this module and the flight's starting premise is gone with it.
        if let Some(growing) = self.growing {
            if growing.to == target && actual == self.current {
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

        // Settled at a value that is no longer the target, mid-flight toward a
        // target that just moved, or flying from a premise the grid has left:
        // resolve fresh from where the grid *actually* is, never from what this
        // module last handed out.
        //
        // The difference is the whole point. Every tab that is not the active
        // one is resized straight to its final size on every frame by
        // [`crate::ui::panes::resize_tab_panes`], which never comes through
        // here, so a pane re-entered after the layout grew is usually already
        // exactly the size the layout wants. Easing from the size it had when
        // it was last on screen would reflow it *down* to that first — a shrink
        // the layout never asked for — and walk it back up over the following
        // frames.
        let from = actual;
        self.current = actual;
        if target <= from || !ease_growth {
            // Shrinking (or exactly arrived): no safe range to ease through,
            // since the frame this resolves for is already drawn into a
            // buffer sized to `target`. Snap, the same as before this module
            // existed.
            //
            // Growing lands here too while pacing is disabled — see
            // [`GROW_EASE_ENABLED`] — so a pane growing into a closed sibling's
            // space is drawn at its final size on the very first frame, with no
            // intermediate one to glitch.
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
#[derive(Debug)]
pub(crate) struct PaneResizeReflow {
    tracked: HashMap<TerminalId, TrackedTerminal>,
    /// Whether a growing axis is paced; [`GROW_EASE_ENABLED`] in production.
    ease_growth: bool,
}

impl Default for PaneResizeReflow {
    fn default() -> Self {
        Self {
            tracked: HashMap::new(),
            ease_growth: GROW_EASE_ENABLED,
        }
    }
}

impl PaneResizeReflow {
    /// The same mechanism with growth pacing forced on, so the eased regime
    /// stays covered while [`GROW_EASE_ENABLED`] is off.
    #[cfg(test)]
    fn with_grow_ease_for_test() -> Self {
        Self {
            ease_growth: true,
            ..Self::default()
        }
    }

    /// The size a terminal's grid should actually be resized to *this frame*,
    /// given the layout wants `target_rows`/`target_cols` and the grid is
    /// currently `grid_rows`/`grid_cols`.
    ///
    /// `grid_rows`/`grid_cols` are the runtime's own current size, not what
    /// this module last handed out — the two disagree whenever something else
    /// resized the pane, which the background-tab path does routinely. An ease
    /// is only ever planned from the real one; see [`AxisReflow::resolve`].
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
        (grid_rows, grid_cols): (u16, u16),
        now: Instant,
    ) -> (u16, u16) {
        // Seeded at the target rather than at the grid, so first sight settles
        // there immediately: seeding from the grid would turn every pane's
        // first layout pass into an animation of its own creation.
        let ease_growth = self.ease_growth;
        let tracked = self.tracked.entry(terminal_id).or_insert(TrackedTerminal {
            rows: AxisReflow::new(target_rows),
            cols: AxisReflow::new(target_cols),
        });
        (
            tracked
                .rows
                .resolve(target_rows, grid_rows, now, ease_growth),
            tracked
                .cols
                .resolve(target_cols, grid_cols, now, ease_growth),
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

    /// One pane's runtime as the render pass sees it.
    ///
    /// Mirrors the real call shape: the grid handed to [`PaneResizeReflow::resolve`]
    /// is the runtime's own size, which is whatever the last resolve resized it
    /// to — until [`Self::resized_outside`] stands in for the background-tab
    /// path writing the same runtime directly.
    struct Pane {
        reflow: PaneResizeReflow,
        terminal: TerminalId,
        grid: (u16, u16),
    }

    impl Pane {
        /// A pane under the shipped configuration.
        fn new(grid_rows: u16, grid_cols: u16) -> Self {
            Self {
                reflow: PaneResizeReflow::default(),
                terminal: id(),
                grid: (grid_rows, grid_cols),
            }
        }

        /// A pane with growth pacing forced on, for the tests that cover the
        /// eased regime itself rather than what the app currently ships.
        fn easing(grid_rows: u16, grid_cols: u16) -> Self {
            Self {
                reflow: PaneResizeReflow::with_grow_ease_for_test(),
                ..Self::new(grid_rows, grid_cols)
            }
        }

        fn frame(&mut self, target_rows: u16, target_cols: u16, now: Instant) -> (u16, u16) {
            let resolved = self.reflow.resolve(
                self.terminal.clone(),
                target_rows,
                target_cols,
                self.grid,
                now,
            );
            self.grid = resolved;
            resolved
        }

        /// The pane's runtime resized by something that never goes through
        /// this module — `crate::ui::panes::resize_tab_panes`, in production.
        fn resized_outside(&mut self, rows: u16, cols: u16) {
            self.grid = (rows, cols);
        }

        fn next_deadline(&self, now: Instant) -> Option<Instant> {
            self.reflow.next_deadline(now)
        }
    }

    #[test]
    fn growth_is_not_paced_under_the_shipped_configuration() {
        // What the app actually does: a growing axis lands on its target on the
        // frame the layout asks for it, so no frame is ever drawn at an
        // intermediate size and there is nothing for the loop to wake up and
        // finish.
        let now = Instant::now();
        let mut pane = Pane::new(20, 100);
        pane.frame(20, 100, now);

        assert_eq!(pane.frame(40, 100, now), (40, 100));
        assert_eq!(pane.next_deadline(now), None);
    }

    #[test]
    fn a_sibling_closing_grows_the_survivor_in_one_step() {
        // The reported artifact, in the module's own terms: the survivor's grid
        // is genuinely behind the layout (25 rows in a rect that now wants 54),
        // which is the one case that used to ease. It must resolve to 54
        // immediately — a 25-row grid drawn into a 54-row rect is exactly the
        // "output jumps to the top, then crawls back down" the captain sees.
        let now = Instant::now();
        let mut pane = Pane::new(25, 183);
        pane.frame(25, 183, now);

        assert_eq!(pane.frame(54, 183, now), (54, 183));
        assert_eq!(pane.next_deadline(now), None);
    }

    #[test]
    fn a_grid_genuinely_behind_its_target_lands_in_one_step() {
        // Same as above but reached through the outside-resize path, the
        // counterpart to `..._still_eases_when_pacing_is_enabled`: it still
        // resolves from the grid's real size, it just no longer stops there.
        let now = Instant::now();
        let mut pane = Pane::new(20, 100);
        pane.frame(20, 100, now);

        pane.resized_outside(24, 100);

        assert_eq!(pane.frame(60, 100, now), (60, 100));
        assert_eq!(pane.next_deadline(now), None);
    }

    #[test]
    fn a_terminal_seen_for_the_first_time_resolves_straight_to_target() {
        let now = Instant::now();
        let mut pane = Pane::new(24, 80);
        assert_eq!(pane.frame(40, 100, now), (40, 100));
    }

    #[test]
    fn growing_eases_rather_than_snapping() {
        let now = Instant::now();
        let mut pane = Pane::easing(20, 100);
        pane.frame(20, 100, now);

        // The very frame the target changes still resolves to the old size:
        // the ease begins on the *next* frame, not this one.
        let first = pane.frame(40, 100, now);
        assert_eq!(first, (20, 100));

        let mid = pane.frame(40, 100, now + RESIZE_REFLOW_DURATION / 2);
        assert!(
            mid.0 > 20 && mid.0 < 40,
            "half-way through should sit strictly between old and new: {mid:?}"
        );

        let done = pane.frame(40, 100, now + RESIZE_REFLOW_DURATION);
        assert_eq!(done, (40, 100));
    }

    #[test]
    fn shrinking_snaps_immediately_never_overshooting_the_new_smaller_bound() {
        let now = Instant::now();
        let mut pane = Pane::new(40, 100);
        pane.frame(40, 100, now);

        // A shrink resolves to its target on the very frame it is issued —
        // never a size larger than the target, which is the one thing that
        // would draw past a buffer already sized to it.
        let shrunk = pane.frame(20, 100, now);
        assert_eq!(shrunk, (20, 100));
        assert_eq!(pane.next_deadline(now), None);
    }

    #[test]
    fn one_axis_can_grow_while_the_other_shrinks() {
        let now = Instant::now();
        let mut pane = Pane::easing(20, 100);
        pane.frame(20, 100, now);

        // Rows grow (20 -> 40), columns shrink (100 -> 50): each axis must
        // follow its own rule independently.
        let first = pane.frame(40, 50, now);
        assert_eq!(first, (20, 50), "rows still easing, cols already snapped");
    }

    #[test]
    fn a_retarget_mid_growth_restarts_from_where_it_visibly_is() {
        let now = Instant::now();
        let mut pane = Pane::easing(20, 100);
        pane.frame(20, 100, now);
        pane.frame(40, 100, now);

        let partway = now + RESIZE_REFLOW_DURATION / 2;
        let visible_before_retarget = pane.frame(40, 100, partway);
        assert!(visible_before_retarget.0 < 40);

        // The target grows again before the first ease finished. The very
        // next resolve must not jump back to 20 (the original start) or snap
        // straight to the new target — it has to continue from what was just
        // visible.
        let just_after_retarget = pane.frame(50, 100, partway);
        assert_eq!(
            just_after_retarget, visible_before_retarget,
            "a retarget must not move the pane on the frame it is issued"
        );
    }

    #[test]
    fn next_deadline_is_armed_only_while_something_is_growing() {
        let now = Instant::now();
        let mut pane = Pane::easing(20, 100);
        assert_eq!(pane.next_deadline(now), None);

        pane.frame(20, 100, now);
        assert_eq!(
            pane.next_deadline(now),
            None,
            "first sight of a terminal settles immediately; nothing to wake for"
        );

        pane.frame(40, 100, now);
        assert_eq!(
            pane.next_deadline(now),
            Some(now + RESIZE_REFLOW_FRAME_INTERVAL)
        );

        pane.frame(40, 100, now + RESIZE_REFLOW_DURATION);
        assert_eq!(
            pane.next_deadline(now + RESIZE_REFLOW_DURATION),
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
        let mut pane = Pane::easing(10, 10);
        pane.frame(10, 10, now);
        pane.frame(90, 90, now);

        let mut t = Duration::ZERO;
        while t <= RESIZE_REFLOW_DURATION {
            let (rows, cols) = pane.frame(90, 90, now + t);
            assert!(
                rows <= 90 && cols <= 90,
                "overshot at {t:?}: ({rows}, {cols})"
            );
            t += RESIZE_REFLOW_FRAME_INTERVAL;
        }
    }

    #[test]
    fn a_pane_resized_while_backgrounded_is_never_shrunk_back_to_its_remembered_size() {
        // The bug this guards: a pane last seen at 19 rows is resized to 49 by
        // the background-tab path while its tab is off screen, so entering the
        // tab again finds a grid that is already exactly what the layout wants.
        // Easing from the remembered 19 would resize it back down first, and
        // ghostty-vt would reflow the pane's whole scrollback into 19 rows —
        // which draws the pane's live output pinned to the top of a 49-row
        // rect with a blank tail below it, then walks it back down.
        let now = Instant::now();
        let mut pane = Pane::new(19, 90);
        pane.frame(19, 90, now);

        pane.resized_outside(49, 90);

        assert_eq!(
            pane.frame(49, 90, now),
            (49, 90),
            "a grid already at the target must be left exactly where it is"
        );
        assert_eq!(
            pane.next_deadline(now),
            None,
            "nothing was reflowed, so nothing should be waking the loop to finish it"
        );
    }

    #[test]
    fn an_outside_resize_mid_ease_is_adopted_rather_than_eased_away_from() {
        // Same collision, caught mid-flight: switching away from a pane that is
        // still easing leaves this module holding an intermediate size, and the
        // background path immediately snaps the runtime to the full one. Only
        // reachable while growth pacing is enabled — nothing is ever mid-ease
        // without it.
        let now = Instant::now();
        let mut pane = Pane::easing(20, 100);
        pane.frame(20, 100, now);
        pane.frame(60, 100, now);

        let partway = pane.frame(60, 100, now + RESIZE_REFLOW_DURATION / 2);
        assert!(partway.0 > 20 && partway.0 < 60);

        pane.resized_outside(60, 100);

        assert_eq!(
            pane.frame(60, 100, now + RESIZE_REFLOW_DURATION / 2),
            (60, 100),
            "the flight was planned from a size the grid has left; reality wins"
        );
    }

    #[test]
    fn a_grid_moved_off_a_target_that_never_changed_is_restored_in_one_step() {
        // A direct terminal attach resizes the runtime to the attaching
        // terminal's size while the layout stands still. Releasing it puts the
        // pane back where the layout always wanted it, which is a restoration
        // and not a transition — `terminal_attach_disconnect_restores_app_pane_size`
        // in the headless server depends on it landing on the frame the lock is
        // released.
        let now = Instant::now();
        let mut pane = Pane::new(39, 93);
        pane.frame(39, 93, now);

        pane.resized_outside(24, 80);

        assert_eq!(pane.frame(39, 93, now), (39, 93));
        assert_eq!(pane.next_deadline(now), None);
    }

    #[test]
    fn a_grid_genuinely_behind_its_target_still_eases_when_pacing_is_enabled() {
        // The counterpart PR #130's fix must not break: when the runtime really
        // is smaller than the layout wants, the reflow this module exists to
        // pace is still ahead of it, so it still eases — under the eased
        // regime, which `GROW_EASE_ENABLED` currently turns off. Kept covering
        // the mechanism so flipping that flag back restores known-good
        // behaviour rather than untested behaviour.
        let now = Instant::now();
        let mut pane = Pane::easing(20, 100);
        pane.frame(20, 100, now);

        pane.resized_outside(24, 100);

        let first = pane.frame(60, 100, now);
        assert_eq!(
            first,
            (24, 100),
            "eases from the grid's real size, not the remembered one"
        );
        assert_eq!(
            pane.next_deadline(now),
            Some(now + RESIZE_REFLOW_FRAME_INTERVAL)
        );
        assert_eq!(pane.frame(60, 100, now + RESIZE_REFLOW_DURATION), (60, 100));
    }
}
