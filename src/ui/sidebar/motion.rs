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
//!   sideways instead, which is what makes an arrival read as a thing coming in
//!   rather than as the whole panel stretching.
//! - **Motion is the placement's, never the artwork's.** Everything here is an
//!   offset applied to where an already-rasterised card is *placed*. Nothing in
//!   it can change a card's pixels, which is what keeps a slide at the cost of
//!   a placement escape rather than of redrawing the tree — see
//!   [`super::image_card::build_cards`].
//!
//! # The character fallback cuts
//!
//! Below `MIN_FOLD_WIDTH`, and on any host without graphics, a row is
//! characters. A character cannot leave its cell — that is a property of
//! terminals, and it is the same reason [`crate::anim::behaviour`] resolves
//! colour and coverage but never position. So there is nothing to slide there
//! and rows appear and disappear on the frame the layout says they do, exactly
//! as they always have. The arrival and departure *phases* still run, so a
//! `row_enter` dissolve still plays on those cells; only the movement is
//! absent.

use crate::anim::{ElementId, Phase};
use crate::app::state::AppState;

/// How far a row travels sideways as it arrives, as a fraction of the panel's
/// own width.
///
/// A whole panel width and a little over. It has to clear the panel's right
/// edge completely at progress zero — a card that starts half on screen reads
/// as one that was already there and then jumped — and the excess is what makes
/// the first frames of the arrival empty rather than showing a sliver crawling
/// in. Past the edge the placement is simply not drawn, which costs nothing.
const SLIDE_REACH: f32 = 1.15;

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
    let reach = panel_width_px * SLIDE_REACH;
    let mut opening = 0.0f32;
    let mut offsets = Vec::with_capacity(rows.len());
    for row in rows {
        let absent = (1.0 - row.settle).clamp(0.0, 1.0);
        // Its own slot is deliberately not in `opening` yet: a row does not
        // make room for itself.
        offsets.push((absent * reach, -opening));
        opening += absent * row.height_px.max(0.0);
    }
    offsets
}

/// How far through its own life the engine says this element is.
///
/// `1.0` — settled — for anything the engine is not tracking at all, which is
/// the honest answer for a panel with no animation configured and for a row the
/// engine has already retired. A missing element must never make a card jump.
pub(crate) fn settle(app: &AppState, id: &ElementId) -> f32 {
    match app.anim.frame(id, None) {
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

    #[test]
    fn the_arriving_row_itself_starts_clear_of_the_panel() {
        let rows = [arriving(60.0, 0.0)];
        let (dx, _) = row_offsets(&rows, PANEL)[0];
        assert!(
            dx > PANEL,
            "an arrival that starts on screen reads as a jump, not an entrance: {dx}"
        );

        // Half way in it is half way across, and settled it is home.
        let half = row_offsets(&[arriving(60.0, 0.5)], PANEL)[0].0;
        assert!((half - dx / 2.0).abs() < 0.01, "{half} against {dx}");
        assert_eq!(row_offsets(&[arriving(60.0, 1.0)], PANEL)[0].0, 0.0);
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

    #[test]
    fn a_progress_outside_the_unit_range_cannot_throw_a_card_off_screen() {
        let rows = [arriving(60.0, 4.0), arriving(60.0, -4.0), settled(60.0)];
        let offsets = row_offsets(&rows, PANEL);
        assert_eq!(offsets[0], (0.0, 0.0));
        assert_eq!(offsets[2].1, -60.0);
    }
}
