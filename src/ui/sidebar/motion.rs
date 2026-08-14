//! Where a sidebar card is drawn while the tree is opening or closing a row.
//!
//! The panel's layout is instantaneous: the frame a pane is created, every row
//! below it already has its new rect, and the frame its exit finishes they all
//! have the old one back. That is correct — the layout is a fact about the
//! session, and hit testing, scrolling and the drag slots all read it. What it
//! is not is *watchable*: a row appears in a gap that was already there, and
//! nothing moved.
//!
//! This module is the offset between where the layout puts a card and where it
//! is drawn, so the gap is seen to open and close.
//!
//! # Four properties this module is responsible for holding
//!
//! - **It holds no state.** An offset is a pure function of the animation
//!   engine's existing per-element progress and the layout this pass just
//!   computed. There is no "where it was last frame", no tween table, no second
//!   clock — so there is nothing for a second attached client to desynchronise,
//!   and nothing to leak when a row goes away mid-flight. See
//!   [`RowLife::settle`].
//! - **The offsets are continuous across the layout change.** At the instant a
//!   row's slot appears the rows below it are offset by exactly minus that
//!   slot's height, which is where they already were; at the instant it
//!   disappears they are offset by exactly minus it, which is where they are
//!   going. Neither edge of a transition is a jump. That is the whole
//!   correctness argument and [`row_offsets`] is where it lives.
//! - **A row's own slot never moves the row itself.** The accumulator is over
//!   rows *above*, so an arriving card sits at its final resting row from the
//!   first frame and the tree opens underneath it. Its own arrival is expressed
//!   as [`ArrivalBeat`] — a light down the rail, the elbow, and the card
//!   generating left to right — and **never as a horizontal translation**. See
//!   [`row_offsets`], whose `dx` is now always exactly zero.
//! - **Motion is the placement's, never the artwork's.** Everything here is an
//!   offset applied to where an already-rasterised card is *placed*. Nothing in
//!   it can change a card's pixels, which is what keeps a slide at the cost of
//!   a placement escape rather than of redrawing the tree — see
//!   [`super::image_card::build_cards`].
//!
//! # The tree's connectors travel with the card
//!
//! A card is pixels but the `├─ ` pointing at it is a character, and the two
//! are drawn by different renderers. They stay attached because the offset is
//! quantized to whole cells once, by [`cell_offsets`], and both read that one
//! number: the placement adds it to its viewport, and
//! [`super::render_card_border_rails`] adds it to the rows it draws the rail
//! on. The offset is published per row on
//! [`crate::app::state::WorkspaceCardArea::motion_cells`], which is drawing
//! state only — where a row *is* stays `rect`, so a click during a transition
//! still lands on the row the layout says it hit.
//!
//! A row that is still travelling *sideways* draws no rail of its own at all.
//! Its card is off the panel's right edge for those frames, so a connector at
//! its resting position would be an arrow pointing at nothing; it appears with
//! the card, at the moment the card lands.
//!
//! # The character fallback cuts
//!
//! Below `MIN_FOLD_WIDTH`, and on any host without graphics or without
//! `[experimental] sidebar_card_shapes`, a row is characters. A character
//! cannot leave its cell — that is a property of terminals, and it is the same
//! reason [`crate::anim::behaviour`] resolves colour and coverage but never
//! position. So there is nothing to slide there and rows appear and disappear
//! on the frame the layout says they do, exactly as they always have.
//!
//! That fallback is held by [`crate::app::state::AppState::sidebar_rows_move`]
//! rather than here, and it has to be held before the animation engine is
//! reached and not after: motion's phase is *synthesized* when nothing else
//! asked for one, and a departure phase on a host that cannot move anything
//! would keep a closed pane's row on screen for the whole of `row_exit_ms`
//! with nothing playing on it. A `row_enter` dissolve the fleet configured for
//! itself still runs there, exactly as it did before motion existed; only the
//! phase motion would have invented is absent.

use crate::anim::{ElementId, Phase};
use crate::app::state::AppState;

/// Where a row is in the four-beat gesture its arrival is.
///
/// # Why a row does not slide in sideways any more
///
/// It used to: an arriving row was translated a panel width and a little over
/// to the right and travelled home. That is exactly what F22 refuses — *no
/// row's transform ever carries a horizontal offset* — and the refusal is not
/// stylistic. A card sliding across the panel is a finished object being moved;
/// the reference's card is **generated**, left to right, from the point a light
/// travelling the tree's own rail landed on its edge. One reads as a thing
/// arriving from off-screen, the other as the tree growing a branch, and the
/// second is what the picture is about.
///
/// The four beats, and what draws each:
///
/// 1. [`Self::Rail`] — light runs **down** the parent's rail toward this row's
///    elbow. Drawn by the character renderer, on the rail cells above the
///    connector row.
/// 2. [`Self::Elbow`] — it turns the corner and runs **right**, into the row's
///    own left edge. Drawn by the same renderer, on the `├──` cells.
/// 3. [`Self::Generate`] — the card is drawn from its left edge rightward, from
///    where the light landed. A **clip on a card being drawn**, never a
///    translation of a finished one.
/// 4. The column below is pushed down. That is [`row_offsets`]'s `dy`, and it
///    runs across all three of the above rather than after them, because the
///    space has to be open by the time the card fills it.
///
/// A departure is the same reading counted down, because the engine hands a
/// dismount back as its mount reversed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ArrivalBeat {
    /// Beat one, `0.0..=1.0` down the rail.
    Rail(f32),
    /// Beat two, `0.0..=1.0` along the elbow.
    Elbow(f32),
    /// Beat three, `0.0..=1.0` of the card generated from its left edge.
    Generate(f32),
    /// The row is at rest and everything about it is drawn.
    Settled,
}

/// Where beat one ends, as a fraction of the row's whole arrival.
const BEAT_RAIL_END: f32 = 0.20;
/// Where beat two ends. The two travel beats together take under half the
/// arrival: the light is the *announcement*, and an announcement that outlasts
/// the thing it announces is a delay rather than a gesture.
const BEAT_ELBOW_END: f32 = 0.40;

/// Which beat a row at this settle is on.
pub(crate) fn arrival_beat(settle: f32) -> ArrivalBeat {
    let settle = settle.clamp(0.0, 1.0);
    if settle >= 1.0 {
        return ArrivalBeat::Settled;
    }
    if settle < BEAT_RAIL_END {
        return ArrivalBeat::Rail(settle / BEAT_RAIL_END);
    }
    if settle < BEAT_ELBOW_END {
        return ArrivalBeat::Elbow((settle - BEAT_RAIL_END) / (BEAT_ELBOW_END - BEAT_RAIL_END));
    }
    ArrivalBeat::Generate((settle - BEAT_ELBOW_END) / (1.0 - BEAT_ELBOW_END))
}

impl ArrivalBeat {
    /// How much of the card is drawn, from its left edge, `0.0..=1.0`.
    ///
    /// Zero through both travel beats: the card does not exist yet, the light
    /// running toward it is the whole of what is on screen.
    pub(crate) fn generated(self) -> f32 {
        match self {
            Self::Rail(_) | Self::Elbow(_) => 0.0,
            Self::Generate(t) => t.clamp(0.0, 1.0),
            Self::Settled => 1.0,
        }
    }
}

/// One row's vertical extent and how far through its own arrival it is.
///
/// Deliberately not the sidebar's own row type: the offsets are an accumulation
/// over a sequence of heights and progresses and nothing else, so it is stated
/// over the two facts it needs and stays testable without a renderer, an engine
/// or a pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RowLife {
    /// The vertical space this row occupies in the layout, in pixels — its own
    /// height plus the gap to the next row, because that whole span is what
    /// appears and disappears with it.
    pub(crate) height_px: f32,
    /// `1.0` when this row is settled, `0.0` when it is entirely absent, and
    /// in between while it is arriving or leaving.
    ///
    /// Read straight off the engine by [`settle`]. A departure is the arrival
    /// played backwards there, so both directions arrive here as the same
    /// number counted the same way and nothing downstream has to know which
    /// way a row is going.
    pub(crate) settle: f32,
}

/// Where each row's card is drawn, relative to where the layout put it.
///
/// `(dx, dy)` in pixels. `dy` is negative or zero: a row is only ever drawn
/// *above* its settled position, because the space it is being moved out of is
/// space that is still opening.
///
/// `panel_width_px` is how far an arriving row starts off to the right.
pub(crate) fn row_offsets(rows: &[RowLife], panel_width_px: f32) -> Vec<(f32, f32)> {
    // Taken and ignored. The signature keeps it because the panel's width is
    // what a slide was measured against and the caller still has it to hand,
    // and dropping the parameter would make "we no longer slide" invisible at
    // every call site instead of stated at exactly one.
    let _ = panel_width_px;
    let mut opening = 0.0f32;
    let mut offsets = Vec::with_capacity(rows.len());
    for row in rows {
        let absent = (1.0 - row.settle).clamp(0.0, 1.0);
        // Its own slot is deliberately not in `opening` yet: a row does not
        // make room for itself.
        //
        // **`dx` is exactly zero and always will be.** F22, and
        // `no_row_ever_carries_a_horizontal_entry_offset` is the gate.
        offsets.push((0.0, -opening));
        opening += absent * row.height_px.max(0.0);
    }
    offsets
}

/// The same offsets in whole cells, which is the only unit anything on the
/// panel is actually placed at.
///
/// Rounded here and nowhere else. A card's placement and the tree's connector
/// beside it are two different renderers reading one number, and they are only
/// attached to each other because it is the *same* number — two roundings of
/// the same pixel offset would be one rounding each and could differ by a row.
/// See [`super::image_card::CardsBuild::motion`], which is how the cell offset
/// reaches the character renderer.
///
/// Whole cells rather than Kitty's sub-cell placement offsets on purpose: the
/// engine's own 50 ms frame step is coarser than a cell is tall over any
/// arrival short enough to read as one, measured in
/// `data/herdr-row-slide-reflow/subcell-test/`.
pub(crate) fn cell_offsets(offsets: &[(f32, f32)], cell_w: f32, cell_h: f32) -> Vec<(i32, i32)> {
    let quantize = |value: f32, cell: f32| {
        if cell > 0.0 && value.is_finite() {
            (value / cell).round() as i32
        } else {
            0
        }
    };
    offsets
        .iter()
        .map(|(dx, dy)| (quantize(*dx, cell_w), quantize(*dy, cell_h)))
        .collect()
}

/// How far through its own life the engine says this element is.
///
/// `1.0` — settled — for anything the engine is not tracking at all, which is
/// the honest answer for a panel with no animation configured and for a row the
/// engine has already retired. A missing element must never make a card jump.
pub(crate) fn settle(app: &AppState, id: &ElementId) -> f32 {
    settle_in(&app.anim, id)
}

/// [`settle`] against the engine alone.
///
/// Split out because the reading is a fact about the animator and nothing else,
/// and `herdr bench combined` drives an `Animator` directly — it has no
/// `AppState`, since `AppState::test_new` is `#[cfg(test)]` and a shipped
/// binary cannot stand a fleet up. Keeping one body means the benchmark's rows
/// slide off exactly the reading the panel's do, rather than off a copy of it
/// that could drift.
pub(crate) fn settle_in(anim: &crate::anim::Animator, id: &ElementId) -> f32 {
    match anim.frame(id, None) {
        Some(frame) => match frame.phase {
            // A dismount's progress already counts down, so this is the same
            // reading in both directions.
            Phase::Mount | Phase::Dismount => frame.progress.clamp(0.0, 1.0),
            Phase::Idle | Phase::Retired => 1.0,
        },
        None => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: f32 = 400.0;

    fn settled(height_px: f32) -> RowLife {
        RowLife {
            height_px,
            settle: 1.0,
        }
    }

    fn arriving(height_px: f32, settle: f32) -> RowLife {
        RowLife { height_px, settle }
    }

    #[test]
    fn a_settled_tree_does_not_move_at_all() {
        let rows = [settled(80.0), settled(60.0), settled(60.0)];
        assert_eq!(row_offsets(&rows, PANEL), vec![(0.0, 0.0); 3]);
    }

    /// The whole point: a row appearing pushes the ones under it, and pushes
    /// them by exactly its own height so the two ends of the transition line up
    /// with the layout on either side of it.
    #[test]
    fn a_row_arriving_moves_every_row_below_it_and_none_above() {
        let rows = [settled(80.0), arriving(60.0, 0.0), settled(60.0)];
        let offsets = row_offsets(&rows, PANEL);
        assert_eq!(offsets[0], (0.0, 0.0), "the row above is untouched");
        assert_eq!(offsets[1].1, 0.0, "a row does not make room for itself");
        assert_eq!(
            offsets[2],
            (0.0, -60.0),
            "at progress zero the row below is exactly where it was before the \
             slot existed"
        );

        // And at the other end of the arrival it is exactly where the layout
        // already had it, so the last frame of the motion and the first settled
        // frame are the same picture.
        let done = [settled(80.0), arriving(60.0, 1.0), settled(60.0)];
        assert_eq!(row_offsets(&done, PANEL)[2], (0.0, 0.0));
    }

    /// F22's gate, stated as the artifact states it: **the largest horizontal
    /// entry offset any row's transform ever carries is exactly 0.** Swept over
    /// the whole of an arrival rather than sampled at the ends, because the old
    /// slide was zero at both of those and a panel width across in the middle.
    #[test]
    fn no_row_ever_carries_a_horizontal_entry_offset() {
        let mut worst = 0.0f32;
        for step in 0..=100 {
            let settle = step as f32 / 100.0;
            let rows = [
                arriving(60.0, settle),
                settled(80.0),
                arriving(40.0, 1.0 - settle),
            ];
            for (dx, _) in row_offsets(&rows, PANEL) {
                worst = worst.max(dx.abs());
            }
        }
        assert_eq!(worst, 0.0, "a row travelled sideways by {worst} px");
    }

    /// The gesture, in order: the light travels the tree first, and only then
    /// is any of the card drawn.
    #[test]
    fn the_arrival_runs_rail_then_elbow_then_generation() {
        assert_eq!(arrival_beat(0.0), ArrivalBeat::Rail(0.0));
        assert!(matches!(arrival_beat(0.1), ArrivalBeat::Rail(_)));
        assert!(matches!(arrival_beat(0.3), ArrivalBeat::Elbow(_)));
        assert!(matches!(arrival_beat(0.7), ArrivalBeat::Generate(_)));
        assert_eq!(arrival_beat(1.0), ArrivalBeat::Settled);

        // Nothing of the card exists until the light has landed on its edge.
        assert_eq!(arrival_beat(0.0).generated(), 0.0);
        assert_eq!(arrival_beat(0.39).generated(), 0.0);
        assert!(arrival_beat(0.41).generated() > 0.0);
        assert_eq!(arrival_beat(1.0).generated(), 1.0);

        // And it grows monotonically from there, so a card is never seen to
        // un-generate part of itself.
        let mut last = 0.0;
        for step in 40..=100 {
            let now = arrival_beat(step as f32 / 100.0).generated();
            assert!(now >= last, "{now} after {last}");
            last = now;
        }
    }

    /// A progress the engine hands back outside the unit range cannot put a
    /// card at a negative width or past whole.
    #[test]
    fn a_beat_outside_the_unit_range_is_clamped() {
        assert_eq!(arrival_beat(-4.0), ArrivalBeat::Rail(0.0));
        assert_eq!(arrival_beat(4.0), ArrivalBeat::Settled);
    }

    /// A departure is the same arithmetic read the other way, because the
    /// engine hands a dismount back as its mount reversed. Nothing here knows
    /// which direction a row is going.
    #[test]
    fn a_row_leaving_closes_the_gap_it_leaves_behind() {
        let mut below = Vec::new();
        for step in [1.0, 0.75, 0.5, 0.25, 0.0] {
            let rows = [settled(80.0), arriving(60.0, step), settled(60.0)];
            below.push(row_offsets(&rows, PANEL)[2].1);
        }
        assert_eq!(below, vec![0.0, -15.0, -30.0, -45.0, -60.0]);
    }

    #[test]
    fn two_rows_in_flight_at_once_stack_their_gaps() {
        let rows = [
            arriving(80.0, 0.5),
            settled(60.0),
            arriving(40.0, 0.0),
            settled(60.0),
        ];
        let offsets = row_offsets(&rows, PANEL);
        assert_eq!(offsets[1].1, -40.0, "half of the first row's 80");
        assert_eq!(offsets[2].1, -40.0, "the second one does not move itself");
        assert_eq!(offsets[3].1, -80.0, "half the first plus all of the second");
    }

    /// The number the placement uses and the number the connector uses are one
    /// number, so a card and the rail pointing at it cannot land a row apart.
    #[test]
    fn the_cell_offset_is_rounded_once_and_shared() {
        let rows = [settled(80.0), arriving(60.0, 0.5), settled(60.0)];
        let cells = cell_offsets(&row_offsets(&rows, PANEL), 10.0, 21.0);
        assert_eq!(cells[0], (0, 0));
        assert_eq!(cells[2], (0, -1), "half of 60 px on a 21 px cell");
    }

    #[test]
    fn a_cell_size_that_is_not_a_size_offsets_nothing() {
        let rows = [arriving(60.0, 0.0), settled(60.0)];
        let offsets = row_offsets(&rows, PANEL);
        assert_eq!(cell_offsets(&offsets, 0.0, 0.0), vec![(0, 0); 2]);
        assert_eq!(cell_offsets(&offsets, -10.0, -21.0), vec![(0, 0); 2]);
    }

    #[test]
    fn a_progress_outside_the_unit_range_cannot_throw_a_card_off_screen() {
        let rows = [arriving(60.0, 4.0), arriving(60.0, -4.0), settled(60.0)];
        let offsets = row_offsets(&rows, PANEL);
        assert_eq!(offsets[0], (0.0, 0.0));
        assert_eq!(offsets[2].1, -60.0);
    }
}
