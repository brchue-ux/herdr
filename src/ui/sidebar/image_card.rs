//! The sidebar tree's card, drawn as pixels instead of as characters.
//!
//! # Two paths, one row model
//!
//! This is a *second* renderer for a row the panel already knows how to lay
//! out, not a second layout. Every card here is drawn into exactly the cells
//! [`super::card_frame_for`] gave that row, so the row is still an integer
//! number of terminal cells at an integer position, and everything keyed on
//! that — `AppState::view.workspace_card_areas`, the click target, the wheel,
//! the scrollbar, the drag slots — keeps working without knowing pixels exist.
//! The pixel card is a *skin* over the character card's rect.
//!
//! That is deliberate and it is the whole integration. An image card that owned
//! its own geometry would have to re-derive hit testing and scrolling in pixel
//! space, and the two would then disagree the first time a row's height changed
//! under a cell boundary.
//!
//! The character card is still drawn underneath, unchanged. The sheet is opaque
//! over each card's own rect, so on a terminal that honours Kitty graphics the
//! characters are covered; on one that silently does not, the panel is exactly
//! what it was before. Nothing is deleted to make room for this.
//!
//! # One sheet, not one image per card
//!
//! Cards are composited into a single image covering the tree, for two reasons.
//! The bloom is measured to run 26–28 px past a card's stroke, which is past
//! that card's own rect and over its neighbour's; and the placement pipeline in
//! [`crate::kitty_graphics`] keys a host image on one signature per surface, so
//! ten cards would be ten uploads and ten placements to reconcile every frame
//! rather than one.
//!
//! The gap between siblings is *not* taken from the measured table. The panel
//! already spaces its rows in cells (`[ui.sidebar.agents] row_gap`), and a
//! second gap in pixels would put the cards somewhere other than the cells the
//! layout reserved for them.
//!
//! # When it draws at all
//!
//! [`is_available`]. Kitty graphics on (which already folds in the direct
//! attach exclusion), a known host cell size, a panel wide enough for a card at
//! all, and a proportional face on the machine. Any of those missing and the
//! panel keeps its character cards; none of them can be missing *and* have this
//! draw a worse card.

pub(crate) mod bench;
mod canvas;
pub(crate) mod crew;
mod font;
mod measured;
mod spider;
mod summary;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::layout::Rect;

use canvas::{coverage, Canvas, Rgb, RoundRect, Triangle};
use font::{CardFont, FontMetrics};

use crate::anim::cell::{LifecycleStage, Severity};
use crate::app::state::AppState;
use crate::detect::AgentState;
use crate::kitty_graphics::HostCellSize;
use crate::ui::sidebar::AgentPanelEntry;

/// The height every card is drawn at, in pixels.
///
/// REC-TIGHT from `data/herdr-card-iteration-2/`: the ratios were originally
/// calibrated for type that grew with the box, and once the title stopped
/// scaling the 96 px card was mostly air.
///
/// # Why one number and not a ladder
///
/// There was a per-depth scale here — `TIER_SCALE = [1.00, 0.65, 0.4225]`,
/// applied to the card's height and to every fraction of it the chrome is
/// measured in. It is gone, by the captain's decision of 2026-08-06:
///
/// > *"i dont want height of the cards adjusted. it was only the width that
/// > changed depending on relationship"*
///
/// **Rank is carried by width, and only by width.** A row's left edge steps in
/// by rank in `super::rank_width_inset` — the cards are right-aligned, so the
/// ladder is spent entirely there — and that step is now the whole signal.
/// So there is nothing left for a depth to scale: the height, the padding, the
/// stroke, the radius, the plate and the bloom's sigma are all fractions of this
/// one number on every rank, and a card's size says *what it is* in one
/// dimension rather than in two that disagreed about which of depth and rank
/// they were reading.
///
/// # The 20% trim of 2026-08-09, and where it was taken from
///
/// This was 68.0. The captain, looking at the shipped tree:
///
/// > *"the cards a still a bit too big, need to be trimmed like 20% all
/// > respectively. i dont think I want the card sizes changing, that ruins
/// > symmetry and truncates titles. herdr will just need to be better about
/// > what it chooses to display as current working summary."*
///
/// It is one number rather than a per-rank cut precisely because of the second
/// sentence: the ladder is still width and only width
/// (`super::rank_width_inset` is untouched), so every card keeps the same air
/// and the rank steps between them are exactly the ones that were there before.
///
/// **The trim came out of the card's air, not out of its type**, and that is
/// what survives as [`CARD_AIR_PER_SIDE_PX`] — the air the captain left, 4.65 px
/// a side against the 11.45 the card had before him. The type is fixed by
/// legibility ([`TITLE_PX`]) and the block it sets is a constant on every face
/// this runs on: `ab_glyph` normalises a face's line height to the scale it is
/// asked for, so the block is arithmetic rather than a per-face measurement.
///
/// # Why the number moved from 54.4 to 64.5
///
/// The block underneath it grew, and the air did not. A card used to carry two
/// title lines and one caption; it now carries two title lines and
/// [`CAPTION_LINES`] — what the body **is** and what it has **done** on a mate,
/// what it is **doing** and what state it is **in** on a worker — because that
/// is what the reference's rows say. One more caption line at
/// [`measured::TIDBIT_SIZE_MUL`] of the title's 14 px is 10.08 px, and the card
/// is exactly that much taller:
///
/// ```text
///                 nominal   content block   air per side   min pad
///   before          68.0        45.11           11.45         3.0
///   trimmed         54.4        45.11            4.65         3.0
///   two captions    64.5        55.19            4.65         3.0
/// ```
///
/// So the captain's trim is intact — the air per side is still his 4.65 — and
/// the growth is entirely lines of type that were not there before.
/// `the_trim_is_air_and_not_the_floor` is what holds that apart.
///
/// It composes with [`content_floor_px`] the same way it always did: the floor
/// is 61.19 px against a nominal of 64.5, so the floor stays a *floor* — it does
/// not engage, the card is not pushed back up, and [`MIN_VERTICAL_PAD_PX`] is
/// still not reached.
///
/// Nothing about the title's capacity moved, which is the answer to *"truncates
/// titles"*: a title's room is lines × column width, and the line count is
/// unchanged at [`TITLE_LINES`].
const BASE_HEIGHT_PX: f32 = 64.5;

/// The air a card keeps above and below its content block, in pixels.
///
/// **The captain's own number**, and the thing his 20% trim actually decided:
/// 4.65 px a side, down from the 11.45 the card had before him. It is stated
/// here rather than left implicit inside [`BASE_HEIGHT_PX`] so that a change to
/// what the card *says* moves the card's height and leaves his decision about
/// its air alone — which is exactly what happened when the row grew its two
/// register captions.
///
/// Test-only, because it is the number [`BASE_HEIGHT_PX`] was *written from*
/// rather than a second input to it: the drawn card reads one constant, and this
/// is what `the_trim_is_air_and_not_the_floor` holds that constant to.
#[cfg(test)]
const CARD_AIR_PER_SIDE_PX: f32 = 4.65;

/// The card height the measured table's *chrome* ratios are fractions of.
///
/// The captain's trimmed 54.4, held still on purpose. See [`nominal_height_px`]
/// for why this stopped tracking [`BASE_HEIGHT_PX`].
const CHROME_NOMINAL_PX: f32 = 54.4;

/// The title's type size, on every card.
///
/// Fixed rather than scaled, and never smaller: the fit ladder in
/// `data/herdr-card-iteration-2/` measured the legibility floor at two numbers
/// rather than one — about 14 px at Light 300 and about 10 px at Medium 500 —
/// because below the floor the stem is thinner than a device pixel and the
/// rasteriser hands back grey instead of ink. The card is set at the Light
/// floor, which is the comfortable one.
///
/// Fixed at *every* rank, and both title lines are kept at every rank: shrinking
/// the title at depth was one of the two ways offered to reach a height ladder,
/// and the captain rejected it along with the ladder. See [`BASE_HEIGHT_PX`].
const TITLE_PX: f32 = 14.0;

/// Lines of title reserved on every card, whatever this card's title is.
///
/// Reserved rather than measured, so a card's height does not move when the
/// agent republishes a longer or shorter `doing` string. A row whose height
/// tracked its text would reflow the whole tree below it every time any agent
/// said anything.
const TITLE_LINES: usize = 2;

/// Leading between the two title lines, as a multiple of the face's own line
/// height.
///
/// Set at 1.25 rather than 1.0 because a face's own line height is the metric
/// for setting continuous prose at its design size, and a card's title is two
/// short lines of a *summary* — set solid they read as one clot of text and the
/// wrap point stops being visible. This is the prototype's `lead = th * 1.30`
/// measured against the line box instead of against the bounding box of `Hxg`.
const TITLE_LEADING: f32 = 1.25;

/// Caption lines reserved under the title on every card, whatever this card
/// carries.
///
/// **Two, and the number is the reference's own.** Its mate pane sets three
/// lines — the project's name, `gas giant · 99 files · 2 moons`, and
/// `streak 5 · T 13.4s · 23 revs` — and its worker pane sets three: the lane's
/// name, the task, and the state as a bare lowercase word. So a row is a name
/// and *two* things said about it, and which two depends on what the row is:
///
/// | row | caption one | caption two |
/// |---|---|---|
/// | a mate (a star or a planet) | what its body **is** | what it has **done** |
/// | a worker (a moon) | what it is **doing** | what state it is **in** |
///
/// herdr reserves [`TITLE_LINES`] for the name rather than the reference's one,
/// because a herdr title is a published `doing` summary and not a project slug —
/// so a card is four lines of type where the reference is three.
///
/// It is not three. Three was tried and the arithmetic refused it: a third
/// caption puts the drawn card at 82 px, which is four 21 px cells with 1.6 px
/// of gutter left between siblings — against the 0.19 h the material was
/// measured at — and the row would have to grow to five cells to get its air
/// back. The captain's standing instruction on card size is the opposite
/// direction (see [`BASE_HEIGHT_PX`]), and at two captions the gutter lands on
/// **13.4 px against a 70.6 px card: 0.19 h exactly**.
///
/// Reserved rather than measured for the reason [`TITLE_LINES`] is: a row whose
/// height tracked its text would reflow the whole tree below it every time any
/// agent said anything.
const CAPTION_LINES: usize = 2;

/// How far the state word sits below the other captions' ink.
///
/// The card's colour and its breath already say what state it is in; this word
/// is the name of that state for a reader who wants it spelled, not a second
/// signal competing with the two lines carrying numbers. Applied on top of
/// [`measured::TIDBIT_INK_MIX`], so it is a rung below caption weight rather
/// than a second scale.
const STATE_INK_MIX: f32 = 0.62;

/// Gap between the title block and the tidbit line, as a multiple of the
/// tidbit's line height.
///
/// Cut from 0.55 in the reality pass, when this gap and [`MIN_VERTICAL_PAD_PX`]
/// were the only air a sub-top-tier card had left to give. The tiers are gone
/// and the pressure with them, but the tighter gap was judged on screen at the
/// top tier too and is kept on its own merits.
const TIDBIT_GAP: f32 = 0.35;

/// The narrowest panel the image card draws on, in columns.
///
/// The same threshold the character card shell uses, deliberately: below it a
/// row is a bare styled line rather than a box, and a pixel card drawn over a
/// row that is not a box would be a third layout. `MIN_FOLD_WIDTH` is 32, which
/// is a 34-column sidebar.
pub(crate) const MIN_FOLD_WIDTH: u16 = super::card::MIN_FOLD_WIDTH;

/// The smallest amount [`lay_bloom`] will paint.
///
/// Below this a bloom pixel cannot move an 8-bit channel: an amount of 1.0 is
/// the stroke's own excess over the canvas, about 173 levels, so the peak
/// amount [`measured::BLOOM_PEAK`] = 0.38 carries about +66 levels and 0.002 is
/// roughly a third of one level. It is also the number [`BLOOM_REACH_SIGMAS`]
/// is derived from — see there.
const BLOOM_PAINT_FLOOR: f32 = 0.002;

/// A card's bloom is carried this many sigmas past its stroke, and truncated
/// there.
///
/// # Why a count of sigmas, and not a fraction of the card's height
///
/// This was `BLOOM_REACH = 0.45`, a fraction of the card's *drawn* height,
/// while the sigma it truncates is a fraction of the nominal height. Those are
/// not the same number whenever the content pushes a card past its nominal, and
/// at the time this was measured that ratio differed across the tier scale that
/// has since been retired ([`BASE_HEIGHT_PX`]) — a top-tier card drawn at its
/// nominal 68 px, a mate's content pushing it to 1.52× its (then-smaller)
/// nominal, a worker's to 1.85×. So one constant produced three different
/// truncations, measured on the real layers:
///
/// ```text
///   tier        drawn/nominal   cut lands at   value there   last painted alpha
///   top tier         1.00          2.37 σ        15.1% of peak        7/255
///   mate             1.52          3.60 σ         4.8% of peak        3/255
///   worker           1.85          2.84 σ         9.3% of peak        5/255
/// ```
///
/// The tiers are gone, so every card now has the same nominal and, on a given
/// face, the same drawn/nominal ratio — but the constant is still a sigma count
/// rather than a fixed fraction for the reason below, and the day any two cards
/// on a face disagree on that ratio again, this argument is why the fix is not
/// a smaller `BLOOM_REACH`.
///
/// `lay_bloom`'s `profile.get(..)` returns `None` past the reach, so the glow
/// **stops dead** rather than fading — a cut at 15% of peak is a visible hard
/// rim, and it was worst on the biggest, most-looked-at card. That is what the
/// captain's *"still needs to retain quality and crispness"* is about, and it is
/// a separate defect from the bleed: shrinking this constant does not darken the
/// gutter at all, it only moves the hard edge closer in.
///
/// # How 3.7 was derived
///
/// Not fitted to a picture — read off the renderer's own paint floor.
/// [`lay_bloom`] already declines to paint an amount at or under
/// [`BLOOM_PAINT_FLOOR`], and the card's bloom multiplier never exceeds 1.0, so
/// the distance at which the profile falls under that floor is the distance past
/// which truncating can remove nothing that would have been drawn. Both the
/// floor and [`measured::BLOOM_PEAK`] are absolute, so that distance is a
/// property of the profile's *shape* alone: the same number of sigmas on every
/// tier, at every cell size, on every card.
///
/// Being a count of sigmas is what let the field be narrowed without touching
/// this: [`measured::BLOOM_SIGMA`] went 0.19 h → 0.07 h → 0.030 h and the reach
/// followed it in pixels each time, so the cut has never had to be refitted to
/// a narrower rim. The *distance* did move when the field's shape changed —
/// **3.64 σ** for the two-lobe field 3.7 was first read off, **3.24 σ** for the
/// single hot-core lobe drawn since ([`measured::BLOOM_FAR_WEIGHT`] `= 0.0`, and
/// a higher peak buys fewer sigmas than a second lobe's tail did). 3.7 covers
/// both, so the cut still sits under the floor rather than on it, and
/// `the_bloom_reach_is_derived_from_the_paint_floor` recomputes that distance
/// from whatever shape the constants currently describe rather than trusting
/// this paragraph to have been updated with them.
///
/// The consequence is that the truncation is no longer visible by construction
/// rather than by measurement — the profile has already stopped painting before
/// the reach cuts it — and `a_card_glow_falls_to_nothing_before_it_is_cut` holds
/// it there on real rendered pixels.
const BLOOM_REACH_SIGMAS: f32 = 3.7;

/// The narrowest a bloom's near lobe is ever drawn, in pixels.
///
/// A sigma under a pixel or two is not a gradient, it is a stroke with a fringe.
const BLOOM_SIGMA_MIN_PX: f32 = 1.6;

/// The height every ratio in the measured table is a fraction of.
///
/// # Why this is not [`BASE_HEIGHT_PX`] any more
///
/// It was, and the two were the same number for as long as the card carried one
/// caption. They came apart when it started carrying [`CAPTION_LINES`]: the
/// card's *height* is block plus air and grows with what the row says, but its
/// *chrome* — the pad, the right pad, the stroke, the corner and the icon plate
/// — is not a function of how many lines are inside it. Left tied together, two
/// extra lines of caption made the pad wider and the title's column **narrower**,
/// which is the captain's own *"truncates titles"* arriving by the back door.
///
/// So the chrome stays measured against the card the captain trimmed —
/// [`CHROME_NOMINAL_PX`] — and the height is free to follow the content. The
/// horizontal budget a title is set in is byte-identical to what it was after
/// his trim, on every face and at every rank.
///
/// Still a function because the cell height is a floor on it, and a host with
/// tall cells is a host where one cell is already more than the nominal.
fn nominal_height_px(cell_height: f32) -> f32 {
    CHROME_NOMINAL_PX.max(cell_height)
}

/// The sigma of the near lobe of a card's bloom, in pixels.
fn bloom_sigma_px(cell_height: f32) -> f32 {
    (measured::BLOOM_SIGMA * nominal_height_px(cell_height)).max(BLOOM_SIGMA_MIN_PX)
}

/// Whether a card paints a bloom at all.
///
/// **No, and this is the reference's rule rather than a taste call.** F1 refuses
/// `box-shadow` and `blur()` outright: the reference has *no drop shadow
/// anywhere* and its panes float by being brighter than the ground rather than
/// by casting onto it. herdr's bloom ran a measured 26–28 px past a card's own
/// stroke, which over a tree of stacked rows is a continuous haze — and the
/// glass material replacing the filled plate has nothing to lift, because a face
/// at a tenth of an alpha is not standing off the panel in the first place.
///
/// A constant rather than a deletion because the bloom is a whole measured,
/// tested subsystem with a GPU path behind it, and the two places it is
/// consulted — [`bloom_reach_px`], which decides how much image is reserved, and
/// [`plan_bloom`], which decides whether any is painted — are the entire gate.
/// Flipping it back is one word and restores exactly the artwork the measured
/// table describes.
const CARD_BLOOM: bool = false;

/// How far a card's bloom is carried past its stroke, in pixels.
///
/// Zero while [`CARD_BLOOM`] is off, which is what stops every card's image from
/// reserving a margin nothing is drawn in.
fn bloom_reach_px(cell_height: f32) -> f32 {
    if !CARD_BLOOM {
        return 0.0;
    }
    bloom_sigma_px(cell_height) * BLOOM_REACH_SIGMAS
}

/// Where across a column the tree's rails put their ink.
///
/// # The bug this exists to fix
///
/// The captain, on his first look at the transparent cards: *"tree trunk not
/// aligned with firstmate/workers. branches not aligned with secondmates."*
///
/// The tree's geometry is settled in characters — `tree_prefix_width` is the
/// single place a prefix is measured, and a card's left border deliberately
/// stands in its connector's own column so the two share it. Under the
/// character shell that works, because the card's border is a `│` and a rail is
/// a `│`: two glyphs in one column, and a font draws a box-drawing vertical
/// down the **middle** of the cell it is in.
///
/// A pixel card's border is not a glyph. It is a stroke on a rounded rect whose
/// left side is `frame.x`, and `frame.x` is a cell **boundary** — so the drawn
/// border landed half a column left of every rail meant to continue it. Half a
/// cell is small in columns and plainly visible in pixels, and it applies to
/// every rail in the tree at once: the trunk under the first mate, and each
/// branch under a second mate. One offset, both of his findings.
///
/// So the pixel card is moved onto the character geometry rather than the other
/// way round. The characters cannot move — a glyph goes where the font puts it —
/// and they remain the layout authority regardless, which is what makes this
/// the side that gives.
///
/// # Why one half and not a measurement
///
/// Cell-centred is what a box-drawing vertical is *for*: `│`, `├` and `└` have
/// to meet each other across rows in a grid that only knows whole cells, so the
/// stem is centred by construction rather than by any one font's taste. Herdr
/// cannot query the host's glyph outlines in any case, and a half-column error
/// is exactly the error being removed here.
const RAIL_INK_COLUMN_FRACTION: f32 = 0.5;

/// How far a card's ink sits from the middle of its own cells, so that it is
/// centred on the row the tree's branch line meets it on.
///
/// The vertical twin of [`RAIL_INK_COLUMN_FRACTION`], and the same argument:
/// the characters are the layout authority because a glyph goes where the font
/// puts it, so the drawn card is the side that gives. A branch line lands on a
/// whole row — [`crate::app::state::WorkspaceCardArea::connector_y`] — and the
/// card is what moves onto it.
///
/// Zero whenever the row is an odd number of cells, which is every row at a
/// 19–27 px cell: the middle row's own centre already *is* the frame's centre.
/// At a 14–18 px cell a card needs four cells and there is no middle row, so
/// the frame's centre falls on the boundary between rows 1 and 2 and the card
/// moves up half a cell onto row 1's centre — *up*, because the row above the
/// tree's first card is the panel's header row and the row below its last may
/// be the signal tray, whose badges are placements on this same plane.
fn connector_row_offset_px(frame_height: u16, cell_h: f32) -> f32 {
    if frame_height == 0 {
        return 0.0;
    }
    let connector_row = f32::from((frame_height - 1) / 2);
    let frame_middle = f32::from(frame_height) / 2.0;
    (connector_row + 0.5 - frame_middle) * cell_h
}

/// One finished image and the cells it covers — one card's shape, or the whole
/// tree's sheet.
///
/// `Clone` so a card whose content did not change can be carried into the next
/// frame's list when a *sibling* did. That copies the encoded bytes — a few
/// kilobytes of flat-fill PNG — and skips the rasterisation, which is the
/// expensive half by roughly an order of magnitude.
#[derive(Clone)]
pub(crate) struct SidebarCardLayer {
    /// The cell rect this image was *drawn for*. Chosen by the tree's own
    /// geometry: for a shape, exactly its own card plus the reach of that card's
    /// bloom; for the sheet, every card plus the reach of theirs.
    ///
    /// Where it is drawn is [`Self::clip`] plus the layer's own viewport offset,
    /// which is the same thing whenever the panel is settled and is not while a
    /// row is arriving or leaving. Keeping the two apart is what lets a card
    /// move without its artwork changing: this rect is what the pixels are a
    /// picture of, so it is what the signature is taken over.
    pub rect: Rect,
    /// The box on the panel this image may draw in.
    ///
    /// The placement's *area*, so the pipeline's existing clipper crops a card
    /// that has slid past the panel's edge against the panel rather than letting
    /// it spill over the terminal panes — and crops it by cropping the source,
    /// so what remains is still at 1:1 and still unscaled. At rest a card is
    /// wholly inside this and nothing is cropped at all.
    pub clip: Rect,
    /// What this image was built from. An entry whose signature is unchanged
    /// keeps the pixels it already has and re-encodes nothing.
    pub signature: u64,
    /// The same, with the transition the image is *in* left out.
    ///
    /// Two signatures because a switch changes one of them every frame and the
    /// other not at all: the rows do not move until the commit instant, which
    /// is the whole point of the switch. Splitting them is what lets a
    /// transition frame reuse [`Self::undissolved`] instead of drawing ten
    /// cards, their bloom and their type again to produce the same pixels it
    /// produced 50 ms ago.
    pub content_signature: u64,
    /// This image before the transition was applied to it, held only while one
    /// is running.
    ///
    /// `None` whenever the panel is settled or the effect is off, so a Herdr
    /// nobody has configured this on carries no extra megabyte around.
    pub undissolved: Option<UndissolvedSheet>,
    /// The pixels this *host image slot* last actually handed the terminal.
    ///
    /// [`Self::signature`] says the artwork may have moved; this says whether
    /// the move is one a viewer could see. A resting card's breath moves the
    /// signature on every frame of the tier and the pixels by a fraction of one
    /// 8-bit level, and without this every one of those is a whole-surface
    /// re-encode and re-upload — see
    /// [`crate::app::state::PublishedSurfaceRaster`], which the signal tray has
    /// used since #104 and which this is the card half of.
    ///
    /// It rides on the layer rather than sitting beside the list because that is
    /// what makes it right on both sides of the wire at once: the server's
    /// `AppState::sidebar_card_layers` and a delegating client's
    /// `previous_card_layers` are the same type through the same
    /// [`Rasteriser::shapes`], so one anchor per layer is one anchor per surface
    /// wherever the surface is drawn.
    ///
    /// **Anchored by slot, not by card.** What the terminal is showing at
    /// `HostSurfaceId::SidebarCards(i)` is whatever layer stood at index `i`, so
    /// this is compared against the previous list *positionally* — and dropped
    /// whenever an image moves to a different slot, because then the id it is
    /// showing under is not the one it was published to.
    pub published: crate::app::state::PublishedSurfaceRaster,
    pub layer: crate::app::state::GraphicsLayer,
}

impl SidebarCardLayer {
    /// Point an image that is already drawn at a place on the panel.
    ///
    /// The one operation a slide performs. It touches no pixels — which is the
    /// whole cost argument: rasterising a card measures about an order of
    /// magnitude more than copying one, so motion that re-places an existing
    /// image is very nearly free while motion that redraws it would cost the
    /// tree on every frame of every transition.
    fn aim_at(&mut self, rect: Rect, clip: Rect, viewport: (i32, i32)) {
        self.rect = rect;
        self.clip = clip;
        self.layer.render.viewport_col = viewport.0;
        self.layer.render.viewport_row = viewport.1;
    }

    /// Where this image is actually placed, relative to [`Self::clip`].
    fn viewport(&self) -> (i32, i32) {
        (
            self.layer.render.viewport_col,
            self.layer.render.viewport_row,
        )
    }
}

/// One rasterised image — the whole sheet, or one card's shape — held across the
/// frames of one transition.
///
/// Opaque outside this module and shared rather than copied: every frame of a
/// switch reads the same pixels, and the only thing that changes between them
/// is the alpha mask laid over a scratch copy.
#[derive(Clone)]
pub(crate) struct UndissolvedSheet(std::sync::Arc<Canvas>);

/// Whether the panel should be drawing pixel cards at all.
///
/// Every caller reads this one function rather than re-deriving the conditions,
/// because the layout and the renderer disagreeing about which path is live
/// would put a pixel card over a row sized for characters.
///
/// The first term is [`AppState::host_paints_pixel_surfaces`] and not the config
/// flag alone, for the same reason one level up: this predicate is half of
/// [`shape_covers_row`], so a term it is missing that the delivery gate has is a
/// term on which the character cards stand down for pixels nobody is sent.
pub(crate) fn is_available(app: &AppState, fold_width: u16) -> bool {
    app.host_paints_pixel_surfaces()
        && app.host_cell_size.is_known()
        && fold_width >= MIN_FOLD_WIDTH
        && card_face_available(app.sidebar_card_font.as_deref())
}

/// Whether this machine has a face a card can be set in.
///
/// One condition of [`is_available`], exposed on its own because
/// [`AppState::sidebar_rows_move`] needs exactly this one and must not take the
/// other two it does not already have. Herdr ships no font, so a minimal
/// container or server routinely has none — the pixel-card tests in this file
/// all branch on it — and a row lifecycle that ignored that would synthesize an
/// exit phase on a host that draws no cards.
///
/// Safe to fold into a *lifecycle*, unlike the panel width, because the search
/// runs once and is cached for the process lifetime: it cannot change under a
/// row that is already mid-flight.
pub(crate) fn card_face_available(override_path: Option<&str>) -> bool {
    font::card_font(override_path).is_some()
}

/// Whether a transparent shape will be drawn over this row's frame, so the
/// character card standing under it must not be drawn at all.
///
/// **Both pixel models answer `true` now.** The sheet used to answer `false`
/// and keep the character card underneath, because it painted an opaque
/// backdrop over every cell a row owned and covered it. It no longer paints one:
/// a card is glass, and a glass pane standing on an opaque plate is not glass —
/// the whole point of the material is that the panel, and on a terminal drawing
/// the whole-screen scene the sky itself, is measurably visible *through* the
/// card. So neither model covers what is beneath it, and anything drawn there
/// would show through — the character card's border and its title, doubled a
/// few pixels off the pixel card's own.
///
/// The row's connectors and rails are outside the card's frame and are left
/// alone: they are the tree, not the card, and the card was never covering them.
///
/// # Why this asks whether shapes were actually published
///
/// [`is_available`] says the pixel path *should* be live; it does not say a card
/// came out of it. A build that fails — a cell-size report that makes an image
/// larger than [`MAX_IMAGE_PIXELS`], an encoder that returns nothing — publishes
/// no layers at all, and suppressing the character cards on the strength of a
/// shape that was never drawn leaves the tree blank. Suppression has to be
/// conditioned on the artwork existing, not on it being intended.
///
/// # Why it asks the *pass* and not the state
///
/// Whether the shapes reach a screen is a per-client fact, and
/// `AppState::sidebar_card_layers` is the foreground client's. A second attached
/// client whose own cell size is unknown — graphics off in its config, or a
/// direct attach — is rendered without one, so its pass deliberately leaves the
/// foreground's artwork alone and is then sent no images at all. Reading the
/// shared layers here would suppress that client's character cards in favour of
/// pixels it never receives, drawing the tree as bare connectors. So the answer
/// comes off `ViewState::sidebar_card_layers_published`, which the pass that
/// built the cards recorded for itself — the same field
/// `kitty_graphics::surface_layer_placement_targets` reads to decide which
/// passes are sent the images, so a pass cannot draw characters and be sent
/// shapes to double them either.
///
/// [`MAX_IMAGE_PIXELS`]: Rasteriser::rasterise
pub(crate) fn shape_covers_row(app: &AppState, fold_width: u16) -> bool {
    card_covers_row(app, fold_width)
}

/// Whether a pixel card is going to be drawn over this row at all, by either
/// drawing model.
///
/// [`shape_covers_row`] without the shapes term, and the two are genuinely
/// different questions. That one asks *"must the character card stand down"*,
/// which only the transparent model forces. This one asks *"is a pixel card
/// covering this row's cells"*, which the opaque sheet does too — it is opaque
/// over exactly the cells each row owns, so anything drawn in characters there
/// is under it rather than beside it.
///
/// The distinction has one caller and it is the failure spider: the marker is
/// drawn *on the card* by [`spider`] on both pixel models, and drawn as a
/// character cell by [`crate::ui::sidebar::render_failure_spiders`] when neither
/// is live. Gating the character one on `shape_covers_row` would leave the sheet
/// path drawing a glyph underneath an opaque sheet *and* the pixel creature on
/// top of it — one marker too many, and the one nobody can see is the one that
/// costs a `Buffer` write on every frame of a climb.
pub(crate) fn card_covers_row(app: &AppState, fold_width: u16) -> bool {
    app.view.sidebar_card_layers_published && is_available(app, fold_width)
}

/// The height a card wants, in pixels. The same on every rank.
///
/// `max(base, what the content needs)`. [`BASE_HEIGHT_PX`] is a floor and not a
/// ceiling because the title's size is fixed: on a face whose line height runs
/// large, two lines of 14 px type and a tidbit want more than 68 px, and the
/// card grows rather than clipping its own words. It is the same growth on every
/// card, because every card carries the same block.
///
/// # Nothing here reads `depth`, and that is the point
///
/// This used to be `max(tier floor, content)` with the tier read off the row's
/// depth, and its own doc conceded the ladder never reached the screen: the
/// content floor sits at about 0.75 of base, so the 0.65 and 0.42 rungs both
/// landed on it and rendered identical. That is retired rather than repaired —
/// see [`BASE_HEIGHT_PX`]. Rank is width, height is one number, and a row's
/// depth no longer changes anything about how tall it is drawn.
fn card_height_px(metrics: FontMetrics, tidbit_metrics: FontMetrics) -> f32 {
    BASE_HEIGHT_PX.max(content_floor_px(metrics, tidbit_metrics))
}

/// The shortest a card carrying the full D-MID block can be.
fn content_floor_px(metrics: FontMetrics, tidbit_metrics: FontMetrics) -> f32 {
    content_block_px(metrics, tidbit_metrics) + MIN_VERTICAL_PAD_PX * 2.0
}

/// The ink a card carries: two title lines and [`CAPTION_LINES`] under them.
///
/// The caption run is set solid — one line height each, no extra leading —
/// because the three of them are one block of caption and not three separate
/// statements. Only the gap between the title and the first of them is a gap.
fn content_block_px(metrics: FontMetrics, tidbit_metrics: FontMetrics) -> f32 {
    let title = metrics.line_height * (TITLE_LEADING * (TITLE_LINES as f32 - 1.0) + 1.0);
    let captions = tidbit_metrics.line_height * (CAPTION_LINES as f32 + TIDBIT_GAP);
    title + captions
}

/// The least air a card keeps above and below its content.
///
/// This, and not the measured 0.148 h padding, is what would set a card's height
/// if the content ever outgrew [`BASE_HEIGHT_PX`]: the measured padding is what
/// a card *wants*, and at 68 px it gets it — that is almost exactly two 14 px
/// lines, a tidbit and 0.148 h on each side, which is why the captain's base
/// height is that number. On a face whose line height runs larger than the one
/// the base was measured on, the padding gives way to this floor first and the
/// card only grows once it has nothing left to give.
///
/// Cut from 5 px in the reality pass, when every pixel of it was a pixel the
/// tier scale did not get. The tier scale is gone ([`BASE_HEIGHT_PX`]) and this
/// is now slack on nearly every face, but it is still the thing standing between
/// a tall face and a title with no air above it.
const MIN_VERTICAL_PAD_PX: f32 = 3.0;

/// Rows a card occupies, or `None` when the pixel path is not live.
///
/// This is the one place the pixel design reaches back into the character
/// layout. Everything else about a row — where it starts, what it can be
/// clicked to select, whether it scrolls off — is unchanged, but its *height*
/// has to come from the card being drawn or the image would not fill its cells.
///
/// The same answer for every row, whatever its depth or rank: that is what
/// "uniform height" is, measured at the layout boundary rather than only in the
/// rasteriser.
pub(crate) fn row_height_cells(app: &AppState, fold_width: u16) -> Option<u16> {
    if !is_available(app, fold_width) {
        return None;
    }
    let font = font::card_font(app.sidebar_card_font.as_deref())?;
    let cell_height = f32::from(u16::try_from(app.host_cell_size.height_px).ok()?);
    if cell_height <= 0.0 {
        return None;
    }
    let wanted = card_height_px(
        font.metrics(TITLE_PX),
        font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
    );
    // Ceil, then draw at the cell height rather than at `wanted`: a card drawn
    // shorter than its cells would leave a band of the character card showing
    // beneath it.
    let rows = (wanted / cell_height).ceil();
    Some((rows as u16).max(super::card::CHROME_ROWS + 1))
}

/// The bands a crew list is laid out on, in pixels, on this host.
///
/// `None` for exactly the reasons [`row_height_cells`] is: this is the same
/// question about the same layout, asked for the rows *inside* a card rather
/// than for the card itself.
fn crew_bands(app: &AppState, fold_width: u16) -> Option<(crew::CrewBands, f32)> {
    let cell_height = f32::from(u16::try_from(app.host_cell_size.height_px).ok()?);
    if cell_height <= 0.0 {
        return None;
    }
    let font = font::card_font(app.sidebar_card_font.as_deref())?;
    if !is_available(app, fold_width) {
        return None;
    }
    Some((
        crew::CrewBands::of(font, TITLE_PX, cell_height),
        cell_height,
    ))
}

/// Cells one worker row occupies inside its Space's card.
///
/// The layout's own number: a crew row is a row of the panel with its own rect
/// and its own click target, so its height is whole cells and this is the one
/// place that decides how many. The rasteriser is handed the answer rather than
/// deriving a second one — see [`crew::CrewBands`].
pub(crate) fn crew_row_cells(app: &AppState, fold_width: u16) -> Option<u16> {
    let (bands, cell_height) = crew_bands(app, fold_width)?;
    Some(((bands.row / cell_height).round() as u16).max(1))
}

/// Cells the dashed rule costs the row that heads a crew.
///
/// Carried by the *head*, not by the first worker, so every worker row is the
/// same height — which is what lets a row arriving anywhere in the list push by
/// exactly one row's worth.
pub(crate) fn crew_divider_cells(app: &AppState, fold_width: u16) -> Option<u16> {
    let (bands, cell_height) = crew_bands(app, fold_width)?;
    Some(((bands.divider / cell_height).round() as u16).max(1))
}

/// The artwork a card carries in its icon slot, when it carries any.
///
/// Nothing constructs one yet: real per-project marks are their own
/// investigation. The type exists so the slot has a *reason* to be there rather
/// than a hardcoded gap — the layout asks "is there a mark" and sizes itself
/// from the answer, so the day a mark arrives it is one constructor away from
/// being drawn with no relayout. Until then every card answers no and the slot
/// is not reserved at all, which is what stops an empty box from standing on
/// screen eating the width the title needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum CardMark {}

/// A state change part-way across a card.
///
/// # Why this is a front and not a highlight
///
/// [`Shape::Band`] peaks as it passes a cell and leaves it exactly as it was;
/// [`Shape::Front`] leaves everything behind it at full amount. So a band is a
/// shimmer that crosses the card and changes nothing, and a front is a *wash*:
/// when it has crossed, the whole card is in the state it changed into and
/// stays there. That is the acceptance criterion, and it is one enum value.
///
/// # Why it carries the state it came from
///
/// Because the front has to have two sides. The card ahead of the edge is still
/// the state it left, the card behind it is the state it arrived at, and the
/// sweep is the boundary between them travelling — which is what makes the
/// change legible as a change rather than as a card that is suddenly a
/// different colour. A pure render pass cannot know the previous state, so
/// [`crate::app::card_wash`] remembers it.
///
/// [`Shape::Band`]: crate::anim::behaviour::Shape::Band
/// [`Shape::Front`]: crate::anim::behaviour::Shape::Front
#[derive(Debug, Clone, Copy, PartialEq)]
struct CardWashFrame {
    /// The state the card is leaving.
    from: AgentState,
    /// How far through the sweep, in `0.0..=1.0`, quantized to
    /// [`CARD_WASH_STEPS`].
    progress: f32,
    /// The behaviour resolving the front. Copied out of the engine's catalogue
    /// rather than borrowed, so a card's content owns its whole appearance and
    /// nothing here grows a lifetime — the sweep is still the engine's, and no
    /// second one is written anywhere in this file.
    behaviour: crate::anim::behaviour::Behaviour,
}

impl CardWashFrame {
    /// How far the wash has taken this column, in `0.0..=1.0`.
    ///
    /// `t` is across the card, left to right. The columns are handed to the
    /// engine as if they were cells, exactly as [`DissolveFrame::apply`] hands
    /// it a particle grid: the pixels and the characters are then the same
    /// effect at two resolutions rather than two effects kept looking alike by
    /// hand.
    fn amount(self, t: f32) -> f32 {
        use crate::anim::cell::{CellExtent, CellPos};
        // A ladder rather than the card's real pixel width: the front is a
        // smooth function of `t`, so the resolution it is sampled at changes
        // nothing anyone could see, and a fixed extent keeps the arithmetic
        // independent of how wide this particular card happens to be.
        const COLUMNS: u16 = 256;
        let col = (t.clamp(0.0, 1.0) * f32::from(COLUMNS - 1)).round() as u16;
        self.behaviour
            .strength(CellPos::col(col), CellExtent::row(COLUMNS), self.progress)
            .clamp(0.0, 1.0)
    }

    /// The quantized step this frame sits on, for the card's signature.
    fn step(self) -> (u16, u8) {
        (
            (self.progress.clamp(0.0, 1.0) * CARD_WASH_STEPS).round() as u16,
            self.from as u8,
        )
    }
}

/// The five stage hues under the theme in force, resolved once for a card.
///
/// A table rather than a palette borrow because a card's content owns its whole
/// appearance — the same reason [`CardWashFrame`] copies its behaviour out of the
/// catalogue. It carries all five and not just the card's own because a wash has
/// two sides and they are two different stages.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
struct StageHues([f32; 5]);

impl StageHues {
    fn resolve(app: &AppState) -> Self {
        let mut hues = [0.0; 5];
        for (slot, stage) in hues.iter_mut().zip(LifecycleStage::ALL) {
            *slot = stage.hue(&app.sidebar_palette, &app.host_terminal_theme);
        }
        Self(hues)
    }

    fn of(self, stage: LifecycleStage) -> f32 {
        let index = LifecycleStage::ALL
            .iter()
            .position(|candidate| *candidate == stage)
            .unwrap_or(0);
        self.0[index]
    }
}

/// What one card says, and how it is lit while it says it.
struct CardContent {
    title: String,
    /// The card's second line: what this body **is** on a mate
    /// (`gas giant · 99 files · 2 moons`), and what this worker is *doing* on a
    /// moon. See [`super::body_register`].
    tidbit: Option<String>,
    /// The card's third line, and what kind of thing it says.
    ///
    /// A mate's is its orbit register — `streak 5 · T 13.4s · 23 revs`. A
    /// worker's is **its state, as a bare dim lowercase word**: no chip, no
    /// pill, no capsule, no uppercase, which is how the reference draws it and
    /// the only place the reference draws it at all. See [`CaptionTone`].
    register: Option<Caption>,
    /// The state in words. Still carried on every card even where no caption
    /// draws it, because the spider, the breath and the wash all name it.
    state_label: String,
    state: AgentState,
    /// Which stage this card's work is at. **The hue channel, and only that.**
    stage: LifecycleStage,
    /// How bad the problem on this card is. **The intensity channel, and only
    /// that.** Independent of `stage` by construction: neither is derived from
    /// the other anywhere, so every one of the twenty combinations is a card
    /// that can actually be on screen.
    severity: Severity,
    /// The theme's answer for every stage's hue, so the two sides of a wash can
    /// each resolve their own.
    hues: StageHues,
    /// The panel colour this card's ink is placed against. The severity channel
    /// is a *distance from the ground*, so the ground is part of resolving it.
    ground: Rgb,
    /// The colours this card's theme has an authored answer for. See
    /// [`CardTheme`] — every field `None` on a theme that authored none,
    /// which is every built-in theme and the default.
    ///
    /// Per card for the same reason `ground` is: it is one fact for the whole
    /// sheet, but [`CardLight::of`] and [`draw_card`] are reached through a
    /// `&CardContent` and nothing else, so putting it anywhere else would mean
    /// threading a second argument through every one of them.
    theme: CardTheme,
    /// Whether the two channels are switched on. Off draws the reference's own
    /// single hue family with its intensity following the stage, which is what
    /// shipped before the split — see [`crate::config::SidebarCardsConfig`].
    split_channels: bool,
    seen: bool,
    depth: u8,
    lifted: bool,
    /// Whether this row is the active/focused Space. **Not the same claim as
    /// `lifted`**: a worker's `lifted` is the pane the terminal is currently
    /// showing, which the captain's flight-deck mockup never accents, but a
    /// Space's `lifted` — the cursor's row, the active Space, or one being
    /// dragged — is exactly the row the mockup's `.card.active` singles out.
    /// So a Space sets this from the same reading `lifted` is, and a worker
    /// always sets it `false`. See [`CardContent::accented`], the only place
    /// this is read.
    focused_space: bool,
    /// The project mark, once there are any. See [`CardMark`].
    mark: Option<CardMark>,
    /// How many finished workers this mate has taken back, already capped to
    /// the stack the card can draw. See [`crate::app::residue`].
    ///
    /// **Not a state channel.** `stage` is the hue, `severity` is the
    /// intensity, and both say what the work on this card is doing *now*. This
    /// says what has already happened and is over, which is why it draws as
    /// contour lines in the card's own edge colour rather than as a third
    /// colour anybody has to learn.
    residue: u8,
    /// The controls hung on the card's right margin. See [`ControlRail`].
    controls: ControlRail,
    /// The failure spider riding this card, if the fleet says it owns an open
    /// defect. See [`spider`].
    spider: Option<spider::CardSpider>,
    /// This card's own opacity, `0.0..=1.0`.
    ///
    /// The card-bloom beat of [`super::motion::ArrivalCircuit`]: the card
    /// fades in whole, at its own final position and size, never a clip or a
    /// translation. `0.0` is a card that does not exist yet and `1.0` is every
    /// card at rest, which is every card on a settled panel. The field is
    /// still named `generate` on the wire — see [`CardContentWire`] — because
    /// renaming it would be a wire-protocol change with nothing behavioural to
    /// show for it.
    generate: f32,
    /// How hard this row's work is running, `0.0..=1.0` — a share of the
    /// fleet's own traffic, and zero on anything that is not working.
    ///
    /// Drawn as filaments *behind* the face, so it cannot make the pane read as
    /// opaque. See [`draw_discharge`].
    discharge: f32,
    /// This frame of the card's breath, quantized to [`CARD_BREATH_STEPS`].
    /// `0.0` is the card at its own settled light, which is what a host with no
    /// card animation draws, and `1.0` is a full breath — but a snapping
    /// behaviour carries *past* `1.0` on its overshoot and this holds that too.
    /// See [`quantize`] for why the ladder has no ceiling.
    breath: f32,
    /// The state change crossing the card right now, if one is.
    wash: Option<CardWashFrame>,
    /// The workers this Space is running, drawn inside this card's own box
    /// under a dashed rule. Empty on every worker's card and on a Space running
    /// nothing, which is the branch a card took before this existed.
    ///
    /// Filled in by [`compute_card_placement`] rather than by [`content_for`]:
    /// a crew is a fact about a row's *neighbours* — the entries the tree walk
    /// put under it — and `content_for` is handed one entry. See [`crew_for`].
    crew: Vec<crew::CrewMember>,
    /// The mockup's literal `.bars`/`.bar` sparkline — up to
    /// [`crate::quality_streak::BARS_MAX`] recent-activity heights, oldest
    /// first, or `None` on a card no publisher has sent a
    /// [`crate::quality_streak::BARS_TOKEN`] for. A worker never carries one:
    /// the token is read off a Space's own metadata, the same scope
    /// [`CardContent::register`]'s orbit line is.
    bars: Option<Vec<u8>>,
}

impl CardContent {
    fn hash_into(&self, hasher: &mut DefaultHasher) {
        self.title.hash(hasher);
        self.tidbit.hash(hasher);
        // The orbit line moves on its own — a revolution completes without
        // anything else about the row changing — so a signature blind to it
        // would carry a stale `N revs` forward forever.
        self.register.hash(hasher);
        // Read fresh every render like the streak it sits beside — see
        // `BARS_TOKEN` — so a card carried forward on a stale signature would
        // freeze the sparkline exactly the way an unhashed orbit line would.
        self.bars.hash(hasher);
        self.state_label.hash(hasher);
        (self.state as u8).hash(hasher);
        self.stage.hash(hasher);
        self.severity.hash(hasher);
        // The resolved hues and the ground go in as bits: a theme change moves
        // them without moving anything else about the card, and a card carried
        // forward on a stale signature would keep the old theme's ink.
        for hue in self.hues.0 {
            hue.to_bits().hash(hasher);
        }
        self.ground.hash(hasher);
        // For the same reason the hues and the ground go in: a theme change
        // moves the card's colours without moving anything else about it, and
        // a card carried forward on a stale signature would keep the old
        // theme's ink.
        self.theme.hash(hasher);
        self.split_channels.hash(hasher);
        self.seen.hash(hasher);
        self.depth.hash(hasher);
        self.lifted.hash(hasher);
        self.focused_space.hash(hasher);
        self.mark.is_some().hash(hasher);
        // A ring is settled state, so nothing else about the card moves when
        // one is added: a signature blind to this would carry the old pixels
        // forward and the sixth absorption would never appear.
        self.residue.hash(hasher);
        // Every part of it: a worker reporting back, a summary being read, and a
        // group being folded all change the card's pixels and nothing else about
        // it, so a card carried forward on a signature blind to this would keep
        // a stale count or a chevron pointing the wrong way.
        self.controls.hash(hasher);
        // Presence first, then the frame: a row that has just been marked and a
        // row that has none are two different cards even before the marker has
        // moved. See [`spider::CardSpider::hash_into`] for why the frame itself
        // is cheap to hash every pass.
        self.spider.is_some().hash(hasher);
        if let Some(spider) = &self.spider {
            spider.hash_into(hasher);
        }
        // Both quantized before they reach here, so a card whose light has not
        // moved by a step anyone could see hashes the same and is carried
        // forward without being redrawn. This is the whole of what keeps a tree
        // of breathing cards from rasterising on every frame.
        ((self.breath * CARD_BREATH_STEPS).round() as u16).hash(hasher);
        // Both quantized for the same reason the breath is: a card mid-arrival
        // or mid-discharge whose picture has not moved by a step anyone could
        // see is carried forward rather than redrawn.
        ((self.generate * GENERATE_STEPS).round() as u16).hash(hasher);
        ((self.discharge * DISCHARGE_STEPS).round() as u16).hash(hasher);
        self.wash.map(CardWashFrame::step).hash(hasher);
        // The crew is part of the card's picture, so a card whose worker list
        // changed — a row arriving, a row's own track opening a step, a status
        // line being republished — is a different card and has to be redrawn.
        // A signature blind to this would freeze the list at whatever it was
        // the last time the card's own state moved.
        self.crew.len().hash(hasher);
        for member in &self.crew {
            member.hash_into(hasher);
        }
    }

    /// Whether this card earns the mockup's strong cyan accent — a border
    /// drawn at full saturation and luminance, with the outer bloom laid at
    /// its peak reach.
    ///
    /// Exactly two reasons, per the captain's own read of the approved
    /// flight-deck mockup against a live capture: this is the focused Space
    /// (`focused_space`), or this card is still mid its own arrival bloom
    /// (`generate < 1.0`, [`super::motion::ArrivalCircuit::card`]). Neither
    /// `state` nor `severity` is consulted — a card's own work state now
    /// reaches the badge, the chip and the discharge filaments, never the
    /// border, so a `working` card sitting quietly off to the side draws
    /// exactly the same thin edge as an `idle` one.
    fn accented(&self) -> bool {
        self.focused_space || self.generate < 1.0
    }

    /// Whether this is a worker's card rather than a Space's — the mockup's
    /// `.card.worker`, which alone carries a `.wk-dot` before its name.
    ///
    /// Read off `register`'s own tone rather than a dedicated field: a
    /// worker's register is *always* `Some(Caption { tone: State, .. })` — its
    /// third line is its state as a bare word, per [`content_for`]'s `Agent`
    /// arm — and a Space's is either absent or `Register` — its own orbit
    /// line, never `State`. No card is built any other way, so the tone
    /// already carries this distinction without a second source of truth to
    /// keep in sync with it.
    fn is_worker(&self) -> bool {
        matches!(
            self.register,
            Some(Caption {
                tone: CaptionTone::State,
                ..
            })
        )
    }

    /// The light of one stage on this card, at this card's severity.
    ///
    /// The two channels are supplied from two different places and meet only in
    /// [`CardLight::of`]: the stage decides which of the five hues is handed
    /// over, the severity decides how far off the panel it is placed, and
    /// neither is consulted about the other's number.
    fn light_of(&self, stage: LifecycleStage) -> CardLight {
        CardLight::of(
            self.severity,
            self.hues.of(stage),
            self.ground,
            self.split_channels,
            self.accented(),
            self.theme,
        )
    }

    /// The light this card has arrived at: the stage it is at, breathing.
    fn arrived_light(&self) -> CardLight {
        self.light_of(self.stage).breathed(self.breath)
    }

    /// The light ahead of a wash's front: the state the card left, breathing.
    ///
    /// The breath is applied to *both* sides of the front rather than to the
    /// result, because a card breathes throughout a state change — breathing
    /// only the destination would make a wash look like the moment the card's
    /// breath was switched on.
    /// The severity is deliberately *not* swept: a wash carries a change of
    /// stage, and how bad the row's trouble is did not change because its work
    /// moved on. Sweeping both would make the two channels one again on exactly
    /// the frames a reader is looking hardest.
    fn leaving_light(&self) -> Option<CardLight> {
        self.wash.map(|wash| {
            self.light_of(crate::app::lifecycle::stage(None, wash.from))
                .breathed(self.breath)
        })
    }

    /// The light the card's chrome and its type are drawn in.
    ///
    /// The destination, never the sweep. Two reasons, and they are different
    /// reasons. The chip already *says* the new state in words, so drawing it
    /// in the old state's ink would be a mark contradicting its own label. And
    /// type is held out of both effects entirely: a title that breathed would
    /// be a title that is periodically harder to read, and the visual-target
    /// spec's digestibility condition does not take a break for half of every
    /// cycle.
    fn settled_light(&self) -> CardLight {
        self.light_of(self.stage)
    }
}

#[cfg(test)]
impl CardContent {
    /// The light this card would be drawn in were its work at the stage `state`
    /// implies, at this card's own severity.
    fn light_at(&self, state: AgentState) -> CardLight {
        self.light_of(crate::app::lifecycle::stage(None, state))
    }
}

/// One caption line, and the weight it is set at.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Caption {
    text: String,
    tone: CaptionTone,
}

/// How loudly a caption is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum CaptionTone {
    /// A readout — a register line, a task, a project. Caption weight.
    Register,
    /// The row's state, as a bare lowercase word.
    ///
    /// A rung quieter than a register, and lowercased on the way to the canvas.
    /// The card's own colour and breath already say what state it is in; this is
    /// the *name* of that state for a reader who wants it spelled, not a second
    /// signal competing with the line carrying numbers. It was a chip on the
    /// first content row and a capitalised pill on the last until the reference
    /// settled it — a word in capitals is a badge whether or not it has a box
    /// drawn round it.
    State,
}

/// The colours a card is drawn in that the *theme* has an answer for.
///
/// # Why every field is optional
///
/// Because [`measured`] is the authority and this is an override, never the
/// other way round. Each field is `Some` only when the user *authored* that
/// role in `[theme.custom]` — a built-in theme leaves every one of them
/// `None`, and so does the default. That keeps the captain's 2026-08-13
/// decision D-c intact by construction: with no custom colours the panel
/// still draws the one measured hue family the reference was sampled from,
/// byte for byte, and every measured test still measures the measurement.
///
/// # Why the resolved palette is not read directly
///
/// [`crate::app::state::AppState::sidebar_palette`] always has an `accent` —
/// Catppuccin's is `#89b4fa` — so reading it would repaint every default
/// user's cards blue for a preference nobody expressed. A `[theme.custom]`
/// entry is the one signal that is unambiguously a *statement about this
/// colour*, which is exactly the authority an override needs.
///
/// # What each field answers
///
/// The roles are the "Rio Window, Assembled" mockup's own `:root` tokens,
/// mapped onto the palette roles that already mean the same thing, so a theme
/// that sets them gets the mockup and a theme that sets them differently gets
/// itself:
///
/// | mockup | palette role | what it draws |
/// |---|---|---|
/// | `--cyan` | `accent` | `.wk-dot`, `.wrow` rail, `.card.active` border |
/// | `--edge` | `surface0` | `.card` border, `hr.divider` |
/// | `--panel` | `panel_bg` | the glass face's own tint |
/// | `--ink` | `text` | the card's title |
/// | `--ok` | `green` | `.badge` |
/// | `--amber` | `yellow` | `.badge.warn` |
///
/// `--cyan-dim` needs no role of its own: the mockup's own second tier is
/// `--cyan` dimmed, which is what [`crew::CrewMember`]'s tier presence
/// already does to whatever ink it is handed.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct CardTheme {
    accent: Option<Rgb>,
    edge: Option<Rgb>,
    face: Option<Rgb>,
    ink: Option<Rgb>,
    ok: Option<Rgb>,
    warn: Option<Rgb>,
}

impl CardTheme {
    /// No role authored: every colour comes from [`measured`]. The default,
    /// and what every card built before a `[theme.custom]` block existed
    /// carries.
    pub(crate) const UNTHEMED: Self = Self {
        accent: None,
        edge: None,
        face: None,
        ink: None,
        ok: None,
        warn: None,
    };

    /// The roles this app's `[theme.custom]` block authored, resolved against
    /// the host theme exactly as [`backdrop_rgb`] and [`rail_rgb`] are.
    ///
    /// Reads `theme_runtime.custom` rather than `sidebar_palette` for the
    /// reason the type's own doc gives — a resolved palette cannot say which
    /// of its colours anyone chose. The *value* still comes from the palette
    /// path where it can: a custom entry is parsed by
    /// [`crate::config::parse_color`], the same function
    /// [`crate::app::state::Palette::with_overrides`] uses, so the two cannot
    /// disagree about what `#5ad1ff` means.
    fn resolve(app: &AppState) -> Self {
        let Some(custom) = app.theme_runtime.custom.as_ref() else {
            return Self::UNTHEMED;
        };
        let host = &app.host_terminal_theme;
        let role = |authored: &Option<String>| -> Option<Rgb> {
            let parsed = crate::config::parse_color(authored.as_ref()?);
            crate::ui::color::resolve_color_rgb(parsed, host).map(|(r, g, b)| Rgb(r, g, b))
        };
        Self {
            accent: role(&custom.accent),
            edge: role(&custom.surface0),
            face: role(&custom.panel_bg),
            ink: role(&custom.text),
            ok: role(&custom.green),
            warn: role(&custom.yellow),
        }
    }

    /// The card's full-strength accent: its lit dots, its crew rails, and the
    /// border of the one Space the panel is accenting.
    fn accent(self) -> Rgb {
        self.accent.unwrap_or(measured::STROKE_A)
    }

    /// A resting card's own border, and the dashed rule inside it.
    ///
    /// Unthemed this is the accent walked back to the `Queued` endpoint of the
    /// one-hue ramp, which is what shipped and what the reference measures. A
    /// theme that authored `--edge` gets it flat instead: the mockup's
    /// `.card { border: 1px solid var(--edge) }` is a *stated* colour, not a
    /// restatement of the accent, and restating it again would land somewhere
    /// neither the theme nor the reference asked for.
    fn edge(self) -> Rgb {
        self.edge.unwrap_or_else(|| {
            let mix = crate::anim::cell::one_hue_stage_mix(LifecycleStage::Queued);
            self.accent().restate(mix.saturation, mix.luminance)
        })
    }

    /// The card's own type ink.
    fn ink(self) -> Rgb {
        self.ink.unwrap_or(measured::INK)
    }

    /// The glass face's tint. See [`measured::GLASS_FACE`] — this changes
    /// which colour the tint is, never how much of it reaches the pixel.
    fn face(self) -> Rgb {
        self.face.unwrap_or(measured::GLASS_FACE)
    }

    fn badge_ok(self) -> Rgb {
        self.ok.unwrap_or(measured::BADGE_OK)
    }

    fn badge_warn(self) -> Rgb {
        self.warn.unwrap_or(measured::BADGE_WARN)
    }
}

/// The ground the cards float on.
///
/// The reference's own canvas is `#09111C`, but the ground under a card is
/// whatever `render_sidebar` actually fills the panel with, and that is
/// `palette.sidebar_bg`. Its default is `Color::Reset` — "inherit the host" —
/// so with no theme override the ground is the RGB Herdr measured with OSC 11,
/// then the panel's own background, and the measured canvas only as a last
/// resort for a host that answered neither. A theme that *does* set a sidebar
/// background takes precedence over the host's, because that fill is the pixel
/// the card's antialiased edge lands on; measuring against the host instead
/// puts a seam around every card.
///
/// It matters because the bloom is *lift*: the reference has no drop shadow
/// anywhere, and its cards float by being brighter than the ground rather than
/// by casting onto it. A bloom with nothing under it to lift is invisible.
fn backdrop_rgb(app: &AppState) -> Rgb {
    crate::ui::sidebar::backdrop_rgb(app)
        .map(|(r, g, b)| Rgb(r, g, b))
        .unwrap_or(measured::CANVAS)
}

/// The ink the tree's own rails are drawn in, read off the same palette the
/// character renderer styles them from.
///
/// Resolved against the host theme exactly as [`backdrop_rgb`] is, because a
/// join painted in a hardcoded grey beside a rail the terminal drew in the
/// user's `overlay0` is two colours where the eye expects one line.
fn rail_rgb(app: &AppState) -> Rgb {
    crate::ui::color::resolve_color_rgb(app.sidebar_palette.overlay0, &app.host_terminal_theme)
        .map(|(r, g, b)| Rgb(r, g, b))
        .unwrap_or(measured::STROKE_A)
}

/// The chip's ink.
///
/// The card's own stage hue, so the chip that *names* the state and the card
/// that *is* it cannot say two different things.
///
/// **At a fixed rung of the severity ramp rather than at the card's own**, and
/// that is deliberate: the chip carries a *word*. Placed at its card's severity
/// it goes as dim as [`Severity::Clear`] asks for, and a clear card — the
/// commonest card in a healthy fleet — then has the one label on it that cannot
/// be read. This is the same rule #60 applied to the title and the breath: type
/// is held out of the light effects, because the visual target's digestibility
/// condition does not take a break for the cards that are fine. Severity is
/// carried by the card's body, which has no words on it.
fn chip_ink(content: &CardContent) -> Rgb {
    if !content.split_channels {
        // The measured family, exactly as the density and icon passes were
        // rendered and reviewed at: one hue, and state carried by saturation
        // and lightness inside it.
        let (h, s, l) = match (content.state, content.seen) {
            (AgentState::Blocked, _) => (181.0, 0.75, 0.72),
            (AgentState::Working, _) => (192.0, 0.62, 0.66),
            (AgentState::Idle, false) => (205.0, 0.40, 0.52),
            (AgentState::Idle, true) => (210.0, 0.16, 0.42),
            (AgentState::Unknown, _) => (210.0, 0.10, 0.36),
        };
        // The angles are the *measured* family's, and they move with the theme
        // for the reason the whole table exists: they were sampled as one hue
        // 175–265° with state carried by saturation and lightness alone. A
        // theme that moved the accent and left these where they were would put
        // the chip in a second hue family beside cards drawn in the first,
        // which is the one thing the measurement rules out. Each angle keeps
        // its own offset from the family's own centre, so the state ladder the
        // table encodes survives the move intact.
        let (h, s, l) = match content.theme.accent {
            Some(accent) => (h + accent.to_hsl().0 - measured::STROKE_A.to_hsl().0, s, l),
            None => (h, s, l),
        };
        return Rgb::from_hsl(h.rem_euclid(360.0), s, l);
    }
    Rgb::from_tuple(crate::anim::cell::signal_ink(
        content.hues.of(content.stage),
        CHIP_RUNG,
        content.ground.as_tuple(),
    ))
}

/// The rung of the severity ramp every chip is drawn at.
///
/// High enough that the label reads on any theme, and not the top, so a chip on
/// a critical card is still quieter than the card around it rather than
/// competing with it.
const CHIP_RUNG: Severity = Severity::Serious;

/// The light one card is drawn in: how saturated its ink is, how bright, and
/// how far its bloom lifts it off the panel.
///
/// One value rather than three loose floats because the breath and the wash
/// both act on all three together, and they have to: the visual-target spec
/// asks a resting card to read as *recessed*, and recession is a depth cue —
/// *"consider glow radius, saturation and contrast against the background
/// together, not brightness alone."* A breath that moved only the luminance
/// would be a dimmer, which is the thing that spec rules out by name.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CardLight {
    /// The card's own ink at this light: **the hue channel and the intensity
    /// channel, already resolved into one colour.** Everything below this point
    /// only ever restates it, and restating in HSL cannot move a hue — which is
    /// what makes the stage channel inviolable no matter what the breath, the
    /// wash or the presence cue do afterwards.
    ink: Rgb,
    /// What the breath has taken off this card's luminance, in `0.0..=1.0`.
    lum: f32,
    /// How much of the card's measured bloom is laid down, in `0.0..=1.0`.
    ///
    /// **The third channel, and neither of the two.** This is depth: the visual
    /// target asks a card with nothing behind it to read as *"on the back
    /// burner — dimmed or recessed slightly"*, and names recession as a depth
    /// cue rather than a dimmer. So presence is carried by the lift alone and
    /// keeps its hands off the ink, which is what lets the severity channel own
    /// contrast outright: a queued card and a running card at the same severity
    /// stand equally far off the panel in *ink* and differently far off it in
    /// *lift*.
    bloom: f32,
}

impl CardLight {
    /// The light one severity is drawn in, over `ground`, at one card's
    /// accent.
    ///
    /// The one place the two channels meet, and they meet by being handed to
    /// different arguments of one function that never crosses them: `hue` goes
    /// only into the hue slot, `severity` only into the saturation and contrast
    /// slots. `accented` is consulted a second time for the bloom, which is the
    /// depth cue and not either channel — see [`CardLight::bloom`].
    ///
    /// **Not `stage`.** The border used to speak a card's own `LifecycleStage`
    /// directly — a queued row dim, a running one at full strength — which is
    /// exactly the "working card glows all the time" defect the captain's
    /// `herdr-card-border-dot-final-match-20260822` screenshots caught: the
    /// approved mockup reserves the strong accent for the focused Space and a
    /// card mid its own arrival, never for a card's work state. `accented`
    /// carries that binary instead — see [`CardContent::accented`].
    fn of(
        severity: Severity,
        hue: f32,
        ground: Rgb,
        split_channels: bool,
        accented: bool,
        theme: CardTheme,
    ) -> Self {
        let ink = if split_channels {
            Rgb::from_tuple(crate::anim::cell::signal_ink(
                hue,
                severity,
                ground.as_tuple(),
            ))
        } else if accented {
            // The reference's own answer to "what carries state without a
            // rainbow": one hue, at the `Running` endpoint of the ramp for the
            // one card the panel is accenting — never picked by the card's own
            // stage any more.
            //
            // Still routed through `restate` even though `Running`'s own mix is
            // `(1.0, 1.0)`: that is an HSL round trip, and dropping it would
            // move an unthemed card's stroke by a rounding step for no reason
            // anyone asked for. The only thing this line changes is *which*
            // colour is being restated.
            let mix = crate::anim::cell::one_hue_stage_mix(LifecycleStage::Running);
            theme.accent().restate(mix.saturation, mix.luminance)
        } else {
            // Every other card sits at the `Queued` endpoint — or, on a theme
            // that authored `--edge`, at that colour flat. See
            // [`CardTheme::edge`].
            theme.edge()
        };
        Self {
            ink,
            lum: 1.0,
            bloom: presence(accented),
        }
    }

    /// The same light after one frame of this card's breath.
    ///
    /// The swing is **subtracted**, always, whichever behaviour supplied it.
    /// That is the whole reading: a card breathes by settling *back* into the
    /// panel and returning, never by brightening past its own state's light. A
    /// breath that went the other way would make an idle card periodically
    /// brighter than a working one at its trough, which inverts the only thing
    /// the card's light is for.
    ///
    /// The bloom gives further than the ink does, because the bloom is the
    /// depth cue — it is the lift that makes a card float, and taking the lift
    /// away is what *recessed* looks like. Saturation is left alone: the
    /// reference's own inactive card holds its hue, and a breath that
    /// desaturated would read as the card losing its state rather than as the
    /// card resting.
    ///
    /// The envelope is taken **whole**, overshoot included. A snap that carries
    /// ten percent past its target is a card that settles ten percent further
    /// back than a full breath before it comes forward again, and that extra
    /// travel at the deepest point is the snap being visible at all. Both dips
    /// stay comfortably positive there — at `1.1` the ink is at `×0.868` and
    /// the bloom at `×0.604` — so nothing downstream needs the cap that used to
    /// be here. Only the floor is kept: a negative envelope would *brighten*
    /// the card past its own state's light, which is the one thing this
    /// function's whole reading rules out.
    fn breathed(self, envelope: f32) -> Self {
        let swing = envelope.max(0.0);
        Self {
            ink: self.ink,
            lum: self.lum * (1.0 - BREATH_LUM_DIP * swing),
            bloom: self.bloom * (1.0 - BREATH_BLOOM_DIP * swing),
        }
    }

    /// The stroke and the bloom, at this light.
    ///
    /// **One flat colour, not a left-to-right gradient.** The captain's
    /// 2026-08-23 mockup-convergence decision
    /// (`card-corner-radius-and-stroke-vs-mockup`) replaced the card's
    /// previous per-card cyan-to-blue gradient stroke with "Rio Window,
    /// Assembled"'s own `.card { border: 1px solid var(--edge) }` — a single
    /// flat edge colour on every card, never a travel across the card's own
    /// width. `stroke_a` and `stroke_b` stay as two fields (rather than one)
    /// only because [`CardInks::at`] still mixes a card *between* two
    /// lifecycle states over the wash's time axis — that temporal mix is
    /// unrelated to this removed spatial one and still needs both ends.
    fn inks(self) -> CardInk {
        let (h, s, l) = self.ink.to_hsl();
        let l = l * self.lum;
        let flat = Rgb::from_hsl(h, s, l);
        let bloomed = flat.restate(measured::BLOOM_SAT_MUL, measured::BLOOM_LUM_MUL);
        CardInk {
            stroke_a: flat,
            stroke_b: flat,
            bloom_a: bloomed,
            bloom_b: bloomed,
            bloom: self.bloom,
        }
    }
}

/// How far off the panel an accented card stands, before the breath moves it.
///
/// The depth channel. It used to be kept on stage — work in flight forward, a
/// queue flat, a finished card part-way back — but that is the same
/// state-driven glow the border's own ink gave up in [`CardLight::of`]: the
/// mockup's box-shadow lift belongs to the focused Space and an arriving card
/// alone, never to what a card's own work happens to be doing. So this is
/// binary now, on the same signal.
fn presence(accented: bool) -> f32 {
    if accented {
        1.0
    } else {
        0.0
    }
}

/// The colours one column of a card is drawn from.
///
/// `stroke_a`/`stroke_b` and `bloom_a`/`bloom_b` are equal now — see
/// [`CardLight::inks`] — kept as pairs only because [`CardInks::at`] still
/// mixes a card between two lifecycle states over the wash's *time* axis, a
/// separate axis from the removed left-to-right spatial gradient.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CardInk {
    stroke_a: Rgb,
    stroke_b: Rgb,
    bloom_a: Rgb,
    bloom_b: Rgb,
    /// How much of the card's measured bloom this column lays down.
    bloom: f32,
}

impl CardInk {
    /// This ink `t` of the way toward `other`.
    ///
    /// Mixed as resolved colour rather than as [`CardLight`], and that is the
    /// cheap half of the wash: the two states either side of the front are
    /// fixed for a whole frame, so they are converted out of HSL once each and
    /// a column is three channel lerps. Converting per column would cost more
    /// arithmetic than drawing the card's pixels does.
    fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        Self {
            stroke_a: self.stroke_a.mix(other.stroke_a, t),
            stroke_b: self.stroke_b.mix(other.stroke_b, t),
            bloom_a: self.bloom_a.mix(other.bloom_a, t),
            bloom_b: self.bloom_b.mix(other.bloom_b, t),
            bloom: self.bloom + (other.bloom - self.bloom) * t,
        }
    }
}

/// How far a full breath pulls a card's own luminance down.
///
/// Small. The card has to stay exactly as readable at the trough of its breath
/// as at the crest — the spec's *digestibility* condition is not suspended for
/// half of every cycle — so the ink barely moves and the bloom carries the
/// swing.
const BREATH_LUM_DIP: f32 = 0.12;

/// How far a full breath pulls a card's bloom down.
///
/// Three times the ink's dip, and this asymmetry is the effect. The bloom is
/// what lifts a card off the panel (see [`lay_bloom`]), so breathing it is
/// breathing the card's apparent *depth* — the card sinks back and comes
/// forward while its own body stays legible throughout.
const BREATH_BLOOM_DIP: f32 = 0.36;

/// Steps of the breath envelope a card's artwork can actually tell apart.
///
/// This is the card path's real cost dial, and it is set here rather than left
/// to the frame tier because the two do different jobs: the tier says how often
/// the engine is *asked*, and this says how often the answer is different enough
/// to be worth a rasterisation. A card whose quantised step has not moved is
/// carried forward by signature with nothing redrawn — which is what keeps a
/// tree of breathing cards off the frame-time tail.
///
/// # Why the ladder is sized by interval and not by amplitude
///
/// This was twelve, on the reasoning that a step of the breath was under two
/// per cent of the card's light and so below what the panel's own dithering
/// would show. The amplitude argument is sound and it is the wrong axis: an eye
/// following a slow ramp reads the *interval between changes*, not the size of
/// one, so a step nobody can see individually still ticks if it arrives every
/// sixth of a second. Twelve steps across [`crate::anim::behaviour`]'s 5,200 ms
/// rest breath is a change every 160 ms — 4.8 a second, against the ~62 the
/// loop offers — which is exactly the tick a resting tree was reported to have.
///
/// Forty-eight puts the rest breath's median step at 40 ms and a live card's at
/// 10 ms, and it is also what renders the snap's overshoot at its stated size:
/// a ladder of twelve rounds a 10% overshoot down to 8.3%, and this one lands
/// it at 10.4%.
///
/// # What it costs, measured
///
/// Through the real builder at the real 16 ms loop, release, a ten-card tree at
/// a 42-column sidebar, three runs of 20 s each: rebuilds go from **14.8 to
/// 41.0 a second** and the card path's mean load from **5.6–7.9% of one core to
/// 16.0–17.0%**.
///
/// The **tail does not move**, which is the number that matters for a 60 fps
/// floor. Worst frame 8.8–10.2 ms before against 9.1–12.7 ms after, p99.9
/// 8.8–9.3 ms against 9.0–10.7 ms — the same distribution, because the worst
/// frame is a whole-tree redraw either way and a finer ladder makes those
/// frames more *frequent*, not more expensive. What actually moves is the
/// median, 0.04 ms to 3.1 ms, which is the cost being paid and not a tail risk.
/// Both stay inside a 16.67 ms budget with room over.
///
/// Going further buys little: the loop cannot show more than ~62 changes a
/// second, and 96 steps doubles the cost again for a median step already under
/// the frame interval.
pub(super) const CARD_BREATH_STEPS: f32 = 48.0;

/// Steps of a card's bloom opacity the artwork is rebuilt at.
///
/// The same ladder the wash uses and for the same reason: opacity is a
/// *continuous* fade, and a coarse ladder reads as the card flickering between
/// discrete shades rather than as smoothly blooming in.
const GENERATE_STEPS: f32 = 24.0;

/// Steps of the discharge the artwork is rebuilt at.
///
/// Much coarser than either. A discharge is an *amplitude*, not a position, and
/// its whole range is a few levels of alpha on filaments already at the bottom
/// of the ink ladder — eight steps is finer than the difference anyone can see
/// and eight times cheaper than tracking a worker's byte counter into the
/// signature.
const DISCHARGE_STEPS: f32 = 8.0;

/// Steps of the wash's sweep the artwork is rebuilt at.
///
/// Finer than the breath, because a *front* crossing a card is the one thing
/// here whose position is legible: a coarse ladder reads as the edge stepping
/// rather than travelling. Twenty-four steps over the configured duration is
/// finer than the 50 ms frame tier can deliver at any wash under about 1.2 s, so
/// in practice the tier is the binding constraint and this only stops a faster
/// loop from paying more. The same reasoning, and the same number, as
/// [`DISSOLVE_STEPS`].
const CARD_WASH_STEPS: f32 = 24.0;

/// The resolved pixel geometry of one card.
struct CardGeometry {
    radius: f32,
    stroke: f32,
    pad: f32,
    pad_right: f32,
    plate: f32,
    plate_radius: f32,
    plate_gap: f32,
    bloom_sigma: f32,
}

impl CardGeometry {
    /// Resolved against the *nominal* height rather than the card's drawn
    /// height.
    ///
    /// Every ratio in the measured table is a fraction of `h`, and the card's
    /// drawn height is `max(nominal, content)` — so measuring the padding and
    /// the plate against the drawn height would give a card the content pushed
    /// taller thicker chrome than its neighbours. The extra height is slack, and
    /// slack belongs to the gap around the content, not to the chrome.
    ///
    /// Takes no depth: since the tiers were retired ([`BASE_HEIGHT_PX`]) the
    /// nominal is one number, so the padding, the stroke, the radius, the plate
    /// and the bloom's sigma are identical on every rank. That is deliberate —
    /// the chrome was the second, quieter size signal the tier scale carried,
    /// and rank is width alone.
    ///
    /// `has_mark` collapses the icon slot. An empty plate is not a placeholder,
    /// it is a box; and at 0.70 h plus its gap it was the single widest thing
    /// on the card that carried no information, taking that width from the one
    /// thing that does. The slot keeps its measured size for the day something
    /// goes in it and is worth nothing until then.
    fn new(cell_height: f32, has_mark: bool) -> Self {
        let nominal = nominal_height_px(cell_height);
        Self {
            // Capped, not scaled: F6 is an absolute — *no radius above 3 px* —
            // and a ratio of the card's height is a rounded plate at every
            // height a card is ever drawn at.
            radius: (measured::RADIUS * nominal).clamp(1.0, measured::RADIUS_MAX_PX),
            stroke: (measured::STROKE_W * nominal).max(1.2),
            pad: measured::PAD * nominal,
            pad_right: measured::PAD_RIGHT * nominal,
            plate: if has_mark {
                (measured::PLATE * nominal).min(measured::PLATE_MAX_PX)
            } else {
                0.0
            },
            plate_radius: measured::PLATE_RADIUS * nominal,
            plate_gap: if has_mark {
                measured::PLATE_GAP * nominal
            } else {
                0.0
            },
            bloom_sigma: bloom_sigma_px(cell_height),
        }
    }

    /// Where the card's ink starts, measured in from its left edge.
    ///
    /// One expression rather than two, so the collapsed slot and the occupied
    /// one cannot be spelled differently by the layout and the renderer — which
    /// is the shape of bug that put a title behind a plate in the first place.
    fn text_inset(&self) -> f32 {
        self.pad + self.plate + self.plate_gap
    }
}

/// Greedy word wrap into at most `max_lines`, with no ellipsis anywhere.
///
/// The captain's rule is that a title that does not read wants a better
/// summary, and producing that summary is the fleet's job rather than Herdr's.
/// So this never shortens a word and never appends a mark saying it gave up: it
/// fills the lines it has with whole words and stops. A single word wider than
/// the column is drawn and clipped at the column edge by the caller, which is
/// the one case where there is nothing to break on.
///
/// `avail` is `(the first line, every line after it)`, and the two really do
/// differ. The card's first title line is the one the control rail stands over
/// — see [`ControlRail`] — and the character row this is a skin over reserves
/// those same two controls on its first content row and no other. So the rail
/// costs one line its width rather than costing the title block its width,
/// which on the narrowest panel Herdr draws cards on is the difference between
/// a title that sets whole and one that loses its last word.
fn wrap_ragged(
    font: &CardFont,
    text: &str,
    px: f32,
    avail: (f32, f32),
    max_lines: usize,
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let width_of = |index: usize| if index == 0 { avail.0 } else { avail.1 };
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if font.width(&candidate, px) <= width_of(lines.len()) || current.is_empty() {
            current = candidate;
            continue;
        }
        lines.push(std::mem::take(&mut current));
        if lines.len() == max_lines {
            return lines;
        }
        current = word.to_string();
    }
    if !current.is_empty() && lines.len() < max_lines {
        lines.push(current);
    }
    lines
}

/// One card's placement in sheet pixels, and the state it is drawn in.
struct PlacedCard<'a> {
    /// The card's own rectangle, which is *inside* the cells the row occupies:
    /// the leftover is the gutter that separates it from its neighbours.
    rect: RoundRect,
    content: &'a CardContent,
    geometry: CardGeometry,
    /// The bands this host lays a crew list on. Carried rather than resolved in
    /// [`draw_card`] so the height the card was *placed* at and the height its
    /// worker rows are *drawn* at come from one number. See
    /// [`crew::CrewBands`].
    crew: crew::CrewBands,
}

impl PlacedCard<'_> {
    /// Where a pixel column sits across this card, in `0.0..=1.0`.
    ///
    /// Clamped, so the columns either side of the card — the ones its bloom
    /// reaches into — resolve to the card's own two ends rather than running
    /// off the field. A halo lit by a state its card never reached would be a
    /// wash that leaked past the shape it crossed.
    fn column_t(&self, x: u32) -> f32 {
        (((x as f32 + 0.5) - self.rect.x) / self.rect.w).clamp(0.0, 1.0)
    }

    /// This card's inks, resolved once, ready to be read at any column.
    ///
    /// Resolving them per column would be the obvious way to write the wash and
    /// it is the expensive one: [`Rgb::restate`] is an HSL round trip, a card is
    /// a few hundred pixels wide, and its bloom band is wider still — so a
    /// per-column resolve costs more arithmetic than drawing the card's pixels
    /// does. The two states either side of the front are *fixed* for the whole
    /// of a frame; only the amount between them varies. So both are converted
    /// once and a column is three channel mixes.
    fn inks(&self) -> CardInks {
        CardInks::of(self.content)
    }

    /// The card's ink and luminance at its settled light, for the chrome and
    /// the type that stay out of both effects. See
    /// [`CardContent::settled_light`].
    fn settled_inks(&self) -> (Rgb, f32) {
        let light = self.content.settled_light();
        (light.inks().stroke_a, light.lum)
    }
}

/// One card's inks, and the front between its two states.
///
/// The whole cost argument of the wash lives here. A card resolves two inks at
/// most — the state it is leaving and the state it arrived at — and reading a
/// column is a mix between them at the amount the engine's own field gives that
/// column. With no wash there is one ink and a column is a copy.
#[derive(Debug, Clone, Copy)]
struct CardInks {
    into: CardInk,
    from: Option<CardInk>,
    wash: Option<CardWashFrame>,
}

impl CardInks {
    fn of(content: &CardContent) -> Self {
        Self {
            into: content.arrived_light().inks(),
            from: content.leaving_light().map(CardLight::inks),
            wash: content.wash,
        }
    }

    /// The ink at `t` across the card, left to right.
    fn at(self, t: f32) -> CardInk {
        let (Some(from), Some(wash)) = (self.from, self.wash) else {
            return self.into;
        };
        from.mix(self.into, wash.amount(t))
    }

    /// The strongest bloom this card lays down anywhere across itself.
    ///
    /// Asked of the whole card rather than of one column because a wash's two
    /// sides can differ: a card arriving from `Unknown` has no bloom ahead of
    /// its front and a full one behind it, and skipping the lay on the strength
    /// of either alone would drop half the halo.
    fn peak_bloom(self) -> f32 {
        self.into
            .bloom
            .max(self.from.map_or(0.0, |from| from.bloom))
    }
}

/// Lay this card's bloom into `bloom`, keeping the brightest contribution.
///
/// Max rather than sum, which is what the prototype's `ImageChops.lighter`
/// does. It matters here more than it did there: a tree packs its cards about
/// four pixels apart while the measured bloom reaches nearly thirty, so every
/// card sits inside two neighbours' halos. Summed, ten cards wash the whole
/// panel to a flat cyan and the measured peak ratio of 0.19 stops meaning
/// anything; maxed, each card's halo is exactly the one it was measured to
/// have.
fn lay_bloom(bloom: &mut BloomField, card: &PlacedCard<'_>) {
    if let Some(splat) = plan_bloom(card, bloom.width, bloom.height) {
        lay_splat(bloom, &splat);
    }
}

/// One card's bloom, resolved down to numbers and nothing else.
///
/// Split out of [`lay_bloom`] so the CPU loop below and the GPU compute pass in
/// [`crate::gpu::bloom`] consume the *same* plan. The card's look is decided
/// here, once: neither backend re-derives a rect, a sigma, a reach or a column
/// ink, so the two cannot disagree about what a card looks like — only about
/// which processor drew it.
#[derive(Clone)]
struct BloomSplat {
    rect: RoundRect,
    near_sigma: f32,
    far_sigma: f32,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    /// The profile is a function of distance alone, so it is a curve rather than
    /// a calculation: sampled once per half pixel out to the reach and read back
    /// by index. Two exponentials per pixel over a card and the ground around it
    /// is most of what drawing a card costs otherwise.
    ///
    /// The card's own bloom strength is deliberately *not* baked into it: it
    /// varies across the card — the breath swings it and a state wash carries
    /// two different values either side of its front — so it is a per-column
    /// multiplier in [`Self::columns`], applied where the profile is read.
    profile: Vec<f32>,
    /// `(ink, strength)` per pixel column in `x0..x1`. The bloom's colour runs
    /// the stroke's own gradient, so like the stroke it depends on the column
    /// and nothing else.
    columns: Vec<(Rgb, f32)>,
}

/// Steps per pixel the bloom's falloff profile is sampled at.
const PROFILE_STEPS_PER_PX: f32 = 8.0;

/// Resolve `card`'s bloom against a `width x height` field, or `None` when it
/// lays down no light at all.
fn plan_bloom(card: &PlacedCard<'_>, width: u32, height: u32) -> Option<BloomSplat> {
    if !CARD_BLOOM {
        return None;
    }
    let inks = card.inks();
    if inks.peak_bloom() <= 0.0 {
        return None;
    }
    let rect = card.rect;
    let near_sigma = card.geometry.bloom_sigma;
    // The reach truncates the field, so it is measured in the field's own units.
    // Against the card's drawn height instead — which is what it used to be — the
    // cut lands at a different brightness on every tier. See [`BLOOM_REACH_SIGMAS`].
    let reach = near_sigma * BLOOM_REACH_SIGMAS;
    let far_sigma = near_sigma * measured::BLOOM_FAR_SIGMA_MUL;

    let x0 = (rect.x - reach).floor().max(0.0) as u32;
    let y0 = (rect.y - reach).floor().max(0.0) as u32;
    let x1 = ((rect.x + rect.w + reach).ceil() as u32).min(width);
    let y1 = ((rect.y + rect.h + reach).ceil() as u32).min(height);

    let profile: Vec<f32> = (0..=((reach * PROFILE_STEPS_PER_PX).ceil() as usize))
        .map(|step| {
            let d = step as f32 / PROFILE_STEPS_PER_PX;
            let near = (-(d * d) / (2.0 * near_sigma * near_sigma)).exp();
            let far = (-(d * d) / (2.0 * far_sigma * far_sigma)).exp();
            measured::BLOOM_PEAK
                * (measured::BLOOM_NEAR_WEIGHT * near + measured::BLOOM_FAR_WEIGHT * far)
        })
        .collect();
    let columns: Vec<(Rgb, f32)> = (x0..x1)
        .map(|x| {
            let t = card.column_t(x);
            let ink = inks.at(t);
            (ink.bloom_a.mix(ink.bloom_b, t), ink.bloom)
        })
        .collect();

    Some(BloomSplat {
        rect,
        near_sigma,
        far_sigma,
        x0,
        y0,
        x1,
        y1,
        profile,
        columns,
    })
}

/// Lay one planned splat into `bloom`, on this thread.
///
/// The scatter half of the two backends: walk the splat's own box and keep the
/// brightest contribution per pixel. [`crate::gpu::bloom`] gathers the same
/// numbers instead — every pixel reads every splat that can reach it — which is
/// the same sum in the same order and comes out bit for bit identical.
fn lay_splat(bloom: &mut BloomField, splat: &BloomSplat) {
    for y in splat.y0..splat.y1 {
        let py = y as f32 + 0.5;
        for (column, x) in (splat.x0..splat.x1).enumerate() {
            let d = splat.rect.distance(x as f32 + 0.5, py);
            if d <= 0.0 {
                continue;
            }
            let Some(amount) = splat.profile.get((d * PROFILE_STEPS_PER_PX) as usize) else {
                continue;
            };
            let (color, bloom_mul) = splat.columns[column];
            let amount = *amount * bloom_mul;
            if amount > BLOOM_PAINT_FLOOR {
                bloom.lighten(x, y, color, amount);
            }
        }
    }
}

/// What the CPU bloom costs per pixel it touches.
///
/// Measured by [`the_gpu_draws_what_the_cpu_draws::the_two_backends_cost`] on
/// this repository's dev box: ten real cards, 355k image pixels and about 266k
/// splat-box pixels on top, at 2.41 ms on one thread. That is a shade under 4 ns
/// for each pixel the splat loop walks plus each pixel the composite pass
/// touches, and it is the only number in the comparison that is *not* measured
/// on the machine actually running — the GPU side calibrates itself, because
/// that is the side that varies by two orders of magnitude between an
/// integrated adapter and a discrete card.
const CPU_BLOOM_NS_PER_PIXEL: f64 = 4.0;

/// How much faster the GPU has to be before it is used at all.
///
/// A tie is not worth taking: the GPU path has a device, a driver and a readback
/// in it, and the CPU path is the one every other machine is already running.
const GPU_MUST_BEAT_THE_CPU_BY: f64 = 1.25;

/// Whether handing this frame's blooms to the GPU actually beats laying them on
/// the threads that are about to draw the cards anyway.
///
/// Both sides are wall clock, which is why `threads` is here: the GPU pass is
/// serial and happens before [`Rasteriser::draw_shapes`] spawns anything, so
/// what it has to beat is not the CPU's total work but that work divided across
/// the pool. On a twelve-core box drawing ten cards that is a factor of six, and
/// ignoring it would send batches to the GPU that the CPU would have finished
/// first.
fn gpu_beats_the_threads(tiles: &[crate::gpu::bloom::Tile], threads: usize) -> bool {
    let Some(gpu_ms) = crate::gpu::bloom::estimated_ms(tiles) else {
        return false;
    };
    if crate::gpu::ignore_cost_model() {
        return true;
    }
    let touched: u64 = tiles.iter().map(crate::gpu::bloom::Tile::cpu_pixels).sum();
    let cpu_ms = touched as f64 * CPU_BLOOM_NS_PER_PIXEL / 1_000_000.0 / threads.max(1) as f64;
    cpu_ms > gpu_ms * GPU_MUST_BEAT_THE_CPU_BY
}

/// The falloff constants the GPU pass needs, so the shader carries no copy of
/// any of them.
fn bloom_curve() -> crate::gpu::bloom::Curve {
    crate::gpu::bloom::Curve {
        steps_per_px: PROFILE_STEPS_PER_PX,
        peak: measured::BLOOM_PEAK,
        near_weight: measured::BLOOM_NEAR_WEIGHT,
        far_weight: measured::BLOOM_FAR_WEIGHT,
        paint_floor: BLOOM_PAINT_FLOOR,
    }
}

impl BloomSplat {
    /// This splat as the compute pass's own wire form.
    ///
    /// The profile becomes a `max_step` and nothing else: the shader evaluates
    /// the curve from [`bloom_curve`] at the same quantised distance rather than
    /// being handed the table, which is the same number without a per-card
    /// upload. What it does need is where the table *ends*, because that
    /// truncation is [`BLOOM_REACH_SIGMAS`] and is part of how a card looks.
    fn for_gpu(&self) -> crate::gpu::bloom::Splat {
        crate::gpu::bloom::Splat {
            rect: [self.rect.x, self.rect.y, self.rect.w, self.rect.h],
            radius: self.rect.r,
            near_sigma: self.near_sigma,
            far_sigma: self.far_sigma,
            max_step: self.profile.len().saturating_sub(1) as u32,
            bounds: [self.x0, self.y0, self.x1, self.y1],
            columns: self
                .columns
                .iter()
                .map(|(ink, strength)| {
                    [
                        f32::from(ink.0),
                        f32::from(ink.1),
                        f32::from(ink.2),
                        *strength,
                    ]
                })
                .collect(),
        }
    }
}

/// The brightest bloom any card laid down on each pixel.
struct BloomField {
    width: u32,
    height: u32,
    /// `(colour, amount)` per pixel.
    cells: Vec<(Rgb, f32)>,
}

impl BloomField {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cells: vec![(Rgb(0, 0, 0), 0.0); (width as usize) * (height as usize)],
        }
    }

    fn lighten(&mut self, x: u32, y: u32, color: Rgb, amount: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let cell = &mut self.cells[(y as usize) * (self.width as usize) + (x as usize)];
        if amount > cell.1 {
            *cell = (color, amount);
        }
    }

    fn composite(&self, sheet: &mut Canvas) {
        for y in 0..self.height {
            for x in 0..self.width {
                let (color, amount) =
                    self.cells[(y as usize) * (self.width as usize) + (x as usize)];
                if amount > 0.0 {
                    sheet.blend(x, y, color, amount);
                }
            }
        }
    }
}

/// The summary mark's side, as a multiple of the control rail's type size.
///
/// Four fifths of an em. The mark stands in for a character the row would
/// otherwise have set, so an em is where it started; it is drawn a fifth under
/// that because at a full em it sat level with the count beside it and read as
/// a second piece of type rather than as a mark. The count itself is unchanged,
/// so the pair now has a size difference as well as a shape one.
const SUMMARY_MARK_MUL: f32 = 0.80;

/// The summary mark's corner radius, as a fraction of its own side.
///
/// Small, and deliberately not the card's own 0.13 h: `▤` is a *square*, and
/// the card's radius applied to a ten-pixel box rounds it into a dot.
const SUMMARY_MARK_RADIUS: f32 = 0.16;

/// Rules drawn inside the summary mark. `▤` is U+25A4, SQUARE WITH HORIZONTAL
/// FILL, and this is the fill: two rules, three bands, at the smallest size the
/// card can still resolve them at.
const SUMMARY_MARK_RULES: [f32; 2] = [1.0 / 3.0, 2.0 / 3.0];

/// Gap between the summary mark and its count, as a multiple of the rail's type
/// size.
const SUMMARY_COUNT_GAP_MUL: f32 = 0.26;

/// The chevron's box, as a multiple of the rail's type size.
///
/// Smaller than the mark, because `▸` is a small triangle in every face that
/// carries it at all — drawn at a full em it would read as a second badge
/// rather than as the disclosure control it is. Taken a further fifth down with
/// the summary mark, so the two controls keep the proportion they were drawn at
/// against each other.
const CHEVRON_MUL: f32 = 0.53;

/// How far the chevron's nose reaches across its own box.
///
/// Under 1.0 so the box is the same square whichever way the chevron points:
/// the reserved width must not change when a group is opened or closed, or the
/// title beside it would reflow on a click that changed nothing about it.
const CHEVRON_NOSE: f32 = 0.86;

/// Gap between the summary badge and the chevron, as a multiple of the rail's
/// type size.
///
/// The character row keeps a whole cell between them so neither can ever be
/// clicked for the other. The rail is drawn rather than laid out in cells, so
/// this is that separation expressed in the card's own units.
const CONTROL_GAP_MUL: f32 = 0.45;

/// Gap between the chip and the text column, as a multiple of the card's pad.
const CHIP_GAP_MUL: f32 = 0.5;

/// Where a card's ink may go, in pixels from the card's own left edge.
///
/// Split out of [`draw_card`] because it is the number the "titles never
/// truncate" promise is actually about, and until it was a function nothing
/// could assert against it: the fit ladder measured `wrap` against invented
/// widths while the renderer computed a different one from the plate, the chip
/// and the pad, and the two only met on screen. Every fit test now measures
/// this, so a change to any of the three has to face the real titles.
struct TextColumn {
    left: f32,
    /// The card's right pad. Nothing takes a share of it any more: the state
    /// chip that used to stand in the middle of every card is gone — see
    /// [`CardContent::state_label`] — so the text runs the card's whole width
    /// on every line but the first.
    right: f32,
    /// The type size the caption lines and the control rail are set at.
    caption_px: f32,
    /// What the control rail takes off the right margin, or `0.0` for a card
    /// carrying neither control. See [`ControlRail`].
    rail_width: f32,
    /// The air a reserved right margin keeps off the text.
    rail_gap: f32,
}

impl TextColumn {
    /// Where the text has to stop.
    fn text_right(&self) -> f32 {
        self.right
    }

    /// Where the *first* title line has to stop.
    ///
    /// The control rail sits in the card's top band, which is the band the
    /// first title line occupies and no other — so it is the one line that has
    /// to clear it, exactly as the character row reserves its badge and its
    /// chevron on its first content row and no other.
    fn first_line_right(&self) -> f32 {
        self.text_right()
            .min(self.right - reserved_margin(self.rail_width, self.rail_gap))
    }

    /// The width the second title line and the tidbit are set in.
    fn available(&self) -> f32 {
        (self.text_right() - self.left).max(0.0)
    }

    /// The width the first title line is set in.
    fn first_line_available(&self) -> f32 {
        (self.first_line_right() - self.left).max(0.0)
    }

    /// Both, in the order [`wrap_ragged`] wants them.
    fn title_widths(&self) -> (f32, f32) {
        (self.first_line_available(), self.available())
    }
}

/// Workers under this card's row that have reported back.
///
/// The pixel twin of the character row's `▤N` badge — see
/// [`super::worker_summary_badge_rect`], which is the cell it is clickable at
/// whether or not anything was drawn there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) struct SummaryBadge {
    /// How many reported. Printed through [`super::worker_summary_count_label`],
    /// so a card and a bare row cannot disagree about what ten of them says.
    count: usize,
    /// At least one of those summaries has not been looked at. The character
    /// row says this in the palette's accent against `overlay0`; the card says
    /// it the way it says every other "still worth a look", by holding the ink
    /// at full instead of dropping it to caption weight.
    fresh: bool,
}

/// Which way a worktree group's chevron points, and therefore whether the rows
/// under this card are hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum GroupChevron {
    /// `▸` — the group's children are folded away under this row.
    Collapsed,
    /// `▾` — they are shown beneath it.
    Expanded,
}

/// The workspace metadata token carrying a Space's own revision count.
///
/// A plain non-negative integer, published by a fleet-side script exactly the
/// way [`crate::quality_streak::STREAK_TOKEN`] and
/// [`crate::quality_streak::DEFECT_TOKEN`] already are — Herdr computes
/// nothing here and has no notion of what a "revision" is; it reads whatever
/// number a publisher sends and draws it. No publisher sends one yet, which is
/// deliberate rather than an oversight: a real per-Space commit/PR count is
/// not a quantity Herdr already has anywhere (the sky's own `revs` is orbital
/// animation cycles keyed to a body's file-count mass, not git history — see
/// [`super::body_register::BodyFacts::orbit_line`] — and would have been a
/// dishonest stand-in), so the badge shows nothing rather than a made-up
/// count until something publishes this token.
pub(super) const REV_TOKEN: &str = "rev";

/// The Space-only badge pill in a card's header — `.badge`/`.badge.warn` in
/// the flight-deck circuit mockup.
///
/// Both readings are fleet-published facts, never derived by Herdr: an open
/// defect always wins the one badge slot over a rev count, matching the
/// mockup's own fixtures (a Space with both shows the bug in the badge and
/// moves its rev count down into the orbit-register caption line instead —
/// not reproduced here; this always drops the rev count while a defect is
/// open, which is the smaller, honestly-scoped half of that behaviour).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum SpaceBadgeMark {
    /// A green "`N` rev" pill: [`REV_TOKEN`], and no open defect.
    Healthy(u32),
    /// An amber "bug" pill: this Space owns an open defect
    /// ([`crate::quality_streak::DEFECT_TOKEN`]). Carries no count — the
    /// fleet's own defect ladder is a severity, `S1..S4`, never a tally of how
    /// many are open, and the mockup's own "1 bug" is not a quantity Herdr has
    /// to draw honestly.
    Warn,
}

impl SpaceBadgeMark {
    fn label(self) -> String {
        match self {
            Self::Healthy(rev) => format!("{rev} rev"),
            Self::Warn => "bug".to_string(),
        }
    }

    fn ink(self, theme: CardTheme) -> Rgb {
        match self {
            Self::Healthy(_) => theme.badge_ok(),
            Self::Warn => theme.badge_warn(),
        }
    }
}

/// Resolve the badge from a Space's own published tokens and its resolved
/// stage.
///
/// [`crate::quality_streak::defect_mark`] already carries the right rule for
/// "is a defect open" — the fleet's own `-` silences even a row detection
/// reads as failed, and an unrated failure still marks at full intensity — so
/// this reuses it rather than re-deriving a second, narrower version of the
/// same question.
fn space_badge(
    rev: Option<&str>,
    defect: Option<&str>,
    stage: LifecycleStage,
) -> Option<SpaceBadgeMark> {
    if crate::quality_streak::defect_mark(defect, stage).is_some() {
        return Some(SpaceBadgeMark::Warn);
    }
    let rev: u32 = rev?.trim().parse().ok()?;
    Some(SpaceBadgeMark::Healthy(rev))
}

/// The two controls a card hangs on its right margin, above its state chip.
///
/// # Why the card has to draw these at all
///
/// Both are drawn by the character row when nothing covers it, at the cells
/// [`super::worker_summary_badge_rect`] and
/// [`super::workspace_group_chevron_rect`] name. Those cells stay *clickable*
/// under a pixel card — the hit tests are cell geometry and know nothing about
/// pixels, which is the whole integration this module is built on — so a card
/// that did not draw them left two live controls with nothing on screen to say
/// they were there. That was raised on the first cards pass and again on the
/// shapes pass, and deferred both times on the grounds that the settled design
/// did not specify them. It specifies them now.
///
/// # Why they are laid out and not overlaid
///
/// Because the title's column has to know about them. Painting the rail over a
/// finished card would put it on top of a title that had already been set
/// through to the card's own right pad — the exact overlap being fixed here,
/// moved one layer inward. So the rail is measured in [`text_column`] beside
/// the chip, and the title stops clear of whichever of the two is wider.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub(crate) struct ControlRail {
    summary: Option<SummaryBadge>,
    group: Option<GroupChevron>,
    space_badge: Option<SpaceBadgeMark>,
}

impl ControlRail {
    fn is_empty(&self) -> bool {
        self.summary.is_none() && self.group.is_none() && self.space_badge.is_none()
    }

    /// Where every part of the rail goes, in pixels from the rail's own top
    /// left corner.
    ///
    /// One function rather than a width here and a placement there: the
    /// reservation the title is set against and the pixels the marks land on
    /// have to be the same arithmetic, for the same reason [`TextColumn`]
    /// exists at all.
    fn layout(&self, font: &CardFont, px: f32) -> RailLayout {
        let mark_side = px * SUMMARY_MARK_MUL;
        let chevron_side = px * CHEVRON_MUL;
        let mut x = 0.0;
        // Leftmost, closest to the title: the mockup's own badge is the only
        // thing in this row's right margin, and this fleet's other two
        // controls are rarer (a worktree-group head, a mate with finished
        // workers) than a Space simply carrying a quality signal.
        let badge_px = px * measured::BADGE_SIZE_MUL;
        let badge_height =
            font.metrics(badge_px).line_height * (1.0 + 2.0 * measured::BADGE_VPAD_MUL);
        let badge = self.space_badge.map(|mark| {
            let label = mark.label();
            let hpad = font.metrics(badge_px).line_height * measured::BADGE_PAD_MUL;
            let text_width = font.width(&label, badge_px);
            let width = text_width + hpad * 2.0;
            x = width;
            RailBadge {
                width,
                height: badge_height,
                hpad,
                label,
                mark,
            }
        });
        let summary = self.summary.map(|badge| {
            if self.space_badge.is_some() {
                x += px * CONTROL_GAP_MUL;
            }
            let start = x;
            let count = super::worker_summary_count_label(badge.count);
            let count_x = start + mark_side + px * SUMMARY_COUNT_GAP_MUL;
            x = count_x + font.width(&count, px);
            RailSummary {
                x: start,
                mark_side,
                count_x,
                count,
                fresh: badge.fresh,
            }
        });
        let chevron = self.group.map(|group| {
            if summary.is_some() || self.space_badge.is_some() {
                x += px * CONTROL_GAP_MUL;
            }
            let at = x;
            x += chevron_side;
            RailChevron {
                x: at,
                side: chevron_side,
                group,
            }
        });
        RailLayout {
            width: x,
            height: font
                .metrics(px)
                .line_height
                .max(mark_side)
                .max(chevron_side)
                .max(badge_height),
            badge,
            summary,
            chevron,
        }
    }
}

/// The resolved pixel placement of one card's control rail.
struct RailLayout {
    /// `0.0` for a rail with nothing on it, which is what a card with no
    /// finished workers and no worktree children has.
    width: f32,
    height: f32,
    badge: Option<RailBadge>,
    summary: Option<RailSummary>,
    chevron: Option<RailChevron>,
}

struct RailBadge {
    width: f32,
    height: f32,
    hpad: f32,
    label: String,
    mark: SpaceBadgeMark,
}

struct RailSummary {
    x: f32,
    mark_side: f32,
    count_x: f32,
    count: String,
    fresh: bool,
}

struct RailChevron {
    x: f32,
    side: f32,
    group: GroupChevron,
}

/// Whether `text` sets whole — every word, no line overrunning — in `avail`,
/// which is `(the first line, every line after it)`.
///
/// Exactly as published: this asks nothing of [`summary`], because it is the
/// predicate [`fit_title`] uses to test one rung of the ladder at a time.
fn sets_whole(font: &CardFont, text: &str, avail: (f32, f32)) -> bool {
    if avail.0 <= 1.0 || avail.1 <= 1.0 {
        return false;
    }
    let lines = wrap_ragged(font, text, TITLE_PX, avail, TITLE_LINES);
    let words = lines
        .iter()
        .map(|line| line.split_whitespace().count())
        .sum::<usize>();
    words == text.split_whitespace().count()
        && lines.iter().enumerate().all(|(index, line)| {
            let width = if index == 0 { avail.0 } else { avail.1 };
            font.width(line, TITLE_PX) <= width + 0.5
        })
}

/// The lines a card sets its summary in, and what that cost.
struct FittedTitle {
    lines: Vec<String>,
    /// Which rung of [`summary::candidates`] set whole, or `None` when none
    /// did and the lines below are a greedy wrap of the lossless rung.
    ///
    /// Ordered rather than merely tested: rung 0 is the publisher's own words
    /// untouched and every rung after it has given something up, so "which
    /// rung" is how much of the summary this width cost.
    #[allow(dead_code)] // read by `title_sets_whole`, which is test-only
    rung: Option<usize>,
}

/// Set `title` in `avail`, condensing only as far as it has to.
///
/// Walks [`summary::candidates`] and stops at the first rendering that sets
/// whole, so a title that already fits is drawn exactly as the fleet published
/// it and costs one wrap. When nothing on the ladder fits, the lines are a
/// greedy wrap of [`summary::lossless_rung`] — the cheapest rendering that gave
/// up no content — which is the old behaviour applied to a better string rather
/// than to the raw one.
///
/// This is the whole of the captain's *"herdr will just need to be better about
/// what it chooses to display"*: the card no longer answers "too long" by
/// silently dropping whatever fell off the end of a greedy wrap. It answers it
/// by picking a shorter *rendering of the same summary* that still reads as a
/// finished phrase, and only falls back to the drop when even the shortest one
/// will not fit.
fn fit_title(font: &CardFont, title: &str, avail: (f32, f32)) -> FittedTitle {
    let ladder = summary::candidates(title);
    if avail.0 > 1.0 && avail.1 > 1.0 {
        for (rung, candidate) in ladder.rungs().enumerate() {
            if sets_whole(font, candidate, avail) {
                return FittedTitle {
                    lines: wrap_ragged(font, candidate, TITLE_PX, avail, TITLE_LINES),
                    rung: Some(rung),
                };
            }
        }
    }
    FittedTitle {
        lines: wrap_ragged(font, ladder.lossless(), TITLE_PX, avail, TITLE_LINES),
        rung: None,
    }
}

/// Whether `title` reaches the card entire, at any rung of the ladder.
#[cfg(test)]
fn title_sets_whole(font: &CardFont, title: &str, avail: (f32, f32)) -> bool {
    fit_title(font, title, avail).rung.is_some()
}

/// Where a card's text runs, and what the right margin still owes the rail.
///
/// # The chip is gone
///
/// A state chip used to stand in the middle of every card's right margin, and
/// the state label was repeated as an uppercase capsule under it. Both are
/// retired: the reference the card is drawn against carries state as **a bare
/// dim lowercase word on its own line** and nothing else — no chip, no pill, no
/// capsule, no uppercase — so the word is now one of the card's caption lines
/// and the margin it used to cost is the title's.
///
/// The control rail stays, and that is the one thing still reserved here. The
/// chip was a *restatement* — the card's own colour already carries its state —
/// so retiring it costs the reader nothing. A chevron is the only thing on
/// screen saying rows are hidden under this one, and a summary badge the only
/// thing saying workers finished; drop either and the card is wrong rather than
/// terse. The rail is also narrow — a badge and a chevron together ran about
/// half a state chip — so it costs the title very little.
fn text_column(
    font: &CardFont,
    geometry: &CardGeometry,
    width: f32,
    _height: f32,
    title: &str,
    rail: ControlRail,
) -> TextColumn {
    let _ = title;
    let caption_px = (TITLE_PX * measured::TIDBIT_SIZE_MUL).max(9.0);
    let left = geometry.text_inset();
    let right = width - geometry.pad_right;
    let rail_gap = geometry.pad * CHIP_GAP_MUL;
    // The rail is set at the caption's own type size, so the card carries two
    // sizes of type and not three.
    let rail_width = rail.layout(font, caption_px).width;

    TextColumn {
        left,
        right,
        caption_px,
        rail_width,
        rail_gap,
    }
}

/// What a margin `width` wide really costs the text: nothing at all when there
/// is nothing in it, and its own gap once there is.
fn reserved_margin(width: f32, gap: f32) -> f32 {
    if width <= 0.0 {
        return 0.0;
    }
    width + gap
}

/// Draw one card's body, plate, chip and text over whatever is already there.
/// Depth of the newest residue ring, as a fraction of the card's own height.
///
/// Measured in from the card's boundary rather than out from it, which is the
/// one real departure from the approved concept's geometry and is forced: the
/// concept draws a 60px box with 90px of clear space around it, and a sidebar
/// row has none — the row above is a card and so is the row below. Rings drawn
/// outward would be drawn on the neighbours. Inward, the same concentric stack
/// reads as contour lines in the card's own surface, which is if anything a
/// truer picture of residue than a halo: it is *in* the mate, not around it.
const RESIDUE_INSET: f32 = 0.075;
/// Gap between one ring and the next, as a fraction of the card's height.
///
/// [`crate::app::residue::MAX_RINGS`] of these plus [`RESIDUE_INSET`] must stay
/// clear of the card's own centre line, or the deepest rings would collapse
/// into each other and the last absorptions would stop being countable — see
/// `a_full_ring_stack_fits_inside_the_card`, which is what holds these two
/// numbers to that.
const RESIDUE_STEP: f32 = 0.042;
/// Half-width of a ring's line, as a fraction of the card's height.
///
/// Thinner than the card's own stroke (`measured::STROKE_W`, 0.033 of the
/// nominal height, so 0.0165 either side of the boundary). A ring must not be
/// mistakeable for the card's edge repeated: it is quieter than the edge in
/// both weight and alpha.
const RESIDUE_HALF_W: f32 = 0.007;

/// The residue stack of one card, resolved once and then sampled per pixel.
///
/// Sampled from the *same* signed distance the fill, the inner glow and the
/// stroke are already reading, so the rings cost the card no second pass over
/// its pixels — which matters because this is inside the pane-scaled render
/// path. The per-pixel work is one divide, one round and one bounds check,
/// independent of how many rings are up.
#[derive(Debug, Clone, Copy)]
struct RingStack {
    count: u8,
    inset: f32,
    step: f32,
    half_w: f32,
    alphas: [f32; crate::app::residue::MAX_RINGS],
}

impl RingStack {
    fn new(residue: u8, height: f32) -> Option<Self> {
        let count = residue.min(crate::app::residue::MAX_RINGS as u8);
        if count == 0 {
            return None;
        }
        let mut alphas = [0.0; crate::app::residue::MAX_RINGS];
        for ring in crate::app::residue::stack(count) {
            alphas[usize::from(ring.age)] = ring.alpha;
        }
        Some(Self {
            count,
            inset: RESIDUE_INSET * height,
            step: (RESIDUE_STEP * height).max(1.0),
            half_w: (RESIDUE_HALF_W * height).max(0.35),
            alphas,
        })
    }

    /// What this pixel owes the rings, given its signed distance to the card's
    /// boundary. `None` everywhere outside the stack, which is most of a card.
    fn at(&self, distance: f32) -> Option<f32> {
        // Inside is negative; a ring lives at a positive depth in from the edge.
        let depth = -distance;
        if depth < self.inset - self.half_w {
            return None;
        }
        let age = ((depth - self.inset) / self.step).round();
        if age < 0.0 || age >= f32::from(self.count) {
            return None;
        }
        let age = age as usize;
        let cover = coverage((depth - (self.inset + age as f32 * self.step)).abs() - self.half_w);
        (cover > 0.0).then(|| cover * self.alphas[age])
    }
}

/// How many filaments a discharging card carries.
///
/// Few, and vertical. The reference's working panes carry *filaments* rather
/// than a wash — a small number of distinct lines is what reads as a discharge
/// inside glass, and a fill at the same total ink reads as the card simply
/// being brighter, which is the state channel and already taken.
const DISCHARGE_FILAMENTS: usize = 7;

/// The most ink one filament carries at a fully-loaded worker.
///
/// Deliberately under half the face's own alpha. H10's constraint is not a
/// suggestion: the discharge must not move the pane's translucency, so the
/// worst case — every filament at full over the face at full — still has to
/// leave the scene behind measurably visible.
const DISCHARGE_PEAK_ALPHA: f32 = 0.040;

/// The filaments a working pane carries behind its face.
///
/// Drawn **before** the face and the edge, which is the whole of H10's rule:
/// behind the glass, so no amount of discharge can make the pane opaque. Their
/// positions are a deterministic function of the card's own rect, so a card
/// carries the same filaments frame to frame and two cards do not share a
/// pattern — there is no clock and no random number generator here, and the
/// picture is a pure function of the card exactly as everything else drawn in
/// this module is.
fn draw_discharge(sheet: &mut Canvas, card: &PlacedCard<'_>, ink: Rgb, opacity: f32) {
    let amount = card.content.discharge.clamp(0.0, 1.0);
    if amount <= 0.0 {
        return;
    }
    let rect = card.rect;
    // Inside the edge, never under it: a filament crossing the boundary would
    // read as the edge fraying rather than as light inside the pane.
    let inset = card.geometry.stroke * 2.0;
    let top = rect.y + inset;
    let bottom = rect.y + rect.h - inset;
    if bottom <= top || rect.w <= inset * 2.0 {
        return;
    }
    let seed = (rect.x * 7.0) as i32 as u32 ^ ((rect.y * 13.0) as i32 as u32).rotate_left(11);
    for index in 0..DISCHARGE_FILAMENTS {
        // One multiply-shift per filament rather than a hash: this is inside
        // the per-card render loop and the only property the placement needs is
        // that it is stable and spread.
        let noise = seed
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add((index as u32).wrapping_mul(0x85EB_CA6B));
        let across = ((noise >> 8) & 0xFFFF) as f32 / 65_535.0;
        // Each filament carries its own share of the amplitude, so a card at
        // half load is half its filaments lit rather than all of them at half —
        // which is what makes the traffic legible as a *count* of lines.
        let lit = (amount * DISCHARGE_FILAMENTS as f32) - index as f32;
        let alpha = DISCHARGE_PEAK_ALPHA * lit.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            continue;
        }
        let x = rect.x + inset + across * (rect.w - inset * 2.0);
        let px = x.floor().max(0.0) as u32;
        if px >= sheet.width() {
            continue;
        }
        for y in top.floor().max(0.0) as u32..(bottom.ceil() as u32).min(sheet.height()) {
            // Brightest in the middle of its run and gone at both ends, so a
            // filament is a discharge arcing inside the pane rather than a rule
            // drawn on it.
            let t = ((y as f32 + 0.5) - top) / (bottom - top);
            let fade = (t * std::f32::consts::PI).sin().max(0.0);
            sheet.blend(px, y, ink, alpha * fade * opacity);
        }
    }
}

/// The worker status dot's diameter, as a fraction of [`TITLE_PX`] — the
/// mockup's `.wk-dot` (5px) against its own worker `.card-name` type size
/// (0.62rem, 9.92px at the mockup's 16px root): a ratio of about one half.
const DOT_DIAMETER_MUL: f32 = 0.5;

/// The air between the dot and the name that follows it, the same way: the
/// mockup's `.wk-dot`'s own `margin-right` (0.35rem, 5.6px) against that same
/// 9.92px type size.
const DOT_GAP_MUL: f32 = 0.55;

/// How far past its own edge the dot's glow reaches, as a multiple of its
/// radius — the mockup's `box-shadow: 0 0 5px`, a blur about as wide as the
/// dot itself.
const DOT_GLOW_MUL: f32 = 2.0;

/// The glow's own peak alpha, right at the dot's edge — the mockup's
/// `rgba(90,209,255,0.6)`.
const DOT_GLOW_ALPHA: f32 = 0.6;

/// A small solid circle with a soft falloff outside it — the worker status
/// dot, `.wk-dot` in the flight-deck mockup.
///
/// Always drawn at the theme's own full-strength accent — unthemed,
/// [`measured::STROKE_A`], the tree's own cyan — and never the card's own
/// (possibly dimmed) stroke ink: the mockup's dot is cyan on every worker row
/// it draws, discharging hard or sitting idle alike, so this is one more place
/// — with the badge and the discharge filaments — a worker's state reaches the
/// card outside the border.
fn draw_worker_dot(sheet: &mut Canvas, center: (f32, f32), radius: f32, opacity: f32, ink: Rgb) {
    let glow_radius = radius * DOT_GLOW_MUL;
    let glow_sigma = (glow_radius - radius).max(0.5);
    let x0 = (center.0 - glow_radius).floor().max(0.0) as u32;
    let y0 = (center.1 - glow_radius).floor().max(0.0) as u32;
    let x1 = ((center.0 + glow_radius).ceil() as u32).min(sheet.width());
    let y1 = ((center.1 + glow_radius).ceil() as u32).min(sheet.height());
    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for x in x0..x1 {
            let px = x as f32 + 0.5;
            let d = ((px - center.0).powi(2) + (py - center.1).powi(2)).sqrt() - radius;
            let fill = coverage(d);
            if fill > 0.0 {
                sheet.blend(x, y, ink, fill * opacity);
                continue;
            }
            // Past the solid disc, a soft falloff carries the glow — never
            // layered on top of the disc itself, which is already at full
            // alpha and has nothing to gain from it.
            let glow = (-(d * d) / (2.0 * glow_sigma * glow_sigma)).exp() * DOT_GLOW_ALPHA;
            if glow > 0.001 {
                sheet.blend(x, y, ink, glow * opacity);
            }
        }
    }
}

fn draw_card(sheet: &mut Canvas, card: &PlacedCard<'_>, font: &CardFont) {
    // The card's chrome and its type are drawn at the settled light; only the
    // body — stroke, fill and inner glow — sweeps with the wash and swings with
    // the breath. See [`CardContent::settled_light`].
    let (stroke_a, lum) = card.settled_inks();
    let content = card.content;
    let geometry = &card.geometry;
    let rect = card.rect;
    let (ox, oy, width, height) = (rect.x, rect.y, rect.w, rect.h);
    let half_stroke = geometry.stroke / 2.0;

    // **The card-bloom opacity.** The card-bloom beat of the arrival: the
    // whole card fades in at its own final position and size, never a clip or
    // a translation — the rail and the branch are what carry the sense of
    // something growing toward it. A settled card's opacity is `1.0`, which is
    // every card on a settled panel and therefore the branch that costs
    // nothing.
    let opacity = content.generate.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }

    // **The card's own band, which is no longer the whole box.** A Space that is
    // running workers draws them inside this same border, so everything the card
    // centres on itself — the icon plate, the control rail, the title and its
    // captions — is centred in the band *above* that list rather than over the
    // whole height. Taken as a subtraction off the drawn height rather than
    // recomputed from the font, because that height is exactly
    // `card_height_px + crew` by construction (see `Rasteriser::place`) and a
    // second derivation could disagree with it by a pixel every frame the list
    // is opening.
    let crew_px = crew::drawn_extent_px(card.crew, &content.crew);
    let head = (height - crew_px).max(0.0);

    // One pass over the card, reading the same distance for the face, the inner
    // glow and the edge.
    let x0 = ox.floor().max(0.0) as u32;
    let y0 = oy.floor().max(0.0) as u32;
    let x1 = ((ox + width).ceil() as u32).min(sheet.width());
    let y1 = ((oy + height).ceil() as u32).min(sheet.height());
    if x1 <= x0 {
        return;
    }
    // Against the card's own band, never the box it now encloses. Every ratio
    // in the measured table is a fraction of a card's height, and a Space
    // carrying six workers is not a card six times as tall — it is a card with a
    // list under it. Measuring the glow against the whole box would light a mate
    // running a crew like a lamp and leave the same mate running none as it
    // always was. The same rule [`CardGeometry::new`] already states for the
    // padding, the stroke, the radius and the plate.
    let inner_sigma = (measured::FILL_INNER_SIGMA * head).max(1.0);
    // Past this far inside the edge the inner glow is below the alpha the
    // canvas can represent, so the exponential is not worth evaluating — which
    // matters because that is every pixel in the middle of the card.
    let inner_reach = inner_sigma * 3.0;

    // The fill's hue travel and the stroke's gradient are both normalised to
    // the card's own width, so both depend on the column and nothing else.
    // Resolved once per column rather than once per pixel: a card is tens of
    // rows tall, and this is the difference between four hundred mixes and
    // twenty-four thousand.
    //
    // The stroke's own ends are resolved per column too now rather than once
    // for the card, which is what carries the wash: with none they are the same
    // pair every column and this is the gradient it always was, and with one
    // the column ahead of the front is still lit by the state the card left.
    // The residue this card is carrying, resolved once. `None` on a card that
    // has absorbed nothing, which is the branch every card took before this
    // existed and is still the branch most cards take.
    // Also the card's own band: a residue stack is a ladder up the card's face,
    // and stretching it over the crew would put a mate's absorbed-worker rings
    // through its own worker list. See `inner_sigma`.
    let rings = RingStack::new(content.residue, head);
    let inks = card.inks();
    let columns: Vec<Rgb> = (x0..x1)
        .map(|x| {
            let t = card.column_t(x);
            let ink = inks.at(t);
            ink.stroke_a.mix(ink.stroke_b, t)
        })
        .collect();

    // ---- the back face ---------------------------------------------------
    //
    // The card's thickness. A second copy of the same boundary, offset down and
    // right by [`measured::GLASS_THICKNESS_PX`] and drawn at a fraction of the
    // front's alpha, so the pane reads as an object with a front rather than as
    // a rectangle painted on the panel. Drawn first, so the front face and its
    // edge stand over it.
    let back = RoundRect {
        x: rect.x + measured::GLASS_THICKNESS_PX,
        y: rect.y + measured::GLASS_THICKNESS_PX,
        w: rect.w,
        h: rect.h,
        r: rect.r,
    };
    let back_x1 = ((ox + width + measured::GLASS_THICKNESS_PX).ceil() as u32).min(sheet.width());
    let back_y1 = ((oy + height + measured::GLASS_THICKNESS_PX).ceil() as u32).min(sheet.height());
    for y in y0..back_y1 {
        let py = y as f32 + 0.5;
        for x in x0..back_x1 {
            let px = x as f32 + 0.5;
            // Only where the back is *not* under the front: a second face
            // showing through the front would double the face's alpha and the
            // pane would stop being see-through in the middle.
            if coverage(rect.distance(px, py)) > 0.0 {
                continue;
            }
            let d = back.distance(px, py);
            // The back face reaches a few pixels past the front face's own
            // last column, so it borrows that column's own edge colour rather
            // than running off the end of the gradient.
            let column = (x.saturating_sub(x0) as usize).min(columns.len().saturating_sub(1));
            let gradient = columns[column];
            let inside = coverage(d);
            if inside > 0.0 {
                sheet.blend(
                    x,
                    y,
                    content.theme.face(),
                    measured::GLASS_BACK_ALPHA * inside * opacity,
                );
            }
            let edge = coverage(d.abs() - half_stroke);
            if edge > 0.0 {
                sheet.blend(
                    x,
                    y,
                    gradient,
                    measured::GLASS_BACK_EDGE_ALPHA * edge * opacity,
                );
            }
        }
    }

    // ---- the discharge ---------------------------------------------------
    //
    // Behind the face and behind the edge, so no amount of it can make the pane
    // read as opaque. See [`draw_discharge`].
    draw_discharge(sheet, card, stroke_a, opacity);

    // ---- the front face --------------------------------------------------
    //
    // One pass over the card, reading the same distance for the face, the inner
    // glow and the edge.
    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for (column, x) in (x0..x1).enumerate() {
            let px = x as f32 + 0.5;
            let d = rect.distance(px, py);
            let gradient = columns[column];

            let body = coverage(d);
            if body > 0.0 {
                // **The face is glass, not a plate.** A tenth of an alpha of a
                // cool tint, so whatever the card is standing on — the panel,
                // and on a terminal drawing the whole-screen scene the sky
                // itself — is measurably visible through it. This is the one
                // number that decides whether the tree hangs *in front of* the
                // system or covers it.
                sheet.blend(
                    x,
                    y,
                    content.theme.face(),
                    measured::GLASS_FACE_ALPHA * body * opacity,
                );
                // The face is not a vertical ramp: it is a symmetric inner glow
                // from both edges in the local edge hue, and it is what gives
                // the pane the internal light a flat wash of alpha does not.
                if d > -inner_reach {
                    let inner = (-(d * d) / (2.0 * inner_sigma * inner_sigma)).exp()
                        * measured::FILL_EDGE_ALPHA;
                    if inner > 0.001 {
                        sheet.blend(x, y, gradient, inner * body * opacity);
                    }
                }
            }

            // Residue, in the card's own edge colour at this column: contour
            // lines of the same material the card's boundary is drawn in, not
            // a new hue. Under the stroke and under every mark and every glyph
            // that follows, because it is the quietest thing on the card and
            // must never take a pixel off the state it is actually in.
            if let Some(rings) = rings {
                if let Some(alpha) = rings.at(d) {
                    sheet.blend(x, y, gradient, alpha * body * opacity);
                }
            }

            // The edge, at full alpha and measurably brighter than the face it
            // encloses — which with the face at a tenth of an alpha it now is by
            // construction rather than by tuning.
            let stroke = coverage(d.abs() - half_stroke);
            if stroke > 0.0 {
                sheet.blend(x, y, gradient, stroke * opacity);
            }
        }
    }

    // ---- the icon slot ---------------------------------------------------
    // Drawn only when there is a mark to put in it — `CardGeometry::new` has
    // already collapsed the slot to nothing when there is not, so this is the
    // same code path either way and the plate simply has no size.
    let plate = geometry.plate.min(head - geometry.pad * 2.0).max(0.0);
    let plate_x = ox + geometry.pad;
    let plate_y = oy + (head - plate) / 2.0;
    if plate > 2.0 {
        let plate_rect = RoundRect {
            x: plate_x,
            y: plate_y,
            w: plate,
            h: plate,
            r: geometry.plate_radius,
        };
        let top = measured::FILL_MID.mix(stroke_a, 0.30);
        let bottom = measured::FILL_MID.mix(stroke_a, 0.08);
        let edge = measured::FILL_MID.mix(stroke_a, 0.55);
        let hairline = (geometry.stroke * 0.7).max(0.8) / 2.0;
        for y in plate_y.floor().max(0.0) as u32..((plate_y + plate).ceil() as u32) {
            let py = y as f32 + 0.5;
            let v = ((py - plate_y) / plate).clamp(0.0, 1.0);
            for x in plate_x.floor().max(0.0) as u32..((plate_x + plate).ceil() as u32) {
                let px = x as f32 + 0.5;
                let d = plate_rect.distance(px, py);
                let inside = coverage(d);
                if inside > 0.0 {
                    sheet.blend(x, y, top.mix(bottom, v), inside * opacity);
                }
                let line = coverage(d.abs() - hairline);
                if line > 0.0 {
                    sheet.blend(x, y, edge, line * opacity);
                }
            }
        }
    }

    // ---- the text column -------------------------------------------------
    let column = text_column(
        font,
        geometry,
        width,
        head,
        &content.title,
        content.controls,
    );
    let text_right = ox + column.text_right();
    let caption_px = column.caption_px;
    let text_left = ox + column.left;

    // ---- the control rail ------------------------------------------------
    // Right-aligned to the card's own right margin, in its top band. The
    // character row puts these two on its *first* content row, so this is that
    // row's own vertical order kept: controls at the top, over the title's
    // first line and nothing else.
    if !content.controls.is_empty() {
        let rail = content.controls.layout(font, caption_px);
        // Anchored under the card's top pad. It only gives way on a card too
        // short to hold it there, and then only as far as its own stroke.
        let ceiling = oy + head - geometry.pad - rail.height;
        let rail_y = (oy + geometry.pad).min(ceiling).max(oy + geometry.stroke);
        draw_control_rail(
            sheet,
            font,
            &rail,
            chip_ink(content),
            caption_px,
            geometry,
            (ox + column.right - rail.width, rail_y),
            opacity,
            content.theme,
        );
    }

    // ---- the crew --------------------------------------------------------
    //
    // The workers this Space is running, under a dashed rule, inside this same
    // border. Drawn here — before the title's own fit gate below — because the
    // list is not part of the title block and a card too narrow to set its
    // title in is not a reason to drop the rows saying what is running.
    //
    // Its inks are the card's, not its own: the caption ink for the status
    // lines and the settled stroke for the rule, the rails and the dots. A list
    // that picked its own colours would be the one thing on the card that did
    // not change when the card's state did.
    if !content.crew.is_empty() {
        let metrics = crew::CrewMetrics::of(font, TITLE_PX);
        crew::draw(
            sheet,
            font,
            &content.crew,
            (&metrics, card.crew),
            (ox + column.left, ox + column.text_right()),
            oy + head,
            (content.theme.ink(), stroke_a, content.theme.accent()),
            // The Space's own ground and stage hue: a worker's marker is
            // resolved against the card it is standing on, which is the one
            // rule [`spider::draw_at`] has.
            spider::Palette {
                ground: content.ground,
                hue: content.hues.of(content.stage),
            },
            opacity,
        );
    }

    // ---- title and captions ----------------------------------------------
    //
    // Three caption lines under the title, in the register the reference sets
    // them in: what this body *is*, what it has *done*, and — as a bare dim
    // lowercase word and nothing else — what state it is in. A card carrying
    // fewer simply leaves the slot empty; the block is reserved on every card so
    // a row's height does not move when the fleet republishes.
    //
    // The same numbers the fit tests measure, not a second pair derived here —
    // less the worker dot's own reserve on the first line, which `text_column`
    // does not know about: a worker's dot sits inline with its name and
    // nowhere else on the card, so it is taken off the *title's* first-line
    // width rather than off every line `text_column` lays out, the same way
    // the rail is taken off the first line and no other.
    let dot_diameter = TITLE_PX * DOT_DIAMETER_MUL;
    let dot_reserve = if content.is_worker() {
        dot_diameter + TITLE_PX * DOT_GAP_MUL
    } else {
        0.0
    };
    let widths = column.title_widths();
    let widths = ((widths.0 - dot_reserve).max(0.0), widths.1);
    if widths.0 <= 1.0 || widths.1 <= 1.0 {
        return;
    }
    let title_metrics = font.metrics(TITLE_PX);
    let caption_metrics = font.metrics(caption_px);
    let lines = fit_title(font, &content.title, widths).lines;
    let leading = title_metrics.line_height * TITLE_LEADING;

    let title_block = leading * (lines.len().max(1) as f32 - 1.0) + title_metrics.line_height;
    // The state word is always drawn, so the caption run is never empty and the
    // block below is never the title alone.
    let captions: [Option<(&str, CaptionTone)>; CAPTION_LINES] = [
        content
            .tidbit
            .as_deref()
            .map(|text| (text, CaptionTone::Register)),
        content
            .register
            .as_ref()
            .map(|caption| (caption.text.as_str(), caption.tone)),
    ];
    let drawn_captions = captions
        .iter()
        .rposition(Option::is_some)
        .map_or(0, |last| last + 1);
    let caption_block = if drawn_captions == 0 {
        0.0
    } else {
        caption_metrics.line_height * (drawn_captions as f32 + TIDBIT_GAP)
    };
    let block_top = oy + (head - title_block - caption_block) / 2.0;

    let ink = content
        .theme
        .ink()
        .restate(1.0, (0.55 + 0.45 * lum).min(1.0));
    let first_line_right = ox + column.first_line_right();
    let first_line_left = text_left + dot_reserve;
    for (index, line) in lines.iter().enumerate() {
        let baseline = block_top + leading * index as f32 + title_metrics.ascent;
        // The first line is clipped short of the rail (and, on a worker,
        // pushed right of its own dot) — every line after it runs the card's
        // full width. A word too wide to break is cut at its own line's edge,
        // which for line one is whichever of those two it met first.
        let (left, right) = if index == 0 {
            (first_line_left, first_line_right)
        } else {
            (text_left, text_right)
        };
        draw_text(
            sheet, font, line, TITLE_PX, left, baseline, ink, left, right, opacity,
        );
    }
    if content.is_worker() {
        let radius = dot_diameter / 2.0;
        let center = (
            text_left + radius,
            block_top + title_metrics.line_height / 2.0,
        );
        draw_worker_dot(sheet, center, radius, opacity, content.theme.accent());
    }

    let caption_ink = measured::FILL_MID.mix(ink, measured::TIDBIT_INK_MIX);
    // The state word is quieter still. It is the one caption a reader is not
    // meant to *read* so much as recognise, and the card's own colour and breath
    // already say it — so it sits at the bottom of the ink ladder rather than
    // competing with the two lines that carry numbers.
    let state_ink = measured::FILL_MID.mix(caption_ink, STATE_INK_MIX);
    let caption_top = block_top + title_block + caption_metrics.line_height * TIDBIT_GAP;
    // The mockup's `.bars` sparkline sits on the tidbit line's own row, taken
    // out of that line's right edge rather than reserving a row of its own:
    // `content_floor_px`/`content_block_px` size every card in the tree by
    // the same two numbers regardless of which row is on screen ("uniform
    // height... whatever its depth or rank"), so a row of bars only some
    // cards carry cannot grow the block those functions hand back without
    // either un-uniforming every card's height or growing all of them for a
    // sparkline most do not draw. Sharing line 0's row keeps that invariant
    // intact.
    let bars_reserve = content
        .bars
        .as_deref()
        .filter(|bars| !bars.is_empty())
        .map(|bars| sparkline_span_px(bars.len(), caption_metrics.line_height))
        .unwrap_or(0.0);
    for (index, caption) in captions.iter().enumerate() {
        let Some((text, tone)) = caption else {
            continue;
        };
        let baseline =
            caption_top + caption_metrics.line_height * index as f32 + caption_metrics.ascent;
        let (ink, text) = match tone {
            CaptionTone::Register => (caption_ink, (*text).to_string()),
            CaptionTone::State => (state_ink, text.to_lowercase()),
        };
        let row_right = if index == 0 {
            (text_right - bars_reserve).max(text_left)
        } else {
            text_right
        };
        draw_text(
            sheet, font, &text, caption_px, text_left, baseline, ink, text_left, row_right, opacity,
        );
    }
    if let Some(bars) = content.bars.as_deref().filter(|bars| !bars.is_empty()) {
        draw_sparkline(
            sheet,
            bars,
            text_right,
            caption_top,
            caption_metrics.line_height,
            caption_ink,
            opacity,
        );
    }
}

/// A bar's width as a share of the row height it stands in, and the gap
/// between bars as a share of the same — chosen so the strip reads as
/// distinct columns rather than one solid block at the caption sizes this
/// draws at (11–14 px), the same way the mockup's `3px` bars in a `12px` row
/// hold a visible gap.
const SPARKLINE_BAR_WIDTH_MUL: f32 = 0.34;
const SPARKLINE_BAR_GAP_MUL: f32 = 0.2;

/// Total pixel width `count` bars take up in [`draw_sparkline`], including
/// the gaps between them — what [`draw_card`] reserves out of the tidbit
/// line before drawing its text, so the strip and the text it took the room
/// from can never overlap.
fn sparkline_span_px(count: usize, row_height: f32) -> f32 {
    let (bar_w, gap) = sparkline_bar_geometry(row_height);
    count as f32 * bar_w + count.saturating_sub(1) as f32 * gap
}

fn sparkline_bar_geometry(row_height: f32) -> (f32, f32) {
    (
        (row_height * SPARKLINE_BAR_WIDTH_MUL).max(1.0),
        (row_height * SPARKLINE_BAR_GAP_MUL).max(1.0),
    )
}

/// The mockup's literal `.bars`/`.bar` sparkline — mechanic 5 of the "Rio
/// Window, Assembled" gap analysis, drawn per the captain's decision to use
/// this encoding rather than [`draw_discharge`]'s groove metaphor. `bars` are
/// `0..=100`, oldest first, right-aligned so the newest sample sits flush
/// against the card's own text column edge exactly as the mockup's
/// `flex-end`-anchored row does.
fn draw_sparkline(
    sheet: &mut Canvas,
    bars: &[u8],
    right: f32,
    top: f32,
    row_height: f32,
    ink: Rgb,
    opacity: f32,
) {
    let (bar_w, gap) = sparkline_bar_geometry(row_height);
    let mut x = right - sparkline_span_px(bars.len(), row_height);
    for &value in bars {
        let height = (row_height * (f32::from(value) / 100.0)).max(1.0);
        fill_rect(
            sheet,
            &RoundRect {
                x,
                y: top + (row_height - height),
                w: bar_w,
                h: height,
                r: (bar_w * 0.3).min(1.5),
            },
            ink,
            opacity,
        );
        x += bar_w + gap;
    }
}

/// Fills `rect` at `opacity`, antialiased the same way every other shape on a
/// card is — coverage from [`RoundRect::distance`], not supersampling. See
/// `canvas`'s module doc for why that is the one distance every shape here
/// reads.
fn fill_rect(sheet: &mut Canvas, rect: &RoundRect, ink: Rgb, opacity: f32) {
    let x0 = rect.x.floor().max(0.0) as u32;
    let y0 = rect.y.floor().max(0.0) as u32;
    let x1 = (rect.x + rect.w).ceil().max(0.0) as u32;
    let y1 = (rect.y + rect.h).ceil().max(0.0) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let coverage = coverage(rect.distance(x as f32 + 0.5, y as f32 + 0.5));
            if coverage > 0.0 {
                sheet.blend(x, y, ink, coverage * opacity);
            }
        }
    }
}

#[cfg(test)]
mod the_cards_own_sparkline {
    use super::*;

    /// A tall bar and a short one paint different amounts of ink at the same
    /// x — the whole reason this is a bar chart and not a fixed-height tick
    /// mark. Checked at the bar's own centre column, not by scanning every
    /// pixel: `fill_rect`'s antialiasing is already covered by
    /// `canvas::tests`, and this test's job is only that `draw_sparkline`
    /// actually varies height with the published value.
    #[test]
    fn a_tall_bar_paints_more_of_its_column_than_a_short_one() {
        let row_height = 12.0;
        let mut sheet = Canvas::new(64, 32);
        draw_sparkline(
            &mut sheet,
            &[10, 90],
            64.0,
            0.0,
            row_height,
            Rgb(255, 255, 255),
            1.0,
        );
        let (bar_w, gap) = sparkline_bar_geometry(row_height);
        let start_x = 64.0 - sparkline_span_px(2, row_height);
        let opaque_rows_in_column = |cx: u32| {
            (0..32)
                .filter(|&y| {
                    let i = ((y as usize) * 64 + cx as usize) * 4;
                    sheet.rgba8()[i + 3] > 0
                })
                .count()
        };
        let short_x = (start_x + bar_w / 2.0) as u32;
        let tall_x = (start_x + bar_w + gap + bar_w / 2.0) as u32;
        assert!(
            opaque_rows_in_column(tall_x) > opaque_rows_in_column(short_x),
            "the 90-value bar must stand taller than the 10-value bar"
        );
    }

    /// A card with no [`CardContent::bars`] reserves nothing and draws
    /// nothing — the whole point of sharing the tidbit line's row rather
    /// than growing it: every card that does not publish [`BARS_TOKEN`]
    /// (crate::quality_streak::BARS_TOKEN) must be pixel-identical to a
    /// build of this feature that never existed.
    #[test]
    fn no_bars_reserves_no_room() {
        assert_eq!(sparkline_span_px(0, 12.0), 0.0);
    }

    /// [`CardContent::hash_into`] must move when `bars` does, the same
    /// invariant every other field on the card already holds — a card
    /// carried forward on a signature blind to this would freeze the
    /// sparkline the first time it changed.
    #[test]
    fn a_changed_sparkline_changes_the_cards_hash() {
        use std::hash::Hasher;
        let app = crate::app::state::AppState::test_new();
        let mut hues = [0.0; 5];
        for (slot, stage) in hues.iter_mut().zip(LifecycleStage::ALL) {
            *slot = stage.hue(&app.sidebar_palette, &app.host_terminal_theme);
        }
        let make = |bars: Vec<u8>| CardContent {
            title: "herdr".into(),
            tidbit: None,
            register: None,
            state_label: "idle".into(),
            state: AgentState::Idle,
            stage: LifecycleStage::Running,
            severity: Severity::Clear,
            hues: StageHues(hues),
            ground: Rgb(9, 17, 28),
            theme: CardTheme::UNTHEMED,
            split_channels: true,
            seen: true,
            depth: 0,
            lifted: false,
            focused_space: false,
            mark: None,
            residue: 0,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            breath: 0.0,
            spider: None,
            wash: None,
            crew: Vec::new(),
            bars: Some(bars),
        };
        let hash_of = |content: &CardContent| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            content.hash_into(&mut hasher);
            hasher.finish()
        };
        assert_ne!(
            hash_of(&make(vec![40, 70, 35, 95, 60])),
            hash_of(&make(vec![41, 70, 35, 95, 60]))
        );
    }
}

/// Draw a card's control rail with its top left corner at `at`.
///
/// `ink` is the card's chip ink — the stage hue at the chip's own rung. Both
/// controls take it for the same reason the chip does: they are the card's
/// affordances, and an affordance drawn in a hue the card is not at would be a
/// second thing on it saying what state its work is in.
#[allow(clippy::too_many_arguments)] // Rail, ink, size, geometry, origin and
                                     // the card's own opacity: every one varies per call site.
fn draw_control_rail(
    sheet: &mut Canvas,
    font: &CardFont,
    rail: &RailLayout,
    ink: Rgb,
    px: f32,
    geometry: &CardGeometry,
    at: (f32, f32),
    opacity: f32,
    theme: CardTheme,
) {
    let (x, y) = at;
    if let Some(badge) = &rail.badge {
        draw_space_badge(
            sheet,
            font,
            badge,
            px,
            geometry,
            (x, y + (rail.height - badge.height) / 2.0),
            opacity,
            theme,
        );
    }
    if let Some(summary) = &rail.summary {
        // Seen summaries drop to the tidbit's own caption weight rather than to
        // a grey: the card carries no neutral ink, and a badge that changed hue
        // when it was read would be the card saying its state had changed.
        let badge_ink = if summary.fresh {
            ink
        } else {
            measured::FILL_MID.mix(ink, measured::TIDBIT_INK_MIX)
        };
        draw_summary_mark(
            sheet,
            RoundRect {
                x: x + summary.x,
                y: y + (rail.height - summary.mark_side) / 2.0,
                w: summary.mark_side,
                h: summary.mark_side,
                r: summary.mark_side * SUMMARY_MARK_RADIUS,
            },
            badge_ink,
            (geometry.stroke * 0.5).max(0.9),
            opacity,
        );
        let metrics = font.metrics(px);
        let baseline = y + (rail.height - metrics.line_height) / 2.0 + metrics.ascent;
        draw_text(
            sheet,
            font,
            &summary.count,
            px,
            x + summary.count_x,
            baseline,
            badge_ink,
            x + summary.count_x,
            x + rail.width,
            opacity,
        );
    }
    if let Some(chevron) = &rail.chevron {
        draw_chevron(
            sheet,
            (x + chevron.x, y + (rail.height - chevron.side) / 2.0),
            chevron.side,
            chevron.group,
            ink,
            opacity,
        );
    }
}

/// The Space badge pill: a fully-rounded `RoundRect` at a tenth of an alpha,
/// its own edge stroke, and centred text — `.badge`/`.badge.warn` from the
/// flight-deck circuit mockup. Unlike the rest of the rail this carries its
/// own colour ([`SpaceBadgeMark::ink`]) rather than the card's chip ink: it is
/// a fleet-published quality signal, not the row's own agent-lifecycle state,
/// and drawing it in the chip's hue would make the two unreadable apart.
#[allow(clippy::too_many_arguments)] // One more than `draw_control_rail`: the
                                     // badge carries its own ink, so it needs
                                     // the theme that answers for it.
fn draw_space_badge(
    sheet: &mut Canvas,
    font: &CardFont,
    badge: &RailBadge,
    px: f32,
    geometry: &CardGeometry,
    at: (f32, f32),
    opacity: f32,
    theme: CardTheme,
) {
    let (x, y) = at;
    let badge_px = px * measured::BADGE_SIZE_MUL;
    let ink = badge.mark.ink(theme);
    let pill = RoundRect {
        x,
        y,
        w: badge.width,
        h: badge.height,
        r: badge.height / 2.0,
    };
    let half_stroke = (geometry.stroke * 0.5).max(0.9) / 2.0;
    let x0 = x.floor().max(0.0) as u32;
    let y0 = y.floor().max(0.0) as u32;
    let x1 = (x + badge.width).ceil() as u32;
    let y1 = (y + badge.height).ceil() as u32;
    for py_i in y0..y1 {
        let py = py_i as f32 + 0.5;
        for px_i in x0..x1 {
            let pxf = px_i as f32 + 0.5;
            let d = pill.distance(pxf, py);
            let fill = coverage(d);
            if fill > 0.0 {
                sheet.blend(px_i, py_i, ink, measured::BADGE_FILL_ALPHA * fill * opacity);
            }
            let edge = coverage(d.abs() - half_stroke);
            if edge > 0.0 {
                sheet.blend(px_i, py_i, ink, measured::BADGE_EDGE_ALPHA * edge * opacity);
            }
        }
    }
    let metrics = font.metrics(badge_px);
    let baseline = y + (badge.height - metrics.line_height) / 2.0 + metrics.ascent;
    draw_text(
        sheet,
        font,
        &badge.label,
        badge_px,
        x + badge.hpad,
        baseline,
        ink,
        x + badge.hpad,
        x + badge.width - badge.hpad,
        opacity,
    );
}

/// `▤`, drawn rather than set: a hairline square with two rules across it.
///
/// The glyph is U+25A4 and no face the card can be set in is guaranteed to
/// carry it — see [`canvas::Triangle`] for the same problem and the same
/// answer. Hairline and rules are one width, and it is the card's own stroke
/// halved: the mark is a tenth of the card's height, so the chrome around it
/// wants to be lighter than the chrome around the card.
fn draw_summary_mark(sheet: &mut Canvas, mark: RoundRect, ink: Rgb, hairline: f32, opacity: f32) {
    let half = hairline / 2.0;
    let rules: [f32; 2] = SUMMARY_MARK_RULES.map(|at| mark.y + mark.h * at);
    for y in mark.y.floor().max(0.0) as u32..((mark.y + mark.h).ceil() as u32) {
        let py = y as f32 + 0.5;
        // The rules stop inside the frame rather than crossing it, so the mark
        // reads as a filled square and not as a grid.
        let rule = rules
            .into_iter()
            .map(|at| coverage((py - at).abs() - half))
            .fold(0.0, f32::max);
        for x in mark.x.floor().max(0.0) as u32..((mark.x + mark.w).ceil() as u32) {
            let px = x as f32 + 0.5;
            let d = mark.distance(px, py);
            let frame = coverage(d.abs() - half);
            // `d + half` is the distance to the frame's *inner* edge, so this is
            // 1 well inside the square and ramps away exactly where the frame's
            // own ink starts.
            let inside = coverage(d + half);
            let alpha = frame.max(rule * inside);
            if alpha > 0.0 {
                sheet.blend(x, y, ink, alpha * opacity);
            }
        }
    }
}

/// `▸` or `▾`, inscribed in a square box of `side` at `at`.
///
/// Both points are drawn from the same box so the rail's reserved width does not
/// change when a group is opened or closed — see [`CHEVRON_NOSE`].
fn draw_chevron(
    sheet: &mut Canvas,
    at: (f32, f32),
    side: f32,
    group: GroupChevron,
    ink: Rgb,
    opacity: f32,
) {
    let (x, y) = at;
    let nose = side * CHEVRON_NOSE;
    let slack = (side - nose) / 2.0;
    let triangle = match group {
        GroupChevron::Collapsed => Triangle {
            a: (x + slack, y),
            b: (x + slack, y + side),
            c: (x + slack + nose, y + side / 2.0),
        },
        GroupChevron::Expanded => Triangle {
            a: (x, y + slack),
            b: (x + side, y + slack),
            c: (x + side / 2.0, y + slack + nose),
        },
    };
    let (x0, y0, x1, y1) = triangle.bounds();
    for py in y0..y1 {
        for px in x0..x1 {
            let alpha = coverage(triangle.distance(px as f32 + 0.5, py as f32 + 0.5));
            if alpha > 0.0 {
                sheet.blend(px, py, ink, alpha * opacity);
            }
        }
    }
}

/// Set `text` with its baseline at `(x, baseline)`, clipped to `[left, right)`.
///
/// The clip is the only thing standing in for an ellipsis: a word wider than
/// its column is drawn and cut at the column edge rather than shortened, which
/// is the behaviour the captain asked for over a mid-word ellipsis.
#[allow(clippy::too_many_arguments)] // Text, size, origin, ink, clip bounds and
                                     // the card's own opacity: every one of these varies per call site.
fn draw_text(
    sheet: &mut Canvas,
    font: &CardFont,
    text: &str,
    px: f32,
    x: f32,
    baseline: f32,
    ink: Rgb,
    left: f32,
    right: f32,
    opacity: f32,
) {
    let left = left.floor() as i32;
    let right = right.ceil() as i32;
    font.draw(text, px, x, baseline, |gx, gy, coverage| {
        if gx < left || gx >= right || gx < 0 || gy < 0 {
            return;
        }
        sheet.blend(gx as u32, gy as u32, ink, coverage * opacity);
    });
}

/// What this card's tidbit line says: `project · N% ctx · Ns`.
///
/// D-MID, the density the captain picked: one dim line carrying the facts the
/// title does not, at 72% of the title's size and 52% of its ink so the eye
/// still lands on the title and reads this as caption. Every part is dropped
/// when the fleet did not publish it, so a pane with no tokens gets no line
/// rather than a line of placeholders.
fn tidbit_line(entry: &AgentPanelEntry, state_age: Option<std::time::Duration>) -> Option<String> {
    tidbit_parts(
        entry.tokens.get("project"),
        entry.tokens.get("context"),
        state_age,
    )
}

fn tidbit_parts(
    project: Option<&String>,
    context: Option<&String>,
    state_age: Option<std::time::Duration>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(project) = project {
        parts.push(project.clone());
    }
    if let Some(context) = context {
        let context = context.trim();
        if context.ends_with("ctx") {
            parts.push(context.to_string());
        } else {
            parts.push(format!("{context} ctx"));
        }
    }
    if let Some(age) = state_age {
        parts.push(crate::state_age::format(age));
    }
    (!parts.is_empty()).then(|| parts.join("  ·  "))
}

/// The title: the work this agent says it is doing.
///
/// `doing` is display-only metadata the fleet publishes and Herdr only ever
/// stores, so the *token* is still never rewritten — this is a display choice
/// made on the way to the card and nothing here writes back. With no `doing`
/// published the card falls back to the same name the character row shows,
/// which is what keeps a plain shell pane from drawing an empty card.
///
/// [`summary::condense`] is applied here rather than in the renderer because
/// what it removes does not depend on how wide the card is — so this is the one
/// place it can run and have the clean string be what gets hashed, cached,
/// carried forward on an unchanged signature and sent to a remote client that
/// rasterises the card itself. Condensing at draw time instead would make two
/// clients disagree about the card's text while agreeing about its signature.
fn title_text(entry: &AgentPanelEntry) -> String {
    let published = entry
        .tokens
        .get("doing")
        .cloned()
        .or_else(|| entry.agent_label.clone())
        .or_else(|| entry.pane_label.clone())
        .unwrap_or_else(|| entry.primary_label.clone());
    display_summary(published)
}

/// One published summary, as the card will say it.
///
/// Falls back to the publisher's own string whenever condensing would leave
/// nothing: a card with no words on it is indistinguishable from a broken one,
/// and a `doing` of `"..."` is still better than a blank.
fn display_summary(published: String) -> String {
    let condensed = summary::condense(&published);
    if condensed.is_empty() {
        published
    } else {
        condensed
    }
}

/// The catalogue behaviour a card in this state breathes with.
///
/// **This mapping is the card's half of the state language**, and it is the same
/// ladder the tray badges use: a card with work behind it and a card on the back
/// burner are two *rhythms* before they are two brightnesses, so a reader who
/// cannot separate the hues still reads the tree. Both names are declared on
/// every row's lifecycle, so a card changing state does not restart the clock of
/// the state it left.
/// A serious problem escalates over both, and over rest as well as over live: a
/// card that has gone quiet with something badly wrong on it is not resting, and
/// this is where the severity channel stops being a colour. The visual target is
/// explicit that state has to survive a reader who cannot separate the hues, so
/// severity says it twice — in how far the card stands off the panel, and in how
/// fast it breathes.
///
/// `pub(crate)` because it is not only a render-time question: the app loop
/// publishes it as [`crate::anim::Member::playing`] so the engine steps a card
/// on the tier of the breath it is on rather than the fastest of the three its
/// row declares. Both callers must agree, which is why there is one function.
pub(crate) fn breath_behaviour(state: AgentState, severity: Severity) -> &'static str {
    if severity.escalates() {
        return crate::anim::behaviour::names::CARD_ALERT;
    }
    match state {
        AgentState::Working | AgentState::Blocked => crate::anim::behaviour::names::CARD_LIVE,
        AgentState::Idle | AgentState::Unknown => crate::anim::behaviour::names::CARD_REST,
    }
}

/// Where this card is in its breath, as the engine's envelope.
///
/// A **pure read** of the engine, exactly as `render` is a pure read of state:
/// [`crate::anim::Animator::frame`] takes `&self` and consults no clock, so
/// asking here cannot make one card disagree with another drawn in the same
/// pass. All the clock work happened in `Animator::advance`, on the app loop.
///
/// Nothing about the motion is expressed here: this asks the catalogue where the
/// card is and [`CardLight::breathed`] says what that means in light. `0.0`
/// whenever the engine has nothing — animation off, a host with no cards, or a
/// row that has only just been published — which is the card at its own settled
/// light.
fn breath(
    app: &AppState,
    row: &crate::anim::ElementId,
    state: AgentState,
    severity: Severity,
) -> f32 {
    use crate::anim::cell::{CellExtent, CellPos};
    if !app.sidebar_cards.pulse {
        return 0.0;
    }
    let Some(frame) = app.anim.frame(row, Some(breath_behaviour(state, severity))) else {
        return 0.0;
    };
    let Some(behaviour) = frame.behaviour else {
        return 0.0;
    };
    // A card is one object, so its breaths are uniform and every cell of the
    // notional extent resolves the same. Asking for the first cell of a 1×1
    // extent is asking for the envelope itself.
    let raw = behaviour.strength(CellPos::new(0, 0), CellExtent::new(1, 1), frame.progress);
    quantize(raw, CARD_BREATH_STEPS)
}

/// The state wash crossing this card right now, or `None` when none is.
///
/// Two conditions, both of which have to hold: the app has to remember a change
/// this card is still inside — which is what
/// [`crate::app::card_wash::CardWashes::live`] answers — and the engine has to
/// still be *playing* it. The second is what actually bounds the sweep: a wash
/// is a mount stage, so it is live exactly while the element is mounting, and it
/// ends because the mount ends rather than because anything here counts frames.
fn wash(app: &AppState, row: crate::anim::CardRow) -> Option<CardWashFrame> {
    if !app.sidebar_cards.wash {
        return None;
    }
    let live = app.sidebar_card_washes.live(&row)?;
    let from = live.from;
    let frame = app
        .anim
        .frame(&crate::anim::ElementId::CardWash(live), None)?;
    if frame.phase != crate::anim::Phase::Mount {
        return None;
    }
    Some(CardWashFrame {
        from,
        progress: quantize(frame.progress, CARD_WASH_STEPS),
        behaviour: *frame.behaviour?,
    })
}

/// One value snapped to a ladder whose rungs are `1.0 / steps` apart.
///
/// Quantized where it is *read* rather than where it is hashed, so the number
/// that reaches the signature and the number that reaches the pixels are the
/// same number. Rounded to the ladder and not merely hashed against it: a card
/// carried forward on a matching signature keeps the pixels it was drawn with,
/// and those pixels have to be the ones the ladder's step means.
///
/// **The ladder is not capped at `1.0`, and that is the point.**
/// [`crate::anim::Curve::SnapPendulum`] deliberately carries about ten percent
/// past its target and swings back — the snap the visual target asks for by
/// name — and [`crate::anim::behaviour::Behaviour::strength`] hands that
/// overshoot over intact precisely so a consumer that wants it can have it.
/// Capping here spent the whole overshoot on the ladder's top rung, so a card
/// held one unchanged signature straight through the snap and the pendulum: the
/// motion happened, and the picture did not move for a third of a second in the
/// middle of it. Only the floor is kept, because a rung below zero is not a
/// dimmer card, it is an envelope read backwards.
fn quantize(value: f32, steps: f32) -> f32 {
    (value.max(0.0) * steps).round() / steps
}

/// How hard a row's discharge runs.
///
/// **Only a working pane carries one.** H10 is specific about that: the
/// discharge is the pane saying its work is *live*, and a card that carried it
/// while idle would be saying the opposite of what it means. The amplitude is
/// that worker's own share of the fleet's traffic rather than a constant, so a
/// pane grinding and a pane ticking over are visibly different — and a row the
/// register cannot resolve draws none rather than a made-up one.
fn discharge_of(state: AgentState, body: Option<&super::body_register::BodyFacts>) -> f32 {
    if state != AgentState::Working {
        return 0.0;
    }
    body.map(super::body_register::BodyFacts::traffic)
        .unwrap_or(0.0)
}

/// The card for one tree row, whichever kind of row it is.
///
/// A mate is a Space and a worker is a pane, and both are rows in the one tree
/// — so both get the same card. The two kinds differ only in where the same
/// four facts are read from, which is what this function is: the title, the
/// tidbit, the state and whether this row is the selected one.
fn content_for(
    app: &AppState,
    entry: &super::WorkspaceListEntry,
    agents: &[AgentPanelEntry],
    bodies: &super::body_register::BodyRegister,
) -> Option<CardContent> {
    match entry {
        super::WorkspaceListEntry::Workspace {
            ws_idx,
            worktree_child,
            ..
        } => {
            let workspace = app.workspaces.get(*ws_idx)?;
            let body = bodies.get(&crate::anim::CardRow::Space(workspace.id.clone()));
            let (state, seen) = workspace.aggregate_state(&app.terminals);
            let tokens = workspace.metadata_tokens.values();
            let label = if *worktree_child {
                super::grouped_child_display_label(
                    &workspace.display_name_from_terminals(&app.terminals),
                    workspace.branch().as_deref(),
                    workspace.custom_name.is_some(),
                )
            } else {
                workspace.display_name_from_terminals(&app.terminals)
            };
            let age = workspace
                .aggregate_state_changed_at(&app.terminals)
                .map(|at| app.state_age_now.saturating_duration_since(at));
            let row = crate::anim::ElementId::workspace_row(&workspace.id);
            // One reading of the row's signal for both channels the card draws
            // and for the marker on it, so the stage a card is painted at and
            // the stage its spider is painted at cannot disagree — the same
            // rule `render_failure_spiders` holds on the character side.
            let signal = crate::app::lifecycle::row_signal(&tokens, state);
            let stage = signal.stage;
            let severity = crate::app::lifecycle::severity(
                tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            );
            let breath = breath(app, &row, state, severity);
            Some(CardContent {
                title: display_summary(tokens.get("doing").cloned().unwrap_or(label)),
                // A Space is a mate, and a mate is a body in the sky: its two
                // caption lines are that body's own registers, so the tree and
                // the system in front of it finally read the same quantities.
                // A Space the register cannot resolve — a roster mid-rebuild —
                // keeps the caption it always had rather than losing its line.
                tidbit: body
                    .and_then(super::body_register::BodyFacts::body_line)
                    .or_else(|| tidbit_parts(tokens.get("project"), tokens.get("context"), age)),
                // A mate's third line is its orbit register — what its body has
                // done. It carries no state word, and neither does the
                // reference's mate pane: the card's own colour, its breath and
                // its spider all say what state it is in.
                register: body
                    .and_then(super::body_register::BodyFacts::orbit_line)
                    .map(|text| Caption {
                        text,
                        tone: CaptionTone::Register,
                    }),
                state_label: crate::ui::status::state_label(state, seen).to_string(),
                state,
                stage,
                severity,
                hues: StageHues::resolve(app),
                theme: CardTheme::resolve(app),
                ground: backdrop_rgb(app),
                split_channels: app.sidebar_cards.stage_hue,
                seen,
                depth: entry.depth(),
                // The same rows the character card lifts its glow ramp for. The
                // drawn card is the *only* thing carrying selection on a row it
                // covers — the row wash that used to show it is not painted
                // under a shape, because it cannot be clipped to a border drawn
                // inside a cell — so a cursor that only lifted the active Space
                // would leave the keyboard cursor with nothing to stand on.
                lifted: super::workspace_row_highlighted(app, *ws_idx),
                // The same reading as `lifted`: a Space's own focus *is* what
                // the mockup's `.card.active` accents. See
                // [`CardContent::focused_space`].
                focused_space: super::workspace_row_highlighted(app, *ws_idx),
                // Filled in by `compute_card_placement`, the pass that knows
                // how far through its own arrival this row is.
                generate: 1.0,
                discharge: discharge_of(state, body),
                mark: None,
                // Keyed by the Space's own tree handle, which is exactly what
                // a worker's `owner` token names and what the credit in
                // `App::absorbing_owner` resolves to — so a mate that has no
                // handle has no rings, rather than sharing a blank key with
                // every other unnamed Space.
                residue: super::space_tree_name(app, *ws_idx)
                    .map(|name| app.residue.rings(&name))
                    .unwrap_or(0),
                // Filled in by `compute_card_placement`, which is the pass that
                // has the row's cells and can therefore ask whether a control is
                // clickable there at all. See `control_rail`.
                controls: ControlRail::default(),
                breath,
                spider: spider::resolve(
                    app,
                    crate::anim::CardRow::Space(workspace.id.clone()),
                    signal,
                ),
                wash: wash(app, crate::anim::CardRow::Space(workspace.id.clone())),
                // Filled in by `compute_card_placement`, which is the pass that
                // has the whole entry list and can see what the tree walk hung
                // under this row.
                crew: Vec::new(),
                bars: tokens
                    .get(crate::quality_streak::BARS_TOKEN)
                    .and_then(|value| crate::quality_streak::parse_bars(value)),
            })
        }
        super::WorkspaceListEntry::Agent { entry_idx, .. } => {
            let detail = agents.get(*entry_idx)?;
            let body = bodies.get(&crate::anim::CardRow::Agent(detail.pane_id));
            let age = detail
                .last_agent_state_change_at
                .map(|at| app.state_age_now.saturating_duration_since(at));
            let row = crate::anim::ElementId::agent_row(detail.pane_id);
            // See the Space arm: one reading, both channels and the marker.
            let signal = crate::app::lifecycle::row_signal(&detail.tokens, detail.state);
            let stage = signal.stage;
            let severity = crate::app::lifecycle::severity(
                detail
                    .tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            );
            let breath = breath(app, &row, detail.state, severity);
            Some(CardContent {
                title: title_text(detail),
                // A worker is a moon, and the reference's worker pane sets its
                // task on line two and **its state as a bare lowercase word** on
                // line three. It carries no register line: a pane is not a
                // checkout, so it has no mass, no streak and nothing the sky
                // reads off it — which is exactly why the reference gives that
                // line to the state instead.
                tidbit: tidbit_line(detail, age),
                register: Some(Caption {
                    text: super::agent_status_label(detail).to_string(),
                    tone: CaptionTone::State,
                }),
                state_label: super::agent_status_label(detail).to_string(),
                state: detail.state,
                stage,
                severity,
                hues: StageHues::resolve(app),
                theme: CardTheme::resolve(app),
                ground: backdrop_rgb(app),
                split_channels: app.sidebar_cards.stage_hue,
                seen: detail.seen,
                depth: entry.depth(),
                lifted: app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id),
                // A worker is never the focused Space — only a Space can be
                // one — so a worker earns the strong accent solely by
                // arriving, never by being the pane the terminal shows. See
                // [`CardContent::focused_space`].
                focused_space: false,
                generate: 1.0,
                discharge: discharge_of(detail.state, body),
                mark: None,
                // A mate can be a pane rather than a Space, and it is named
                // the same way: `agent_name` is "this pane's own handle, the
                // thing another pane's `owner` token names".
                residue: detail
                    .agent_name
                    .as_deref()
                    .map(|name| app.residue.rings(name))
                    .unwrap_or(0),
                controls: ControlRail::default(),
                breath,
                spider: spider::resolve(app, crate::anim::CardRow::Agent(detail.pane_id), signal),
                wash: wash(app, crate::anim::CardRow::Agent(detail.pane_id)),
                // A worker is not a mate: nothing is dispatched *under* it, so
                // it carries no list of its own and never grows one.
                crew: Vec::new(),
                // A worker is not a checkout either, so it has no Space-scoped
                // metadata tokens of its own to read `BARS_TOKEN` off — same
                // reasoning as `register` above.
                bars: None,
            })
        }
    }
}

/// The workers drawn inside the Space at `index`'s own card.
///
/// # Where the two tiers come from
///
/// [`super::crew_tier`], and so from the ownership tree the fleet already
/// publishes with its `owner` tokens — never from a new flag. A worker the Space
/// dispatched itself is tier `0`; one that came through a second mate is tier
/// `1`, whichever second mate it was and however deep the chain that opened it
/// ran. Both tiers are in this one list, in the order the tree walk put them,
/// which is newest-first inside every group by [`super::enter_at_head`]. That is
/// the captain's "a new worker always lands above whichever one was most
/// recently added", and it is an invariant of the list rather than something the
/// spawning frame has to catch.
///
/// # Why the arrival is read here
///
/// Because a crew row has no card of its own, so nothing else resolves its
/// life. Both beats come off the same [`super::motion::ArrivalCircuit`] the
/// panel's own rows arrive on — the push beat opens the row's track, the card
/// beat fades its ink in — and they are non-overlapping there by construction,
/// in both directions. A host with no card motion reads every row settled, which
/// is the reduced-motion answer: the list is simply drawn at its resting state.
fn crew_for(
    app: &AppState,
    entries: &[super::WorkspaceListEntry],
    index: usize,
    agents: &[AgentPanelEntry],
    moving: bool,
    limit: usize,
) -> Vec<crew::CrewMember> {
    (index + 1..index + 1 + super::crew_len(entries, index).min(limit))
        .filter_map(|row| {
            let tier = super::crew_tier(entries, row)?;
            let super::WorkspaceListEntry::Agent { entry_idx, .. } = entries.get(row)? else {
                return None;
            };
            let detail = agents.get(*entry_idx)?;
            // The same reading a worker's own card took: one row signal, and
            // the marker resolved off it. It is keyed on the pane, so the engine
            // is still driving the same element it always was — only where the
            // creature is drawn has changed.
            let signal = crate::app::lifecycle::row_signal(&detail.tokens, detail.state);
            let spider = spider::resolve(app, crate::anim::CardRow::Agent(detail.pane_id), signal);
            let arrival = if moving {
                let circuit = super::motion::arrival_circuit(super::motion::settle(
                    app,
                    &crate::anim::ElementId::agent_row(detail.pane_id),
                ));
                crew::CrewArrival {
                    open: circuit.push,
                    bloom: circuit.card,
                }
            } else {
                crew::CrewArrival::SETTLED
            };
            let age = detail
                .last_agent_state_change_at
                .map(|at| app.state_age_now.saturating_duration_since(at));
            // The mockup's `.wk-dot.pulse`, read off the worker's *own* row
            // rather than off the card it is drawn inside. That is what makes
            // it say something: a Space's card breathes on the Space's state,
            // and a list of workers all breathing that one rhythm would be five
            // copies of a fact the card above them already carries. On its own
            // row it is each worker's own state, and the row already exists —
            // it is the element the arrival circuit and the spider are keyed on
            // three lines up, and it already declares these behaviours (see
            // `AppState::sidebar_row_lifecycle`), so this starts no clock and
            // mounts no element. `breath_behaviour` also gives the dot the
            // card's own tiering for free: `card-live` while it works,
            // `card-rest` when it does not, `card-alert` when something is
            // badly wrong.
            let pulse = breath(
                app,
                &crate::anim::ElementId::agent_row(detail.pane_id),
                detail.state,
                crate::app::lifecycle::severity(
                    detail
                        .tokens
                        .get(crate::app::lifecycle::SEVERITY_TOKEN)
                        .map(String::as_str),
                ),
            );
            Some(crew::CrewMember {
                // The same two lines the worker's *own* card set, so a fleet
                // that publishes a `doing` sees the same words wherever the
                // worker is drawn rather than two summaries to keep in step.
                name: title_text(detail),
                // Its task line when it has published one, and its state as a
                // bare word when it has not — the reference's own second line
                // for a worker, and never a blank row.
                detail: tidbit_line(detail, age)
                    .or_else(|| Some(super::agent_status_label(detail).to_lowercase())),
                tier,
                arrival,
                pulse,
                spider,
            })
        })
        .collect()
}

/// The controls this row's card has to carry, resolved from the same functions
/// the character row and the click targets read.
///
/// # Why this is asked here and not in [`content_for`]
///
/// Because it needs the row's *cells*. Both controls are drawn over a row rather
/// than laid out in it, and both are gated on the row being wide enough to hold
/// them — [`super::worker_summary_badge_rect`] returns an empty rect on a row
/// that is not, and that empty rect is also the row's click target. A card that
/// drew a badge the row had already declined would be a mark nothing could
/// click, which is the same defect as the one being fixed here with the sign
/// flipped. So the gate is the rect, taken from the one function that decides
/// it, and drawn == clickable holds by construction.
fn control_rail(
    app: &AppState,
    entry: &super::WorkspaceListEntry,
    agents: &[AgentPanelEntry],
    card: &crate::app::state::WorkspaceCardArea,
) -> ControlRail {
    let summary = super::worker_summary_badge_for_entry(app, entry, agents)
        .filter(|(_, count)| super::worker_summary_badge_rect(card, *count).width > 0)
        .map(|(owner, count)| SummaryBadge {
            count,
            fresh: crate::app::worker_summary::summaries_for_owner(agents, &owner)
                .iter()
                .any(|summary| summary.is_unseen_finish()),
        });
    // Spaces only, and only the one that heads its group — a worktree child
    // carries no chevron of its own, and an agent pane is not a worktree space
    // at all. The same two conditions `render_workspace_list` draws under.
    let group = match entry {
        super::WorkspaceListEntry::Workspace {
            ws_idx,
            worktree_child: false,
            ..
        } => super::workspace_parent_group_state(app, *ws_idx),
        _ => None,
    }
    .filter(|_| super::workspace_group_chevron_rect(card).width > 0)
    .map(|(_, collapsed)| {
        if collapsed {
            GroupChevron::Collapsed
        } else {
            GroupChevron::Expanded
        }
    });
    // Spaces only, same as the chevron: a worker pane is not a checkout and
    // carries no quality tokens of its own to read.
    let badge = match entry {
        super::WorkspaceListEntry::Workspace { ws_idx, .. } => {
            app.workspaces.get(*ws_idx).and_then(|workspace| {
                let (state, _seen) = workspace.aggregate_state(&app.terminals);
                let tokens = workspace.metadata_tokens.values();
                let stage = crate::app::lifecycle::row_signal(&tokens, state).stage;
                space_badge(
                    tokens.get(REV_TOKEN).map(String::as_str),
                    tokens
                        .get(crate::quality_streak::DEFECT_TOKEN)
                        .map(String::as_str),
                    stage,
                )
            })
        }
        super::WorkspaceListEntry::Agent { .. } => None,
    };
    ControlRail {
        summary,
        group,
        space_badge: badge,
    }
}

/// What one pass over the tree's cards concluded.
///
/// Three outcomes rather than an `Option`, because "keep what you have" and
/// "there is nothing to draw" are opposites and an `Option<Layer>` spells them
/// the same way — which is how a stale card outlives the row it was a picture
/// of.
pub(crate) enum CardsUpdate {
    /// Nothing the cards are a picture of moved. Keep them, encode nothing.
    Unchanged,
    Rebuilt(Vec<SidebarCardLayer>),
    /// The pixel path is not live, or the tree has no agent cards in it.
    Empty,
    /// Every viewer draws the cards itself from a `ServerMessage::CardScene`, so
    /// this pass laid them out and stopped there.
    ///
    /// Opposite to [`Self::Empty`] in the one way that matters even though both
    /// leave the panel holding no artwork: the cards *are* coming, just not from
    /// here, so the character cards still stand down. Spelling them the same way
    /// is how a delegating client ends up with the tree drawn twice.
    Delegated,
}

/// What one pass over the tree's cards concluded, and where it decided to put
/// them.
///
/// The two travel together because a pass that changed no artwork still moved
/// it: every frame of a slide is a [`CardsUpdate::Rebuilt`] of *held* pixels at
/// a new placement, and the settled frames either side of it are `Unchanged` at
/// no offset. Returning the offsets beside the update is what lets the
/// character renderer draw the tree's connectors at exactly the cells the
/// placement used, instead of deriving a second answer from the same engine.
pub(crate) struct CardsBuild {
    pub(crate) update: CardsUpdate,
    /// One entry per input card, in the same order, in whole cells. `(0, 0)`
    /// for a card that is settled, that this pass did not draw, or whenever
    /// rows do not move on this host.
    ///
    /// Stamped onto [`crate::app::state::WorkspaceCardArea::motion_cells`] by
    /// the caller.
    pub(crate) motion: Vec<(i32, i32)>,
}

/// Build the images for the tree's current cards.
///
/// `previous` is what the last frame produced, and it answers two questions.
/// A frame whose content signature matches it reports [`CardsUpdate::Unchanged`]:
/// nothing is rasterised and nothing is re-encoded, which is what makes a fleet
/// whose cards change about once every ninety seconds cost about that often
/// rather than sixty times a second. A frame whose signature *moved* but whose
/// pixels did not — every resting card, on every step of its breath — is drawn
/// and then measured against what the same slot last put on screen, and carried
/// forward rather than encoded again. See
/// [`SidebarCardLayer::published`] and [`Rasteriser::finish`].
///
/// When `AppState::sidebar_card_graphics_client_rasterized` says every attached
/// viewer draws its own cards, this stops after the layout and reports
/// [`CardsUpdate::Delegated`] without drawing anything at all.
///
/// # Two drawing models
///
/// Under `[experimental] sidebar_card_shapes` this returns **one layer per
/// card**: each is its own RGBA image, transparent outside its own glow, at its
/// own placement and its own position. Otherwise it returns a single layer — one
/// opaque sheet spanning the whole tree, with each row's background painted into
/// it.
///
/// The difference is not cosmetic. The sheet's glow terminates at the sheet's
/// rectangle instead of falling off into whatever is behind it, so a card cannot
/// be moved relative to its neighbours without that rectangle's edge shearing
/// across them. A shape has no rectangle to clip: two shapes that overlap are two
/// placements, and the terminal composites their glows. That is what makes
/// sliding, fading and reflowing one card independently expressible at all.
pub(crate) fn build_cards(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
    previous: &[SidebarCardLayer],
) -> CardsBuild {
    // Sized and zeroed up front so every early return out of the build answers
    // "nothing moved" rather than "no answer": a pass that drew nothing must
    // leave the character renderer drawing the tree where the layout put it.
    let mut motion = vec![(0, 0); cards.len()];
    let update = build_cards_inner(app, cards, sidebar_area, cell_size, previous, &mut motion);
    CardsBuild { update, motion }
}

/// Which cards are placed where and what they say, independent of how the
/// result is turned into pixels.
///
/// Shared by the server's own embed path (which adds a font/cell-size-aware
/// [`Rasteriser`] and rasterises immediately) and [`build_card_scene`] (which
/// ships this data to a client that rasterises for itself). Pulling it out is
/// what keeps the two from ever disagreeing about where a card is or what it
/// contains.
struct CardPlacement {
    placed: Vec<(Rect, CardContent)>,
    offsets: Vec<(i32, i32)>,
    field: Rect,
    bounds: Rect,
    bloom_floor: u16,
    backdrop: Rgb,
    /// See [`Rasteriser::rail`]. Not on [`CardScene`]: the wire path draws
    /// shapes, which paint no backdrop and so cover no rail.
    rail: Rgb,
    font: &'static CardFont,
    title_metrics: FontMetrics,
    tidbit_metrics: FontMetrics,
    cell_w: f32,
    cell_h: f32,
}

/// `Err` is none at all — see [`build_cards_inner`] and [`build_card_scene`]
/// for what their callers do with that.
///
/// `motion` is filled in with the offset each card was placed at, indexed like
/// `cards`.
fn compute_card_placement(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
    motion: &mut [(i32, i32)],
) -> Result<CardPlacement, ()> {
    let fold_width = super::row_fold_width(app, super::workspace_list_rect(sidebar_area));
    if !is_available(app, fold_width) {
        return Err(());
    }
    let font = font::card_font(app.sidebar_card_font.as_deref()).ok_or(())?;
    let cell_w = f32::from(u16::try_from(cell_size.width_px).map_err(|_| ())?);
    let cell_h = f32::from(u16::try_from(cell_size.height_px).map_err(|_| ())?);
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return Err(());
    }

    let entries = super::workspace_list_entries(app);
    let agents = super::sidebar_agent_entries(app);
    // Ranked once for the whole pass, never once per card: the ranking is
    // `O(n log n)` over the roster, and resolving it inside the loop below would
    // make the panel `O(n^2 log n)` in the rows on screen. See
    // [`super::body_register`].
    let bodies = super::body_register::BodyRegister::resolve(app);
    let bounds = super::sidebar_content_rect(sidebar_area);
    // How far down the panel the sheet may reach. Everything but the tray is
    // the panel's own floor: blooming over the footer row the `new` button sits
    // on is harmless, because nothing there is a graphics placement.
    //
    // The tray is, and at the same `z` as this sheet. Its badges are their own
    // layer over its rows, so a sheet reaching into them is two placements on
    // one plane with no defined order — the last card's bloom would land on the
    // tray's top row of badges. The tree's rows already stop at the tray's top
    // edge; the bloom has to stop there too.
    let bloom_floor = {
        let tray = super::tray::tray_rect(app, bounds);
        if tray.height == 0 {
            bounds.y.saturating_add(bounds.height)
        } else {
            tray.y
        }
    };
    let backdrop = backdrop_rgb(app);
    let rail = rail_rgb(app);

    // How far through its own arrival or departure each drawn row is, gathered
    // in layout order beside the cards themselves so the two lists index
    // together. Only when rows are configured to move: with motion off nothing
    // reads the engine here at all and every card is placed exactly where the
    // layout put it, which is what the panel has always done.
    let moving = app.sidebar_rows_move();
    // Whether the panel is drawing worker lists inside its cards at all. The
    // same reading the *layout* took when it decided these rows' heights — so a
    // card can never be handed a crew the rows below it did not make room for,
    // which would draw a list over the cards under it.
    let nested = super::crew_is_drawn(app, fold_width);
    let mut placed: Vec<(Rect, CardContent)> = Vec::new();
    // Every drawn row, in layout order — **not only the ones that get an image
    // of their own**. A worker drawn inside its Space's card has no card here,
    // but its row still opens and closes, and the rows below the whole group are
    // pushed by exactly that. Accumulating over the placed cards alone would
    // leave a spawning worker growing its Space's box while nothing under it
    // moved.
    let mut lives: Vec<super::motion::RowLife> = Vec::new();
    // Which input card each entry of `placed` came from, so the resolved
    // offsets can be handed back on the caller's own indexing. A card without a
    // frame or without content is skipped here and stays at rest.
    let mut placed_from: Vec<usize> = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let circuit = super::motion::arrival_circuit(row_settle(app, card));
        if moving {
            lives.push(super::motion::RowLife {
                // The distance to the next row's own top, so the span a row
                // opens and closes is its height *and* the gap the layout puts
                // after it. Taken off the layout rather than recomputed from
                // `row_gap`, so the two can never disagree about what a row
                // occupies.
                height_px: row_span_cells(cards, index) * cell_h,
                // The push beat, not the raw engine settle: the space below
                // this row finishes opening before the rail starts growing,
                // never at the same time as the rail, the branch or the bloom.
                settle: circuit.push,
            });
        }
        let Some(frame) = card.card_frame else {
            continue;
        };
        if frame.width == 0 || frame.height == 0 {
            continue;
        }
        let Some(entry) = entries.get(card.entry_idx) else {
            continue;
        };
        // A worker drawn inside its Space's card is not a card. Its row is still
        // here — it has a rect, it takes a click, it opens and closes — but the
        // ink on it belongs to the head's own image, so nothing is placed for it
        // and nothing of it is rasterised twice.
        if nested && super::drawn_crew_head(app, &entries, card.entry_idx).is_some() {
            continue;
        }
        let Some(mut content) = content_for(app, entry, &agents, &bodies) else {
            continue;
        };
        content.controls = control_rail(app, entry, &agents, card);
        if nested {
            // Only the crew rows this pass actually laid out. A list that ran
            // off the bottom of the panel would otherwise be drawn past the box
            // the layout closed under its last visible row.
            let drawn = cards[index + 1..]
                .iter()
                .take_while(|row| {
                    super::drawn_crew_head(app, &entries, row.entry_idx) == Some(card.entry_idx)
                })
                .count();
            content.crew = crew_for(app, &entries, card.entry_idx, &agents, moving, drawn);
        }
        // The card-bloom beat, resolved where the row's own life is known. A
        // panel with motion off leaves every card whole, which is what it has
        // always done.
        if moving {
            content.generate = circuit.card;
        }
        placed.push((frame, content));
        placed_from.push(index);
    }
    if placed.is_empty() {
        return Err(());
    }
    let panel_px = f32::from(bounds.width) * cell_w;
    let all_offsets = if moving {
        super::motion::cell_offsets(
            &super::motion::row_offsets(&lives, panel_px),
            cell_w,
            cell_h,
        )
    } else {
        vec![(0, 0); cards.len()]
    };
    // Back onto the placed cards' own indexing, which is what the rasteriser
    // and the scene both walk.
    let offsets: Vec<(i32, i32)> = placed_from
        .iter()
        .map(|slot| all_offsets.get(*slot).copied().unwrap_or_default())
        .collect();
    // Published before anything is drawn, so the offsets the connectors follow
    // are the ones the placement was planned from even on the frame the
    // rasterisation fails. Every row, including the crew rows that have no card
    // of their own: the character renderer draws their rails off this and a row
    // left at zero would stand still while its Space's box slid under it.
    for (slot, offset) in all_offsets.iter().enumerate() {
        if let Some(cell) = motion.get_mut(slot) {
            *cell = *offset;
        }
    }

    let title_metrics = font.metrics(TITLE_PX);
    let tidbit_metrics = font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);

    // The tree's whole extent: every card's own image, unioned, clamped into the
    // panel. A placement whose rect leaves the panel would be clipped by the
    // pipeline anyway; keeping it inside means the clip never has to run.
    //
    // The sheet is exactly this rect. A shape is not — it is only as large as its
    // own card — but it still needs this, because it is the field the view-switch
    // dissolve is resolved over. Sizing the particle grid to each card's own image
    // instead would give every card the same local dissolve at a different scale,
    // and the wave that is supposed to cross the tree would break at every card's
    // edge.
    let extents: Vec<Rect> = placed.iter().map(|(frame, _)| *frame).collect();
    let field = dissolve_field_rect(&extents, (cell_w, cell_h), bounds, bloom_floor).ok_or(())?;

    Ok(CardPlacement {
        placed,
        offsets,
        field,
        bounds,
        bloom_floor,
        backdrop,
        rail,
        font,
        title_metrics,
        tidbit_metrics,
        cell_w,
        cell_h,
    })
}

/// `Ok(Some)` is new artwork, `Ok(None)` is what is already held, `Err` is none
/// at all.
///
/// `motion` is filled in with the offset each card was placed at, indexed like
/// `cards`.
fn build_cards_inner(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
    previous: &[SidebarCardLayer],
    motion: &mut [(i32, i32)],
) -> CardsUpdate {
    let Ok(placement) = compute_card_placement(app, cards, sidebar_area, cell_size, motion) else {
        return CardsUpdate::Empty;
    };

    // Laid out, and that is the whole of it: every attached viewer draws these
    // cards for itself from a `ServerMessage::CardScene`, so rasterising and
    // PNG-encoding ten of them here produces pixels this pass will then withhold
    // — see `crate::server::headless`, which sends the scene *instead of* them.
    //
    // The placement above still runs, and has to. It is the cheap half (rects,
    // content and the row offsets, no pixels), it is what stamps each row's
    // motion back for the character connectors, and its failing is the one
    // honest way to say the client will have no cards either. This is the card
    // half of what `AppRuntime::refresh_signal_tray_graphics` has done for the
    // tray since #95.
    if app.sidebar_card_graphics_client_rasterized {
        return CardsUpdate::Delegated;
    }

    let rasteriser = Rasteriser {
        font: placement.font,
        title_metrics: placement.title_metrics,
        tidbit_metrics: placement.tidbit_metrics,
        cell_size,
        cell_w: placement.cell_w,
        cell_h: placement.cell_h,
        field: placement.field,
        bounds: placement.bounds,
        bloom_floor: placement.bloom_floor,
        backdrop: placement.backdrop,
        // Only the sheet claims the cells the tree's lines cross. `shapes`
        // leaves them transparent and the character rail shows through
        // untouched, so it owes the tree nothing. See `draw_tree_joins`.
        rail: (!app.sidebar_card_shapes).then_some(placement.rail),
        dissolve: sheet_dissolve(app, cell_size),
        host_terminal_kind: app.host_terminal_kind,
        host_graphics_is_local: app.host_graphics_is_local,
        crew_bands: crew::CrewBands::of(placement.font, TITLE_PX, placement.cell_h),
    };

    let drawn = if app.sidebar_card_shapes {
        rasteriser.shapes(&placement.placed, &placement.offsets, previous)
    } else {
        rasteriser.sheet(&placement.placed, previous)
    };
    match drawn {
        Ok(Some(layers)) => CardsUpdate::Rebuilt(layers),
        Ok(None) => CardsUpdate::Unchanged,
        Err(()) => CardsUpdate::Empty,
    }
}

/// A wire-safe mirror of [`CardContent`], for `ServerMessage::CardScene`.
///
/// `wash` is left out on purpose — see [`CardScene`]'s own doc for why — so a
/// client-reconstructed [`CardContent`] always carries `wash: None`, the same
/// "no transition in progress" branch every card already renders correctly
/// through.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CardContentWire {
    title: String,
    tidbit: Option<String>,
    /// The third caption line. Carried like every other resolved string on this
    /// wire, because a client that rasterises its own cards has no fleet to rank
    /// and could not derive it.
    register: Option<Caption>,
    state_label: String,
    state: AgentState,
    stage: LifecycleStage,
    severity: Severity,
    hues: StageHues,
    ground: Rgb,
    /// The theme's authored card colours — see [`CardContent::theme`].
    /// Carried, like `ground` and `hues`, because a client that rasterises its
    /// own cards reads the *server's* config and has no `[theme.custom]` block
    /// of its own to resolve. New field: see `focused_space` below for why
    /// `#[serde(default)]` alone is not what makes one safe here.
    #[serde(default)]
    theme: CardTheme,
    split_channels: bool,
    seen: bool,
    depth: u8,
    lifted: bool,
    /// `#[serde(default)]` alone does **not** make this field skew-safe: bincode's
    /// struct encoding is positional, and this field sits inside a `Vec` of possibly
    /// many cards with more `CardScene` fields following it, so a decoder never
    /// truly runs out of bytes at this point — it just misreads the next sibling's
    /// bytes as this field's, which is what raises the wire-tag error, not what
    /// gracefully defaults it. What actually protects against this shape change is
    /// `crate::protocol::wire::PROTOCOL_VERSION` (bumped for it — see its own doc),
    /// so a mismatched server/client pairing gets a clean handshake rejection instead
    /// of a silently frozen card panel.
    #[serde(default)]
    focused_space: bool,
    mark: Option<CardMark>,
    residue: u8,
    controls: ControlRail,
    /// Both carried, because a client that rasterises its own cards has no
    /// animation engine of the server's and no fleet traffic counter.
    generate: f32,
    discharge: f32,
    breath: f32,
    /// Carried, unlike `wash`: the spider is four resolved numbers with no
    /// borrowed catalogue entry behind them, so a client that rasterises its own
    /// cards draws exactly the marker the server would have.
    spider: Option<spider::CardSpider>,
    /// The workers inside this card's own box.
    ///
    /// Carried whole rather than rebuilt: the crew is read off the ownership
    /// tree the *server* walks, and a client that rasterises its own cards has
    /// no fleet, no `owner` tokens and no entry list to walk. The bands the rows
    /// are laid on are **not** carried — see [`crew::CrewBands::of`], which both
    /// ends resolve from the same face and the same cell.
    ///
    /// See `focused_space` above for why `#[serde(default)]` is not what makes a
    /// new field here safe: `crate::protocol::wire::PROTOCOL_VERSION` is.
    #[serde(default)]
    crew: Vec<crew::CrewMember>,
    /// The mockup's literal sparkline — see [`CardContent::bars`]. Carried
    /// like every other resolved fleet fact on this wire, for the same reason
    /// `register` is: a client rasterising its own cards has no fleet
    /// metadata to read `BARS_TOKEN` off. New field — see `focused_space`
    /// above for why `#[serde(default)]` alone would not make it safe.
    #[serde(default)]
    bars: Option<Vec<u8>>,
}

impl From<&CardContent> for CardContentWire {
    fn from(content: &CardContent) -> Self {
        Self {
            title: content.title.clone(),
            tidbit: content.tidbit.clone(),
            register: content.register.clone(),
            state_label: content.state_label.clone(),
            state: content.state,
            stage: content.stage,
            severity: content.severity,
            hues: content.hues,
            ground: content.ground,
            theme: content.theme,
            split_channels: content.split_channels,
            seen: content.seen,
            depth: content.depth,
            lifted: content.lifted,
            focused_space: content.focused_space,
            mark: content.mark,
            residue: content.residue,
            controls: content.controls,
            generate: content.generate,
            discharge: content.discharge,
            breath: content.breath,
            spider: content.spider,
            crew: content.crew.clone(),
            bars: content.bars.clone(),
        }
    }
}

impl From<CardContentWire> for CardContent {
    fn from(wire: CardContentWire) -> Self {
        Self {
            title: wire.title,
            tidbit: wire.tidbit,
            register: wire.register,
            state_label: wire.state_label,
            state: wire.state,
            stage: wire.stage,
            severity: wire.severity,
            hues: wire.hues,
            ground: wire.ground,
            theme: wire.theme,
            split_channels: wire.split_channels,
            seen: wire.seen,
            depth: wire.depth,
            lifted: wire.lifted,
            focused_space: wire.focused_space,
            mark: wire.mark,
            residue: wire.residue,
            generate: wire.generate,
            discharge: wire.discharge,
            controls: wire.controls,
            breath: wire.breath,
            spider: wire.spider,
            wash: None,
            crew: wire.crew,
            bars: wire.bars,
        }
    }
}

/// Everything a client needs to rasterise the sidebar's cards itself, in
/// place of server-embedded card pixels. Sent as the opaque payload of
/// `ServerMessage::CardScene` to clients that set
/// `ClientMessage::Hello::wants_client_rasterized_cards`.
///
/// # Scope cut: no wash, no dissolve
///
/// [`CardContent::wash`] and the sheet's dissolve transition both ultimately
/// hold a [`crate::anim::behaviour::Behaviour`] carrying `&'static [char]`
/// glyph tables copied from a named catalogue — not serializable as data. A
/// client always reconstructs cards with `wash: None` and rasterises with
/// `dissolve: None`, which is the same "nothing in transition" branch this
/// module already renders correctly through, just without the wash-sweep and
/// view-switch-dissolve effects. Restoring those needs the catalogue lookup
/// key shipped alongside the resolved value, so the client can re-resolve the
/// same `&'static Behaviour` locally; that is follow-up work, not this.
///
/// # What is deliberately not here
///
/// Cell/font metrics, `cell_size`, and `host_terminal_kind`/
/// `host_graphics_is_local` are not shipped — the client already knows or
/// computes those itself, from its own attaching terminal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CardScene {
    placed: Vec<(Rect, CardContentWire)>,
    offsets: Vec<(i32, i32)>,
    field: Rect,
    bounds: Rect,
    bloom_floor: u16,
    backdrop: Rgb,
}

/// Builds the wire snapshot for a client that rasterises cards itself.
/// `None` when cards are not available to draw at all — the `CardScene`
/// equivalent of [`build_cards_inner`]'s `Err(())`.
pub(crate) fn build_card_scene(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
) -> Option<CardScene> {
    let mut motion = vec![(0, 0); cards.len()];
    let placement =
        compute_card_placement(app, cards, sidebar_area, cell_size, &mut motion).ok()?;
    Some(CardScene {
        placed: placement
            .placed
            .into_iter()
            .map(|(rect, content)| (rect, CardContentWire::from(&content)))
            .collect(),
        offsets: placement.offsets,
        field: placement.field,
        bounds: placement.bounds,
        bloom_floor: placement.bloom_floor,
        backdrop: placement.backdrop,
    })
}

/// Encodes a [`CardScene`] as the opaque bincode payload carried by
/// `ServerMessage::CardScene { bytes }`.
pub(crate) fn encode_card_scene(scene: &CardScene) -> Result<Vec<u8>, bincode::error::EncodeError> {
    bincode::serde::encode_to_vec(scene, bincode::config::standard())
}

/// Decodes a [`CardScene`] from the opaque bincode payload carried by
/// `ServerMessage::CardScene { bytes }`.
pub(crate) fn decode_card_scene(bytes: &[u8]) -> Result<CardScene, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(bytes, bincode::config::standard()).map(|(scene, _)| scene)
}

/// Rasterises a [`CardScene`] shipped by the server into the same Kitty
/// graphics layers [`Rasteriser::shapes`] would have produced server-side,
/// using this client's own font, cell size, and host-terminal capability
/// facts — `Rasteriser::shapes` itself is unchanged and reused as-is.
///
/// `font_override` is this client's own `[experimental] sidebar_card_font`
/// config value, the same override `compute_card_placement` reads from
/// `AppState` server-side. `previous` is this client's own last rasterised
/// layers, so a card whose content did not change is carried forward without
/// being redrawn, exactly as the server-side embed path already does.
pub(crate) fn rasterise_card_scene(
    scene: &CardScene,
    font_override: Option<&str>,
    cell_size: HostCellSize,
    host_terminal_kind: crate::kitty_graphics::HostTerminalKind,
    host_graphics_is_local: bool,
    previous: &[SidebarCardLayer],
) -> Result<Option<Vec<SidebarCardLayer>>, ()> {
    let font = font::card_font(font_override).ok_or(())?;
    let cell_w = f32::from(u16::try_from(cell_size.width_px).map_err(|_| ())?);
    let cell_h = f32::from(u16::try_from(cell_size.height_px).map_err(|_| ())?);
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return Err(());
    }
    let title_metrics = font.metrics(TITLE_PX);
    let tidbit_metrics = font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);

    let placed: Vec<(Rect, CardContent)> = scene
        .placed
        .iter()
        .cloned()
        .map(|(rect, wire)| (rect, CardContent::from(wire)))
        .collect();

    let rasteriser = Rasteriser {
        font,
        title_metrics,
        tidbit_metrics,
        cell_size,
        cell_w,
        cell_h,
        field: scene.field,
        bounds: scene.bounds,
        bloom_floor: scene.bloom_floor,
        backdrop: scene.backdrop,
        // A client rasterises shapes and never the sheet, so nothing it draws
        // covers the tree's own rails: the character line is still on screen
        // under the transparency and needs no join painted over it.
        rail: None,
        dissolve: None,
        host_terminal_kind,
        host_graphics_is_local,
        // Resolved here rather than carried on the scene: the same face and the
        // same cell the server laid the rows out against, so the client's bands
        // are the server's without a wire field to keep in step.
        crew_bands: crew::CrewBands::of(font, TITLE_PX, cell_h),
    };

    rasteriser.shapes(&placed, &scene.offsets, previous)
}

/// How many cell rows the row at `index` occupies before the next one starts.
///
/// The gap the layout puts after a row belongs to that row for this purpose:
/// what appears and disappears with a row is its box *and* the air under it, so
/// a reflow that moved only the box would leave a gap growing on its own.
fn row_span_cells(cards: &[crate::app::state::WorkspaceCardArea], index: usize) -> f32 {
    let card = &cards[index];
    let span = cards
        .get(index + 1)
        .map(|next| next.rect.y.saturating_sub(card.rect.y))
        // The last row has nothing under it to measure against, so it opens and
        // closes over its own height.
        .unwrap_or(card.rect.height);
    f32::from(span)
}

/// How far through its own life the engine says this row is.
///
/// The row's element is the same one [`crate::app::runtime`] publishes the
/// membership for, keyed the same way, so motion cannot end up watching a
/// different clock from the one the row's own arrival runs on.
///
/// Every row reads as fully settled — every [`super::motion::ArrivalCircuit`]
/// field at `1.0` — on a host where rows do not move.
///
/// The character renderer's half of the gesture: the rail growing down and the
/// branch growing right are drawn in characters here. See
/// [`super::render_card_border_rails`].
pub(crate) fn row_arrival(
    app: &AppState,
    card: &crate::app::state::WorkspaceCardArea,
) -> super::motion::ArrivalCircuit {
    if !app.sidebar_rows_move() {
        return super::motion::ArrivalCircuit {
            push: 1.0,
            rail: 1.0,
            tick: 1.0,
            card: 1.0,
        };
    }
    super::motion::arrival_circuit(row_settle(app, card))
}

fn row_settle(app: &AppState, card: &crate::app::state::WorkspaceCardArea) -> f32 {
    let id = match card.agent.as_ref() {
        Some(target) => crate::anim::ElementId::agent_row(target.pane_id),
        None => match app.workspaces.get(card.ws_idx) {
            Some(workspace) => crate::anim::ElementId::workspace_row(&workspace.id),
            None => return 1.0,
        },
    };
    super::motion::settle(app, &id)
}

/// The cells one card's own image covers: its frame plus the reach of its own
/// bloom, clamped into the panel.
///
/// The margin is [`bloom_reach_px`] and not a fraction of the height the card is
/// *drawn* at, because that is the reach [`lay_bloom`] paints to. The two used to
/// be spelled differently and a card whose content pushed it past its nominal
/// carried transparent padding it never lit.
///
/// The reach is now the same on every card, since the tiers it used to scale with
/// are gone ([`BASE_HEIGHT_PX`]) — so this is a per-card *frame* plus a shared
/// margin, rather than a per-card margin. It stays a function of the frame
/// because the frames still differ: width reads as rank.
fn card_image_rect(frame: Rect, cell: (f32, f32), bounds: Rect, bloom_floor: u16) -> Option<Rect> {
    let (cell_w, cell_h) = cell;
    let reach = bloom_reach_px(cell_h);
    // The vertical margin also has to carry however far the card was moved off
    // the middle of its own cells to sit on its branch line
    // ([`connector_row_offset_px`]). Without it the image is exactly the reach
    // wide and the card is drawn half a cell up it, so the top of the glow — and
    // on a tight face the top stroke — is cropped by the image's own edge rather
    // than by the panel.
    let shifted = reach + connector_row_offset_px(frame.height, cell_h).abs();
    clamp_bloomed(
        frame,
        (reach / cell_w).ceil() as u16,
        (shifted / cell_h).ceil() as u16,
        bounds,
        bloom_floor,
    )
}

/// The field every card is dissolved against: the union of their images.
///
/// Built out of the same [`card_image_rect`] each shape is drawn into, and not
/// out of a margin of its own, because a field that does not contain a card it
/// resolves fails silently: `Rasteriser::dissolve_origin` saturates and
/// [`DissolveFrame::apply`] clamps to the field's grid, so the part of that card
/// outside the field simply keeps full alpha for the whole transition while its
/// neighbours fade. A constant margin is exactly how the two drift apart —
/// [`card_height_px`] floors a card at what its content needs, so a proportional
/// face with a tall line height draws a card past [`BASE_HEIGHT_PX`] and gives it
/// a larger margin than a constant read off that base.
fn dissolve_field_rect(
    frames: &[Rect],
    cell: (f32, f32),
    bounds: Rect,
    bloom_floor: u16,
) -> Option<Rect> {
    frames
        .iter()
        .filter_map(|frame| card_image_rect(*frame, cell, bounds, bloom_floor))
        .reduce(|field, rect| field.union(rect))
        .filter(|field| field.width > 0 && field.height > 0)
}

/// Grow `rect` by a bloom margin and clamp it into the panel.
///
/// `None` when nothing survives the clamp, which is the one case a caller must
/// not turn into a zero-sized placement.
fn clamp_bloomed(
    rect: Rect,
    margin_x: u16,
    margin_y: u16,
    bounds: Rect,
    bloom_floor: u16,
) -> Option<Rect> {
    let x = rect.x.saturating_sub(margin_x).max(bounds.x);
    let y = rect.y.saturating_sub(margin_y).max(bounds.y);
    let right = rect
        .x
        .saturating_add(rect.width)
        .saturating_add(margin_x)
        .min(bounds.x.saturating_add(bounds.width));
    let bottom = rect
        .y
        .saturating_add(rect.height)
        .saturating_add(margin_y)
        .min(bloom_floor);
    let out = Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y));
    (out.width > 0 && out.height > 0).then_some(out)
}

/// Everything both drawing models need to turn placed cards into images.
///
/// The two differ in *how many images they cut the tree into* and in whether
/// each one paints a background — not in how a card is drawn. Sharing this is
/// what keeps the geometry, the type, the bloom and the state inks identical
/// across the flag, so turning it on changes the card's edges and nothing else
/// about the card.
struct Rasteriser<'a> {
    font: &'static CardFont,
    title_metrics: FontMetrics,
    tidbit_metrics: FontMetrics,
    cell_size: HostCellSize,
    cell_w: f32,
    cell_h: f32,
    /// The tree's whole extent, and the field the dissolve is resolved over.
    field: Rect,
    bounds: Rect,
    bloom_floor: u16,
    backdrop: Rgb,
    /// The ink the character tree draws its rails in, for the joins this image
    /// has to carry itself. `Some` only on the path that paints the backdrop —
    /// see [`Rasteriser::draw_tree_joins`] for why the two go together.
    rail: Option<Rgb>,
    dissolve: Option<DissolveFrame<'a>>,
    /// The foreground client's detected host terminal, from `AppState`. See
    /// `crate::kitty_graphics::preferred_sidebar_pixel_format`.
    host_terminal_kind: crate::kitty_graphics::HostTerminalKind,
    host_graphics_is_local: bool,
    /// The bands a crew list is laid out on here, resolved once for the pass
    /// rather than per card: it is one answer for the whole panel, the same way
    /// [`row_height_cells`] is.
    crew_bands: crew::CrewBands,
}

impl Rasteriser<'_> {
    /// One opaque image for the whole tree.
    fn sheet(
        &self,
        placed: &[(Rect, CardContent)],
        previous: &[SidebarCardLayer],
    ) -> Result<Option<Vec<SidebarCardLayer>>, ()> {
        let sheet_rect = self.field;
        let mut hasher = DefaultHasher::new();
        self.hash_common(&mut hasher, sheet_rect);
        for (frame, content) in placed {
            hash_placed(&mut hasher, frame, &sheet_rect, content);
        }
        let content_signature = hasher.finish();
        // The transition the sheet is *in*, on top of the cards it is a picture
        // of. Without this the content signature is unchanged for the whole of a
        // switch — the rows have not moved yet, that is the point of the switch —
        // so the cards would stand perfectly still while the characters around
        // them came apart, which is exactly the hard cut this is here to remove.
        // Quantized to [`DISSOLVE_STEPS`], so a settled panel still hashes to
        // `None` and rasterises nothing.
        self.dissolve.map(DissolveFrame::step).hash(&mut hasher);
        let signature = hasher.finish();

        let previous = previous.first();
        let viewport = self.aim(sheet_rect, (0, 0));
        if previous.is_some_and(|previous| {
            previous.signature == signature
                && previous.rect == sheet_rect
                && previous.viewport() == viewport
        }) {
            return Ok(None);
        }

        // A transition frame whose *cards* are the ones already drawn re-uses
        // those pixels. This is the difference between a switch costing one
        // rasterisation and costing one per frame, and the rasterisation is nine
        // tenths of the cost: drawing ten cards, their bloom and their type
        // measures about 16 ms against about 1.4 ms to encode the result and
        // under 1 ms to take it apart.
        let held = previous
            .filter(|previous| {
                previous.content_signature == content_signature && previous.rect == sheet_rect
            })
            .and_then(|previous| previous.undissolved.clone());
        // The sheet is one image at one slot, so what is standing under its id is
        // simply the previous sheet.
        let mut layer = self.finish(
            sheet_rect,
            held,
            signature,
            content_signature,
            previous,
            // The sheet is the server's own path and stays on the CPU: see
            // `Rasteriser::gpu_prebloom` for why the GPU is offered to the
            // shapes path only.
            || self.rasterise(placed, sheet_rect, None),
        )?;
        // The sheet never moves: it is one image spanning every row, so there is
        // no "one row" in it to slide. It is aimed anyway, at zero offset, so
        // both paths hand the pipeline the same shape and the clip box is not a
        // thing only one of them has.
        layer.aim_at(sheet_rect, self.clip(), viewport);
        Ok(Some(vec![layer]))
    }

    /// One transparent image per card.
    ///
    /// The card is the unit of everything here: its own rect, its own signature,
    /// its own placement. A card whose content did not change is carried forward
    /// without being rasterised or re-encoded even when a sibling changed, or
    /// when *it* moved — moving one card costs one card's placement, not the
    /// tree's artwork.
    ///
    /// `offsets` is where each card is drawn relative to where the layout put
    /// it, in whole cells, from [`super::motion::cell_offsets`], and is all
    /// zeroes whenever rows do not move on this host.
    fn shapes(
        &self,
        placed: &[(Rect, CardContent)],
        offsets: &[(i32, i32)],
        previous: &[SidebarCardLayer],
    ) -> Result<Option<Vec<SidebarCardLayer>>, ()> {
        // Measured first, drawn second. Deciding whether anything moved before
        // rasterising anything is what makes a settled panel free: the common
        // frame walks this list, matches every entry, and returns having encoded
        // nothing.
        let planned: Vec<PlannedShape> = placed
            .iter()
            .enumerate()
            .map(|(index, (frame, content))| {
                self.plan(
                    *frame,
                    content,
                    offsets.get(index).copied().unwrap_or_default(),
                )
            })
            .collect::<Option<_>>()
            .ok_or(())?;

        if planned.len() == previous.len()
            && planned.iter().zip(previous).all(|(planned, previous)| {
                planned.signature == previous.signature
                    && planned.rect == previous.rect
                    && planned.viewport == previous.viewport()
            })
        {
            return Ok(None);
        }

        // Matched first, drawn second, assembled third, and the *matching* stays
        // strictly serial. It is the only part of this with a carried
        // dependency — `taken` — and it is also the cheap part: comparing two
        // `u64`s a few dozen times against rasterising a card. Splitting it out
        // is what lets the expensive half fan out over threads without any of
        // the ordering questions, because by the time a thread starts drawing,
        // every decision about *which* cards are drawn has already been made,
        // in order, by one thread.
        let sources = self.match_held(&planned, previous);
        CARDS_RASTERISED.fetch_add(
            sources
                .iter()
                .filter(|source| matches!(source, ShapeSource::Draw(_)))
                .count() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );

        // The expensive half, and the only part that is parallel.
        let prebloom = self.gpu_prebloom(&planned, &sources, placed);
        let mut drawn = self.draw_shapes(&planned, &sources, placed, previous, prebloom);

        let clip = self.clip();
        let mut layers = Vec::with_capacity(planned.len());
        for (index, planned) in planned.iter().enumerate() {
            let mut layer = match &sources[index] {
                // Untouched, or moved and nothing more. The bytes are copied but
                // the drawing is not redone, and the drawing is the expensive
                // half by an order of magnitude. This is the case every frame of
                // a slide takes.
                ShapeSource::Held(slot) => {
                    let mut held = SidebarCardLayer::clone(&previous[*slot]);
                    if *slot != index {
                        // A held image that changed slot goes to the terminal
                        // under a different id — `HostSurfaceId::SidebarCards`
                        // is keyed by position — so what it published is no
                        // longer what this slot is showing, and the next raster
                        // here has nothing it may be measured against.
                        held.published.forget();
                    }
                    held
                }
                // Assembled in layout order out of slots keyed by index, so what
                // comes back does not depend on which thread finished first, or
                // on how many threads there were. A slot that is still empty
                // means the thread that owned it unwound; that is a failed
                // rasterisation like any other and the frame keeps the artwork
                // it already has.
                ShapeSource::Draw(_) => drawn[index].take().ok_or(())??,
            };
            layer.aim_at(planned.rect, clip, planned.viewport);
            layers.push(layer);
        }
        Ok(Some(layers))
    }

    /// Decide, in order, which planned cards can take a held image and which
    /// have to be drawn.
    ///
    /// Serial on purpose: `taken` carries from one card to the next, and it is
    /// what stops two rows that really are the same picture from both claiming
    /// the same held image.
    fn match_held(
        &self,
        planned: &[PlannedShape],
        previous: &[SidebarCardLayer],
    ) -> Vec<ShapeSource> {
        // Which held cards have already been claimed.
        let mut taken = vec![false; previous.len()];
        planned
            .iter()
            .map(|planned| {
                // Matched by what a card *is*, not by which slot it stood in. A
                // row inserted or removed in the middle of the tree shifts every
                // slot under it while changing not one of their signatures — the
                // signature is content and size, deliberately not position (see
                // [`Self::hash_common`]) — so slot matching would declare all of
                // them different and redraw the lot. That was measured: a
                // departure finishing re-uploaded better than half the tree.
                //
                // Two cards that hash the same are the same pixels, so which of
                // them claims a held image cannot be observable.
                let held = (0..previous.len())
                    .find(|slot| !taken[*slot] && previous[*slot].signature == planned.signature);
                match held {
                    Some(slot) => {
                        taken[slot] = true;
                        ShapeSource::Held(slot)
                    }
                    // A base is *borrowed*, not claimed: it is only the
                    // undissolved pixels this card is about to lay a new mask
                    // over, so unlike a held image two cards may legitimately
                    // read the same one. `taken` is deliberately not set here.
                    None => ShapeSource::Draw(
                        (0..previous.len())
                            .find(|slot| {
                                !taken[*slot]
                                    && previous[*slot].content_signature
                                        == planned.content_signature
                            })
                            .and_then(|slot| previous[slot].undissolved.clone()),
                    ),
                }
            })
            .collect()
    }

    /// Rasterise and encode every card that needs it, across a bounded number of
    /// threads.
    ///
    /// Returns one slot per planned card, indexed like `planned`: `Some` for a
    /// card that was drawn, `None` for one taking a held image — and `None` too
    /// for one whose drawing thread unwound.
    ///
    /// This is the [`crate::ui::sidebar::image_card`] hot spot. A card is drawn
    /// into its own `Canvas` out of `&self` and its own slice of `placed`, with
    /// nothing shared and nothing written outside its own slot, so the work is
    /// parallel by construction rather than by refactor. Twelve cards measured
    /// 8.24 ms on one thread and 1.89 ms on sixteen; the point of it is not the
    /// average frame, which is cached and free, but the frame where the tree's
    /// content actually changes and pays for all twelve at once.
    ///
    /// **Determinism.** A card's pixels are a pure function of `self`, its own
    /// `placed` entry and its own [`ShapeSource`] base — none of which any other
    /// card can touch — and results land in slots keyed by index rather than by
    /// completion. So the bytes this returns are identical to the serial ones,
    /// for any thread count, including one.
    fn draw_shapes(
        &self,
        planned: &[PlannedShape],
        sources: &[ShapeSource],
        placed: &[(Rect, CardContent)],
        previous: &[SidebarCardLayer],
        prebloom: Vec<Option<Canvas>>,
    ) -> Vec<Option<Result<SidebarCardLayer, ()>>> {
        // One slot per card, taken by whichever thread draws that card. Behind a
        // lock only because the slots are handed out at run time rather than
        // sliced up front; it is uncontended by construction, since no two
        // threads ever reach for the same index.
        let prebloom: Vec<std::sync::Mutex<Option<Canvas>>> =
            prebloom.into_iter().map(std::sync::Mutex::new).collect();
        let take_prebloom = |index: usize| -> Option<Canvas> {
            prebloom
                .get(index)
                .and_then(|slot| slot.lock().ok().and_then(|mut held| held.take()))
        };
        let todo: Vec<usize> = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| matches!(source, ShapeSource::Draw(_)))
            .map(|(index, _)| index)
            .collect();

        let mut out: Vec<Option<Result<SidebarCardLayer, ()>>> = std::iter::repeat_with(|| None)
            .take(planned.len())
            .collect();

        let threads = raster_threads(todo.len());
        if threads <= 1 {
            // One card to draw, or a machine with no width to draw it on. No
            // scope, no spawn, no atomic — the settled and near-settled frames
            // this path takes most often are exactly the ones that would only
            // pay for the machinery.
            for &index in &todo {
                out[index] = Some(self.draw_one(
                    index,
                    planned,
                    sources,
                    placed,
                    previous,
                    take_prebloom(index),
                ));
            }
            return out;
        }

        // Cards are not all the same size — a card with a tidbit is taller than
        // one without, and a depth-0 card is over twice the area of a depth-2 one
        // — so the work is handed out one card at a time rather than sliced into
        // equal chunks up front. A static split would leave the thread that drew
        // the big cards still working while the others sat idle.
        let next = std::sync::atomic::AtomicUsize::new(0);
        let todo = &todo;
        let collected: Vec<Vec<(usize, Result<SidebarCardLayer, ()>)>> =
            std::thread::scope(|scope| {
                let handles: Vec<_> = (0..threads)
                    .map(|_| {
                        let next = &next;
                        scope.spawn(move || {
                            let mut local = Vec::new();
                            loop {
                                let slot = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let Some(&index) = todo.get(slot) else {
                                    break;
                                };
                                local.push((
                                    index,
                                    self.draw_one(
                                        index,
                                        planned,
                                        sources,
                                        placed,
                                        previous,
                                        take_prebloom(index),
                                    ),
                                ));
                            }
                            local
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    // A panic in one card's rasterisation costs that card's
                    // slot and nothing else: the slot stays `None`, the caller
                    // reads that as a failed build, and the panel keeps the
                    // artwork it is already holding. It must not take the other
                    // eleven cards or the render loop with it.
                    .map(|handle| handle.join().unwrap_or_default())
                    .collect()
            });

        for (index, result) in collected.into_iter().flatten() {
            out[index] = Some(result);
        }
        out
    }

    /// Draw and encode exactly one card.
    ///
    /// Takes `&self` and reads nothing else that any other card writes, which is
    /// the whole reason [`Self::draw_shapes`] can call it from several threads.
    fn draw_one(
        &self,
        index: usize,
        planned: &[PlannedShape],
        sources: &[ShapeSource],
        placed: &[(Rect, CardContent)],
        previous: &[SidebarCardLayer],
        prebloom: Option<Canvas>,
    ) -> Result<SidebarCardLayer, ()> {
        let ShapeSource::Draw(base) = &sources[index] else {
            return Err(());
        };
        let planned = &planned[index];
        // One card, drawn into an image that is only as large as that card and
        // the reach of its own bloom, with no background painted anywhere.
        // Everything outside the glow stays at alpha zero.
        let one = &placed[index..index + 1];
        self.finish(
            planned.rect,
            base.clone(),
            planned.signature,
            planned.content_signature,
            // Positional: what the terminal is showing under this card's own
            // host image id is whichever layer stood at this index.
            previous.get(index),
            || self.rasterise(one, planned.rect, prebloom),
        )
    }

    /// The box on the panel a card may draw in: everything the tree's own rects
    /// are already clamped into.
    ///
    /// Exactly the bounds [`clamp_bloomed`] holds a settled card inside, so at
    /// rest every card is wholly within it and the clipper has nothing to do.
    /// A card in motion is the only thing that ever reaches it, and reaching it
    /// is what stops a slide from spilling over the terminal panes.
    fn clip(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            self.bloom_floor.saturating_sub(self.bounds.y),
        )
    }

    /// Where an image drawn for `rect` is placed, relative to [`Self::clip`].
    ///
    /// `offset` is already in whole cells — a card's placement is a cell
    /// position, and the quantisation happens once, in
    /// [`super::motion::cell_offsets`], so the connector rails drawn in
    /// characters beside this card are following exactly this number.
    fn aim(&self, rect: Rect, offset: (i32, i32)) -> (i32, i32) {
        let clip = self.clip();
        (
            i32::from(rect.x) - i32::from(clip.x) + offset.0,
            i32::from(rect.y) - i32::from(clip.y) + offset.1,
        )
    }

    /// What one card's image will be, and where it goes, before anything is
    /// drawn.
    fn plan(&self, frame: Rect, content: &CardContent, offset: (i32, i32)) -> Option<PlannedShape> {
        let rect = self.card_rect(frame)?;
        let mut hasher = DefaultHasher::new();
        self.hash_common(&mut hasher, rect);
        hash_placed(&mut hasher, &frame, &rect, content);
        let content_signature = hasher.finish();
        self.dissolve.map(DissolveFrame::step).hash(&mut hasher);
        if self.dissolve.is_some() {
            // Where the card sits in the field decides which particles it loses
            // and when, so two cards on the same step still come apart at
            // different moments. Only while a transition is running: with none,
            // the field has no bearing on the card's pixels and hashing it would
            // rebuild every card whenever the tree's extent changed.
            self.dissolve_origin(rect).hash(&mut hasher);
            self.field_px().hash(&mut hasher);
        }
        Some(PlannedShape {
            rect,
            viewport: self.aim(rect, offset),
            signature: hasher.finish(),
            content_signature,
        })
    }

    /// Encode one finished image, reusing held pixels when a transition frame
    /// already has them.
    ///
    /// `standing` is what this host image slot is currently showing — the
    /// previous list's layer at the *same index*, because that is what the
    /// terminal has under this surface's id. A freshly drawn card within
    /// [`crate::app::state::SURFACE_DRIFT_LEVELS`] of it is not encoded at all:
    /// the standing layer's bytes are carried forward under the new signature,
    /// so the graphics cache sees an unchanged `data_fingerprint` and the
    /// terminal keeps the image it already has.
    ///
    /// The signature is still adopted, so this is not a throttle and the card
    /// does not stall: the next frame is measured against the *published*
    /// raster rather than against this one, which is what bounds the drift — a
    /// breath that really is going somewhere crosses the tolerance and arrives.
    fn finish(
        &self,
        rect: Rect,
        held: Option<UndissolvedSheet>,
        signature: u64,
        content_signature: u64,
        standing: Option<&SidebarCardLayer>,
        draw: impl FnOnce() -> Result<Canvas, ()>,
    ) -> Result<SidebarCardLayer, ()> {
        // A held canvas covers the frame that *ends* a transition too, which has
        // no dissolve left to apply and would otherwise pay a full rasterisation
        // to arrive back at pixels it is already holding.
        let base = match held {
            Some(base) => base,
            None => UndissolvedSheet(std::sync::Arc::new(draw()?)),
        };
        let (width_px, height_px) = (base.0.width(), base.0.height());
        let dissolved;
        let canvas: &Canvas = match self.dissolve {
            Some(dissolve) => {
                let mut canvas = Canvas::clone(&base.0);
                dissolve.apply(&mut canvas, self.dissolve_origin(rect), self.field_px());
                dissolved = canvas;
                &dissolved
            }
            None => &base.0,
        };
        // Drawn, and within a couple of levels of the artwork this slot already
        // put on screen: the terminal keeps what it has, and the PNG encode
        // below — which on an idle fleet is the whole of the card path's server
        // cost — does not happen either. `rect` as well as the pixels because it
        // is what `card_layer` counts the placement's grid out of, so a layer
        // drawn for another box is not this box's image however close its pixels
        // land.
        if let Some(standing) = standing.filter(|standing| {
            standing.rect == rect
                && standing
                    .published
                    .holds(canvas.width(), canvas.height(), canvas.rgba8())
        }) {
            return Ok(SidebarCardLayer {
                rect,
                clip: rect,
                signature,
                content_signature,
                // Cloned rather than moved: `canvas` may still be borrowing it
                // for the comparison just made, and it is an `Arc` either way.
                undissolved: self.dissolve.map(|_| base.clone()),
                // Unchanged: what is on screen is still what was published, and
                // moving the anchor to these pixels is exactly the creep the
                // published anchor exists to prevent.
                published: standing.published.clone(),
                layer: standing.layer.clone(),
            });
        }
        // Picked once per image rather than per client: the same rasterised
        // bytes back every attached client's placement of this image, so a
        // single "is the host terminal local and known-fast" answer — the
        // foreground client's, detected at attach — is what all of them get.
        // See `preferred_sidebar_pixel_format`.
        let format = crate::kitty_graphics::preferred_sidebar_pixel_format(
            canvas_is_fully_opaque(canvas),
            self.host_terminal_kind,
            self.host_graphics_is_local,
        );
        let data = encode_canvas(canvas, format).ok_or(())?;
        // These are the bytes going to the terminal, so they are what every
        // later frame of this surface is measured against. Taken before the
        // struct below, which moves the canvas the dissolve is held in.
        let published = crate::app::state::PublishedSurfaceRaster::of(
            canvas.width(),
            canvas.height(),
            canvas.rgba8(),
        );
        Ok(SidebarCardLayer {
            rect,
            // Replaced by `aim_at` before this reaches anything that draws. The
            // image's own rect is the honest default: it is where a card with no
            // motion configured goes, so a layer that somehow escaped un-aimed
            // would be placed exactly where it has always been placed.
            clip: rect,
            signature,
            content_signature,
            // Only while a transition is running: a settled panel keeps no
            // second copy of artwork it is not about to take apart.
            undissolved: self.dissolve.map(|_| base),
            published,
            layer: card_layer(format, width_px, height_px, data, rect),
        })
    }

    /// The pixel size of an image covering `rect`, or `None` when that is no
    /// image at all or an implausibly large one.
    ///
    /// An image larger than the ceiling here is a sidebar nobody has — 8
    /// megapixels is a panel over a thousand pixels wide and seven thousand
    /// tall. The guard exists so a nonsense cell-size report cannot turn into a
    /// huge allocation: at four bytes a pixel for the canvas and eight more for
    /// the bloom field, that ceiling is about 96 MB, held only while it is being
    /// built.
    fn image_size_px(&self, rect: Rect) -> Option<(u32, u32)> {
        const MAX_IMAGE_PIXELS: u32 = 8_000_000;
        let width_px = u32::from(rect.width) * self.cell_size.width_px;
        let height_px = u32::from(rect.height) * self.cell_size.height_px;
        (width_px > 0 && height_px > 0 && width_px.saturating_mul(height_px) <= MAX_IMAGE_PIXELS)
            .then_some((width_px, height_px))
    }

    /// This frame's card blooms, computed on the GPU in one dispatch.
    ///
    /// Returns one slot per planned card, `None` for every card the GPU did not
    /// draw — which on a machine with no adapter, a build without the
    /// `gpu-raster` feature, or any process that is not a Windows client is
    /// every one of them. A `None` slot is not a failure: [`Self::rasterise`]
    /// simply lays that card's bloom on the CPU as it always has.
    ///
    /// # Why the whole frame at once
    ///
    /// A card's image is around 400x110 pixels and its bloom costs about half a
    /// millisecond on one core, which is the same order as the fixed cost of
    /// reaching a GPU and getting bytes back. Per card, that is a loss. Batched
    /// across the frame — one buffer, one pass, one readback — the round trip is
    /// paid once for all twelve, which is the case worth accelerating: the frame
    /// where the tree's content actually changed and every card is redrawn at
    /// once.
    ///
    /// Only cards that will genuinely be rasterised are in the batch. A card
    /// holding a base (`ShapeSource::Draw(Some(_))`) re-uses pixels it already
    /// has and never reaches `rasterise`, and one taking a held image
    /// (`ShapeSource::Held`) is not redrawn at all.
    fn gpu_prebloom(
        &self,
        planned: &[PlannedShape],
        sources: &[ShapeSource],
        placed: &[(Rect, CardContent)],
    ) -> Vec<Option<Canvas>> {
        let mut out: Vec<Option<Canvas>> = std::iter::repeat_with(|| None)
            .take(planned.len())
            .collect();
        if !crate::gpu::enabled() {
            return out;
        }

        let mut slots = Vec::new();
        let mut tiles = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            if !matches!(source, ShapeSource::Draw(None)) {
                continue;
            }
            let (Some(planned), Some((frame, content))) = (planned.get(index), placed.get(index))
            else {
                continue;
            };
            // A card whose image is nonsense is left out and fails in
            // `rasterise` exactly as it would have without a GPU in the picture.
            let Some((width, height)) = self.image_size_px(planned.rect) else {
                continue;
            };
            let card = self.place(*frame, content, planned.rect);
            let splats = plan_bloom(&card, width, height)
                .map(|splat| vec![splat.for_gpu()])
                .unwrap_or_default();
            tiles.push(crate::gpu::bloom::Tile {
                width,
                height,
                splats,
            });
            slots.push(index);
        }

        if !gpu_beats_the_threads(&tiles, raster_threads(tiles.len())) {
            return out;
        }
        match crate::gpu::bloom::compose(&tiles, bloom_curve()) {
            Ok(images) => {
                for ((slot, tile), image) in slots.iter().zip(&tiles).zip(images) {
                    out[*slot] = Canvas::from_rgba8(tile.width, tile.height, image);
                }
            }
            Err(declined) => crate::gpu::bloom::warn_once(&declined),
        }
        out
    }

    /// Draw `placed` into an image covering `rect`.
    ///
    /// `paint_backdrop` is true only for the sheet, which has to cover the
    /// character card standing under it. A shape leaves those pixels transparent
    /// — that is the whole point of it — which is sound because the character
    /// content beneath a shape is not drawn at all.
    ///
    /// `prebloom` is this image's bloom already drawn, from
    /// [`Self::gpu_prebloom`]. `None` is the ordinary case and means "lay it
    /// here, on this thread".
    fn rasterise(
        &self,
        placed: &[(Rect, CardContent)],
        rect: Rect,
        prebloom: Option<Canvas>,
    ) -> Result<Canvas, ()> {
        let (width_px, height_px) = self.image_size_px(rect).ok_or(())?;

        let cards: Vec<PlacedCard<'_>> = placed
            .iter()
            .map(|(frame, content)| self.place(*frame, content, rect))
            .collect();

        // **Nothing paints a backdrop any more, on either model.** The sheet
        // used to fill every cell a row owned with the panel's own colour, to
        // cover the character card standing under it. A card is glass now: an
        // opaque plate under a face at a tenth of an alpha is not a see-through
        // pane, it is a plate with a tint on it, and what the material exists to
        // show through is exactly what that fill was hiding. The character card
        // stands down instead — see [`shape_covers_row`] — so there is nothing
        // left underneath to cover.
        //
        // The tree's own rails and connectors are characters, and they now show
        // through the image where it is transparent rather than being painted
        // over and repaired.
        let mut canvas = match prebloom
            .filter(|canvas| canvas.width() == width_px && canvas.height() == height_px)
        {
            Some(canvas) => canvas,
            None => {
                let mut canvas = Canvas::new(width_px, height_px);
                let mut bloom = BloomField::new(width_px, height_px);
                for card in &cards {
                    lay_bloom(&mut bloom, card);
                }
                bloom.composite(&mut canvas);
                canvas
            }
        };

        for card in &cards {
            draw_card(&mut canvas, card, self.font);
            if card.content.lifted {
                // Selection is a change of intensity, never of hue — the same
                // rule the character card's lifted glow ramp follows.
                lift(&mut canvas, card);
            }
            // After the card and after the lift, and outside `draw_card`, which
            // returns early on a card too narrow to set its title in. A marker
            // that a narrow panel silently dropped is the same defect this
            // module was added to fix, one gate further in. It is deliberately
            // not lifted with the card either: the selection ramp is the card
            // saying it is the selected one, and a defect marker is not part of
            // that sentence.
            spider::draw(&mut canvas, card);
        }
        Ok(canvas)
    }

    /// One card's rounded rect, in the coordinates of an image covering `rect`.
    fn place<'c>(&self, frame: Rect, content: &'c CardContent, rect: Rect) -> PlacedCard<'c> {
        let geometry = CardGeometry::new(self.cell_h, content.mark.is_some());
        // The card is drawn at the one height every card is drawn at, centred on
        // the row its branch line meets it on. That is the middle of the cells
        // the row was given whenever there is a middle row to be the middle of,
        // and half a cell above it when there is not — see
        // [`connector_row_offset_px`]. The leftover is the gutter, and it is the
        // same leftover either way because every row in the tree is offset by
        // the same amount: this is where the measured 0.19 h sibling gap comes
        // back after the row height was rounded up to a whole number of cells.
        let cell_top = f32::from(frame.y.saturating_sub(rect.y)) * self.cell_h;
        let cell_height = f32::from(frame.height) * self.cell_h;
        // The crew's own extent, at the amount its rows have actually opened.
        // **Drawn, not reserved.** The layout already gave this row the cells
        // for a settled list — a row mid-arrival owns its cells — so a box
        // drawn at the reserved height would stand at its final size from the
        // first frame and there would be nothing to see opening. The rows below
        // it are offset by exactly the difference, off the same settle, which is
        // what keeps the box's bottom edge and the next card's top edge
        // together for the whole of the gesture.
        let crew = crew::drawn_extent_px(self.crew_bands, &content.crew);
        let wanted =
            (card_height_px(self.title_metrics, self.tidbit_metrics) + crew).min(cell_height);
        // Clamped into the gutter the card actually has.
        //
        // The offset is half a cell on an even-cell row, and a row whose card
        // fills most of its cells has less than half a cell of gutter to give.
        // Moving the card the whole way would carry its ink out of the cells the
        // layout reserved for it — into its neighbour's, and on the first row
        // straight off the top of the image — and this module's whole
        // integration is that *"every card here is drawn into exactly the cells
        // `card_frame_for` gave that row"*. So the card travels as far onto the
        // connector's row as its own gutter allows and no further.
        //
        // The residual is bounded by half a cell by construction, and in
        // practice much less: at the captain's 10x21 it is 0.75 px.
        // `a_cards_ink_is_centred_on_the_row_its_branch_line_meets_it_on`
        // measures both halves — how close the line lands, and that the card
        // never leaves its cells to get there.
        let gutter = (cell_height - wanted).max(0.0);
        let connector_offset =
            connector_row_offset_px(frame.height, self.cell_h).clamp(-gutter / 2.0, gutter / 2.0);
        // The left border stands where the tree's rails have their ink, not
        // where the card's first cell begins. See [`RAIL_INK_COLUMN_FRACTION`].
        let left =
            (f32::from(frame.x.saturating_sub(rect.x)) + RAIL_INK_COLUMN_FRACTION) * self.cell_w;
        PlacedCard {
            rect: RoundRect {
                x: left,
                y: cell_top + (cell_height - wanted) / 2.0 + connector_offset,
                // The right edge does not move: nothing in the tree is drawn
                // against it, so pulling the left one in is what aligns the card
                // rather than sliding the whole box off the columns the layout
                // gave it.
                w: (f32::from(frame.width) - RAIL_INK_COLUMN_FRACTION) * self.cell_w,
                h: wanted,
                r: geometry.radius,
            },
            content,
            geometry,
            crew: self.crew_bands,
        }
    }

    /// The cells one card's own image covers, from the same [`card_image_rect`]
    /// the dissolve field is built out of.
    fn card_rect(&self, frame: Rect) -> Option<Rect> {
        card_image_rect(
            frame,
            (self.cell_w, self.cell_h),
            self.bounds,
            self.bloom_floor,
        )
    }

    /// Where an image at `rect` sits inside the dissolve field, in pixels.
    fn dissolve_origin(&self, rect: Rect) -> (u32, u32) {
        (
            u32::from(rect.x.saturating_sub(self.field.x)) * self.cell_size.width_px,
            u32::from(rect.y.saturating_sub(self.field.y)) * self.cell_size.height_px,
        )
    }

    /// The dissolve field's own size, in pixels.
    fn field_px(&self) -> (u32, u32) {
        (
            u32::from(self.field.width) * self.cell_size.width_px,
            u32::from(self.field.height) * self.cell_size.height_px,
        )
    }

    /// The facts every image's signature starts from.
    ///
    /// # Why the rect's position is deliberately not one of them
    ///
    /// An image is drawn entirely in its own coordinates: [`Self::place`] puts a
    /// card at `frame - rect`, and [`Self::rasterise`] sizes the canvas from
    /// `rect.width`/`rect.height`. So two images whose rects differ only by a
    /// translation are the same pixels, and hashing where the rect *is* would
    /// declare them different and redraw one to arrive back at the other.
    ///
    /// That is exactly what a reflow does to every card below an arriving row,
    /// on the one frame the layout changes — so a signature that moved with the
    /// panel would make a slide cost a full tree rasterisation at the moment it
    /// begins. Blind to position, it costs a clone.
    ///
    /// Position is not lost: a *transition* frame folds it back in through
    /// [`Self::dissolve_origin`], because there a card's pixels genuinely do
    /// depend on where in the field it sits.
    fn hash_common(&self, hasher: &mut DefaultHasher, rect: Rect) {
        self.cell_size.width_px.hash(hasher);
        self.cell_size.height_px.hash(hasher);
        rect.width.hash(hasher);
        rect.height.hash(hasher);
        self.backdrop.0.hash(hasher);
        self.backdrop.1.hash(hasher);
        self.backdrop.2.hash(hasher);
        // The tree's joins are drawn in it, so a theme change that moves it has
        // to redraw the sheet exactly as one that moves the backdrop does.
        self.rail.hash(hasher);
    }
}

/// What one card's image will be, and where it goes, decided before any pixel
/// is drawn.
struct PlannedShape {
    /// The cell rect the image is drawn for.
    rect: Rect,
    /// Where it is placed, relative to the clip box — the image rect's own
    /// position plus whatever motion offset this frame calls for.
    viewport: (i32, i32),
    signature: u64,
    content_signature: u64,
}

/// Where one planned card's image comes from, decided before any of them are
/// drawn.
///
/// The split exists so the decision and the drawing are two passes: the decision
/// carries state from card to card and has to stay in order, the drawing carries
/// nothing and does not. See [`Rasteriser::match_held`].
enum ShapeSource {
    /// Slot in the previous frame's layers whose image this card takes as-is.
    Held(usize),
    /// This card is drawn. Carries a held *undissolved* canvas for the same
    /// content when one exists, which is what makes a transition frame cost a
    /// mask instead of a rasterisation.
    Draw(Option<UndissolvedSheet>),
}

/// The most threads a card rebuild may take, whatever the machine has.
///
/// Six, against a measured ceiling of 4.36× at sixteen threads on a 12-core box:
/// six of those threads already reached 3.77×, so the cap gives up about an
/// eighth of the available speedup. What it buys is that a Herdr running a fleet
/// — every agent on the box is a child of this process — never has a sidebar
/// repaint take the whole machine. The work being bounded matters more than the
/// last 0.3 ms of it: twelve cards is a few milliseconds once, on the rare frame
/// where the tree's content changed, not a steady load.
const CARD_RASTER_MAX_THREADS: usize = 6;

/// How many threads to draw `work` cards on.
///
/// Bounded three ways, and the tightest wins: never more threads than cards to
/// draw, never more than [`CARD_RASTER_MAX_THREADS`], and never more than half
/// the machine's parallelism — the other half belongs to the fleet this process
/// is hosting. One card, or a machine reporting fewer than four ways of
/// parallelism, draws on the calling thread with no scope at all.
fn raster_threads(work: usize) -> usize {
    #[cfg(test)]
    {
        let forced = test_thread_override();
        if forced > 0 {
            return work.min(forced).max(1);
        }
    }
    if work < 2 {
        return 1;
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    work.min(CARD_RASTER_MAX_THREADS).min((cores / 2).max(1))
}

/// [`raster_threads`] for `herdr bench cards`, which has to say in its report
/// how many threads the CPU column was drawn on: the GPU pass is serial and
/// races that pool rather than one core, so the thread count is half of what
/// any speedup number means.
pub(crate) fn raster_threads_for_bench(cards: usize) -> usize {
    raster_threads(cards)
}

/// Pin the thread count for a test that needs to compare two of them.
///
/// Zero means "use the real bound". Test-only: there is no runtime knob for
/// this, because the bound is a property of the machine and not a preference.
#[cfg(test)]
fn test_thread_override() -> usize {
    RASTER_THREADS_FOR_TEST.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
static RASTER_THREADS_FOR_TEST: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Cards this process has actually rasterised, as opposed to carried forward.
///
/// The positive control for `herdr bench combined`, and the same kind of thing
/// `crate::gpu::bloom::TILES_COMPOSED` is for the compute pass: a churn run's
/// whole claim is that rows really are arriving and leaving, and the only
/// evidence for it that the frame times themselves cannot supply is how many
/// cards the matcher declined to carry. A run that rasterised one card per
/// frame was measuring a settled panel whatever its churn rate said.
///
/// One relaxed add per `shapes` call, not per card, so nothing in the pass
/// scales with it. Never reset — a reader takes a difference across the window
/// it cares about.
pub(crate) static CARDS_RASTERISED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// One card's frame and content, fed into a signature.
///
/// The frame is hashed *relative to the image it is drawn into*, for the reason
/// [`Rasteriser::hash_common`] gives: what the pixels depend on is where the
/// card sits inside its own image, never where that image sits on the panel.
/// Signed, because the panel clamp can put an image's origin past its card's.
fn hash_placed(hasher: &mut DefaultHasher, frame: &Rect, rect: &Rect, content: &CardContent) {
    (i32::from(frame.x) - i32::from(rect.x)).hash(hasher);
    (i32::from(frame.y) - i32::from(rect.y)).hash(hasher);
    frame.width.hash(hasher);
    frame.height.hash(hasher);
    content.hash_into(hasher);
}

/// The placement a finished image is published as.
fn card_layer(
    format: crate::api::schema::PaneGraphicsFormat,
    width_px: u32,
    height_px: u32,
    data: Vec<u8>,
    sheet_rect: Rect,
) -> crate::app::state::GraphicsLayer {
    crate::app::state::GraphicsLayer::new(
        format,
        width_px,
        height_px,
        data,
        crate::api::schema::PaneGraphicsPlacementParams {
            viewport_col: 0,
            viewport_row: 0,
            grid_cols: u32::from(sheet_rect.width),
            grid_rows: u32::from(sheet_rect.height),
            // Over the text, so the tree's connectors and its Space rows keep
            // showing through wherever the image is transparent while the
            // character card under each pixel card is covered.
            //
            // One band for every card rather than a stack. Two placements at the
            // same `z` composite — measured on a real Kitty, and exactly, in
            // linear light — so overlapping cards blend their glows instead of
            // one winning, and no card needs to be told where it sits in a
            // stacking order to look right beside its neighbours.
            z: 0,
        },
    )
}

/// Lift the selected card, without recolouring it.
fn lift(sheet: &mut Canvas, card: &PlacedCard<'_>) {
    let opacity = card.content.generate.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let rect = card.rect;
    for y in rect.y.max(0.0) as u32..((rect.y + rect.h).ceil() as u32).min(sheet.height()) {
        for x in rect.x.max(0.0) as u32..((rect.x + rect.w).ceil() as u32).min(sheet.width()) {
            let inside = coverage(rect.distance(x as f32 + 0.5, y as f32 + 0.5));
            if inside > 0.0 {
                sheet.blend(x, y, measured::STROKE_A, 0.07 * inside * opacity);
            }
        }
    }
}

/// The view transition, resolved onto the sheet's own pixels.
///
/// # Why the sheet has to carry the transition at all
///
/// `super::render_tree_view_transition` takes the view apart cell by cell, and
/// on a terminal with no Kitty graphics that is the whole effect. The card sheet
/// is *opaque over every cell a card occupies*, so on a terminal that does draw
/// it the character dissolve is happening entirely underneath a picture that is
/// standing still: what is left visible is the connectors and the Space rows
/// around the cards, which is a thin border of the panel dissolving around a
/// block of cards that hard-cut at the commit instant.
///
/// # Why a particle is a square of pixels and not a cell
///
/// A cell dissolve on the character path has exactly one resolution available
/// to it — the cell — because a letter cannot be half drawn. The sheet has no
/// such limit: it is an image, and the only thing deciding how fine its
/// dissolve is is how large a block of pixels shares one draw. `particle_px` is
/// the edge of that block, so the particle *count* goes as its inverse square:
/// at a 10x21 px host cell, a 21 px particle is one particle per cell, and
/// halving the edge is four times the particles.
///
/// # Why it reads the engine rather than rolling its own scatter
///
/// The particle grid is handed to [`crate::anim::ElementFrame::cell`] as if it
/// were a grid of cells, so the configured behaviour — `dissolve` out of the
/// box, but `collapse`, `wipe` and the rest all work — decides the order and the
/// front exactly as it does for the characters. The pixels and the characters
/// are then the same effect at two resolutions rather than two effects that
/// have to be kept looking alike by hand.
#[derive(Debug, Clone, Copy)]
struct DissolveFrame<'a> {
    /// How present the view is, in `0.0..=1.0`.
    ///
    /// Taken straight off [`crate::anim::ElementFrame::progress`], which already
    /// counts *down* through a dismount — leaving is arriving played backwards,
    /// and the engine reverses it once so every consumer agrees which way the
    /// effect runs. Nothing here reverses it a second time.
    progress: f32,
    /// Particle edge, in sheet pixels. Never zero.
    particle_px: u32,
    /// The behaviour resolving the front, borrowed from the engine's catalogue
    /// so the pixels play exactly the behaviour the characters do.
    behaviour: &'a crate::anim::behaviour::Behaviour,
}

/// Steps of progress the sheet is rebuilt at.
///
/// A re-encode is the expensive half of this effect, so the sheet is quantized
/// to a fixed ladder rather than to whatever the render loop happened to tick
/// at: 24 steps across a half-transition is finer than the 50 ms animation
/// interval can deliver at any duration under about 1.2 s, so in practice the
/// loop's own frame rate is the binding constraint and this only stops a
/// faster loop from paying more.
const DISSOLVE_STEPS: f32 = 24.0;

impl DissolveFrame<'_> {
    /// The quantized step this frame sits on, for the sheet's signature.
    fn step(self) -> (u16, u32) {
        (
            (self.progress.clamp(0.0, 1.0) * DISSOLVE_STEPS).round() as u16,
            self.particle_px,
        )
    }

    /// Particle columns and rows over a sheet of this size.
    fn grid(self, width_px: u32, height_px: u32) -> (u16, u16) {
        let edge = self.particle_px.max(1);
        let cols = width_px.div_ceil(edge).clamp(1, u32::from(u16::MAX));
        let rows = height_px.div_ceil(edge).clamp(1, u32::from(u16::MAX));
        (cols as u16, rows as u16)
    }

    /// Take one image apart at this frame of the transition.
    ///
    /// `origin` is where the image sits inside `field`, both in pixels. The
    /// particle grid is laid over the **field** and not over the image, which is
    /// what lets a tree cut into one image per card come apart as a single wave
    /// crossing the panel: every card reads the same grid at its own offset
    /// rather than running its own dissolve at its own scale. The sheet passes
    /// `(0, 0)` and its own size, and reduces to the whole-canvas walk this was.
    fn apply(self, canvas: &mut Canvas, origin: (u32, u32), field: (u32, u32)) {
        use crate::anim::cell::{CellExtent, CellPos};

        let (cols, rows) = self.grid(field.0, field.1);
        let extent = CellExtent::new(cols, rows);
        let progress = self.progress.clamp(0.0, 1.0);
        let edge = self.particle_px.max(1);
        // Only the particles this image actually overlaps. A card's image is a
        // small window onto the field, so walking the whole grid per card would
        // make the dissolve cost the tree's area once for every card in it.
        let first_col = (origin.0 / edge).min(u32::from(cols));
        let first_row = (origin.1 / edge).min(u32::from(rows));
        let last_col = (origin.0 + canvas.width())
            .div_ceil(edge)
            .min(u32::from(cols));
        let last_row = (origin.1 + canvas.height())
            .div_ceil(edge)
            .min(u32::from(rows));
        for row in first_row..last_row {
            for col in first_col..last_col {
                let present =
                    self.behaviour
                        .strength(CellPos::new(col as u16, row as u16), extent, progress);
                if present >= 1.0 {
                    continue;
                }
                // Field pixels back to this image's own. A particle straddling
                // the image's edge is clamped to it rather than dropped, so the
                // wave does not develop a seam at every card boundary.
                let x0 = (col * edge).saturating_sub(origin.0);
                let y0 = (row * edge).saturating_sub(origin.1);
                let x1 = ((col + 1) * edge).saturating_sub(origin.0);
                let y1 = ((row + 1) * edge).saturating_sub(origin.1);
                canvas.scale_alpha(x0, y0, x1, y1, present);
            }
        }
    }
}

/// The transition frame the sheet should be drawn at, or `None` when the panel
/// is settled or the effect is configured off.
///
/// `particle_px` is read off the config in *cells* of the host's own cell so a
/// setting means the same thing on a 10x21 px cell and a 7x15 px one: the
/// captain's knob is "how many particles per cell", and the pixel edge is
/// derived from the cell he is actually looking at.
fn sheet_dissolve(app: &AppState, cell_size: HostCellSize) -> Option<DissolveFrame<'_>> {
    let per_cell = app.sidebar_animation.view_switch_particles();
    if per_cell == 0 {
        return None;
    }
    let frame = app
        .anim
        .frame(&crate::app::tree_view::view_element(), None)?;
    // A settled view is not a transition, and the view's lifecycle is
    // deliberately still, so there is no idle behaviour here to mistake for one.
    if !matches!(
        frame.phase,
        crate::anim::Phase::Mount | crate::anim::Phase::Dismount
    ) {
        return None;
    }
    let behaviour = frame.behaviour?;
    // Particles are square, so the edge that puts `per_cell` of them in one
    // cell is the cell's own area divided by the count, rooted. Rounded to the
    // nearest whole pixel rather than up, because the count goes as the edge
    // *squared*: on a 10x21 px cell an edge rounded up from 3.2 to 4 delivers
    // 13 particles per cell against the 20 asked for, and rounding to 3
    // delivers 23. Floored at one pixel, because a particle finer than a pixel
    // is not a finer dissolve — it is the same dissolve costing more to draw.
    let cell_area = (cell_size.width_px * cell_size.height_px).max(1) as f32;
    let particle_px = (cell_area / f32::from(per_cell)).sqrt().round().max(1.0) as u32;
    Some(DissolveFrame {
        progress: frame.progress,
        particle_px,
        behaviour,
    })
}

/// Whether every pixel is fully opaque — the gate `preferred_sidebar_pixel_format`
/// needs before it will hand a canvas to an alpha-losing raw format. Herdr's
/// cards are translucent by design (gutters, glow falloff, rounded corners),
/// so this is expected to come back `false` for most real card sheets; a
/// single-image API payload or a full-bleed background wash are the cases
/// it exists for.
fn canvas_is_fully_opaque(sheet: &Canvas) -> bool {
    sheet.rgba8().chunks_exact(4).all(|pixel| pixel[3] == 255)
}

/// Encodes a finished canvas in whichever format the host terminal is fast
/// at (`preferred_sidebar_pixel_format`), falling back to PNG for anything else.
fn encode_canvas(
    sheet: &Canvas,
    format: crate::api::schema::PaneGraphicsFormat,
) -> Option<Vec<u8>> {
    crate::kitty_graphics::encode_layer_pixels(format, sheet.width(), sheet.height(), sheet.rgba8())
}

#[cfg(test)]
mod pixel_format_tests {
    use super::*;

    fn opaque_canvas(width: u32, height: u32) -> Canvas {
        let mut canvas = Canvas::new(width, height);
        for y in 0..height {
            for x in 0..width {
                canvas.blend(x, y, Rgb(10, 20, 30), 1.0);
            }
        }
        canvas
    }

    #[test]
    fn a_fresh_canvas_is_fully_transparent_not_opaque() {
        // `Canvas::new` zero-fills, which is alpha 0 everywhere — the gutter
        // state every real card sheet starts from.
        let canvas = Canvas::new(4, 4);
        assert!(!canvas_is_fully_opaque(&canvas));
    }

    #[test]
    fn a_canvas_blended_at_full_alpha_everywhere_is_opaque() {
        let canvas = opaque_canvas(4, 4);
        assert!(canvas_is_fully_opaque(&canvas));
    }

    #[test]
    fn a_single_untouched_pixel_makes_the_whole_canvas_non_opaque() {
        // Blending translucent paint over an already-opaque pixel keeps it
        // opaque (source-over composites alpha, it does not overwrite it),
        // so the only way to get a non-opaque pixel here is to leave one
        // unpainted — exactly what a real sheet's gutter looks like.
        let mut canvas = Canvas::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                if (x, y) != (2, 2) {
                    canvas.blend(x, y, Rgb(10, 20, 30), 1.0);
                }
            }
        }
        assert!(!canvas_is_fully_opaque(&canvas));
    }

    #[test]
    fn encode_canvas_rgba_is_the_canvas_bytes_verbatim() {
        let canvas = opaque_canvas(2, 2);
        let encoded = encode_canvas(&canvas, crate::api::schema::PaneGraphicsFormat::Rgba)
            .expect("rgba encode");
        assert_eq!(encoded, canvas.rgba8());
    }

    #[test]
    fn encode_canvas_rgb_strips_alpha_from_the_canvas_bytes() {
        let canvas = opaque_canvas(2, 2);
        let encoded = encode_canvas(&canvas, crate::api::schema::PaneGraphicsFormat::Rgb)
            .expect("rgb encode");
        let expected: Vec<u8> = canvas
            .rgba8()
            .chunks_exact(4)
            .flat_map(|pixel| pixel[..3].to_vec())
            .collect();
        assert_eq!(encoded, expected);
        assert_eq!(encoded.len(), canvas.rgba8().len() / 4 * 3);
    }

    #[test]
    fn encode_canvas_png_round_trips_through_the_png_decoder() {
        let canvas = opaque_canvas(3, 3);
        let encoded = encode_canvas(&canvas, crate::api::schema::PaneGraphicsFormat::Png)
            .expect("png encode");
        let decoder = png::Decoder::new(encoded.as_slice());
        let mut reader = decoder.read_info().expect("png header");
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("png frame");
        assert_eq!(&buf[..info.buffer_size()], canvas.rgba8());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    /// The single-sheet call shape these tests were written against.
    ///
    /// They exercise the sheet, which publishes exactly one layer, and they are
    /// the characterization of everything the two drawing models share —
    /// geometry, tiers, wrapping, the fit ladder, the transition. Rewriting each
    /// one to unwrap a list would put list handling in front of what they are
    /// actually asserting without testing anything new, so the shim keeps them
    /// reading the way they did. The shapes path has its own tests, which assert
    /// the things only it can be wrong about.
    pub(super) fn build_sheet(
        app: &AppState,
        cards: &[crate::app::state::WorkspaceCardArea],
        sidebar_area: Rect,
        cell_size: HostCellSize,
        previous: Option<&SidebarCardLayer>,
    ) -> SheetUpdate {
        assert!(
            !app.sidebar_card_shapes,
            "the single-sheet shim was handed an app on the shapes path"
        );
        match build_cards(
            app,
            cards,
            sidebar_area,
            cell_size,
            previous.map(std::slice::from_ref).unwrap_or_default(),
        )
        .update
        {
            CardsUpdate::Unchanged => SheetUpdate::Unchanged,
            CardsUpdate::Rebuilt(layers) => match layers.into_iter().next() {
                Some(layer) => SheetUpdate::Rebuilt(layer),
                None => SheetUpdate::Empty,
            },
            // No test drives this shim with the delegated flag set: the sheet
            // is server artwork by definition, and a client that draws for
            // itself always draws shapes.
            CardsUpdate::Empty | CardsUpdate::Delegated => SheetUpdate::Empty,
        }
    }

    /// [`CardsUpdate`] with the list collapsed to the sheet's one layer.
    pub(super) enum SheetUpdate {
        Unchanged,
        Rebuilt(SidebarCardLayer),
        Empty,
    }

    /// A fleet of ten agents at three depths, with the real `doing` strings the
    /// fit ladder was measured against — including the one that says
    /// "Investigateing", because a layout that only survives clean input has
    /// not been tested.
    pub(super) struct FleetRow {
        pub(super) name: &'static str,
        owner: Option<&'static str>,
        pub(super) doing: &'static str,
        pub(super) state: AgentState,
        pub(super) project: &'static str,
        pub(super) context: &'static str,
    }

    const fn row(
        name: &'static str,
        owner: Option<&'static str>,
        doing: &'static str,
        state: AgentState,
        project: &'static str,
        context: &'static str,
    ) -> FleetRow {
        FleetRow {
            name,
            owner,
            doing,
            state,
            project,
            context,
        }
    }

    pub(super) const FLEET: &[FleetRow] = &[
        row(
            "2ndmate-herdr",
            Some("firstmate"),
            "Herdr workspace manager second mate bootstrap",
            AgentState::Idle,
            "herdr",
            "41%",
        ),
        row(
            "herdr-card-image-card",
            Some("2ndmate-herdr"),
            "Making sidebar accept image placements",
            AgentState::Working,
            "herdr",
            "6%",
        ),
        row(
            "herdr-card-iteration-2",
            Some("2ndmate-herdr"),
            "Refactor work cards with improved chip icons and typography",
            AgentState::Working,
            "herdr",
            "6%",
        ),
        row(
            "2ndmate-homeauto",
            Some("firstmate"),
            "Home automation v1 secondmate deployment",
            AgentState::Blocked,
            "homeauto",
            "11%",
        ),
        row(
            "homeauto-audit",
            Some("2ndmate-homeauto"),
            "Re-survey host findings and verify open audit decisions",
            AgentState::Blocked,
            "homeauto",
            "11%",
        ),
        row(
            "homeauto-backup",
            Some("2ndmate-homeauto"),
            "Implement automated backup of Home Assistant critical state",
            AgentState::Idle,
            "homeauto",
            "9%",
        ),
        row(
            "2ndmate-budget",
            Some("firstmate"),
            "Establish home_budget_app secondmate operations",
            AgentState::Idle,
            "budget",
            "18%",
        ),
        row(
            "budget-guard",
            Some("2ndmate-budget"),
            "Add main branch guard to sync commit hooks",
            AgentState::Idle,
            "budget",
            "7%",
        ),
        row(
            "budget-anchor",
            Some("2ndmate-budget"),
            "Validate FM_HOME anchor fix and ship PR",
            AgentState::Idle,
            "budget",
            "27%",
        ),
        row(
            "firstmate",
            None,
            "Investigateing killed Okta corpus and Herdr work sessions",
            AgentState::Idle,
            "firstmate",
            "12%",
        ),
    ];

    fn fleet_app() -> AppState {
        let mut app = AppState::test_new();
        let mut space = Workspace::test_new("fleet");
        let mut panes = vec![*space.tabs[0]
            .panes
            .keys()
            .next()
            .expect("a workspace starts with one pane")];
        for _ in 1..FLEET.len() {
            panes.push(space.test_split(ratatui::layout::Direction::Vertical));
        }
        app.workspaces = vec![space];
        app.ensure_test_terminals();
        app.active = Some(0);
        let now = std::time::Instant::now();
        app.state_age_now = now;

        for (pane, row) in panes.iter().zip(FLEET) {
            let terminal_id = app.workspaces[0].tabs[0].panes[pane]
                .attached_terminal_id
                .clone();
            let Some(terminal) = app.terminals.get_mut(&terminal_id) else {
                continue;
            };
            terminal.set_agent_name(row.name.to_string());
            terminal.state = row.state;
            let mut tokens = std::collections::HashMap::from([
                ("doing".to_string(), Some(row.doing.to_string())),
                ("project".to_string(), Some(row.project.to_string())),
                ("context".to_string(), Some(row.context.to_string())),
            ]);
            if let Some(owner) = row.owner {
                tokens.insert("owner".to_string(), Some(owner.to_string()));
            }
            terminal.metadata_tokens.patch(tokens, None, now);
            terminal.last_agent_state_change_at = Some(now - std::time::Duration::from_secs(31));
        }
        app
    }

    /// The same fleet with the pixel path live: kitty graphics on and a host
    /// cell size reported. Whether it is actually live also depends on this
    /// machine having a proportional face, which [`is_available`] decides.
    pub(super) fn pixel_fleet_app() -> AppState {
        let mut app = fleet_app();
        app.kitty_graphics_enabled = true;
        // The host answered the capability probe, which is the other half of
        // `AppState::host_paints_pixel_surfaces` and so the other half of
        // `is_available`. Without it this fixture is a host that draws
        // character cards, and every pixel assertion below it passes vacuously.
        app.kitty_graphics_capability_confirmed = true;
        app.host_cell_size = HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        app
    }

    /// A first mate owning a second mate owning a worker, with the pixel path
    /// live — the same shape as `super::super::tests::owned_fleet_sidebar_rows`,
    /// but the mates are real `Workspace`s so `entry.rank()` actually resolves
    /// to `SecondMate` rather than every non-first-mate row collapsing to
    /// `Worker` the way it does when a fleet is one workspace of agent panes.
    /// [`fleet_app`] cannot produce a `SecondMate` at all: `AgentRelation::rank`
    /// only reads `Worker` off an `Agent` row, whatever its depth, so a second
    /// mate has to be its own Space.
    pub(super) fn three_rank_pixel_app() -> AppState {
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let worker_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("firstmate"), second_mate];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        app.workspaces[1].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            now,
        );

        let worker_terminal = app.workspaces[1].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.terminals.get_mut(&worker_terminal).unwrap();
        terminal.set_agent_name("worker".to_string());
        terminal.state = AgentState::Idle;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([(
                "owner".to_string(),
                Some("2ndmate-explore".to_string()),
            )]),
            None,
            now,
        );

        app.kitty_graphics_enabled = true;
        app.kitty_graphics_capability_confirmed = true;
        app.host_cell_size = HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        app
    }

    pub(super) fn sidebar_rect() -> Rect {
        Rect::new(0, 0, 42, 46)
    }

    /// The rows are the tree, and they must tile the panel: no row may overlap
    /// its neighbour and no card may reach a cell outside its own row.
    ///
    /// This *is* the hit-testing argument. Clicking resolves a row from a `y`
    /// through `AppState::view.workspace_card_areas`, so as long as the rows
    /// still tile and each card stays inside the row that owns it, the pixel
    /// path cannot make a click land anywhere the character path would not have
    /// put it — which is why nothing about the click path had to change.
    fn assert_rows_tile(cards: &[crate::app::state::WorkspaceCardArea]) {
        let mut previous_bottom: Option<u16> = None;
        for card in cards {
            assert!(card.rect.height > 0, "a row with no cells is not a row");
            if let Some(bottom) = previous_bottom {
                assert!(
                    card.rect.y >= bottom,
                    "row at {} overlaps the row ending at {bottom}",
                    card.rect.y
                );
            }
            if let Some(frame) = card.card_frame {
                assert!(frame.y >= card.rect.y, "a card started above its row");
                assert!(
                    frame.y + frame.height <= card.rect.y + card.rect.height,
                    "a card reached past the bottom of its row"
                );
                assert!(frame.x >= card.rect.x, "a card started left of its row");
                assert!(
                    frame.x + frame.width <= card.rect.x + card.rect.width,
                    "a card reached past the right of its row"
                );
            }
            previous_bottom = Some(card.rect.y + card.rect.height);
        }
    }

    /// The pixel path changes how tall a row is and nothing else about it.
    ///
    /// Same rows, same order, same click targets — only the heights move. A
    /// pixel card that reordered or dropped a row would take the click target
    /// with it, and this is the test that would say so.
    #[test]
    fn the_pixel_path_changes_row_heights_and_nothing_else_about_a_row() {
        // One fleet, drawn twice. Two fleets would have two sets of pane ids
        // and the comparison would pass by accident or fail by accident.
        let mut app = fleet_app();
        let character_cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());

        app.kitty_graphics_enabled = true;
        app.kitty_graphics_capability_confirmed = true;
        app.host_cell_size = HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        if !is_available(&app, super::super::row_fold_width(&app, sidebar_rect())) {
            return; // No proportional face on this machine; the fallback is the point.
        }
        let pixel_cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        assert!(!pixel_cards.is_empty(), "the fixture drew no rows");
        assert_rows_tile(&character_cards);
        assert_rows_tile(&pixel_cards);

        for (character, pixel) in character_cards.iter().zip(&pixel_cards) {
            assert_eq!(character.entry_idx, pixel.entry_idx, "the rows reordered");
            assert_eq!(character.ws_idx, pixel.ws_idx);
            assert_eq!(
                character.agent.map(|agent| agent.pane_id),
                pixel.agent.map(|agent| agent.pane_id),
                "a row changed which pane clicking it selects"
            );
        }
    }

    /// Below the width the card shell itself gives up at, the pixel path gives
    /// up with it: the panel is back on the bare styled line, and no sheet is
    /// built to draw over it.
    #[test]
    fn a_panel_too_narrow_for_a_card_gets_no_sheet() {
        let app = pixel_fleet_app();
        let narrow = Rect::new(0, 0, MIN_FOLD_WIDTH, 46);
        assert!(
            !is_available(&app, super::super::row_fold_width(&app, narrow)),
            "a panel narrower than the card shell still tried to draw pixels"
        );
        let cards = super::super::compute_workspace_card_areas(&app, narrow);
        assert!(matches!(
            build_sheet(&app, &cards, narrow, app.host_cell_size, None),
            SheetUpdate::Empty
        ));
    }

    /// Kitty graphics off — the direct-attach case included, since that is
    /// already folded into the one flag — and the panel is exactly what it was.
    #[test]
    fn graphics_off_is_the_character_path_and_no_sheet() {
        let mut app = pixel_fleet_app();
        app.kitty_graphics_enabled = false;
        assert!(!is_available(
            &app,
            super::super::row_fold_width(&app, sidebar_rect())
        ));
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        assert!(matches!(
            build_sheet(&app, &cards, sidebar_rect(), app.host_cell_size, None),
            SheetUpdate::Empty
        ));
        assert_eq!(
            cards.len(),
            super::super::compute_workspace_card_areas(&fleet_app(), sidebar_rect()).len()
        );
    }

    /// A frame that changed nothing re-encodes nothing.
    ///
    /// This is the redraw budget: a real fleet's cards change about once every
    /// ninety seconds, and rasterising ten cards and deflating the sheet sixty
    /// times a second to draw the same picture would be most of the cost of the
    /// feature.
    #[test]
    fn an_unchanged_tree_rebuilds_no_sheet() {
        let app = pixel_fleet_app();
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let SheetUpdate::Rebuilt(first) =
            build_sheet(&app, &cards, sidebar_rect(), app.host_cell_size, None)
        else {
            return; // No face on this machine.
        };
        assert!(!first.layer.data.is_empty());
        assert!(matches!(
            build_sheet(
                &app,
                &cards,
                sidebar_rect(),
                app.host_cell_size,
                Some(&first)
            ),
            SheetUpdate::Unchanged
        ));

        // And a card that *did* change is rebuilt, or the cache would be a
        // freeze rather than a cache.
        let mut moved = app;
        moved.state_age_now += std::time::Duration::from_secs(600);
        let SheetUpdate::Rebuilt(second) = build_sheet(
            &moved,
            &cards,
            sidebar_rect(),
            moved.host_cell_size,
            Some(&first),
        ) else {
            panic!("a tree whose ages moved did not rebuild");
        };
        assert_ne!(first.signature, second.signature);
    }

    /// The sheet is a picture of the cells the tree drew, so it has to land on
    /// exactly those cells: the placement is a whole number of columns and rows
    /// inside the panel, and the image is a whole number of cells of pixels.
    #[test]
    fn the_sheet_covers_whole_cells_inside_the_panel() {
        let app = pixel_fleet_app();
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let SheetUpdate::Rebuilt(sheet) =
            build_sheet(&app, &cards, sidebar_rect(), app.host_cell_size, None)
        else {
            return; // No face on this machine.
        };
        let content = super::super::sidebar_content_rect(sidebar_rect());
        assert!(sheet.rect.x >= content.x);
        assert!(sheet.rect.y >= content.y);
        assert!(sheet.rect.x + sheet.rect.width <= content.x + content.width);
        assert!(sheet.rect.y + sheet.rect.height <= content.y + content.height);
        assert_eq!(
            sheet.layer.image_width,
            u32::from(sheet.rect.width) * app.host_cell_size.width_px
        );
        assert_eq!(
            sheet.layer.image_height,
            u32::from(sheet.rect.height) * app.host_cell_size.height_px
        );
        assert_eq!(sheet.layer.render.grid_cols, u32::from(sheet.rect.width));
        assert_eq!(sheet.layer.render.grid_rows, u32::from(sheet.rect.height));
        // Over the text. The sheet is transparent everywhere it is not a card,
        // so the tree's connectors keep showing through it.
        assert_eq!(sheet.layer.render.z, 0);
        assert_eq!(
            sheet.layer.format,
            crate::api::schema::PaneGraphicsFormat::Png
        );
    }

    /// Every card the tree drew is in the sheet, and every card in the sheet is
    /// one the tree drew. A sheet that skipped a row would leave one character
    /// card standing in a column of pixel cards.
    #[test]
    fn every_row_with_a_card_frame_is_drawn() {
        let app = pixel_fleet_app();
        if !is_available(&app, super::super::row_fold_width(&app, sidebar_rect())) {
            return;
        }
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let entries = super::super::workspace_list_entries(&app);
        let agents = super::super::sidebar_agent_entries(&app);
        let framed = cards
            .iter()
            .filter(|card| card.card_frame.is_some())
            .count();
        let drawable = cards
            .iter()
            .filter(|card| card.card_frame.is_some())
            .filter_map(|card| entries.get(card.entry_idx))
            .filter(|entry| {
                content_for(
                    &app,
                    entry,
                    &agents,
                    &crate::ui::sidebar::body_register::BodyRegister::resolve(&app),
                )
                .is_some()
            })
            .count();
        assert!(framed > 0, "the fixture drew no framed rows");
        assert_eq!(framed, drawable, "a framed row had no card content");
    }

    /// The whole path, not just the builder: `compute_view` is what actually
    /// puts the sheet in `AppState`, and it is also what a background frame
    /// must not take back out again.
    #[test]
    fn compute_view_gives_the_panel_its_cards_and_a_sizeless_pass_does_not_take_them_away() {
        let mut app = pixel_fleet_app();
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 100, 46);
        app.sidebar_width = 42;
        let cell_size = app.host_cell_size;

        crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
        if !is_available(
            &app,
            super::super::row_fold_width(&app, app.view.sidebar_rect),
        ) {
            return; // No proportional face on this machine.
        }
        let sheet = app
            .sidebar_card_layers
            .first()
            .expect("compute_view drew the tree's cards");
        let signature = sheet.signature;
        assert!(sheet.rect.width > 0 && sheet.rect.height > 0);

        // A pass with no cell size — a virtual client, or a background frame —
        // leaves the foreground client's sheet alone. Clearing it here would
        // cost the next real frame a re-encode and a re-upload.
        crate::ui::compute_view_without_resizing_panes(&mut app, &runtimes, area);
        assert_eq!(
            app.sidebar_card_layers.first().map(|sheet| sheet.signature),
            Some(signature),
            "a pass that cannot see pixels threw away the sheet"
        );

        // Graphics off puts the panel back on characters and takes the sheet
        // with it, so nothing is left on the host to delete later.
        app.kitty_graphics_enabled = false;
        crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
        assert!(app.sidebar_card_layers.is_empty());
    }

    /// The sheet and the notification tray are two graphics placements on one
    /// plane, so the sheet must not reach into the tray.
    ///
    /// Both publish at `z: 0`, and the tray's badges are its own layer over its
    /// rows. The tree's rows already stop at the tray's top edge, but the sheet
    /// spans the cards *plus their bloom*, and that bloom used to be clamped to
    /// the whole panel — which put the last card's glow on the tray's top row
    /// of badges with no defined order between the two. Neither change could
    /// see this alone: the tray reserves rows the cards never asked about.
    #[test]
    fn the_card_sheet_stops_at_the_notification_tray() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        // Sized so the tray lands *inside* the reach of the last card's bloom,
        // which is the only arrangement in which the clamp is under test at
        // all — a tree that stops short of the tray would pass this whatever
        // the clamp did. The height is a fixture detail and the assertions
        // below no longer depend on it: they compare the clamped sheet against
        // the unclamped one, and refuse to pass if the clamp never engaged.
        let area = Rect::new(0, 0, 100, 42);

        let sheet_for = |tray_on: bool| {
            let mut app = pixel_fleet_app();
            app.sidebar_width = 42;
            app.sidebar_signal_tray.enabled = tray_on;
            let cell_size = app.host_cell_size;
            crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
            if !is_available(
                &app,
                super::super::row_fold_width(&app, app.view.sidebar_rect),
            ) {
                return None; // No proportional face on this machine.
            }
            let sheet = app
                .sidebar_card_layers
                .first()
                .expect("the tree drew no cards")
                .rect;
            let content = super::super::sidebar_content_rect(app.view.sidebar_rect);
            let last_card = app
                .view
                .workspace_card_areas
                .iter()
                .filter_map(|card| card.card_frame)
                .map(|frame| frame.y + frame.height)
                .max()
                .expect("the tree drew no card frames");
            Some((
                sheet,
                super::super::tray::tray_rect(&app, content),
                last_card,
            ))
        };

        let Some((sheet, tray, _last_card)) = sheet_for(true) else {
            return;
        };
        assert!(tray.height > 0, "the fixture drew no tray");
        assert!(
            sheet.y + sheet.height <= tray.y,
            "the card sheet reached {} rows into the tray",
            (sheet.y + sheet.height).saturating_sub(tray.y)
        );

        // The tray is the only thing that *can* move the floor, and with no bloom
        // to reach past its last card the sheet no longer asks it to.
        //
        // This half of the test used to assert the clamp engaging: a card's
        // bloom ran 26–28 px past its own stroke, so the sheet wanted rows past
        // its last row and the tray took them back. There is no bloom now
        // ([`CARD_BLOOM`] — F1 refuses it and a glass card has nothing to lift
        // off the panel), so the sheet is exactly its rows and the clamp is a
        // no-op. Stated rather than deleted, because the clamp itself must stay:
        // it is what stands between the tray's own placements and any future
        // surface that does reach past a card, and a silently unreachable clamp
        // is one nobody notices has stopped working.
        let Some((sheet_off, tray_off, last_card_off)) = sheet_for(false) else {
            return;
        };
        assert_eq!(tray_off, Rect::default(), "the tray drew while disabled");
        // One row of slack, and only one: the field is a *pixel* rect rounded out
        // to whole cells, so a card whose last row does not end on a cell
        // boundary costs the sheet the cell it lands in. Anything more than that
        // is something painting outside a card again.
        assert!(
            sheet_off.y + sheet_off.height <= last_card_off + 1,
            "the sheet reached {} rows past its own last card, so something is \
             painting outside a card again",
            (sheet_off.y + sheet_off.height).saturating_sub(last_card_off)
        );
        // Deliberately nothing comparing the two sheets' floors to each other:
        // the tray does not only clamp, it takes rows off the *tree*, so the two
        // runs do not draw the same cards and their last rows are different
        // rows. What each has to hold, it holds above.
    }

    fn metrics(line_height: f32) -> FontMetrics {
        FontMetrics {
            ascent: line_height * 0.8,
            line_height,
        }
    }

    /// The dissolve field contains every card it dissolves.
    ///
    /// Both come out of [`card_image_rect`], so containment is by construction —
    /// and it is asserted because losing it fails silently:
    /// `Rasteriser::dissolve_origin` saturates and `DissolveFrame::apply` clamps
    /// to the field's grid, so the part of a card outside the field keeps full
    /// alpha for the whole transition while its neighbours fade. The fixture is a
    /// face tall enough to push the top tier past [`BASE_HEIGHT_PX`], which is
    /// the case a margin read off that constant got wrong.
    #[test]
    fn the_dissolve_field_contains_every_card_it_dissolves() {
        let title = metrics(40.0);
        let tidbit = metrics(30.0);
        assert!(
            card_height_px(title, tidbit) > BASE_HEIGHT_PX,
            "the fixture's face is not tall enough to test anything"
        );
        let cell = (10.0f32, 21.0f32);
        let bounds = Rect::new(0, 0, 40, 60);
        let bloom_floor = bounds.y + bounds.height;
        // Three ranks, narrowing from the left the way `rank_width_inset` does.
        let cards = [
            Rect::new(1, 2, 38, 8),
            Rect::new(3, 10, 36, 6),
            Rect::new(5, 16, 34, 5),
        ];

        let field = dissolve_field_rect(&cards, cell, bounds, bloom_floor)
            .expect("a tree of three cards has a field");
        for frame in cards {
            let rect = card_image_rect(frame, cell, bounds, bloom_floor)
                .expect("a card with a frame has an image");
            assert_eq!(
                field.union(rect),
                field,
                "the card at {frame:?} reaches outside the field it is \
                 dissolved against"
            );
        }
    }

    /// The image path and the character path are chosen at the *same* width, so
    /// there is never a panel drawing pixel cards over rows laid out as bare
    /// lines.
    #[test]
    fn the_image_card_shares_the_character_card_s_width_threshold() {
        assert_eq!(MIN_FOLD_WIDTH, super::super::card::MIN_FOLD_WIDTH);
    }

    /// Card height does not read `depth` at all: the captain's decision of
    /// 2026-08-06 retired the per-rank height ladder in favour of width alone,
    /// so a first mate, a second mate and a worker's card all want the same
    /// height on the same face.
    #[test]
    fn card_height_is_the_same_at_every_depth() {
        let title = metrics(TITLE_PX);
        let tidbit = metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);
        let height = card_height_px(title, tidbit);
        for depth in 0..5u8 {
            // No depth parameter exists to pass; this asserts the one height
            // function every depth resolves to is a single number, not a table.
            let _ = depth;
            assert_eq!(card_height_px(title, tidbit), height);
        }
        // The real face's numbers at 14 px land exactly on the settled base:
        // two 14 px lines, three caption lines and the captain's own air, which
        // is why that is the number. See [`BASE_HEIGHT_PX`]'s own table.
        assert_eq!(height, BASE_HEIGHT_PX);
        assert!(
            (height - (content_block_px(title, tidbit) + CARD_AIR_PER_SIDE_PX * 2.0)).abs() < 0.05,
            "the base has drifted off the block it is supposed to be the block plus air of"
        );
    }

    /// [`BASE_HEIGHT_PX`] is a floor and not a ceiling: on a face whose line
    /// height runs large, two lines of 14 px type and a tidbit want more room
    /// than the base gives, and the card grows rather than clipping its words.
    #[test]
    fn card_height_grows_past_the_base_when_the_content_needs_it() {
        let title = metrics(40.0);
        let tidbit = metrics(30.0);
        let block = content_block_px(title, tidbit);
        let height = card_height_px(title, tidbit);
        assert!(
            height >= block + MIN_VERTICAL_PAD_PX * 2.0,
            "the fixture's block ({block}px) does not fit the card ({height}px)"
        );
        assert!(
            height > BASE_HEIGHT_PX,
            "a tall face did not push the card past the base"
        );
    }

    /// The air a card had per side before the captain's 2026-08-09 trim.
    ///
    /// Kept as a number so the tests below can state the trim against what it
    /// actually replaced, rather than restating [`CARD_AIR_PER_SIDE_PX`] and
    /// proving only that a constant equals itself.
    ///
    /// Stated as *air* and not as a total height, because the total is the one
    /// thing that legitimately moves: the block the card sets grew by two
    /// caption lines when the row started saying what its body is, and a ratio
    /// of two totals would have called that a trim being undone.
    const PRE_TRIM_AIR_PER_SIDE_PX: f32 = 11.45;

    /// The height the card was drawn at before the trim, for the fixtures that
    /// want a "how big it used to be" number rather than a contract.
    const PRE_TRIM_HEIGHT_PX: f32 = 68.0;

    /// The card really is a fifth shorter, on every face and at every rank.
    ///
    /// A fifth of the *card*, not of some faces' cards and none of the others':
    /// `card_height_px` is `max(base, content)`, so a trim that dropped under
    /// the content floor would be silently undone on exactly the faces whose
    /// type runs large and the constant would stop describing the screen.
    #[test]
    fn the_trim_is_air_and_not_the_floor() {
        let faces = font::all_available_faces();
        assert!(
            !faces.is_empty() || font::card_font(None).is_none(),
            "a machine with a card face must expose it to this test"
        );
        for (face, font) in faces {
            let title = font.metrics(TITLE_PX);
            let tidbit = font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);
            let floor = content_floor_px(title, tidbit);
            assert!(
                floor < BASE_HEIGHT_PX,
                "the content floor ({floor:.2}px) has caught up with the trimmed base \
                 ({BASE_HEIGHT_PX}px) in {face}: the card is no longer the size this \
                 constant says it is"
            );
            let drawn = card_height_px(title, tidbit);
            assert_eq!(
                drawn, BASE_HEIGHT_PX,
                "the trim did not reach the drawn card in {face}"
            );
            // The captain's trim, stated where it actually landed: the air.
            // A card is `block + 2 * air`, so the air is what is left after the
            // type — and it is his 4.65 a side, not the 11.45 the card had
            // before him, whatever the block has since grown to.
            let air = (drawn - content_block_px(title, tidbit)) / 2.0;
            assert!(
                (air - CARD_AIR_PER_SIDE_PX).abs() < 0.05,
                "the card keeps {air:.2}px of air a side in {face}, not the \
                 {CARD_AIR_PER_SIDE_PX}px the trim left it"
            );
            assert!(
                air < PRE_TRIM_AIR_PER_SIDE_PX * 0.5,
                "the trim came back in {face}: {air:.2}px a side against the \
                 {PRE_TRIM_AIR_PER_SIDE_PX}px it replaced"
            );
        }
    }

    /// The trim is uniform, so the rank ladder it sits inside is untouched.
    ///
    /// Height carries no rank — that was the 2026-08-06 decision — so "every
    /// tier proportionally smaller" means every card takes the same fifth and
    /// the width steps between ranks come out the far side identical. This
    /// measures the second half of that: `rank_width_inset` is what a rank is
    /// worth on screen, and the trim must not have moved it.
    #[test]
    fn the_trim_left_the_rank_ladder_exactly_where_it_was() {
        use crate::app::agent_tree::AgentRelation;
        for fold in [MIN_FOLD_WIDTH, 34, 40, 48, 64] {
            let insets: Vec<u16> = [
                AgentRelation::FirstMate,
                AgentRelation::SecondMate,
                AgentRelation::Worker,
            ]
            .into_iter()
            .map(|rank| super::super::rank_width_inset(rank, fold))
            .collect();
            assert_eq!(
                insets[0], 0,
                "the top rank stopped being the full-width one at fold {fold}"
            );
            assert!(
                insets[1] <= insets[2],
                "the rank ladder is not monotonic at fold {fold}: {insets:?}"
            );
        }
    }

    /// The trim cost the title nothing — it paid it.
    ///
    /// This is the captain's *"truncates titles"* stated as an assertion. A
    /// title's room is lines × column width; the line count did not move, and
    /// the column is measured in from the nominal height by [`measured::PAD`]
    /// and [`measured::PAD_RIGHT`], so a shorter card is a *wider* text column.
    /// `CardGeometry::new` floors the nominal at the cell height, so passing
    /// the old 68 px as a cell height reconstructs the pre-trim chrome exactly
    /// and the two can be measured against each other.
    #[test]
    fn the_trim_did_not_cost_the_title_a_single_pixel() {
        let before = CardGeometry::new(PRE_TRIM_HEIGHT_PX, false);
        let after = CardGeometry::new(21.0, false);
        assert!(
            after.text_inset() < before.text_inset(),
            "the trimmed card starts its ink no further left"
        );
        let gained =
            (before.text_inset() - after.text_inset()) + (before.pad_right - after.pad_right);
        assert!(
            gained > 4.0,
            "the trim gave the title only {gained:.2}px back; it was supposed to be \
             about six"
        );
        // And that is real width in the column the title is actually set in,
        // chip and all, at the captain's panel.
        for (face, font) in font::all_available_faces() {
            for depth in 0..3u8 {
                for title in REAL_FLEET_TITLES {
                    let now =
                        real_text_column(&font, 42, 9.0, depth, title, widest_rail()).available();
                    let then = pre_trim_text_column(&font, 42, 9.0, depth, title).available();
                    assert!(
                        now >= then,
                        "the trimmed card gives the title {now:.1}px where the old one \
                         gave {then:.1}px, in {face} at depth {depth}"
                    );
                }
            }
        }
    }

    /// [`real_text_column`] as it would have measured before the trim.
    fn pre_trim_text_column(
        font: &CardFont,
        sidebar_width: u16,
        cell_w: f32,
        depth: u8,
        title: &str,
    ) -> TextColumn {
        let prefix = if depth == 0 {
            1
        } else {
            3 * u16::from(depth) + 1
        };
        let frame_cells = sidebar_width.saturating_sub(1).saturating_sub(prefix);
        // The cell-height floor in `nominal_height_px` is what makes the old
        // nominal reachable without a second copy of `CardGeometry`.
        let geometry = CardGeometry::new(PRE_TRIM_HEIGHT_PX, false);
        text_column(
            font,
            &geometry,
            f32::from(frame_cells) * cell_w,
            PRE_TRIM_HEIGHT_PX,
            title,
            widest_rail(),
        )
    }

    /// A row is fewer cells tall than it was, which is the trim the captain can
    /// actually see.
    ///
    /// The pixel height is the design; the *footprint* is `ceil(height / cell)`
    /// floored at the character card's chrome, and that is what closes the gap
    /// between two agents in the tree. Swept over the cell heights a real
    /// terminal reports rather than asserted at one, because the ceiling means
    /// the trim lands as a whole row at some of them and as a wider gutter at
    /// the rest — and both are correct, but only the first is what the captain
    /// asked to see.
    #[test]
    fn a_row_is_shorter_in_cells_at_the_cell_heights_a_terminal_reports() {
        let mut shrank = 0;
        for cell_h in 14..=28u16 {
            let cells = |height: f32| {
                ((height / f32::from(cell_h)).ceil() as u16)
                    .max(super::super::card::CHROME_ROWS + 1)
            };
            // The same card with the air it had before the captain's trim
            // against the air it has now. Stated this way rather than against a
            // fixed pre-trim *height* because the block between the air has
            // since grown a caption line — see [`CAPTION_LINES`] — and a
            // comparison of two totals would score that growth as the trim
            // being undone.
            let pre_trim =
                BASE_HEIGHT_PX - CARD_AIR_PER_SIDE_PX * 2.0 + PRE_TRIM_AIR_PER_SIDE_PX * 2.0;
            let before = cells(pre_trim);
            let after = cells(BASE_HEIGHT_PX);
            assert!(
                after <= before,
                "a {cell_h}px cell makes the trimmed card *taller*: {after} rows against \
                 {before}"
            );
            shrank += usize::from(after < before);
        }
        assert!(
            shrank >= 6,
            "the trim only bought a row back at {shrank} of the fifteen cell heights \
             swept; it is not reaching the layout"
        );
        // The captain's own terminal, which is the one this was asked for. Four
        // cells rather than the three it was: the card grew a caption line when
        // the row started saying what its body is, and 64.5 px does not fit in
        // three 21 px cells. The trim is still in it — the same card with the
        // pre-trim air needs five.
        let his = 21.0f32;
        assert_eq!(
            (BASE_HEIGHT_PX / his).ceil() as u16,
            4,
            "the card no longer fits four of his cells"
        );
        let pre_trim = BASE_HEIGHT_PX - CARD_AIR_PER_SIDE_PX * 2.0 + PRE_TRIM_AIR_PER_SIDE_PX * 2.0;
        assert_eq!(
            (pre_trim / his).ceil() as u16,
            4,
            "the pre-trim air no longer costs a row at his own cell"
        );
        assert_eq!(
            (PRE_TRIM_HEIGHT_PX / his).ceil() as u16,
            4,
            "the fixture cell height no longer reproduces the four-row card"
        );
    }

    /// Never truncate, never shrink: the wrap fills whole words and stops. No
    /// ellipsis is ever appended, at any width, including one narrower than a
    /// single word.
    #[test]
    fn wrapping_never_shortens_a_word_and_never_appends_an_ellipsis() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        let title = "Refactor work cards with improved chip icons and typography";
        for avail in [40.0, 90.0, 160.0, 300.0, 1000.0] {
            let lines = wrap_ragged(font, title, TITLE_PX, (avail, avail), TITLE_LINES);
            assert!(lines.len() <= TITLE_LINES);
            for line in &lines {
                assert!(!line.contains('…'), "{line} was elided");
                assert!(!line.ends_with(' '));
                for word in line.split(' ') {
                    assert!(title.contains(word), "{word} is not a word from the title");
                }
            }
        }
    }

    /// The captain's own fleet, verbatim, in the column the renderer actually
    /// gives a title — at every tier and at the panel widths he runs.
    ///
    /// This is the test the shipped card did not have. The old fit ladder
    /// measured `wrap` against invented widths and passed; the renderer derived
    /// its own width from the plate, the chip and the pad, and on a real panel
    /// that width was a third of what had been measured. Nothing compared the
    /// two, so `Investigateing killed Okta corpus and Herdr work sessions` came
    /// out as `Investi`.
    ///
    /// The strings are read off `herdr api snapshot` rather than invented,
    /// because the previous verification used short made-up names and the
    /// truncation simply never appeared.
    const REAL_FLEET_TITLES: &[&str] = &[
        "Investigateing killed Okta corpus and Herdr work sessions",
        "Fixing card rendering and truncation issues in herdr",
        "Establish home_budget_app secondmate operations",
        "Herdr workspace manager second mate bootstrap",
        "Adding main branch guard to sync commit hooks",
        "Validating FM_HOME anchor fix and ship PR",
    ];

    /// The heaviest control rail a card can carry: a two-glyph count and a
    /// chevron.
    ///
    /// The fit ladder measures against this and not against an empty rail, so
    /// the "titles are never truncated" guarantee is asserted on the cards that
    /// have the *least* room rather than on the ones that have the most.
    fn widest_rail() -> ControlRail {
        ControlRail {
            summary: Some(SummaryBadge {
                count: 10,
                fresh: true,
            }),
            group: Some(GroupChevron::Collapsed),
            // Not the Space badge: `every_real_fleet_title_is_set_whole_in_every_face_at_every_width`
            // already fails with the badge folded in here — these two controls
            // alone leave no spare width for at least one real title at one
            // real narrow sidebar/depth, so any further reservation drops a
            // word. That is a pre-existing budget the badge did not create;
            // see `a_space_badge_reserves_real_width_and_the_title_wraps_around_it`
            // for the badge's own width coverage instead of widening this
            // fixture into a failure the fitting ladder cannot yet absorb.
            space_badge: None,
        }
    }

    /// The column a card at `depth` gives its title, on a `sidebar_width`
    /// panel whose cells are `cell_w` wide. The same arithmetic `card_frame_for`
    /// and `build_sheet_inner` do, so a test measuring this is measuring the
    /// card that gets drawn.
    fn real_text_column(
        font: &CardFont,
        sidebar_width: u16,
        cell_w: f32,
        depth: u8,
        title: &str,
        rail: ControlRail,
    ) -> TextColumn {
        // The card starts after the tree's own prefix, exactly as
        // `card_frame_for` places it, on the scrollbar-narrow width
        // `row_fold_width` measures against.
        let prefix = if depth == 0 {
            1
        } else {
            3 * u16::from(depth) + 1
        };
        let frame_cells = sidebar_width.saturating_sub(1).saturating_sub(prefix);
        let geometry = CardGeometry::new(16.0, false);
        let height = card_height_px(
            font.metrics(TITLE_PX),
            font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
        );
        text_column(
            font,
            &geometry,
            f32::from(frame_cells) * cell_w,
            height,
            title,
            rail,
        )
    }

    /// The panel and cell sizes worth sweeping, at every tier.
    ///
    /// Boundaries rather than a dense grid, because these run against every face
    /// on the machine and the cost is real. 34 is the narrowest panel a card is
    /// drawn on at all, 42 is the captain's, 60 is a wide one. 5 px is the
    /// narrowest cell `HostCellSize::is_plausible` lets through, 8 px is the
    /// fallback and the floor the whole-title guarantee is claimed at, 9 px is
    /// what his terminal answers `CSI 16 t` with, and 24 px is a HiDPI cell.
    /// 6 px sits below the guarantee on purpose: it is what exercises the
    /// graceful-degradation path rather than the guarantee.
    fn card_widths() -> impl Iterator<Item = (u16, f32, u8)> {
        [34u16, 38, 42, 60].into_iter().flat_map(|sidebar_width| {
            [5.0f32, 6.0, 8.0, 9.0, 12.0, 24.0]
                .into_iter()
                .flat_map(move |cell_w| (0..3u8).map(move |depth| (sidebar_width, cell_w, depth)))
        })
    }

    /// A line is never wider than its column while a break was available.
    ///
    /// This is the invariant that actually broke. `wrap` never shortens a word,
    /// but a line wider than its column is *drawn and clipped* by `draw_text`,
    /// and that clip is what turned `Investigateing killed …` into `Investi`.
    /// The one line allowed to overrun is a single word wider than the whole
    /// column — there is nothing to break on, and shortening it is the elision
    /// the captain ruled out. Everything else has to fit.
    #[test]
    fn a_real_title_never_overruns_a_column_it_could_have_broken_in() {
        for (face, font) in font::all_available_faces() {
            for (sidebar_width, cell_w, depth) in card_widths() {
                for title in REAL_FLEET_TITLES {
                    // Both rails: a card carrying controls sets its first line
                    // in a narrower column than its second, and that line is
                    // clipped at its own edge — so it is the one most able to
                    // overrun.
                    for rail in [ControlRail::default(), widest_rail()] {
                        let column =
                            real_text_column(&font, sidebar_width, cell_w, depth, title, rail);
                        let widths = column.title_widths();
                        for (index, line) in
                            wrap_ragged(&font, title, TITLE_PX, widths, TITLE_LINES)
                                .into_iter()
                                .enumerate()
                        {
                            let avail = if index == 0 { widths.0 } else { widths.1 };
                            if font.width(&line, TITLE_PX) <= avail + 0.5 {
                                continue;
                            }
                            assert!(
                                !line.contains(' '),
                                "{line:?} overruns its {avail:.1}px column with a break \
                                 available, in {face} at sidebar {sidebar_width}, cell \
                                 {cell_w}, depth {depth}, rail {rail:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The cell floor the whole-title guarantee below is claimed at.
    ///
    /// 8 px is [`crate::kitty_graphics::HostCellSize::FALLBACK`]'s width — the
    /// narrowest cell Herdr will ever *assume*, and therefore the narrowest one
    /// a card is laid out in unless a terminal genuinely reports something
    /// smaller. Below it a 57-character title does not physically fit two 14 px
    /// lines on a 34-column panel in any face, and the card's answer there is to
    /// set the words it can rather than to shrink the type.
    const GUARANTEED_CELL_WIDTH_PX: f32 = 8.0;

    /// Every real title, set whole, in every face on this machine, at every
    /// panel width a card is drawn on and every cell at or above the fallback.
    ///
    /// The version of this that shipped red asserted the same thing about
    /// `card_font(None)` — whichever face the machine happened to pick first.
    /// A developer box with the Ubuntu fonts installed picks `UbuntuSans` and
    /// passes; CI has `DejaVuSans`, which sets the longest title 16% wider, and
    /// fails. Measuring every face is the fix, and it is why the guarantee below
    /// is stated against a cell width rather than against a panel width: with
    /// the chip yielding, the panel width stopped being the binding constraint.
    ///
    /// # The one accepted exception
    ///
    /// Retiring the tier scale (`BASE_HEIGHT_PX`) means every rank now measures
    /// its title column against the same chrome — the chrome no longer shrinks
    /// with depth, because depth no longer carries a size signal at all. At the
    /// narrowest guaranteed floor (34-column panel, 8 px cell) a depth-2 card
    /// used to get *smaller* padding than the top tier, which bought it more
    /// text width than it has now; the longest fixture title no longer sets
    /// whole there. The captain's call on 2026-08-06 (`leave it for now`,
    /// `data/decisions/2026-08-06-uniform-card-height-chrome-floor.md`): accept
    /// this one combination as a known edge case rather than shrinking base
    /// chrome proportions globally to buy it back.
    const ACCEPTED_NARROW_FLOOR_TRUNCATION: (u16, f32, u8) = (34, GUARANTEED_CELL_WIDTH_PX, 2);

    /// # The second accepted exception: a card carrying both controls
    ///
    /// A card with a worker-summary badge *and* a group chevron reserves them
    /// on its first title line — the same line, and only the line, the character
    /// row reserves them on. That width has to come from somewhere, and on the
    /// 34-column panel there is nothing spare: the longest fixture titles lose
    /// their last word there in every face.
    ///
    /// Taken deliberately, on the captain's own precedent for this exact panel
    /// width — accept the narrow floor rather than shrink chrome globally to buy
    /// it back — and on the direction of the trade. A missing word of a title
    /// the fleet chose is a card that reads short; a missing chevron is a card
    /// that does not say rows are folded under it while the cell that unfolds
    /// them stays clickable, which is the defect the rail exists to fix. The
    /// guarantee is unchanged and still asserted for every card that carries no
    /// controls, which is most of them.
    ///
    /// 38 columns and wider is clean in every face, with or without the rail —
    /// `the_captains_own_geometry_carries_every_real_title_with_its_chip` pins
    /// that at the geometry he actually runs.
    const RAILED_TITLES_TRUNCATE_BELOW: u16 = 38;

    #[test]
    fn every_real_fleet_title_is_set_whole_in_every_face_at_every_width() {
        let faces = font::all_available_faces();
        assert!(
            !faces.is_empty() || font::card_font(None).is_none(),
            "a machine with a card face must expose it to this test"
        );
        for (face, font) in faces {
            for (sidebar_width, cell_w, depth) in card_widths() {
                if cell_w < GUARANTEED_CELL_WIDTH_PX {
                    continue;
                }
                if (sidebar_width, cell_w, depth) == ACCEPTED_NARROW_FLOOR_TRUNCATION {
                    continue;
                }
                for (title, rail) in REAL_FLEET_TITLES.iter().flat_map(|title| {
                    [ControlRail::default(), widest_rail()]
                        .into_iter()
                        .map(move |rail| (*title, rail))
                }) {
                    if !rail.is_empty() && sidebar_width < RAILED_TITLES_TRUNCATE_BELOW {
                        continue;
                    }
                    let column = real_text_column(&font, sidebar_width, cell_w, depth, title, rail);
                    let widths = column.title_widths();
                    let lines = wrap_ragged(&font, title, TITLE_PX, widths, TITLE_LINES);
                    let set = lines.join(" ");
                    assert_eq!(
                        set.split_whitespace().collect::<Vec<_>>(),
                        title.split_whitespace().collect::<Vec<_>>(),
                        "dropped words in {face} at sidebar {sidebar_width}, cell {cell_w}, \
                         depth {depth}, rail {rail:?}: {set:?} from {title:?}"
                    );
                    for (index, line) in lines.iter().enumerate() {
                        let avail = if index == 0 { widths.0 } else { widths.1 };
                        assert!(
                            font.width(line, TITLE_PX) <= avail + 0.5,
                            "{line:?} would be clipped in {face} at sidebar {sidebar_width}, \
                             cell {cell_w}, depth {depth}, rail {rail:?}"
                        );
                    }
                }
            }
        }
    }

    /// The rail is taken off the first line and no other.
    ///
    /// The card's right margin used to be shared with a state chip standing
    /// over every line; with the chip retired the rail is the only thing
    /// reserved there, and it stands in the card's top band — which is the
    /// first title line's band and no other's. So a card carrying both controls
    /// loses width on line one and none anywhere else, exactly as the character
    /// row reserves its badge and its chevron on its first content row only.
    #[test]
    fn the_control_rail_is_taken_off_the_first_line_only() {
        for (face, font) in font::all_available_faces() {
            for title in REAL_FLEET_TITLES {
                let bare = real_text_column(
                    &font,
                    FLEET_SIDEBAR_COLUMNS,
                    FLEET_CELL_WIDTH_PX,
                    0,
                    title,
                    ControlRail::default(),
                );
                let railed = real_text_column(
                    &font,
                    FLEET_SIDEBAR_COLUMNS,
                    FLEET_CELL_WIDTH_PX,
                    0,
                    title,
                    widest_rail(),
                );
                assert_eq!(
                    bare.available(),
                    railed.available(),
                    "the rail narrowed a line it does not stand over, in {face}"
                );
                assert!(
                    railed.first_line_available() < bare.first_line_available(),
                    "the rail cost the first line nothing in {face}"
                );
            }
        }
    }

    /// The Space badge reserves real width off the card's first title line,
    /// the same as the summary badge and the chevron — proven in isolation
    /// from them, since [`widest_rail`] deliberately leaves the badge out
    /// (see its own doc comment for why).
    #[test]
    fn a_space_badge_reserves_real_width_and_the_title_wraps_around_it() {
        for (face, font) in font::all_available_faces() {
            for title in REAL_FLEET_TITLES {
                let bare = real_text_column(
                    &font,
                    FLEET_SIDEBAR_COLUMNS,
                    FLEET_CELL_WIDTH_PX,
                    0,
                    title,
                    ControlRail::default(),
                );
                let badged = real_text_column(
                    &font,
                    FLEET_SIDEBAR_COLUMNS,
                    FLEET_CELL_WIDTH_PX,
                    0,
                    title,
                    ControlRail {
                        summary: None,
                        group: None,
                        space_badge: Some(SpaceBadgeMark::Healthy(24)),
                    },
                );
                assert_eq!(
                    bare.available(),
                    badged.available(),
                    "the badge narrowed a line it does not stand over, in {face}"
                );
                assert!(
                    badged.first_line_available() < bare.first_line_available(),
                    "the badge cost the first line nothing in {face}"
                );
            }
        }
    }

    /// An open defect always wins the one badge slot, whatever the fleet also
    /// published for [`REV_TOKEN`] — the mockup's own fixtures never show
    /// both a bug and a rev count on the same card.
    #[test]
    fn a_defect_wins_the_badge_slot_over_a_rev_count() {
        assert_eq!(
            space_badge(Some("24"), Some("S2"), LifecycleStage::Running),
            Some(SpaceBadgeMark::Warn)
        );
    }

    /// Herdr's own failure detection still shows the amber badge with no
    /// fleet publisher in the loop at all — the same "detection is the
    /// floor, publication is the ceiling" rule
    /// [`crate::quality_streak::defect_mark`] is built on.
    #[test]
    fn a_detected_failure_shows_the_badge_with_nothing_published() {
        assert_eq!(
            space_badge(None, None, LifecycleStage::Failed),
            Some(SpaceBadgeMark::Warn)
        );
    }

    /// The fleet's own `-` closes the defect even on a row detection reads as
    /// failed, so a rev count published alongside it reaches the badge.
    #[test]
    fn a_closed_defect_lets_the_rev_count_through_even_while_failed() {
        assert_eq!(
            space_badge(Some("7"), Some("-"), LifecycleStage::Failed),
            Some(SpaceBadgeMark::Healthy(7))
        );
    }

    /// No defect and a real [`REV_TOKEN`] is the green pill, trimmed of
    /// whatever whitespace a publisher's shell script left in it.
    #[test]
    fn a_rev_count_alone_is_the_healthy_badge() {
        assert_eq!(
            space_badge(Some("  24  "), None, LifecycleStage::Running),
            Some(SpaceBadgeMark::Healthy(24))
        );
    }

    /// Nothing published, no detected failure: no badge at all — the common
    /// case for every Space today, since no publisher sends [`REV_TOKEN`]
    /// yet.
    #[test]
    fn nothing_published_draws_no_badge() {
        assert_eq!(space_badge(None, None, LifecycleStage::Running), None);
    }

    /// A [`REV_TOKEN`] a publisher could not have meant as a count — anything
    /// that does not parse as a plain non-negative integer — draws no badge
    /// rather than a wrong one.
    #[test]
    fn an_unparseable_rev_token_draws_no_badge() {
        for bad in ["", "-1", "3.5", "twelve", "24 open PRs"] {
            assert_eq!(
                space_badge(Some(bad), None, LifecycleStage::Running),
                None,
                "{bad:?} should not have parsed as a rev count"
            );
        }
    }

    /// The badge pill actually paints in its own ink — green for a rev count,
    /// amber for an open defect — never the card's own chip hue, and a card
    /// with neither draws no badge-coloured pixel at all.
    #[test]
    fn a_space_badge_paints_its_own_ink() {
        let Some(font) = font::card_font(None) else {
            return; // No face on this machine.
        };
        let geometry = CardGeometry::new(21.0, false);
        let rect = RoundRect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 64.0,
            r: geometry.radius,
        };
        fn content(badge: Option<SpaceBadgeMark>) -> CardContent {
            CardContent {
                title: String::new(),
                tidbit: None,
                register: None,
                state_label: String::new(),
                state: AgentState::Idle,
                stage: LifecycleStage::Running,
                severity: Severity::Clear,
                hues: StageHues([196.0; 5]),
                ground: measured::CANVAS,
                theme: CardTheme::UNTHEMED,
                split_channels: false,
                seen: true,
                depth: 1,
                lifted: false,
                focused_space: false,
                mark: None,
                residue: 0,
                controls: ControlRail {
                    summary: None,
                    group: None,
                    space_badge: badge,
                },
                generate: 1.0,
                discharge: 0.0,
                spider: None,
                breath: 0.0,
                wash: None,
                crew: Vec::new(),
                bars: None,
            }
        }
        let paints_near = |badge: Option<SpaceBadgeMark>, target: Rgb| {
            let drawn = content(badge);
            let mut canvas = Canvas::new(240, 90);
            draw_card(
                &mut canvas,
                &PlacedCard {
                    rect,
                    content: &drawn,
                    geometry: CardGeometry::new(21.0, false),
                    crew: crew::CrewBands::default(),
                },
                font,
            );
            canvas.rgba8().chunks_exact(4).any(|c| {
                i32::from(c[0]).abs_diff(i32::from(target.0)) <= 24
                    && i32::from(c[1]).abs_diff(i32::from(target.1)) <= 24
                    && i32::from(c[2]).abs_diff(i32::from(target.2)) <= 24
                    && c[3] > 40
            })
        };
        assert!(
            paints_near(Some(SpaceBadgeMark::Healthy(24)), measured::BADGE_OK),
            "a healthy badge drew no pixel close to its own green"
        );
        assert!(
            paints_near(Some(SpaceBadgeMark::Warn), measured::BADGE_WARN),
            "a warn badge drew no pixel close to its own amber"
        );
        assert!(
            !paints_near(None, measured::BADGE_OK) && !paints_near(None, measured::BADGE_WARN),
            "a card with no badge drew badge-coloured pixels anyway"
        );
    }

    /// **Only the focused Space and an arriving card get the strong accent.**
    ///
    /// The captain's own read of `herdr-card-border-dot-final-match-20260822`'s
    /// screenshots: a plain `working` card, neither the focused Space nor mid
    /// its own arrival, has to draw the same thin border an idle one does —
    /// the accent is `focused_space || generate < 1.0` and nothing else,
    /// whatever `AgentState`/`severity` the card carries.
    #[test]
    fn only_the_focused_space_or_an_arriving_card_is_accented() {
        fn content(focused_space: bool, generate: f32) -> CardContent {
            CardContent {
                title: String::new(),
                tidbit: None,
                register: None,
                state_label: String::new(),
                state: AgentState::Working,
                stage: LifecycleStage::Running,
                severity: Severity::Critical,
                hues: StageHues([196.0; 5]),
                ground: measured::CANVAS,
                theme: CardTheme::UNTHEMED,
                split_channels: false,
                seen: true,
                depth: 1,
                lifted: false,
                focused_space,
                mark: None,
                residue: 0,
                controls: ControlRail::default(),
                generate,
                discharge: 0.0,
                spider: None,
                breath: 0.0,
                wash: None,
                crew: Vec::new(),
                bars: None,
            }
        }

        let ordinary = content(false, 1.0);
        let focused = content(true, 1.0);
        let arriving = content(false, 0.4);

        assert!(
            !ordinary.accented(),
            "a plain working card read as accented"
        );
        assert!(
            focused.accented(),
            "the focused Space did not read as accented"
        );
        assert!(
            arriving.accented(),
            "an arriving card did not read as accented"
        );

        let dim = ordinary.arrived_light();
        let bright_focused = focused.arrived_light();
        let bright_arriving = arriving.arrived_light();
        assert_eq!(
            bright_focused.ink, bright_arriving.ink,
            "the focused Space and an arriving card should draw the same strong accent"
        );
        assert_ne!(
            dim.ink, bright_focused.ink,
            "an unaccented working card drew the same ink as the focused Space"
        );
        assert!(
            dim.bloom < bright_focused.bloom,
            "an unaccented card's bloom ({:.3}) should be strictly less than an \
             accented one's ({:.3})",
            dim.bloom,
            bright_focused.bloom
        );
    }

    /// **The worker status dot exists, and only on a worker.**
    ///
    /// `image_card.rs` never drew the pixel twin of the character path's
    /// `state_icon` dots (PR #180) — the mockup's `.wk-dot`, a small solid
    /// circle at the tree's own full-strength cyan before a worker's name.
    /// Checked by colour rather than by position: an unaccented card at rest
    /// draws no other pixel anywhere near `measured::STROKE_A`'s own
    /// saturation — the border and face are both dimmed off it — so any pixel
    /// that close can only be the dot (or its glow).
    #[test]
    fn a_worker_card_draws_its_status_dot_and_a_space_does_not() {
        let Some(font) = font::card_font(None) else {
            return; // No face on this machine.
        };
        let geometry = CardGeometry::new(21.0, false);
        let rect = RoundRect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 64.0,
            r: geometry.radius,
        };
        fn content(register: Option<Caption>) -> CardContent {
            CardContent {
                title: "fm/verve-notes".into(),
                tidbit: None,
                register,
                state_label: "working".into(),
                state: AgentState::Working,
                stage: LifecycleStage::Running,
                severity: Severity::Clear,
                hues: StageHues([196.0; 5]),
                ground: measured::CANVAS,
                theme: CardTheme::UNTHEMED,
                split_channels: false,
                seen: true,
                depth: 1,
                lifted: false,
                focused_space: false,
                mark: None,
                residue: 0,
                controls: ControlRail::default(),
                generate: 1.0,
                discharge: 0.0,
                spider: None,
                breath: 0.0,
                wash: None,
                crew: Vec::new(),
                bars: None,
            }
        }

        let worker = content(Some(Caption {
            text: "working".into(),
            tone: CaptionTone::State,
        }));
        let space = content(Some(Caption {
            text: "streak 5 · T 13.4s".into(),
            tone: CaptionTone::Register,
        }));
        assert!(
            worker.is_worker(),
            "the fixture's own worker did not read as one"
        );
        assert!(
            !space.is_worker(),
            "the fixture's own Space read as a worker"
        );

        let close_to_cyan = |canvas: &Canvas| {
            canvas.rgba8().chunks_exact(4).any(|c| {
                c[3] > 40
                    && i32::from(c[0]).abs_diff(i32::from(measured::STROKE_A.0)) <= 24
                    && i32::from(c[1]).abs_diff(i32::from(measured::STROKE_A.1)) <= 24
                    && i32::from(c[2]).abs_diff(i32::from(measured::STROKE_A.2)) <= 24
            })
        };

        let mut worker_canvas = Canvas::new(220, 84);
        draw_card(
            &mut worker_canvas,
            &PlacedCard {
                rect,
                content: &worker,
                geometry: CardGeometry::new(21.0, false),
                crew: crew::CrewBands::default(),
            },
            font,
        );
        assert!(
            close_to_cyan(&worker_canvas),
            "a worker card drew no pixel near the tree's own cyan — the status dot is missing"
        );

        let mut space_canvas = Canvas::new(220, 84);
        draw_card(
            &mut space_canvas,
            &PlacedCard {
                rect,
                content: &space,
                geometry: CardGeometry::new(21.0, false),
                crew: crew::CrewBands::default(),
            },
            font,
        );
        assert!(
            !close_to_cyan(&space_canvas),
            "a Space card, which the mockup never gives a dot, drew one anyway"
        );
    }

    /// Summaries at the three lengths a fleet actually publishes.
    ///
    /// [`REAL_FLEET_TITLES`] are all one length — they were read off a snapshot
    /// on one evening — and every one of them already fits. So they test the
    /// wrap and test nothing at all about the *choosing*, which only happens
    /// when a summary does not fit. These are the other two ends: a label, and
    /// the sort of sentence an agent writes when it is describing a whole
    /// investigation rather than naming a task.
    const SUMMARY_LENGTHS: &[&str] = &[
        // Short — a label. Must reach the card exactly as published.
        "Building connector",
        "Just arrived",
        "Fixing the flake",
        // Medium — the common case, and the length the card was measured at.
        "Adding main branch guard to sync commit hooks",
        "Currently working on the sidebar card trim and the summary logic",
        "Fixing src/ui/sidebar/image_card.rs so the trim composes with the floor",
        // Long — where the choosing has to do the work.
        "Investigating why the sidebar card rasteriser drops oversized graphics \
         payloads on a 42-column sidebar at 1600x1000 and whether the cap is the \
         right one",
        "Trimmed every card by a fifth and rewrote the summary fitter. Now verifying \
         both against a real kitty under Xvfb before opening the pull request.",
        "Working on: validating the FM_HOME anchor fix (which turned out to be two \
         separate bugs) and shipping the pull request before the handoff window \
         closes",
    ];

    /// Every summary, at every length, on every face, at every width the card is
    /// drawn at — and not one line of it is ever clipped.
    ///
    /// The whole point of the fitter is that this holds for text far longer than
    /// the card, which is the case `every_real_fleet_title_is_set_whole_…` does
    /// not reach because every string in it fits. Here the assertion is not that
    /// nothing is given up — a 150-character summary in a 230 px column has to
    /// give something up — but that whatever survives is *drawn*, whole, inside
    /// its column.
    #[test]
    fn no_summary_at_any_length_overruns_the_column_it_is_set_in() {
        for (face, font) in font::all_available_faces() {
            for (sidebar_width, cell_w, depth) in card_widths() {
                for published in SUMMARY_LENGTHS {
                    let title = display_summary((*published).to_string());
                    // The widest rail, because a card carrying both controls is
                    // the narrowest first line a summary is ever set in.
                    let column = real_text_column(
                        &font,
                        sidebar_width,
                        cell_w,
                        depth,
                        &title,
                        widest_rail(),
                    );
                    let widths = column.title_widths();
                    let fitted = fit_title(&font, &title, widths);
                    assert!(
                        fitted.lines.len() <= TITLE_LINES,
                        "{:?} was set in {} lines in {face}",
                        fitted.lines,
                        fitted.lines.len()
                    );
                    for (index, line) in fitted.lines.iter().enumerate() {
                        let avail = if index == 0 { widths.0 } else { widths.1 };
                        if font.width(line, TITLE_PX) <= avail + 0.5 {
                            continue;
                        }
                        assert!(
                            !line.contains(' '),
                            "{line:?} overruns its {avail:.1}px column with a break \
                             available, in {face} at sidebar {sidebar_width}, cell \
                             {cell_w}, depth {depth}"
                        );
                    }
                }
            }
        }
    }

    /// The card omits the publisher's words; it never writes its own.
    ///
    /// This is the guardrail on the whole idea of Herdr choosing. Every word
    /// drawn has to be a word the fleet published, so the card can be trusted as
    /// a report rather than read as a paraphrase — and the order has to survive
    /// too, or a condensed summary could say something the agent did not.
    #[test]
    fn the_fitter_only_ever_omits_the_publishers_own_words() {
        for (face, font) in font::all_available_faces() {
            for (sidebar_width, cell_w, depth) in card_widths() {
                for published in SUMMARY_LENGTHS {
                    let title = display_summary((*published).to_string());
                    let column = real_text_column(
                        &font,
                        sidebar_width,
                        cell_w,
                        depth,
                        &title,
                        widest_rail(),
                    );
                    let drawn = fit_title(&font, &title, column.title_widths())
                        .lines
                        .join(" ");
                    let mut source = published.split_whitespace();
                    for word in drawn.split_whitespace() {
                        assert!(
                            source.any(|from| from.contains(word) || word.contains(from)),
                            "{drawn:?} is not the publisher's own words in order, from \
                             {published:?} in {face} at sidebar {sidebar_width}, cell \
                             {cell_w}, depth {depth}"
                        );
                    }
                }
            }
        }
    }

    /// A summary that fits is drawn exactly as it was published.
    ///
    /// The ladder is a response to not fitting, never a house style. A card with
    /// room for its title has to show that title and nothing else — otherwise
    /// the choosing is editing the fleet's copy for its own sake, which is not
    /// what was asked for.
    #[test]
    fn the_ladder_does_not_engage_on_a_summary_that_already_fits() {
        for (face, font) in font::all_available_faces() {
            for (sidebar_width, cell_w, depth) in card_widths() {
                if cell_w < GUARANTEED_CELL_WIDTH_PX {
                    continue;
                }
                for published in ["Building connector", "Just arrived", "Fixing the flake"] {
                    let column = real_text_column(
                        &font,
                        sidebar_width,
                        cell_w,
                        depth,
                        published,
                        widest_rail(),
                    );
                    let fitted = fit_title(&font, published, column.title_widths());
                    assert_eq!(
                        fitted.rung,
                        Some(0),
                        "{published:?} was condensed at rung {:?} despite fitting, in \
                         {face} at sidebar {sidebar_width}, cell {cell_w}, depth {depth}",
                        fitted.rung
                    );
                    assert_eq!(fitted.lines.join(" "), published);
                }
            }
        }
    }

    /// The words a summary is allowed to end on.
    ///
    /// A card that stops on "and", "the" or "with" has visibly been cut; a card
    /// that stops on a noun has been *edited*. This is the difference the
    /// captain is pointing at, and it is checkable.
    const DANGLING_TAILS: &[&str] = &[
        "and", "or", "the", "a", "an", "with", "for", "to", "of", "in", "on", "at", "from", "that",
        "which", "then", "but", "while", "before", "after", "because", "into", "by",
    ];

    /// A summary the card had to shorten still reads as a finished phrase.
    ///
    /// This is the regression the fitter exists for. The old path answered "too
    /// long" with a greedy wrap that stopped wherever the second line ran out —
    /// which, on the longest real fleet title, is the word *"and"*. The fitter
    /// answers it by choosing a shorter rendering that ends where the publisher
    /// put a boundary, so what is on screen is a phrase rather than a stump.
    ///
    /// Asserted only where the ladder actually fired: a card wide enough to set
    /// the whole thing has nothing to prove, and one so narrow that even the
    /// shortest rung overflows has fallen back to the wrap on purpose.
    #[test]
    fn a_shortened_summary_still_ends_where_a_phrase_ends() {
        let mut exercised = 0;
        for (face, font) in font::all_available_faces() {
            for (sidebar_width, cell_w, depth) in card_widths() {
                if cell_w < GUARANTEED_CELL_WIDTH_PX {
                    continue;
                }
                for published in SUMMARY_LENGTHS {
                    let title = display_summary((*published).to_string());
                    let column = real_text_column(
                        &font,
                        sidebar_width,
                        cell_w,
                        depth,
                        &title,
                        widest_rail(),
                    );
                    let fitted = fit_title(&font, &title, column.title_widths());
                    let Some(rung) = fitted.rung else {
                        continue; // Fell back to the wrap; nothing was chosen.
                    };
                    if rung == 0 {
                        continue; // Set whole as published.
                    }
                    exercised += 1;
                    let drawn = fitted.lines.join(" ");
                    let last = drawn
                        .split_whitespace()
                        .next_back()
                        .unwrap_or_default()
                        .trim_end_matches([',', ';', ':'])
                        .to_lowercase();
                    assert!(
                        !DANGLING_TAILS.contains(&last.as_str()),
                        "the card was shortened to {drawn:?}, which ends on {last:?} — a \
                         sentence sliced, not a phrase chosen. {face} at sidebar \
                         {sidebar_width}, cell {cell_w}, depth {depth}"
                    );
                }
            }
        }
        assert!(
            exercised > 0,
            "no width in the sweep made the card choose, so the choosing is untested"
        );
    }

    /// The longest title the fleet has actually published now reaches the card
    /// at the one width where it used not to.
    ///
    /// `ACCEPTED_NARROW_FLOOR_TRUNCATION` is the combination the captain waved
    /// through on 2026-08-06 — the raw wrap drops words there in the widest
    /// face. The trim widened the column and the fitter can shorten, so between
    /// them that hole is closed; this pins it shut so a later change to either
    /// has to reopen it deliberately.
    ///
    /// # Stated for a card with no control rail, and why that is the honest
    /// scope
    ///
    /// A rail takes width off the first line and, unlike the chip, does not
    /// yield — that is deliberate, and `text_column` says why. So on the very
    /// narrowest card *also* carrying a badge and a chevron, a title with
    /// nothing in it to condense can still overrun: "Establish home_budget_app
    /// secondmate operations" is one clause, one sentence, no path and no
    /// aside, so its ladder has exactly one rung and the fitter has nothing to
    /// offer. Claiming the hole is closed there would be claiming the fitter
    /// can shorten text that has no shorter rendering. The rail-free card is
    /// the one the 2026-08-06 exception was about, and it is closed.
    #[test]
    fn the_accepted_narrow_floor_truncation_is_no_longer_a_truncation() {
        let (sidebar_width, cell_w, depth) = ACCEPTED_NARROW_FLOOR_TRUNCATION;
        for (face, font) in font::all_available_faces() {
            for title in REAL_FLEET_TITLES {
                let column = real_text_column(
                    &font,
                    sidebar_width,
                    cell_w,
                    depth,
                    title,
                    ControlRail::default(),
                );
                let fitted = fit_title(&font, title, column.title_widths());
                assert!(
                    fitted.rung.is_some(),
                    "{title:?} still falls off the card in {face} at the narrow floor"
                );
            }
        }
    }

    /// The empty icon plate was taking this much of the title's column.
    ///
    /// Kept as a number rather than a description because "far larger than it
    /// should be" is what the captain could see and this is what it cost: the
    /// collapsed slot hands the title back more than the width of the state
    /// chip. There is one geometry now, not one per tier, so this is checked
    /// once rather than swept across depths.
    #[test]
    fn collapsing_the_empty_plate_gives_real_width_back_to_the_title() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        let height = card_height_px(
            font.metrics(TITLE_PX),
            font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
        );
        let width = 42.0 * 12.0;
        let with_plate = text_column(
            font,
            &CardGeometry::new(16.0, true),
            width,
            height,
            REAL_FLEET_TITLES[0],
            ControlRail::default(),
        );
        let without = text_column(
            font,
            &CardGeometry::new(16.0, false),
            width,
            height,
            REAL_FLEET_TITLES[0],
            ControlRail::default(),
        );
        assert!(
            without.available() > with_plate.available(),
            "collapsing the slot gained nothing"
        );
        // The gain is worth more than the chip.
        assert!(without.available() - with_plate.available() >= measured::PLATE_MAX_PX);
    }

    /// The panel and cell the captain's fleet actually runs on.
    ///
    /// 42 columns from his config, and a 9x18 cell read straight off his
    /// terminal's own answer to `CSI 16 t`. Nothing about the card is allowed to
    /// degrade here, in any face.
    const FLEET_SIDEBAR_COLUMNS: u16 = 42;
    const FLEET_CELL_WIDTH_PX: f32 = 9.0;

    /// Every real title still sets whole on the fleet's own geometry.
    ///
    /// This was the chip's own test — the chip was the first thing a card too
    /// narrow for its content stopped drawing, so it was the canary. The chip is
    /// retired and the state is a caption line now, which cannot be dropped for
    /// width; what is left worth pinning is the thing the chip was competing
    /// with, and it is the more important half.
    #[test]
    fn every_real_title_sets_whole_on_the_fleets_own_geometry() {
        for (face, font) in font::all_available_faces() {
            for depth in 0..3u8 {
                for title in REAL_FLEET_TITLES {
                    let column = real_text_column(
                        &font,
                        FLEET_SIDEBAR_COLUMNS,
                        FLEET_CELL_WIDTH_PX,
                        depth,
                        title,
                        widest_rail(),
                    );
                    assert!(
                        title_sets_whole(&font, title, column.title_widths()),
                        "{title:?} is not whole in {face} at the fleet's own geometry, \
                         depth {depth}"
                    );
                }
            }
        }
    }

    /// A title short enough for one line stays on one line rather than being
    /// broken to fill the reserved two.
    #[test]
    fn a_short_title_keeps_one_line() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        assert_eq!(
            wrap_ragged(font, "Ship PR", TITLE_PX, (400.0, 400.0), TITLE_LINES).len(),
            1
        );
    }

    /// With the split switched off the card is exactly what it was: the
    /// reference's one hue family, with the accent carried by saturation and
    /// bloom — but keyed on `accented` rather than on the card's own stage,
    /// since the border no longer speaks work state at all.
    ///
    /// This is the invariant the card shipped with, kept as the `stage_hue =
    /// false` contract rather than deleted, so the fallback really is the old
    /// look and not an untested branch nobody has drawn.
    #[test]
    fn without_the_split_a_card_stays_inside_the_measured_hue_family() {
        for accented in [false, true] {
            let light = CardLight::of(
                Severity::Critical,
                0.0,
                measured::CANVAS,
                false,
                accented,
                CardTheme::UNTHEMED,
            );
            assert!((0.0..=1.0).contains(&light.lum));
            assert!((0.0..=1.0).contains(&light.bloom));
            // Desaturating toward grey is allowed; rotating the hue is not, and
            // no severity may rotate it either — the whole channel is off.
            let Rgb(r, g, b) = light.ink;
            assert!(
                b >= r && g >= r,
                "accented={accented} moved the stroke out of the blue-cyan family: {r},{g},{b}"
            );
        }
    }

    /// **Channel independence, on the card's own ink.**
    ///
    /// Two claims, and they are the two halves of the split: changing the
    /// severity may not move the hue by so much as a degree, and changing the
    /// stage may not move what the severity is saying. The second is checked as
    /// the *saturation and the distance from the panel* — the two numbers the
    /// severity channel owns — being the same at every stage.
    #[test]
    fn a_cards_hue_answers_only_to_its_stage_and_its_intensity_only_to_its_severity() {
        let ground = measured::CANVAS;
        let hues = [30.0, 120.0, 200.0, 280.0, 340.0];

        for (stage, hue) in LifecycleStage::ALL.into_iter().zip(hues) {
            let inks: Vec<_> = Severity::ALL
                .into_iter()
                .map(|severity| {
                    CardLight::of(severity, hue, ground, true, false, CardTheme::UNTHEMED)
                        .ink
                        .to_hsl()
                })
                .collect();
            for (severity, (h, _, _)) in Severity::ALL.into_iter().zip(&inks) {
                assert!(
                    (h - hue).abs() < 1.5,
                    "{stage:?} at {severity:?} landed on hue {h:.1} instead of {hue:.1}: \
                     the severity channel reached into the stage channel"
                );
            }
        }

        for severity in Severity::ALL {
            let placed = crate::anim::cell::signal_light(severity, ground.as_tuple());
            for (stage, hue) in LifecycleStage::ALL.into_iter().zip(hues) {
                let (_, sat, light) =
                    CardLight::of(severity, hue, ground, true, false, CardTheme::UNTHEMED)
                        .ink
                        .to_hsl();
                assert!(
                    (light - placed).abs() < 0.02,
                    "{stage:?} is placed at lightness {light:.3} where {severity:?} \
                     asks for {placed:.3}: the stage channel reached into the \
                     severity channel"
                );
                let first =
                    CardLight::of(severity, hues[0], ground, true, false, CardTheme::UNTHEMED)
                        .ink
                        .to_hsl()
                        .1;
                assert!(
                    (sat - first).abs() < 0.02,
                    "{stage:?} is drawn at saturation {sat:.3} where {severity:?} \
                     asks for {first:.3}"
                );
            }
        }
    }

    /// Every one of the twenty cards the two channels can express is a card
    /// somebody could tell from the other nineteen.
    ///
    /// The point of the split: a severe problem at one stage must not render as
    /// a mild one at another. Two inks count as told apart when they differ in
    /// hue by more than the card's own gradient can account for, or in the
    /// intensity the severity channel places them at.
    #[test]
    fn every_stage_by_severity_combination_is_distinguishable() {
        let ground = measured::CANVAS;
        let matrix: Vec<_> = LifecycleStage::ALL
            .into_iter()
            .flat_map(|stage| {
                Severity::ALL.into_iter().map(move |severity| {
                    let hue = stage.hue(
                        &crate::app::state::Palette::catppuccin(),
                        &crate::terminal_theme::TerminalTheme::default(),
                    );
                    let (h, _, l) =
                        CardLight::of(severity, hue, ground, true, false, CardTheme::UNTHEMED)
                            .ink
                            .to_hsl();
                    (stage, severity, h, l)
                })
            })
            .collect();
        assert_eq!(matrix.len(), 20);

        for (i, a) in matrix.iter().enumerate() {
            for b in &matrix[i + 1..] {
                // Hue distance the short way round the wheel.
                let hue_gap = {
                    let raw = (a.2 - b.2).abs() % 360.0;
                    raw.min(360.0 - raw)
                };
                let light_gap = (a.3 - b.3).abs();
                assert!(
                    hue_gap > measured::HUE_TRAVEL || light_gap > 0.06,
                    "{:?}/{:?} and {:?}/{:?} render alike: {hue_gap:.1}° apart at \
                     {light_gap:.3} lightness",
                    a.0,
                    a.1,
                    b.0,
                    b.1
                );
            }
        }
    }

    /// **Severity survives the colour being taken away.**
    ///
    /// The visual target is explicit that state has to read for someone who
    /// cannot separate two hues, so the severity channel is placed by *contrast
    /// against the panel* — a quantity defined on relative luminance alone.
    /// Convert the whole matrix to greyscale and the four severities are still
    /// four, in order, at every stage.
    #[test]
    fn severity_is_still_four_steps_in_greyscale() {
        let ground = measured::CANVAS;
        for stage in LifecycleStage::ALL {
            let hue = stage.hue(
                &crate::app::state::Palette::catppuccin(),
                &crate::terminal_theme::TerminalTheme::default(),
            );
            let greys: Vec<f32> = Severity::ALL
                .into_iter()
                .map(|severity| {
                    let ink =
                        CardLight::of(severity, hue, ground, true, false, CardTheme::UNTHEMED).ink;
                    crate::ui::color::relative_luminance(ink.as_tuple())
                })
                .collect();
            for pair in greys.windows(2) {
                assert!(
                    pair[1] > pair[0] * 1.2,
                    "{stage:?} in greyscale: {pair:?} is not a step anyone could see"
                );
            }
        }
    }

    /// And it says it a second time, in rhythm.
    ///
    /// Contrast alone would still be a *light* channel, and the visual target's
    /// answer to "colour measurably fails" is behaviour. A serious problem puts
    /// the card on the escalated rung of the same ladder the tray badges climb —
    /// over rest and over live alike, because a card that has gone quiet with
    /// something badly wrong on it is not resting.
    #[test]
    fn a_serious_problem_escalates_the_cards_rhythm_whatever_it_was_doing() {
        use crate::anim::behaviour::names;
        for state in [
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Idle,
            AgentState::Unknown,
        ] {
            let quiet = breath_behaviour(state, Severity::Clear);
            assert_ne!(quiet, names::CARD_ALERT);
            assert_eq!(breath_behaviour(state, Severity::Mild), quiet);
            for severity in [Severity::Serious, Severity::Critical] {
                assert_eq!(breath_behaviour(state, severity), names::CARD_ALERT);
            }
        }

        // The rungs are genuinely different rhythms, not the same one relabelled.
        let catalogue = crate::anim::behaviour::Catalogue::built_in();
        let period = |name| {
            catalogue
                .get(name)
                .expect("a built-in")
                .effective_period(crate::anim::behaviour::DriveInputs { activity: 1.0 })
        };
        assert!(
            period(names::CARD_ALERT) * 2 < period(names::CARD_LIVE),
            "the escalated rung has to be better than twice the live one, or the \
             two read as one rhythm that drifted"
        );
        assert!(period(names::CARD_ALERT) < period(names::CARD_REST));
    }

    /// A card's chrome is measured against the tier's nominal height, so a card
    /// the content pushed taller keeps the padding and plate its tier asked
    /// for rather than inflating them.
    /// The tiers are retired: there is one nominal height, so every card's
    /// chrome is the same regardless of rank. This is what stops the tier
    /// scale's old per-depth chrome step from becoming a second, quieter size
    /// signal now that height no longer carries one.
    #[test]
    fn chrome_is_identical_on_every_card_since_the_tiers_were_retired() {
        let a = CardGeometry::new(16.0, true);
        let b = CardGeometry::new(16.0, true);
        assert_eq!(a.pad, b.pad);
        assert_eq!(a.plate, b.plate);
        assert_eq!(a.radius, b.radius);
        assert_eq!(a.stroke, b.stroke);
        assert_eq!(a.bloom_sigma, b.bloom_sigma);
    }

    /// The plate cap is the named deviation from the measured 0.70 h: without
    /// it the card has the narrowest text column it could have.
    #[test]
    fn the_plate_is_capped_so_the_card_is_not_the_narrowest_column() {
        const {
            assert!(measured::PLATE * BASE_HEIGHT_PX > measured::PLATE_MAX_PX);
        }
        assert_eq!(CardGeometry::new(16.0, true).plate, measured::PLATE_MAX_PX);
    }

    /// No mark, no slot — and every pixel the slot was taking goes to the text
    /// column rather than to a wider gap.
    #[test]
    fn a_card_with_no_mark_reserves_no_icon_slot() {
        let marked = CardGeometry::new(16.0, true);
        let bare = CardGeometry::new(16.0, false);
        assert_eq!(bare.plate, 0.0);
        assert_eq!(bare.plate_gap, 0.0);
        assert_eq!(bare.text_inset(), bare.pad);
        assert!(
            bare.text_inset() < marked.text_inset(),
            "collapsing the slot has to move the ink left, not just stop drawing a box"
        );
        // The slot is collapsed, not deleted: the measured size is still there
        // for the day a mark arrives.
        assert!(marked.plate > 0.0);
    }

    /// The distance field is what the fill, the stroke and the bloom all read,
    /// so its sign has to be right or all three are.
    #[test]
    fn the_rounded_rect_knows_inside_from_outside() {
        let rect = RoundRect {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 20.0,
            r: 5.0,
        };
        assert!(rect.distance(20.0, 10.0) < 0.0, "centre should be inside");
        assert!(rect.distance(-5.0, 10.0) > 0.0, "left of it is outside");
        assert!(rect.distance(20.0, 30.0) > 0.0, "below it is outside");
        // The corner is cut: a point inside the bounding box but outside the
        // radius has to read as outside.
        assert!(rect.distance(0.4, 0.4) > 0.0, "the corner was not rounded");
        assert!(
            (rect.distance(0.0, 10.0)).abs() < 0.6,
            "the left edge is the boundary"
        );
    }

    /// A tidbit is built only out of what the fleet published; nothing is
    /// invented to fill the line.
    #[test]
    fn the_tidbit_reports_only_published_facts() {
        let mut entry = AgentPanelEntry::test_new("worker");
        assert_eq!(tidbit_line(&entry, None), None);

        entry
            .tokens
            .insert("project".to_string(), "herdr".to_string());
        assert_eq!(tidbit_line(&entry, None).as_deref(), Some("herdr"));

        entry.tokens.insert("context".to_string(), "6%".to_string());
        assert_eq!(
            tidbit_line(&entry, None).as_deref(),
            Some("herdr  ·  6% ctx")
        );

        // Already spelled with its unit: not doubled.
        entry
            .tokens
            .insert("context".to_string(), "6% ctx".to_string());
        assert_eq!(
            tidbit_line(&entry, None).as_deref(),
            Some("herdr  ·  6% ctx")
        );
    }

    /// The title is the fleet's own words, and the token behind it is never
    /// written back.
    ///
    /// The captain's 2026-08-09 change is that Herdr may now *choose* — see
    /// [`summary`] — so "verbatim" is no longer the whole rule and this states
    /// the part that survived it: a summary with nothing redundant in it comes
    /// through untouched, and the fallback chain down to the pane's own name is
    /// unchanged. What the choosing removes is pinned in `summary`'s own tests,
    /// against strings that have something to remove.
    #[test]
    fn a_published_doing_string_with_nothing_redundant_in_it_is_untouched() {
        let mut entry = AgentPanelEntry::test_new("worker");
        assert_eq!(title_text(&entry), "worker");
        let doing = "Investigateing killed Okta corpus and Herdr work sessions";
        entry.tokens.insert("doing".to_string(), doing.to_string());
        assert_eq!(title_text(&entry), doing);
        // And the token itself is still the publisher's, not Herdr's copy of it.
        assert_eq!(entry.tokens.get("doing").map(String::as_str), Some(doing));
    }

    /// A summary the fleet padded reaches the card without the padding.
    #[test]
    fn a_padded_doing_string_is_chosen_down_before_it_is_ever_measured() {
        let mut entry = AgentPanelEntry::test_new("worker");
        entry.tokens.insert(
            "doing".to_string(),
            "Currently working on `the card trim`.".to_string(),
        );
        assert_eq!(title_text(&entry), "the card trim");
    }

    /// Condensing must never blank a card. A `doing` that is nothing but
    /// punctuation still puts the publisher's own string on screen.
    #[test]
    fn a_summary_that_condenses_to_nothing_falls_back_to_what_was_published() {
        assert_eq!(display_summary("...".to_string()), "...");
        assert_eq!(display_summary("   ".to_string()), "   ");
    }
}

#[cfg(test)]
mod the_sheet_carries_the_view_transition {
    use super::tests::{build_sheet, pixel_fleet_app, sidebar_rect, SheetUpdate};
    use super::*;

    /// Start a re-root and return the app mid-dismount, or `None` when this
    /// machine has no proportional face and there is no pixel path to test.
    fn switching(per_cell: u16) -> Option<(AppState, std::time::Instant)> {
        let mut app = pixel_fleet_app();
        app.sidebar_animation.view_switch_particles_per_cell = per_cell;
        let rect = sidebar_rect();
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let SheetUpdate::Rebuilt(_) = build_sheet(&app, &cards, rect, app.host_cell_size, None)
        else {
            return None;
        };
        let now = std::time::Instant::now();
        assert!(app.select_tree_root(
            crate::app::tree_view::TreeRoot::Node("2ndmate-herdr".to_string()),
            now
        ));
        Some((app, now))
    }

    fn sheet_at(app: &AppState, previous: Option<&SidebarCardLayer>) -> Option<SidebarCardLayer> {
        let rect = sidebar_rect();
        let cards = super::super::compute_workspace_card_areas(app, rect);
        match build_sheet(app, &cards, rect, app.host_cell_size, previous) {
            SheetUpdate::Rebuilt(layer) => Some(layer),
            _ => None,
        }
    }

    /// The default. A Herdr nobody has configured this on rasterises the sheet
    /// exactly once across a whole switch, holds no second copy of it, and the
    /// cards cut straight from one view to the next the way they always have.
    #[test]
    fn off_by_default_costs_one_sheet_and_holds_no_canvas() {
        let Some((mut app, now)) = switching(0) else {
            return;
        };
        let mut held = sheet_at(&app, None);
        assert!(
            held.as_ref()
                .is_some_and(|layer| layer.undissolved.is_none()),
            "a settled sheet keeps no undissolved copy"
        );
        let mut rebuilds = 0;
        for step in 1..=12 {
            app.anim
                .advance(now + std::time::Duration::from_millis(step * 50));
            if let Some(layer) = sheet_at(&app, held.as_ref()) {
                rebuilds += 1;
                held = Some(layer);
            }
        }
        assert_eq!(rebuilds, 0, "the sheet does not move while the view leaves");
    }

    /// Turned on, every frame of the half produces a new sheet — that is the
    /// effect — and each one is a fresh picture rather than the same bytes.
    #[test]
    fn a_switch_redraws_the_sheet_on_every_frame() {
        let Some((mut app, now)) = switching(20) else {
            return;
        };
        let mut held = sheet_at(&app, None);
        let mut seen = std::collections::HashSet::new();
        for step in 1..=8 {
            app.anim
                .advance(now + std::time::Duration::from_millis(step * 50));
            let Some(layer) = sheet_at(&app, held.as_ref()) else {
                panic!("frame {step} of a running transition produced no sheet");
            };
            assert!(
                seen.insert(layer.layer.data_fingerprint),
                "frame {step} re-sent the pixels of an earlier frame"
            );
            held = Some(layer);
        }
    }

    /// The expensive half of a transition frame is rasterising ten cards, their
    /// bloom and their type — about nine tenths of it. A frame whose cards have
    /// not moved reuses those pixels instead of drawing them again, and this is
    /// the check that it actually does.
    #[test]
    fn a_transition_frame_reuses_the_cards_it_already_rasterised() {
        let Some((mut app, now)) = switching(20) else {
            return;
        };
        let first = sheet_at(&app, None).expect("a running transition has a sheet");
        assert!(
            first.undissolved.is_some(),
            "a transition frame holds the sheet it drew"
        );
        app.anim
            .advance(now + std::time::Duration::from_millis(100));
        let second = sheet_at(&app, Some(&first)).expect("the next frame draws");
        assert_eq!(
            first.content_signature, second.content_signature,
            "the cards have not moved: the switch has not committed yet"
        );
        assert_ne!(
            first.signature, second.signature,
            "but the frame of the transition has"
        );
        assert!(
            first
                .undissolved
                .as_ref()
                .zip(second.undissolved.as_ref())
                .is_some_and(|(a, b)| std::sync::Arc::ptr_eq(&a.0, &b.0)),
            "the second frame rasterised the cards again instead of reusing them"
        );
    }

    /// The knob is particles per *cell*, so it has to mean the same thing on a
    /// tall cell and a square one — and it has to actually deliver roughly the
    /// count it is asked for, which is the whole of what the captain's "20x" is.
    #[test]
    fn the_particle_count_tracks_the_setting_on_any_cell() {
        for cell in [
            HostCellSize {
                width_px: 10,
                height_px: 21,
            },
            HostCellSize {
                width_px: 7,
                height_px: 15,
            },
            HostCellSize {
                width_px: 12,
                height_px: 12,
            },
        ] {
            let cell_px = f64::from(cell.width_px * cell.height_px);
            for asked in [1u16, 4, 20] {
                let mut app = pixel_fleet_app();
                app.host_cell_size = cell;
                app.sidebar_animation.view_switch_particles_per_cell = asked;
                assert!(app.select_tree_root(
                    crate::app::tree_view::TreeRoot::Node("2ndmate-herdr".to_string()),
                    std::time::Instant::now()
                ));
                let frame = sheet_dissolve(&app, cell).expect("a leaving view dissolves");
                let edge = f64::from(frame.particle_px * frame.particle_px);
                let delivered = cell_px / edge;
                let ratio = delivered / f64::from(asked);
                assert!(
                    (0.5..=2.0).contains(&ratio),
                    "{cell:?} asked {asked}/cell and got {delivered:.1}/cell"
                );
            }
        }
    }

    /// A particle finer than a pixel is the same dissolve costing more to draw,
    /// so the edge never goes below one.
    #[test]
    fn a_particle_is_never_finer_than_a_pixel() {
        let mut app = pixel_fleet_app();
        app.sidebar_animation.view_switch_particles_per_cell = u16::MAX;
        assert!(app.select_tree_root(
            crate::app::tree_view::TreeRoot::Node("2ndmate-herdr".to_string()),
            std::time::Instant::now()
        ));
        let frame = sheet_dissolve(&app, app.host_cell_size).expect("a leaving view dissolves");
        assert_eq!(frame.particle_px, 1);
    }

    /// What a denser dissolve actually costs, measured rather than reasoned
    /// about.
    ///
    /// Ignored by default because it prints a table and times things; run it
    /// with `cargo test --release --bin herdr dissolve_cost -- --ignored
    /// --nocapture`. It exists so the number in the pull request that
    /// introduced this can be re-derived rather than believed — a debug build
    /// reports about six times the per-frame cost of a release one, which is
    /// exactly the kind of gap a quoted figure hides.
    #[test]
    #[ignore = "measurement, not an assertion: run with --ignored --nocapture"]
    fn dissolve_cost() {
        let app = pixel_fleet_app();
        let rect = sidebar_rect();
        let cell = app.host_cell_size;
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let SheetUpdate::Rebuilt(base) = build_sheet(&app, &cards, rect, cell, None) else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        let cell_px = cell.width_px * cell.height_px;
        println!(
            "sheet {}x{} cells = {}x{} px, settled PNG {} B, host cell {}x{} px",
            base.rect.width,
            base.rect.height,
            base.layer.image_width,
            base.layer.image_height,
            base.layer.data.len(),
            cell.width_px,
            cell.height_px,
        );
        let half = std::time::Duration::from_millis(app.sidebar_animation.view_switch_ms);
        let tick = std::time::Duration::from_millis(50);
        println!(
            "view_switch_ms {} per half, engine tick 50 ms\n",
            half.as_millis()
        );
        println!("per_cell | delivered | edge px | sheets | PNG B/sheet | ms/sheet | half KB");
        println!("---------+-----------+---------+--------+-------------+----------+--------");
        for per_cell in [0u16, 1, 4, 20, 64, 210] {
            let mut app = pixel_fleet_app();
            app.sidebar_animation.view_switch_particles_per_cell = per_cell;
            let now = std::time::Instant::now();
            assert!(app.select_tree_root(
                crate::app::tree_view::TreeRoot::Node("2ndmate-herdr".to_string()),
                now
            ));
            let (mut held, mut sheets, mut bytes, mut millis, mut edge) =
                (None, 0u32, 0usize, 0f64, 0u32);
            let mut elapsed = std::time::Duration::ZERO;
            while elapsed <= half {
                app.anim.advance(now + elapsed);
                let cards = super::super::compute_workspace_card_areas(&app, rect);
                let started = std::time::Instant::now();
                let update = build_sheet(&app, &cards, rect, cell, held.as_ref());
                let took = started.elapsed().as_secs_f64() * 1000.0;
                if let SheetUpdate::Rebuilt(layer) = update {
                    sheets += 1;
                    bytes += layer.layer.data.len();
                    millis += took;
                    held = Some(layer);
                }
                if let Some(frame) = sheet_dissolve(&app, cell) {
                    edge = frame.particle_px;
                }
                elapsed += tick;
            }
            let n = f64::from(sheets.max(1));
            let delivered = if edge == 0 {
                0.0
            } else {
                f64::from(cell_px) / f64::from(edge * edge)
            };
            println!(
                "{per_cell:>8} | {delivered:>9.1} | {edge:>7} | {sheets:>6} | {:>11.0} | {:>8.2} | {:>7.0}",
                bytes as f64 / n,
                millis / n,
                bytes as f64 / 1024.0,
            );
        }
    }

    /// Writes the real frames of a switch to PNG files so they can be looked at.
    #[test]
    #[ignore = "capture, not an assertion"]
    fn dissolve_capture() {
        let out = std::env::var("HERDR_DISSOLVE_CAPTURE_DIR").unwrap_or_default();
        if out.is_empty() {
            println!("SKIP: set HERDR_DISSOLVE_CAPTURE_DIR");
            return;
        }
        let rect = sidebar_rect();
        for per_cell in [0u16, 1, 21] {
            let mut app = pixel_fleet_app();
            app.sidebar_animation.view_switch_particles_per_cell = per_cell;
            let cell = app.host_cell_size;
            let now = std::time::Instant::now();
            assert!(app.select_tree_root(
                crate::app::tree_view::TreeRoot::Node("2ndmate-herdr".to_string()),
                now
            ));
            let mut held: Option<SidebarCardLayer> = None;
            for step in 0..=12u64 {
                app.anim
                    .advance(now + std::time::Duration::from_millis(step * 50));
                let cards = super::super::compute_workspace_card_areas(&app, rect);
                if let SheetUpdate::Rebuilt(layer) =
                    build_sheet(&app, &cards, rect, cell, held.as_ref())
                {
                    let path = format!("{out}/p{per_cell:03}-f{step:02}.png");
                    std::fs::write(&path, &layer.layer.data).expect("writes");
                    held = Some(layer);
                }
            }
        }
    }

    /// The transition ends, and the sheet stops holding a second copy of itself.
    ///
    /// The cache is scoped to the switch it is for. A panel that has settled
    /// keeps only the pixels it is showing.
    #[test]
    fn the_canvas_is_released_when_the_switch_finishes() {
        let Some((mut app, now)) = switching(20) else {
            return;
        };
        let mut held = sheet_at(&app, None);
        assert!(held.as_ref().is_some_and(|l| l.undissolved.is_some()));
        // Past both halves and the commit between them, then far enough past
        // the arrival that the view element has retired.
        let over = std::time::Duration::from_millis(app.sidebar_animation.view_switch_ms * 4);
        let mut elapsed = std::time::Duration::from_millis(50);
        while elapsed <= over {
            app.anim.advance(now + elapsed);
            app.advance_tree_view(now + elapsed);
            if let Some(layer) = sheet_at(&app, held.as_ref()) {
                held = Some(layer);
            }
            elapsed += std::time::Duration::from_millis(50);
        }
        assert!(
            held.as_ref().is_some_and(|l| l.undissolved.is_none()),
            "a settled panel is still holding the sheet it dissolved"
        );
    }

    /// A dissolve takes presence away and never puts colour back: a particle
    /// that has gone is transparent, and one that has not is untouched.
    #[test]
    fn a_dissolved_particle_gives_up_alpha_and_keeps_its_colour() {
        let mut sheet = Canvas::new(8, 8);
        for y in 0..8 {
            for x in 0..8 {
                sheet.blend(x, y, Rgb(200, 100, 50), 1.0);
            }
        }
        let before = sheet.rgba8().to_vec();
        sheet.scale_alpha(0, 0, 4, 4, 0.25);
        let after = sheet.rgba8();
        for y in 0..8u32 {
            for x in 0..8u32 {
                let i = ((y * 8 + x) * 4) as usize;
                assert_eq!(
                    &after[i..i + 3],
                    &before[i..i + 3],
                    "the dissolve recoloured ({x},{y})"
                );
                let expected = if x < 4 && y < 4 { 64 } else { 255 };
                assert_eq!(after[i + 3], expected, "alpha at ({x},{y})");
            }
        }
    }
}

/// The card is an object made of its own glowing outline, not a picture of a
/// rectangle.
///
/// The captain diagnosed the sheet from a screenshot: *"I can tell that it is
/// still technically the full width, and all you've done is match the background
/// to make the card look a different shape and size, because I can see the sharp
/// rectangular edge of the background that has not been blended with the glow."*
/// Everything here is that sentence turned into an assertion.
#[cfg(test)]
mod a_card_is_its_own_shape {
    use super::tests::{pixel_fleet_app, sidebar_rect, three_rank_pixel_app};
    use super::*;
    use crate::workspace::Workspace;

    /// The same fleet, drawing shapes instead of one sheet.
    fn shape_fleet_app() -> AppState {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        app
    }

    /// A whole terminal wide enough to hold [`sidebar_rect`]'s panel.
    fn pass_area() -> Rect {
        Rect::new(0, 0, 100, sidebar_rect().height)
    }

    /// Run one real foreground pass over `app`, as a client with a known cell
    /// size gets. `None` when this machine has no proportional face.
    ///
    /// Through `compute_view` rather than `build_cards` directly, because the
    /// fact under test — whether *this* pass published shapes — is recorded
    /// there and nowhere else.
    fn shape_pass(
        app: &mut AppState,
        runtimes: &crate::terminal::TerminalRuntimeRegistry,
    ) -> Option<u16> {
        app.sidebar_width = sidebar_rect().width;
        let cell_size = app.host_cell_size;
        crate::ui::compute_view_with_cell_size(app, runtimes, pass_area(), cell_size);
        (!app.sidebar_card_layers.is_empty())
            .then(|| super::super::row_fold_width(app, app.view.sidebar_rect))
    }

    /// A menu opened on the tree puts the character cards back; one opened
    /// clear of it leaves the pixel cards exactly where they are.
    ///
    /// This is the "rows vanish on a menu click" defect, from the publication
    /// side. A Kitty image composites above the cell text, so an open overlay
    /// takes the artwork under it off the terminal — and the character cards
    /// stood down for artwork that pass was no longer going to place
    /// ([`shape_covers_row`]), leaving bare tree rails where every row had been.
    ///
    /// One test rather than two because either half alone is satisfied by a
    /// wrong rule: standing down for *every* overlay is safe but flips the whole
    /// tree's art on a menu that changed nothing about it, and standing down for
    /// none of them is the defect.
    #[test]
    fn an_overlay_on_the_tree_stands_the_pixel_cards_down_and_one_clear_of_it_does_not() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let mut app = shape_fleet_app();
        // Stated rather than inherited: `AppState::test_new` starts in
        // `Mode::Navigate`, and this test is about what a mode's overlay does.
        app.mode = crate::app::Mode::Terminal;
        if shape_pass(&mut app, &runtimes).is_none() {
            println!("SKIP: no proportional face on this machine");
            return;
        }
        assert!(
            app.view.sidebar_card_layers_published,
            "the resting pass published no cards, so this tests nothing"
        );
        let published_layers = app.sidebar_card_layers.len();
        let tree_bottom = app
            .view
            .workspace_card_areas
            .iter()
            .filter_map(|card| card.card_frame)
            .map(|frame| frame.y + frame.height)
            .max()
            .expect("the tree drew no card frames");

        let open_menu_at = |app: &mut AppState, y: u16| {
            app.mode = crate::app::Mode::ContextMenu;
            app.context_menu = Some(crate::app::state::ContextMenuState {
                kind: crate::app::state::ContextMenuKind::Workspace { ws_idx: 0 },
                x: 1,
                y,
                list: crate::app::state::MenuListState::new(0),
            });
        };

        // Below the last card: the tree is untouched and keeps its artwork.
        open_menu_at(&mut app, tree_bottom + 2);
        shape_pass(&mut app, &runtimes);
        assert!(
            app.view.sidebar_card_layers_published,
            "a menu clear of the tree flipped it back to character cards"
        );

        // On the first card: the artwork will not be placed, so the characters
        // have to be there instead.
        open_menu_at(&mut app, 1);
        shape_pass(&mut app, &runtimes);
        assert!(
            !app.view.sidebar_card_layers_published,
            "a menu drawn over the tree left the character cards stood down, \
             so those rows render as bare rails"
        );
        assert!(
            !shape_covers_row(
                &app,
                super::super::row_fold_width(&app, app.view.sidebar_rect)
            ),
            "the row still believes a shape is covering it"
        );
        assert_eq!(
            app.sidebar_card_layers.len(),
            published_layers,
            "the artwork was dropped rather than merely withheld, so closing \
             the menu costs the whole tree a re-raster"
        );

        // And closing it puts the pixel cards straight back.
        app.mode = crate::app::Mode::Terminal;
        app.context_menu = None;
        shape_pass(&mut app, &runtimes);
        assert!(
            app.view.sidebar_card_layers_published,
            "the tree did not come back when the menu closed"
        );
    }

    /// A `CardScene` built from a real fleet survives an encode/decode round
    /// trip byte-for-byte — the contract `ServerMessage::CardScene` rests on,
    /// checked without a live terminal or a connected client on either end.
    #[test]
    fn card_scene_round_trips_through_bincode() {
        let app = pixel_fleet_app();
        let rect = sidebar_rect();
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let Some(scene) = build_card_scene(&app, &cards, rect, app.host_cell_size) else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        assert!(
            !scene.placed.is_empty(),
            "fleet should place at least one card"
        );

        let bytes = encode_card_scene(&scene).expect("encode CardScene");
        let decoded = decode_card_scene(&bytes).expect("decode CardScene");

        assert_eq!(scene, decoded);
    }

    /// Garbage bytes decode to an error rather than a panic — the client's
    /// only defence against a version-skewed or corrupted `CardScene` payload.
    #[test]
    fn card_scene_decode_rejects_garbage_bytes() {
        assert!(decode_card_scene(&[0xff, 0x00, 0x13, 0x37]).is_err());
    }

    /// A pre-#186 `CardContentWire` shape — `focused_space` spliced back out,
    /// same field order otherwise — fails to decode against the current
    /// decoder rather than silently defaulting `focused_space` to `false`.
    ///
    /// Pins down why `focused_space`'s own `#[serde(default)]` is not what
    /// protects a version-skewed server/client pairing: bincode's struct
    /// encoding is positional, so a decoder only "runs out of bytes" for a
    /// truly trailing field at the very end of the whole message, never for
    /// one nested inside a `Vec` of possibly many cards with more `CardScene`
    /// fields following. What actually protects this pairing is
    /// `crate::protocol::wire::PROTOCOL_VERSION`. If this test ever starts
    /// passing, that means `CardContentWire` (or its embedding) changed to
    /// tolerate schema evolution for real — and the `PROTOCOL_VERSION` doc
    /// comment referencing this test should be revisited.
    #[test]
    fn card_scene_decode_rejects_pre_186_wire_shape() {
        #[derive(serde::Serialize)]
        struct OldCardContentWire {
            title: String,
            tidbit: Option<String>,
            register: Option<Caption>,
            state_label: String,
            state: AgentState,
            stage: LifecycleStage,
            severity: Severity,
            hues: StageHues,
            ground: Rgb,
            split_channels: bool,
            seen: bool,
            depth: u8,
            lifted: bool,
            // No `focused_space`: this is the pre-#186 field order.
            mark: Option<CardMark>,
            residue: u8,
            controls: ControlRail,
            generate: f32,
            discharge: f32,
            breath: f32,
            spider: Option<spider::CardSpider>,
        }

        #[derive(serde::Serialize)]
        struct OldCardScene {
            placed: Vec<(Rect, OldCardContentWire)>,
            offsets: Vec<(i32, i32)>,
            field: Rect,
            bounds: Rect,
            bloom_floor: u16,
            backdrop: Rgb,
        }

        let app = pixel_fleet_app();
        let rect = sidebar_rect();
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let Some(scene) = build_card_scene(&app, &cards, rect, app.host_cell_size) else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        assert!(!scene.placed.is_empty(), "need at least one real card");

        let old_scene = OldCardScene {
            placed: scene
                .placed
                .iter()
                .cloned()
                .map(|(rect, wire)| {
                    (
                        rect,
                        OldCardContentWire {
                            title: wire.title,
                            tidbit: wire.tidbit,
                            register: wire.register,
                            state_label: wire.state_label,
                            state: wire.state,
                            stage: wire.stage,
                            severity: wire.severity,
                            hues: wire.hues,
                            ground: wire.ground,
                            split_channels: wire.split_channels,
                            seen: wire.seen,
                            depth: wire.depth,
                            lifted: wire.lifted,
                            mark: wire.mark,
                            residue: wire.residue,
                            controls: wire.controls,
                            generate: wire.generate,
                            discharge: wire.discharge,
                            breath: wire.breath,
                            spider: wire.spider,
                        },
                    )
                })
                .collect(),
            offsets: scene.offsets.clone(),
            field: scene.field,
            bounds: scene.bounds,
            bloom_floor: scene.bloom_floor,
            backdrop: scene.backdrop,
        };

        let old_bytes = bincode::serde::encode_to_vec(&old_scene, bincode::config::standard())
            .expect("encode old-shaped CardScene");

        assert!(
            decode_card_scene(&old_bytes).is_err(),
            "a pre-#186 CardContentWire payload decoded without error; \
             focused_space's #[serde(default)] is masking version skew that \
             PROTOCOL_VERSION should be catching instead"
        );
    }

    /// The cards a build published, or `None` when this machine has no
    /// proportional face and there is no pixel path to test.
    fn built(app: &AppState) -> Option<Vec<SidebarCardLayer>> {
        let cards = super::super::compute_workspace_card_areas(app, sidebar_rect());
        match build_cards(app, &cards, sidebar_rect(), app.host_cell_size, &[]).update {
            CardsUpdate::Rebuilt(layers) => Some(layers),
            _ => None,
        }
    }

    /// The rows that carry a card, which is what a published layer answers to.
    /// The frame of every row that gets an image of its own — which is every
    /// row with a *card*, and so not the worker rows drawn inside a Space's own
    /// box. Those keep a frame, because they keep a rect and a click target, but
    /// their ink is in their Space's image and nothing is placed for them. See
    /// [`crew_for`].
    fn framed(app: &AppState) -> Vec<Rect> {
        let entries = super::super::workspace_list_entries(app);
        super::super::compute_workspace_card_areas(app, sidebar_rect())
            .into_iter()
            .filter(|card| super::super::crew_head(&entries, card.entry_idx).is_none())
            .filter_map(|card| card.card_frame)
            .filter(|frame| frame.width > 0 && frame.height > 0)
            .collect()
    }

    /// Straight RGBA8 back out of a published layer.
    fn decode(layer: &SidebarCardLayer) -> (u32, u32, Vec<u8>) {
        let decoder = png::Decoder::new(layer.layer.data.as_slice());
        let mut reader = decoder.read_info().expect("a layer that is not a PNG");
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("a PNG with no frame");
        buf.truncate(info.buffer_size());
        (info.width, info.height, buf)
    }

    /// A fleet where the two controls are actually on: a second mate that owns
    /// a worker who reported back, and that also heads a worktree group with a
    /// linked child under it.
    ///
    /// Both conditions on one tree rather than two fixtures, because the
    /// interesting card is the one carrying *both* — that is the card the
    /// character row reserves two controls on, and the one whose rail is
    /// widest.
    fn railed_fleet_app(summary: bool, collapsed: bool) -> AppState {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;

        let mut mate = Workspace::test_new("2ndmate-explore");
        mate.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            checkout_path: std::path::PathBuf::from("/repo/herdr"),
            is_linked_worktree: false,
        });
        let worker_pane = mate.test_split(ratatui::layout::Direction::Vertical);

        let mut child = Workspace::test_new("2ndmate-explore-issue");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            checkout_path: std::path::PathBuf::from("/repo/herdr-issue"),
            is_linked_worktree: true,
        });

        app.workspaces = vec![Workspace::test_new("firstmate"), mate, child];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.sidebar_agents.rows = vec![vec![crate::config::AgentSidebarToken::Agent]];

        let now = std::time::Instant::now();
        app.workspaces[1].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            now,
        );

        let worker_terminal = app.workspaces[1].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&worker_terminal)
            .expect("the worker has a terminal");
        terminal.set_agent_name("worker".to_string());
        terminal.state = AgentState::Idle;
        let mut tokens = std::collections::HashMap::from([(
            "owner".to_string(),
            Some("2ndmate-explore".to_string()),
        )]);
        if summary {
            tokens.insert("summary".to_string(), Some("rebased and green".to_string()));
        }
        terminal.metadata_tokens.patch(tokens, None, now);
        // Unseen either way, so the *only* difference a published summary makes
        // to this tree is the mate's badge: `is_unseen_finish` is idle and
        // unseen, and a test pane is born seen, so flipping it alongside the
        // token would change the worker's own card too.
        if let Some(pane) = app.workspaces[1].tabs[0].panes.get_mut(&worker_pane) {
            pane.seen = false;
        }

        if collapsed {
            app.collapsed_space_keys.insert("repo-key".to_string());
        }
        app.view.sidebar_rect = sidebar_rect();
        app.view.workspace_card_areas =
            super::super::compute_workspace_card_areas(&app, sidebar_rect());
        app
    }

    /// The rail each card is given, beside the row it belongs to.
    fn rails(app: &AppState) -> Vec<(String, ControlRail)> {
        let entries = super::super::workspace_list_entries(app);
        let agents = super::super::sidebar_agent_entries(app);
        super::super::compute_workspace_card_areas(app, sidebar_rect())
            .into_iter()
            .filter_map(|card| {
                let entry = entries.get(card.entry_idx)?;
                let name = match entry {
                    super::super::WorkspaceListEntry::Workspace { ws_idx, .. } => app
                        .workspaces
                        .get(*ws_idx)?
                        .display_name_from_terminals(&app.terminals),
                    super::super::WorkspaceListEntry::Agent { entry_idx, .. } => {
                        agents.get(*entry_idx)?.agent_name.clone()?
                    }
                };
                Some((name, control_rail(app, entry, &agents, &card)))
            })
            .collect()
    }

    /// Both controls reach the card, on exactly the rows the character tree
    /// would have drawn them on.
    ///
    /// The badge follows *ownership* — the mate whose workers reported — and
    /// the chevron follows the worktree group's head. They land on the same row
    /// here, which is the case worth pinning: two controls on one card is what
    /// the rail was sized for.
    #[test]
    fn a_mate_that_owns_a_finished_worker_and_heads_a_group_carries_both_controls() {
        let rails = rails(&railed_fleet_app(true, false));
        let mate = rails
            .iter()
            .find(|(name, _)| name.contains("2ndmate-explore") && !name.contains("issue"))
            .map(|(_, rail)| *rail)
            .expect("the mate has a card");
        assert_eq!(
            mate.summary,
            Some(SummaryBadge {
                count: 1,
                fresh: true
            }),
            "the mate's card lost the badge its row draws"
        );
        assert_eq!(
            mate.group,
            Some(GroupChevron::Expanded),
            "the mate's card lost the chevron its row draws"
        );
    }

    /// And neither appears on a row the character tree would not have drawn it
    /// on: the worktree *child* carries no chevron of its own, and no row that
    /// owns nothing carries a badge.
    #[test]
    fn no_card_grows_a_control_the_character_row_would_not_have_drawn() {
        for (name, rail) in rails(&railed_fleet_app(true, false)) {
            if name.contains("issue") {
                assert_eq!(
                    rail.group, None,
                    "{name} is a worktree child and drew a chevron of its own"
                );
            }
            if name == "firstmate" || name == "worker" {
                assert_eq!(rail, ControlRail::default(), "{name} grew a control");
            }
        }
    }

    /// A folded group points its chevron the other way, and that is a different
    /// card — so the artwork is rebuilt rather than carried forward.
    ///
    /// The signature is the whole of what decides that. A rail hashed out of it
    /// would leave a collapsed group showing the chevron it had before the
    /// click, which is worse than not drawing one at all.
    #[test]
    fn folding_a_group_turns_the_chevron_and_redraws_the_card() {
        let expanded = railed_fleet_app(true, false);
        let collapsed = railed_fleet_app(true, true);
        assert_eq!(
            rails(&expanded)
                .iter()
                .filter_map(|(_, rail)| rail.group)
                .collect::<Vec<_>>(),
            vec![GroupChevron::Expanded]
        );
        assert_eq!(
            rails(&collapsed)
                .iter()
                .filter_map(|(_, rail)| rail.group)
                .collect::<Vec<_>>(),
            vec![GroupChevron::Collapsed]
        );

        let (Some(before), Some(after)) = (built(&expanded), built(&collapsed)) else {
            return; // No face on this machine.
        };
        assert!(
            before
                .iter()
                .map(|layer| layer.content_signature)
                .collect::<Vec<_>>()
                != after
                    .iter()
                    .map(|layer| layer.content_signature)
                    .collect::<Vec<_>>(),
            "turning the chevron changed no card's signature, so the old artwork \
             would be carried forward"
        );
    }

    /// The rail is ink on the card, not a reservation nobody filled.
    ///
    /// The card with the controls draws pixels in the band above its chip that
    /// the same card without them leaves to the card's own fill. This is the
    /// whole defect being fixed, asserted on the bytes a real pass publishes
    /// rather than on the layout that precedes them.
    #[test]
    fn the_controls_put_ink_on_the_card_where_the_bare_row_would_have_drawn_them() {
        let with = railed_fleet_app(true, false);
        let without = railed_fleet_app(false, false);
        let (Some(with_layers), Some(without_layers)) = (built(&with), built(&without)) else {
            return; // No face on this machine.
        };
        // Same tree either way — one worker's summary is the only difference —
        // so the cards line up and the mate's is the one that differs.
        assert_eq!(with_layers.len(), without_layers.len());
        let differing = with_layers
            .iter()
            .zip(&without_layers)
            .filter(|(a, b)| decode(a).2 != decode(b).2)
            .count();
        assert_eq!(
            differing, 1,
            "a published summary changed {differing} cards, not just the mate's"
        );
    }

    /// The rail's ink lands inside the cells its click targets name.
    ///
    /// This is the property that makes the rail *correct* rather than merely
    /// present: `worker_summary_badge_rect` and `workspace_group_chevron_rect`
    /// are still the only things a click resolves against, so a mark drawn
    /// outside them would be a control pointing at a cell it does not occupy —
    /// the same defect as one drawn nowhere, wearing a costume.
    #[test]
    fn the_rail_is_drawn_inside_the_cells_its_controls_are_clicked_at() {
        let app = railed_fleet_app(true, false);
        let Some(font) = font::card_font(app.sidebar_card_font.as_deref()) else {
            return; // No face on this machine.
        };
        let cell_w = f32::from(u16::try_from(app.host_cell_size.width_px).expect("a sane cell"));
        let cell_h = f32::from(u16::try_from(app.host_cell_size.height_px).expect("a sane cell"));
        let entries = super::super::workspace_list_entries(&app);
        let agents = super::super::sidebar_agent_entries(&app);

        let mut checked = 0;
        for card in super::super::compute_workspace_card_areas(&app, sidebar_rect()) {
            let Some(frame) = card.card_frame else {
                continue;
            };
            let Some(entry) = entries.get(card.entry_idx) else {
                continue;
            };
            let rail = control_rail(&app, entry, &agents, &card);
            if rail.is_empty() {
                continue;
            }
            let Some(content) = content_for(
                &app,
                entry,
                &agents,
                &crate::ui::sidebar::body_register::BodyRegister::resolve(&app),
            ) else {
                continue;
            };
            let geometry = CardGeometry::new(cell_h, content.mark.is_some());
            let column = text_column(
                font,
                &geometry,
                (f32::from(frame.width) - RAIL_INK_COLUMN_FRACTION) * cell_w,
                card_height_px(
                    font.metrics(TITLE_PX),
                    font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
                ),
                &content.title,
                rail,
            );
            let layout = rail.layout(font, column.caption_px);

            // The card's own left edge stands half a column inside its first
            // cell, so the rail's pixels are measured back to that same origin.
            let card_left_px = (f32::from(frame.x) + RAIL_INK_COLUMN_FRACTION) * cell_w;
            let rail_left = card_left_px + column.right - layout.width;
            let rail_right = card_left_px + column.right;

            // Right to left, the way the character row lays them out: the
            // chevron in the last control cell, the badge immediately left of
            // it.
            let chevron_cell = super::super::workspace_group_chevron_rect(&card);
            if let Some(chevron) = &layout.chevron {
                let left = card_left_px + column.right - layout.width + chevron.x;
                let right = left + chevron.side;
                let cell_left = f32::from(chevron_cell.x) * cell_w;
                let cell_right = cell_left + f32::from(chevron_cell.width) * cell_w;
                assert!(
                    left >= cell_left - 1.0 && right <= cell_right + 1.0,
                    "the chevron is drawn at {left:.1}..{right:.1} px but clicked at cell \
                     {chevron_cell:?} = {cell_left:.1}..{cell_right:.1} px"
                );
                checked += 1;
            }
            if let Some(badge) = rail.summary {
                let badge_cells = super::super::worker_summary_badge_rect(&card, badge.count);
                assert!(
                    badge_cells.width > 0,
                    "a badge was drawn with no click target"
                );
                let cell_left = f32::from(badge_cells.x) * cell_w;
                assert!(
                    rail_left >= cell_left - 1.0,
                    "the rail starts at {rail_left:.1} px, left of the badge's own cells at \
                     {cell_left:.1} px"
                );
                checked += 1;
            }
            // And neither reaches the card's right border.
            assert!(
                rail_right
                    <= card_left_px + (f32::from(frame.width) - RAIL_INK_COLUMN_FRACTION) * cell_w,
                "the rail overran the card's right edge"
            );
        }
        assert!(checked >= 2, "no rail was actually measured");
    }

    /// The two chevron directions are two different pictures.
    ///
    /// Drawn rather than set — the faces a card can use carry neither `▸` nor
    /// `▾` — so nothing but this says the triangle really turns.
    #[test]
    fn the_chevron_points_the_way_its_group_is_folded() {
        let ink = Rgb(255, 255, 255);
        let painted = |group| {
            let mut sheet = Canvas::new(16, 16);
            draw_chevron(&mut sheet, (2.0, 2.0), 10.0, group, ink, 1.0);
            sheet.rgba8().to_vec()
        };
        let collapsed = painted(GroupChevron::Collapsed);
        let expanded = painted(GroupChevron::Expanded);
        assert_ne!(collapsed, expanded, "the chevron drew the same both ways");

        let lit = |px: &[u8], x: u32, y: u32| px[((y * 16 + x) * 4 + 3) as usize] > 128;
        // A right-pointing triangle has its nose on the middle row and nothing
        // at the far corner of it; a down-pointing one is the same claim turned
        // a quarter turn.
        assert!(lit(&collapsed, 8, 7) && !lit(&collapsed, 10, 3));
        assert!(lit(&expanded, 7, 8) && !lit(&expanded, 3, 10));
    }

    /// An unread summary is stated and a read one is caption weight, and both
    /// are the card's own hue rather than a colour from outside it.
    #[test]
    fn a_fresh_summary_badge_is_louder_than_one_already_read() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        let ink = Rgb(120, 220, 220);
        let painted = |fresh| {
            let rail = ControlRail {
                summary: Some(SummaryBadge { count: 3, fresh }),
                group: None,
                space_badge: None,
            };
            let layout = rail.layout(font, 10.0);
            let mut sheet = Canvas::new(64, 24);
            draw_control_rail(
                &mut sheet,
                font,
                &layout,
                ink,
                10.0,
                &CardGeometry::new(21.0, false),
                (4.0, 4.0),
                1.0,
                CardTheme::UNTHEMED,
            );
            sheet.rgba8().to_vec()
        };
        let fresh = painted(true);
        let seen = painted(false);
        assert_ne!(
            fresh, seen,
            "a read summary looks the same as an unread one"
        );

        // Brightest well-covered pixel each way: the fresh badge is drawn at the
        // card's ink, the seen one mixed toward the card's own fill.
        //
        // The coverage bar is half rather than near-opaque. The mark is drawn a
        // fifth under the em it used to fill, and at the rail's real ~10 px type
        // that is an eight-pixel box whose strokes and rules are now thin enough
        // that antialiasing keeps every one of them off full opacity. That is a
        // softer mark, which is the change; it is not a missing one, and this
        // test is about the two badges' relative weight, not about either one's
        // absolute coverage.
        let peak = |px: &[u8]| {
            (0..px.len() / 4)
                .filter(|i| px[i * 4 + 3] > 128)
                .map(|i| u32::from(px[i * 4]) + u32::from(px[i * 4 + 1]) + u32::from(px[i * 4 + 2]))
                .max()
                .unwrap_or(0)
        };
        assert!(
            peak(&fresh) > peak(&seen),
            "the read badge is not quieter than the unread one: {} against {}",
            peak(&fresh),
            peak(&seen)
        );
    }

    /// Both paths clamp a large crew the same way, because they share the one
    /// function that decides it.
    #[test]
    fn the_card_and_the_bare_row_agree_on_what_a_large_crew_says() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        for count in [1usize, 9, 10, 400] {
            let rail = ControlRail {
                summary: Some(SummaryBadge {
                    count,
                    fresh: false,
                }),
                group: None,
                space_badge: None,
            };
            let drawn = rail
                .layout(font, 10.0)
                .summary
                .expect("a rail with a badge lays one out")
                .count;
            assert_eq!(
                super::super::worker_summary_badge_label(count),
                format!("{}{drawn}", super::super::WORKER_SUMMARY_BADGE_GLYPH),
                "the card and the row printed different counts for {count}"
            );
        }
    }

    /// Every card in a real tree — first mate, second mates, and their
    /// workers — is drawn at the same height on screen.
    ///
    /// Through the real render path rather than the constants: builds the
    /// captain's own fleet at his 42-column width, decodes the actual PNG the
    /// sheet publishes, and measures each card's own drawn ink (the rounded
    /// rect, found by where the alpha inside the card's cell footprint stops
    /// being zero) rather than trusting the row height it was handed. A second,
    /// quieter scale hiding in the chrome instead of the row height would still
    /// show up here.
    #[test]
    fn every_rank_renders_at_the_same_card_height() {
        let app = three_rank_pixel_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let sheet = layers.first().expect("the sheet is one layer");
        let (img_w, img_h, px) = decode(sheet);
        let cell_w = f32::from(u16::try_from(app.host_cell_size.width_px).unwrap());
        let cell_h = f32::from(u16::try_from(app.host_cell_size.height_px).unwrap());

        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let entries = super::super::workspace_list_entries(&app);
        let agents = super::super::sidebar_agent_entries(&app);
        let mut heights_by_rank: std::collections::BTreeMap<
            crate::app::agent_tree::AgentRelation,
            Vec<u32>,
        > = std::collections::BTreeMap::new();

        for card in &cards {
            let Some(frame) = card.card_frame else {
                continue;
            };
            if frame.width == 0 || frame.height == 0 {
                continue;
            }
            let Some(entry) = entries.get(card.entry_idx) else {
                continue;
            };
            if content_for(
                &app,
                entry,
                &agents,
                &crate::ui::sidebar::body_register::BodyRegister::resolve(&app),
            )
            .is_none()
            {
                continue;
            }
            // The pixel band this row's card is *drawn* in, which is its own
            // cells shifted by [`connector_row_offset_px`]: on an even-cell row
            // the card moves half a cell up onto the row its branch line lands
            // on, so its top edge falls on the last pixel row of the band above.
            // Measured against the unshifted cells, this row's band would catch
            // the *next* row's top edge and report every card but the last one
            // as a full band tall.
            let offset = connector_row_offset_px(frame.height, cell_h);
            let band_top =
                (u32::from(frame.y.saturating_sub(sheet.rect.y)) as f32 * cell_h) + offset;
            let y0 = band_top.max(0.0) as u32;
            let y1 = y0 + (u32::from(frame.height) as f32 * cell_h) as u32;
            let x0 = (u32::from(frame.x.saturating_sub(sheet.rect.x)) as f32 * cell_w) as u32;
            let x1 = (x0 + (u32::from(frame.width) as f32 * cell_w) as u32).min(img_w);
            let (y0, y1) = (y0.min(img_h), y1.min(img_h));

            let mut top = None;
            let mut bottom = None;
            for y in y0..y1 {
                let mut lit = false;
                for x in x0..x1 {
                    if px[((y * img_w + x) * 4 + 3) as usize] > 0 {
                        lit = true;
                        break;
                    }
                }
                if lit {
                    top.get_or_insert(y);
                    bottom = Some(y);
                }
            }
            if let (Some(top), Some(bottom)) = (top, bottom) {
                heights_by_rank
                    .entry(entry.rank())
                    .or_default()
                    .push(bottom - top + 1);
            }
        }

        assert!(
            heights_by_rank.len() >= 3,
            "the fixture did not exercise at least three ranks: {heights_by_rank:?}"
        );
        let mut all_heights = heights_by_rank.values().flatten().copied();
        let first = all_heights.next().expect("at least one card was measured");
        for h in all_heights {
            assert!(
                h.abs_diff(first) <= 1,
                "card heights differ by rank (a device-pixel rounding tolerance of 1 is \
                 allowed): {heights_by_rank:?}"
            );
        }
    }

    /// Width still reads as rank in the real render path: a worker's card is
    /// narrower than its second mate's, whose is narrower than the first mate's,
    /// at the captain's 42-column width. This is [`super::rank_width_inset`],
    /// unrelated to and unaffected by the height change — this test exists so a
    /// future change to height cannot silently take width down with it.
    #[test]
    fn width_still_narrows_by_rank_at_the_captains_width() {
        let app = three_rank_pixel_app();
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let entries = super::super::workspace_list_entries(&app);
        let agents = super::super::sidebar_agent_entries(&app);

        let mut widths_by_rank: std::collections::BTreeMap<
            crate::app::agent_tree::AgentRelation,
            u16,
        > = std::collections::BTreeMap::new();
        for card in &cards {
            let Some(frame) = card.card_frame else {
                continue;
            };
            if frame.width == 0 {
                continue;
            }
            let Some(entry) = entries.get(card.entry_idx) else {
                continue;
            };
            if content_for(
                &app,
                entry,
                &agents,
                &crate::ui::sidebar::body_register::BodyRegister::resolve(&app),
            )
            .is_none()
            {
                continue;
            }
            widths_by_rank.insert(entry.rank(), frame.width);
        }

        let first = widths_by_rank
            .get(&crate::app::agent_tree::AgentRelation::FirstMate)
            .copied();
        let second = widths_by_rank
            .get(&crate::app::agent_tree::AgentRelation::SecondMate)
            .copied();
        let worker = widths_by_rank
            .get(&crate::app::agent_tree::AgentRelation::Worker)
            .copied();

        match (first, second, worker) {
            (Some(first), Some(second), Some(worker)) => {
                assert!(
                    first > second,
                    "a first mate's card ({first} cols) is not wider than a second \
                     mate's ({second} cols)"
                );
                assert!(
                    second > worker,
                    "a second mate's card ({second} cols) is not wider than a \
                     worker's ({worker} cols)"
                );
            }
            _ => panic!("fixture did not carry all three ranks: {widths_by_rank:?}"),
        }
    }

    /// One card, one image, one placement.
    ///
    /// This is the property every queued motion item rests on: a card that can
    /// be moved, faded or reflowed on its own has to *be* on its own first.
    #[test]
    fn each_card_is_its_own_placement() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        assert_eq!(
            layers.len(),
            framed(&app).len(),
            "the tree published a different number of images than it has cards"
        );
        assert!(layers.len() > 1, "a fleet of one card cannot test this");
        for layer in &layers {
            assert!(!layer.layer.data.is_empty(), "a card published no pixels");
            assert!(layer.rect.width > 0 && layer.rect.height > 0);
        }

        // And the sheet, on the same fleet, is still exactly one.
        let sheet = pixel_fleet_app();
        assert_eq!(
            built(&sheet).map(|layers| layers.len()),
            Some(1),
            "the sheet stopped being one image for the whole tree"
        );
    }

    /// **The card's left border stands where the tree's rails have their ink.**
    ///
    /// The captain's two structural findings on the shapes path — *"tree trunk
    /// not aligned with firstmate/workers. branches not aligned with
    /// secondmates"* — are one offset seen twice. A rail is a box-drawing glyph
    /// and a font draws those down the middle of a cell; the card's border was a
    /// stroke on `frame.x`, which is a cell boundary. Half a column apart, at
    /// every level of the tree at once.
    ///
    /// Measured in the published pixels rather than off [`Rasteriser::place`],
    /// because the thing that was wrong was where the ink landed. The two
    /// candidate positions are half a cell apart — five pixels on this fixture's
    /// 10 px cell — so the tolerance here tells them apart and would fail the
    /// old geometry.
    #[test]
    fn a_cards_left_border_stands_in_the_middle_of_its_column_where_the_rails_do() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let frames = framed(&app);
        assert_eq!(layers.len(), frames.len());
        let cell_w = f32::from(u16::try_from(app.host_cell_size.width_px).expect("a sane cell"));

        let mut checked = 0;
        for (layer, frame) in layers.iter().zip(&frames) {
            let (width_px, height_px, rgba) = decode(layer);
            // The card's own band, away from the rounded corners and from the
            // bloom that gathers under it.
            let y = height_px / 2;
            let row = |x: u32| rgba[((y * width_px + x) * 4 + 3) as usize];
            let Some(first_lit) = (0..width_px).find(|x| row(*x) > 200) else {
                continue;
            };

            let column_left = f32::from(frame.x.saturating_sub(layer.rect.x)) * cell_w;
            let rail_ink = column_left + RAIL_INK_COLUMN_FRACTION * cell_w;
            let measured = first_lit as f32;
            assert!(
                (measured - rail_ink).abs() <= 2.0,
                "a card at column {} put its border at {measured} px, not at the {rail_ink} px \
                 its column's rails are drawn down",
                frame.x
            );
            assert!(
                (measured - column_left).abs() > 2.0,
                "the border is still on the cell boundary at {column_left} px, half a column \
                 left of every rail meant to continue it"
            );
            checked += 1;
        }
        assert!(checked > 1, "no card was actually measured");
    }

    /// **A card's ink is centred on the row its branch line meets it on.**
    ///
    /// The vertical half of the same finding, reported live on a real Rio at the
    /// captain's 42 columns: *"branch lines are not centered on the card's
    /// vertical span."* The line lands on
    /// [`crate::app::state::WorkspaceCardArea::connector_y`] and the card was
    /// centred in its own cells, and those are the same row only while the row
    /// is an odd number of cells. At a 15 px cell a card needs four, and every
    /// branch in the tree ran into its card 7 px above the middle at once.
    ///
    /// Driven at cell heights on both sides of that boundary and measured in the
    /// published pixels, for the reason the horizontal test gives: what was
    /// wrong was where the ink landed.
    #[test]
    fn a_cards_ink_is_centred_on_the_row_its_branch_line_meets_it_on() {
        for cell_height in [21u32, 18, 15] {
            let mut app = shape_fleet_app();
            app.host_cell_size.height_px = cell_height;
            let rect = sidebar_rect();
            let cards = super::super::compute_workspace_card_areas(&app, rect);
            let Some(layers) = built(&app) else {
                return; // No face on this machine.
            };
            let framed: Vec<&crate::app::state::WorkspaceCardArea> = cards
                .iter()
                .filter(|card| {
                    card.card_frame
                        .is_some_and(|frame| frame.width > 0 && frame.height > 0)
                })
                .collect();
            assert_eq!(layers.len(), framed.len());
            let cell_h = cell_height as f32;

            let mut checked = 0;
            for (layer, card) in layers.iter().zip(&framed) {
                let frame = card.card_frame.expect("filtered to framed rows");
                assert!(
                    card.drawn_card,
                    "this fixture must be on the drawn-card path or the row it \
                     measures is a box of characters"
                );
                let (width_px, height_px, rgba) = decode(layer);
                // The card's own left border column, away from the rounded
                // corners: the topmost and bottommost opaque pixel of that
                // column are the card's own top and bottom strokes.
                let column = ((f32::from(frame.x.saturating_sub(layer.rect.x))
                    + RAIL_INK_COLUMN_FRACTION)
                    * f32::from(u16::try_from(app.host_cell_size.width_px).expect("a cell")))
                    as u32;
                let alpha =
                    |y: u32| rgba[((y * width_px + column.min(width_px - 1)) * 4 + 3) as usize];
                let Some(top) = (0..height_px).find(|y| alpha(*y) > 200) else {
                    continue;
                };
                let Some(bottom) = (0..height_px).rev().find(|y| alpha(*y) > 200) else {
                    continue;
                };
                let ink_centre = f32::from(layer.rect.y) * cell_h + (top + bottom) as f32 / 2.0;
                let line_centre = (f32::from(card.connector_y()) + 0.5) * cell_h;
                // Within half a cell, which is the bound `Rasteriser::place`'s
                // clamp guarantees: a card travels onto the connector's row as
                // far as its own gutter allows, and a card that fills most of
                // its cells has less gutter than the offset wants. Half a cell
                // is the worst case and it is a quarter of that at the
                // captain's own 10x21.
                assert!(
                    (ink_centre - line_centre).abs() <= cell_h / 2.0 + 1.5,
                    "at a {cell_height} px cell a {}-cell card put its middle at \
                     {ink_centre} px, more than half a cell off the {line_centre} px \
                     middle of the row its branch line lands on",
                    frame.height,
                );
                // And it did not leave its own cells to get there, which is what
                // the clamp is actually for: a card whose ink reaches above its
                // frame is a card drawn over the row above it, and on the first
                // row it is a card with its top edge cut off by the image.
                let frame_top = f32::from(frame.y.saturating_sub(layer.rect.y)) * cell_h;
                let frame_bottom = frame_top + f32::from(frame.height) * cell_h;
                assert!(
                    top as f32 >= frame_top - 0.5 && (bottom as f32) < frame_bottom + 0.5,
                    "at a {cell_height} px cell a {}-cell card's ink runs {top}..{bottom} \
                     px, outside the {frame_top}..{frame_bottom} px its row was given",
                    frame.height,
                );
                checked += 1;
            }
            assert!(checked > 1, "no card was actually measured");
        }
    }

    /// The offset is zero wherever there is a middle row to be the middle of,
    /// and exactly half a cell — upwards — wherever there is not.
    ///
    /// Pinned separately from the pixels because it is the whole of the rule and
    /// a fixture can only ever reach the row heights its own face produces.
    #[test]
    fn a_card_only_gives_ground_when_its_row_has_no_middle_cell() {
        for (rows, expected) in [(1u16, 0.0), (2, -10.0), (3, 0.0), (4, -10.0), (5, 0.0)] {
            assert_eq!(
                connector_row_offset_px(rows, 20.0),
                expected,
                "a {rows}-cell row"
            );
        }
        assert_eq!(connector_row_offset_px(0, 20.0), 0.0, "a row with no cells");
    }

    /// The right edge did not move with it. Nothing in the tree is drawn against
    /// a card's right edge, so pulling the left one in is what aligns the card —
    /// sliding the whole box would take it off the columns the layout gave it
    /// and into the scrollbar's.
    #[test]
    fn aligning_the_left_border_did_not_push_the_card_past_its_own_columns() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return;
        };
        for (layer, frame) in layers.iter().zip(&framed(&app)) {
            assert!(
                layer.rect.x + layer.rect.width >= frame.x + frame.width,
                "a card's image no longer covers the columns its row was given"
            );
            assert!(
                layer.rect.x + layer.rect.width <= sidebar_rect().width,
                "a card reached past the panel"
            );
        }
    }

    /// Nothing is lit outside the card's own outline and the reach of its glow.
    ///
    /// This is the assertion the sheet cannot pass, and the test asserts that
    /// too — a check that both models satisfy would be checking nothing. The
    /// boundary is built from the row's frame, which comes from the character
    /// layout and not from the rasteriser, so this is not the drawing code
    /// marking its own homework.
    #[test]
    fn a_shape_lights_nothing_outside_its_own_glow() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let frames = framed(&app);
        let cell_w = f32::from(app.host_cell_size.width_px as u16);
        let cell_h = f32::from(app.host_cell_size.height_px as u16);

        let mut checked = 0;
        for (layer, frame) in layers.iter().zip(&frames) {
            let (width, height, px) = decode(layer);
            // The card's own box inside this image, from the row's frame.
            let left = f32::from(frame.x.saturating_sub(layer.rect.x)) * cell_w;
            let top = f32::from(frame.y.saturating_sub(layer.rect.y)) * cell_h;
            let box_w = f32::from(frame.width) * cell_w;
            let box_h = f32::from(frame.height) * cell_h;
            // Every card's glow reaches at most the shared bloom reach, plus a
            // pixel for the antialiasing ramp. The same bound for every card
            // since the tiers were retired — see `BASE_HEIGHT_PX`.
            let reach = bloom_reach_px(cell_h) + 1.0;

            for y in 0..height {
                for x in 0..width {
                    if px[((y * width + x) * 4 + 3) as usize] == 0 {
                        continue;
                    }
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let dx = (left - fx).max(fx - (left + box_w)).max(0.0);
                    let dy = (top - fy).max(fy - (top + box_h)).max(0.0);
                    assert!(
                        dx.hypot(dy) <= reach,
                        "a shape lit a pixel {:.1} px outside its own card — that \
                         is a painted background, not a glow",
                        dx.hypot(dy),
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no shape drew anything to check");
    }

    /// The gutter between two cards is genuinely darker than the rim beside them.
    ///
    /// The captain, on the cards as they shipped: *"fade radius too wide"* — one
    /// card's glow landing on the next. The cause was arithmetic rather than a
    /// bug: [`measured::BLOOM_SIGMA`] at 0.19 h put a top-tier card's sigma at
    /// 12.9 px while the gap between two cards is 10–16 px, so the glow's own
    /// half-width *was* the gutter and the gap sat at 85–100% of the brightness
    /// against a card's own stroke.
    ///
    /// Measured on the composited panel and not on the layers, because that is
    /// where the question lives: two neighbours' halos are two separate graphics
    /// placements and the terminal composites them source-over in linear light,
    /// so the gutter carries *both*. A per-layer check would measure half of it
    /// and pass on a panel that still bleeds.
    ///
    /// The threshold is a contract and not the measurement: this reads 51.6% at
    /// the worst gutter in the fixture and the constants it replaced read 100%.
    #[test]
    fn a_card_glow_leaves_the_gutter_darker_than_the_rim() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let cell = app.host_cell_size;
        let rect = sidebar_rect();
        let (pw, ph) = (
            u32::from(rect.width) * cell.width_px,
            u32::from(rect.height) * cell.height_px,
        );
        let canvas = backdrop_rgb(&app);
        let mut panel = vec![
            [
                srgb_to_linear(canvas.0),
                srgb_to_linear(canvas.1),
                srgb_to_linear(canvas.2)
            ];
            (pw * ph) as usize
        ];
        // Where each card's own ink sits on the panel, so the gutters between
        // them can be found without asking the layout a second time.
        let mut inks: Vec<(u32, u32)> = Vec::new();
        for layer in &layers {
            let (lw, lh, px) = decode(layer);
            let ox = u32::from(layer.rect.x) * cell.width_px;
            let oy = u32::from(layer.rect.y) * cell.height_px;
            let cx = lw / 2;
            let opaque: Vec<u32> = (0..lh)
                .filter(|y| px[(((y * lw + cx) * 4) + 3) as usize] > 200)
                .collect();
            if let (Some(first), Some(last)) = (opaque.first(), opaque.last()) {
                inks.push((oy + first, oy + last));
            }
            for y in 0..lh {
                for x in 0..lw {
                    let (px_, py) = (ox + x, oy + y);
                    if px_ >= pw || py >= ph {
                        continue;
                    }
                    let s = ((y * lw + x) * 4) as usize;
                    let a = f32::from(px[s + 3]) / 255.0;
                    if a <= 0.0 {
                        continue;
                    }
                    let dst = &mut panel[(py * pw + px_) as usize];
                    for (k, d) in dst.iter_mut().enumerate() {
                        *d = srgb_to_linear(px[s + k]) * a + *d * (1.0 - a);
                    }
                }
            }
        }
        assert!(inks.len() > 2, "a gutter needs two cards either side of it");

        // Luminance and not a channel excess: every card carries its own hue
        // since the stage/severity split, so a cyan-excess reads *negative* on an
        // amber card. "Does the gutter get dark" is a luminance question on every
        // hue. Taken as the brightest the row gets anywhere across the panel,
        // which is the honest floor.
        let ground = luminance([
            srgb_to_linear(canvas.0),
            srgb_to_linear(canvas.1),
            srgb_to_linear(canvas.2),
        ]);
        let row = |y: u32| {
            (0..pw)
                .map(|x| luminance(panel[(y * pw + x) as usize]))
                .fold(f32::MIN, f32::max)
        };
        let mut checked = 0;
        for pair in inks.windows(2) {
            let (above, below) = (pair[0].1, pair[1].0);
            if below <= above + 2 {
                continue; // Cards that touch have no gutter to measure.
            }
            let rim = row(above + 1).max(row(below - 1));
            let floor = (above + 1..below).map(row).fold(f32::MAX, f32::min);
            let share = (floor - ground) / (rim - ground).max(f32::EPSILON);
            assert!(
                share < 0.60,
                "the gutter in rows {}..{} sits at {:.1}% of the glow against the \
                 card's own stroke — the glow's half-width is the whole gap and \
                 one card's halo is landing on the next",
                above + 1,
                below - 1,
                100.0 * share,
            );
            checked += 1;
        }
        assert!(checked > 0, "no gutter was measured");
    }

    /// The reach is still derived from the paint floor, whatever shape the
    /// field currently has.
    ///
    /// [`a_card_glow_falls_to_nothing_before_it_is_cut`] asserts the visible
    /// consequence of that derivation on rendered pixels; this asserts the
    /// derivation itself, in the field's own units. Two reasons it is worth
    /// having both:
    ///
    /// - The rendered check can only speak for the cards a fixture happens to
    ///   build, and skips entirely on a machine with no font face. This one is
    ///   arithmetic on the constants, so it holds on every card, at every cell
    ///   size, on every host, and runs everywhere.
    /// - The distance is a property of the profile's *shape*, and the shape has
    ///   changed under this constant before: the field went two-lobe at 0.19
    ///   peak to a single hot core at 0.38, which moved the derived distance
    ///   from 3.64 σ to 3.24 σ while [`BLOOM_REACH_SIGMAS`] stayed 3.7. That was
    ///   the safe direction. Nothing but this test would have caught the other
    ///   one, and the prose that recorded the old figure went stale in exactly
    ///   the way a comment does.
    #[test]
    fn the_bloom_reach_is_derived_from_the_paint_floor() {
        // The profile `lay_bloom` samples, restated in sigmas: `d / near_sigma`,
        // which is what makes the answer independent of the cell size. Taken at
        // the card's strongest bloom, because the per-column multiplier only
        // ever dips it — `presence` peaks at 1.0 and `BREATH_BLOOM_DIP`
        // subtracts from there.
        let amount = |sigmas: f32| {
            let near = (-(sigmas * sigmas) / 2.0).exp();
            let far = (-(sigmas * sigmas)
                / (2.0 * measured::BLOOM_FAR_SIGMA_MUL * measured::BLOOM_FAR_SIGMA_MUL))
                .exp();
            measured::BLOOM_PEAK
                * (measured::BLOOM_NEAR_WEIGHT * near + measured::BLOOM_FAR_WEIGHT * far)
        };

        // Where the field falls under what `lay_bloom` will paint at all.
        let mut derived = f32::NAN;
        let mut sigmas = 0.0_f32;
        while sigmas <= 64.0 {
            if amount(sigmas) <= BLOOM_PAINT_FLOOR {
                derived = sigmas;
                break;
            }
            sigmas += 0.001;
        }
        assert!(
            derived.is_finite(),
            "the bloom profile never falls under BLOOM_PAINT_FLOOR, so no reach can \
             truncate it invisibly — the peak or the weights are wrong",
        );
        assert!(
            derived <= BLOOM_REACH_SIGMAS,
            "the bloom is still worth {:.5} at BLOOM_REACH_SIGMAS = {}, above the \
             {BLOOM_PAINT_FLOOR} lay_bloom will paint, so the truncation is a hard edge \
             in open panel. The field's shape changed and the reach did not: it has to \
             be at least {derived:.2} σ now",
            amount(BLOOM_REACH_SIGMAS),
            BLOOM_REACH_SIGMAS,
        );
    }

    /// A card's glow fades out before it is truncated, rather than stopping dead.
    ///
    /// The other half of *"still needs to retain quality and crispness"*.
    /// [`lay_bloom`] carries the field to [`BLOOM_REACH_SIGMAS`] and its
    /// `profile.get(..)` returns `None` past it, so whatever the field is still
    /// worth at that distance becomes a hard edge in open panel. It used to be
    /// worth 15% of peak on a top-tier card — an outermost painted alpha of 7 —
    /// and, because the reach was a fraction of the card's *drawn* height while
    /// the sigma is a fraction of its tier's *nominal* height, a different amount
    /// on every tier.
    ///
    /// The reach is now derived from [`BLOOM_PAINT_FLOOR`], so the profile has
    /// already fallen under what `lay_bloom` will paint before the truncation
    /// reaches it. This asserts the observable consequence: the outermost pixel a
    /// card lights is the faintest one representable. It is the assertion that
    /// goes red if any of the bloom's shape constants move without the reach
    /// being re-derived with them.
    #[test]
    fn a_card_carries_no_glow_past_its_own_edge() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let mut checked = 0;
        for layer in &layers {
            let (w, h, px) = decode(layer);
            let alpha = |x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize];
            // Out of the card in each direction, from the middle of the side it
            // leaves — clear of the corners, where two falloffs overlap.
            let (cx, cy) = (w / 2, h / 2);
            let edges = [
                (0..h).find(|y| alpha(cx, *y) > 0).map(|y| alpha(cx, y)),
                (0..h)
                    .rev()
                    .find(|y| alpha(cx, *y) > 0)
                    .map(|y| alpha(cx, y)),
                (0..w).find(|x| alpha(*x, cy) > 0).map(|x| alpha(x, cy)),
                (0..w)
                    .rev()
                    .find(|x| alpha(*x, cy) > 0)
                    .map(|x| alpha(x, cy)),
            ];
            for (side, outermost) in edges.into_iter().enumerate() {
                let Some(outermost) = outermost else {
                    continue;
                };
                // A card clipped by the panel's own edge is cut by the clamp and
                // not by the reach, which is a different question.
                if is_clipped(layer, side) {
                    continue;
                }
                // **The inverse of what this used to assert, and deliberately.**
                // It used to require the outermost lit pixel to be alpha 2 or
                // less: a card was a lit plate with a bloom running 26–28 px
                // past its stroke, and the question was whether that glow had
                // faded to nothing by the point the reach truncated it.
                //
                // There is no bloom now ([`CARD_BLOOM`]) — F1 refuses
                // `box-shadow` and `blur()`, the reference has no drop shadow
                // anywhere, and its panes float by being brighter than the
                // ground rather than by casting onto it. So the outermost lit
                // pixel is the card's own edge, and what is worth pinning is
                // that it is an *edge*: a hard boundary at real alpha, with no
                // low-alpha tail outside it. A bloom coming back would put a
                // fringe here and this would catch it.
                assert!(
                    outermost >= 64,
                    "a card's outermost lit pixel is alpha {outermost} — that is the \
                     tail of a glow rather than the card's own edge, so something is \
                     painting past the boundary again",
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no card edge was checked");
    }

    /// Whether this layer runs into the panel's own boundary on `side`, in the
    /// order [`a_card_carries_no_glow_past_its_own_edge`] scans them.
    fn is_clipped(layer: &SidebarCardLayer, side: usize) -> bool {
        let bounds = super::super::sidebar_content_rect(sidebar_rect());
        let r = layer.rect;
        match side {
            0 => r.y <= bounds.y,
            1 => r.y + r.height >= bounds.y + bounds.height,
            2 => r.x <= bounds.x,
            _ => r.x + r.width >= bounds.x + bounds.width,
        }
    }

    /// sRGB byte to linear light, which is where the terminal composites.
    fn srgb_to_linear(c: u8) -> f32 {
        let s = f32::from(c) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance([r, g, b]: [f32; 3]) -> f32 {
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }

    /// Alpha at the four corners of the cells one card owns.
    ///
    /// A card is a *rounded* rect, so its own frame's corners are outside it by
    /// the corner radius whatever else is true — which makes them the one place
    /// that answers "is this a shape, or a rectangle painted to look like one"
    /// on every card, at every tier, gutter or no gutter.
    fn frame_corner_alphas(layer: &SidebarCardLayer, frame: Rect, cell: HostCellSize) -> [u8; 4] {
        let (width, height, px) = decode(layer);
        let left = u32::from(frame.x.saturating_sub(layer.rect.x)) * cell.width_px;
        let top = u32::from(frame.y.saturating_sub(layer.rect.y)) * cell.height_px;
        let right = (left + u32::from(frame.width) * cell.width_px).min(width) - 1;
        let bottom = (top + u32::from(frame.height) * cell.height_px).min(height) - 1;
        let at = |x: u32, y: u32| px[((y * width + x) * 4 + 3) as usize];
        [
            at(left, top),
            at(right, top),
            at(left, bottom),
            at(right, bottom),
        ]
    }

    /// A shape's own frame corners are not opaque.
    ///
    /// The card is rounded, so its corners are where a painted background gives
    /// itself away — this is the "sharp rectangular edge of the background that
    /// has not been blended with the glow" the captain read off a screenshot.
    #[test]
    fn a_shape_has_no_opaque_rectangle_behind_it() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        for (layer, frame) in layers.iter().zip(framed(&app)) {
            for alpha in frame_corner_alphas(layer, frame, app.host_cell_size) {
                assert!(
                    alpha < 255,
                    "a card's corner was fully opaque, so it is still a painted \
                     rectangle rather than a shape"
                );
            }
        }
    }

    /// **The tree is measured, not asserted.**
    ///
    /// Two numbers off the pixels the panel actually publishes.
    ///
    /// **H1, the hue band.** The scout measured the reference's whole tree
    /// column: *99.94% of chromatic pixels above L25 sit inside 175–265°, and
    /// 99.7% of those in a single 15° bucket at 195°.* One hue family;
    /// everything else in the panel is brightness. Held here at 99%.
    ///
    /// **The density, restated.** The scout's other pair — *84% of its area is
    /// lit ink against the reference's 20%, a 4.1x density* — is **not
    /// reproduced literally here, and that is stated rather than smoothed
    /// over.** The two crops it compared had different denominators (the
    /// reference's whole 352x1060 column against a card-tight crop of herdr's),
    /// and over a near-black canvas the reference's own sampled face —
    /// `rgba(122, 196, 222, .10)` — composites to L32.7, which is *above* the
    /// L25 floor the count used. So "lit ink above L25" cannot tell a glass face
    /// from a filled plate at all, and a number tuned until it passed would be a
    /// number that measured the fixture's panel height.
    ///
    /// What the pair was reaching for is measurable exactly, and is the physical
    /// quantity rather than a proxy for it: **how much of what is behind the
    /// tree the tree replaces.** A filled, bloomed plate covers its own rect at
    /// alpha 1 and hazes 26–28 px past it; a glass pane at
    /// [`measured::GLASS_FACE_ALPHA`] with a thin bright edge and no bloom
    /// covers a small fraction of it. That is the number gated below, and it is
    /// the one H7 is about.
    #[test]
    fn the_trees_ink_is_one_hue_family_and_replaces_little_of_what_is_behind_it() {
        let app = pixel_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let ground = backdrop_rgb(&app);
        let mut chromatic = 0u64;
        let mut in_band = 0u64;
        let mut alpha_sum = 0f64;
        let mut pixels = 0u64;
        for layer in &layers {
            let (w, h, px) = decode(layer);
            pixels += u64::from(w) * u64::from(h);
            for chunk in px.chunks_exact(4) {
                let alpha = f32::from(chunk[3]) / 255.0;
                alpha_sum += f64::from(alpha);
                // Source-over onto the panel, which is what the terminal does
                // with this image.
                let over = |channel: usize, ground: u8| {
                    f32::from(chunk[channel]) * alpha + f32::from(ground) * (1.0 - alpha)
                };
                let (r, g, b) = (over(0, ground.0), over(1, ground.1), over(2, ground.2));
                let luminance = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                if luminance < 25.0 {
                    continue;
                }
                let rgb = Rgb(r.round() as u8, g.round() as u8, b.round() as u8);
                let (hue, saturation, _) = rgb.to_hsl();
                // "Chromatic" is a real distinction: a near-grey pixel has a hue
                // that is arithmetic rather than colour, and counting it either
                // way would be counting noise.
                if saturation < 0.12 {
                    continue;
                }
                chromatic += 1;
                if (measured::HUE_BAND.0..=measured::HUE_BAND.1).contains(&hue) {
                    in_band += 1;
                }
            }
        }
        assert!(chromatic > 0, "the tree drew no chromatic ink at all");
        let band = in_band as f64 / chromatic as f64;
        assert!(
            band >= 0.99,
            "only {:.2}% of the tree's chromatic ink is inside H1's 175-265 band, \
             against the reference's 99.94%",
            band * 100.0
        );

        assert!(pixels > 0, "the tree published no pixels");
        let covered = alpha_sum / pixels as f64;
        assert!(
            covered <= 0.30,
            "the tree replaces {:.1}% of what is behind it — a glass pane at a tenth \
             of an alpha with a thin edge and no bloom cannot, so the card is a \
             filled plate again",
            covered * 100.0
        );
    }

    /// **A card's own interior is the panel's colour, whatever state it is
    /// in — the state lives on the border, not the body.**
    ///
    /// `GLASS_FACE` is a fixed tint (`measured.rs`), never a function of
    /// [`AgentState`]/severity, so the deep-interior pixel a card publishes
    /// must be the same colour on an idle card, a working card and a blocked
    /// one — the visible difference between states is carried entirely by
    /// the stroke (drawn at full alpha in the state's own ink) and the thin
    /// inner glow right at the edge, never by the face. Before the
    /// flight-deck circuit mockup restyle, `GLASS_FACE` was a bright cyan
    /// (`rgba(122,196,222,.10)`) that read as a colour wash across the whole
    /// card regardless of state; this only checks the second half of that —
    /// that the shared interior colour is dark and desaturated like the
    /// mockup's own `#0a1220` panel, not a light tint — because the first
    /// half (state-independence) held before the restyle too and was never
    /// the bug.
    #[test]
    fn a_cards_interior_is_state_independent_and_reads_as_the_panel() {
        let Some(font) = font::card_font(None) else {
            return; // No face on this machine.
        };
        let geometry = CardGeometry::new(21.0, false);
        let rect = RoundRect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 64.0,
            r: geometry.radius,
        };
        // No title, tidbit, register, mark or residue: the card's dead centre
        // is guaranteed plain glass, never a glyph, an icon plate or a
        // contour ring, so the sample can only be the face and the (edge-
        // decayed, and at this distance from every edge negligible) inner
        // glow.
        fn content(state: AgentState, stage: LifecycleStage, hues: [f32; 5]) -> CardContent {
            CardContent {
                title: String::new(),
                tidbit: None,
                register: None,
                state_label: String::new(),
                state,
                stage,
                severity: Severity::Clear,
                hues: StageHues(hues),
                ground: measured::CANVAS,
                theme: CardTheme::UNTHEMED,
                split_channels: false,
                seen: true,
                depth: 1,
                lifted: false,
                focused_space: false,
                mark: None,
                residue: 0,
                controls: ControlRail::default(),
                generate: 1.0,
                discharge: 0.0,
                spider: None,
                breath: 0.0,
                wash: None,
                crew: Vec::new(),
                bars: None,
            }
        }

        // Three states as far apart as the alphabet gets — failed (red),
        // idle (green) and working (gold) — each a completely different
        // `hues` array, which is what `CardInks`/the stroke gradient reads.
        let variants = [
            (AgentState::Blocked, LifecycleStage::Failed, [10.0; 5]),
            (AgentState::Idle, LifecycleStage::Running, [140.0; 5]),
            (AgentState::Working, LifecycleStage::Running, [45.0; 5]),
        ];

        let mut interiors = Vec::new();
        for (state, stage, hues) in variants {
            let content = content(state, stage, hues);
            let mut canvas = Canvas::new(220, 84);
            draw_card(
                &mut canvas,
                &PlacedCard {
                    rect,
                    content: &content,
                    geometry: CardGeometry::new(21.0, false),
                    crew: crew::CrewBands::default(),
                },
                font,
            );
            let cx = (rect.x + rect.w / 2.0) as u32;
            let cy = (rect.y + rect.h / 2.0) as u32;
            let idx = ((cy * 220 + cx) * 4) as usize;
            let px = canvas.rgba8();
            interiors.push((px[idx], px[idx + 1], px[idx + 2]));
        }

        let (r0, g0, b0) = interiors[0];
        for (i, &(r, g, b)) in interiors.iter().enumerate() {
            let delta = i32::from(r).abs_diff(i32::from(r0))
                + i32::from(g).abs_diff(i32::from(g0))
                + i32::from(b).abs_diff(i32::from(b0));
            assert!(
                delta <= 6,
                "variant[{i}]'s interior {:?} differs from variant[0]'s {:?} by \
                 {delta} — the state ink is leaking into the face rather than \
                 staying on the border and the edge glow",
                (r, g, b),
                (r0, g0, b0)
            );
        }

        let (_, saturation, lightness) = Rgb(r0, g0, b0).to_hsl();
        assert!(
            lightness < 0.25,
            "the card's own interior tint is L{:.2} — that reads as a light wash, \
             not the dark, neutral panel (#0a1220 is L0.08) the mockup draws",
            lightness
        );
        assert!(
            saturation < 0.70,
            "the card's own interior tint is S{:.2} — a saturated cyan wash, not \
             the mockup's desaturated navy panel",
            saturation
        );
    }

    /// **A card blooms in whole, at its own final position and size — never a
    /// clip and never a translation.**
    ///
    /// The card-bloom beat of [`super::motion::ArrivalCircuit`], on the
    /// pixels: a card part-way through its own bloom is drawn at its full
    /// width and height, fainter throughout rather than clipped down one
    /// side — which is what makes the arrival read as the card fading into
    /// existence in place rather than as a finished object sliding in, or as a
    /// wall being built brick by brick.
    #[test]
    fn a_blooming_card_is_drawn_whole_but_faint() {
        let Some(font) = font::card_font(None) else {
            return; // No face on this machine.
        };
        let geometry = CardGeometry::new(21.0, false);
        let rect = RoundRect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 64.0,
            r: geometry.radius,
        };
        let base = CardContent {
            title: "2ndmate-explore".into(),
            tidbit: Some("gas giant · 99 files · 2 moons".into()),
            register: Some(Caption {
                text: "streak 5 · T 13.4s · 23 revs".into(),
                tone: CaptionTone::Register,
            }),
            state_label: "working".into(),
            state: AgentState::Working,
            stage: LifecycleStage::Running,
            severity: Severity::Clear,
            hues: StageHues([196.0; 5]),
            ground: measured::CANVAS,
            theme: CardTheme::UNTHEMED,
            split_channels: false,
            seen: true,
            depth: 1,
            lifted: false,
            focused_space: false,
            mark: None,
            residue: 0,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            spider: None,
            breath: 0.0,
            wash: None,
            crew: Vec::new(),
            bars: None,
        };

        // Nothing at all through beats one and two: the light is still
        // travelling the tree and the card does not exist yet.
        let mut empty = Canvas::new(240, 90);
        let none = CardContent {
            generate: 0.0,
            title: base.title.clone(),
            tidbit: base.tidbit.clone(),
            register: base.register.clone(),
            state_label: base.state_label.clone(),
            crew: base.crew.clone(),
            bars: base.bars.clone(),
            ..base
        };
        draw_card(
            &mut empty,
            &PlacedCard {
                rect,
                content: &none,
                geometry: CardGeometry::new(21.0, false),
                crew: crew::CrewBands::default(),
            },
            font,
        );
        assert!(
            empty.rgba8().chunks_exact(4).all(|px| px[3] == 0),
            "a card whose light has not landed yet drew something"
        );

        // Half way: fainter everywhere the whole card is drawn, never clipped
        // to one side of it.
        let mut whole_canvas = Canvas::new(240, 90);
        let whole = CardContent {
            generate: 1.0,
            title: base.title.clone(),
            tidbit: base.tidbit.clone(),
            register: base.register.clone(),
            state_label: base.state_label.clone(),
            crew: base.crew.clone(),
            bars: base.bars.clone(),
            ..base
        };
        draw_card(
            &mut whole_canvas,
            &PlacedCard {
                rect,
                content: &whole,
                geometry: CardGeometry::new(21.0, false),
                crew: crew::CrewBands::default(),
            },
            font,
        );
        let mut half_canvas = Canvas::new(240, 90);
        let half = CardContent {
            generate: 0.5,
            title: whole.title.clone(),
            tidbit: whole.tidbit.clone(),
            register: whole.register.clone(),
            state_label: whole.state_label.clone(),
            ..whole
        };
        draw_card(
            &mut half_canvas,
            &PlacedCard {
                rect,
                content: &half,
                geometry: CardGeometry::new(21.0, false),
                crew: crew::CrewBands::default(),
            },
            font,
        );
        // The card's own right half — well past where the old left-to-right
        // reveal would have clipped a 0.5-generated card — carries just as
        // much ink at half opacity as it does settled, only fainter.
        let mut lit_on_the_right_half = 0;
        let mut whole_total_alpha: u64 = 0;
        let mut half_total_alpha: u64 = 0;
        let right_half_left = (rect.x + rect.w * 0.6) as u32;
        for y in 0..90u32 {
            for x in 0..240u32 {
                let idx = ((y * 240 + x) * 4 + 3) as usize;
                let whole_alpha = whole_canvas.rgba8()[idx];
                let half_alpha = half_canvas.rgba8()[idx];
                whole_total_alpha += u64::from(whole_alpha);
                half_total_alpha += u64::from(half_alpha);
                if x >= right_half_left && half_alpha > 0 {
                    lit_on_the_right_half += 1;
                }
            }
        }
        assert!(
            lit_on_the_right_half > 0,
            "a half-blooming card drew nothing on its own right half — it was \
             clipped rather than faded"
        );
        assert!(
            half_total_alpha > 0 && half_total_alpha < whole_total_alpha,
            "a half-blooming card ({half_total_alpha}) should carry less total \
             ink than a settled one ({whole_total_alpha}), never the same or more"
        );
    }

    /// **A working pane's discharge cannot make it read as opaque.**
    ///
    /// H10's own constraint, and it is why the filaments are drawn *behind* the
    /// face rather than on it: the discharge is a working row saying its work is
    /// live, and a row that stopped being see-through to say so would have
    /// traded the material for the signal. Measured at full load, against the
    /// same card with no traffic at all.
    #[test]
    fn a_full_discharge_leaves_the_pane_see_through() {
        let Some(font) = font::card_font(None) else {
            return; // No face on this machine.
        };
        let mut canvas_quiet = Canvas::new(240, 90);
        let mut canvas_loud = Canvas::new(240, 90);
        let geometry = CardGeometry::new(21.0, false);
        let rect = RoundRect {
            x: 10.0,
            y: 10.0,
            w: 200.0,
            h: 64.0,
            r: geometry.radius,
        };
        // No type on it: a glyph is opaque by necessity — it has to be legible —
        // so a card carrying words would report its own title's alpha rather
        // than its face's. What is under test is the material.
        let content = CardContent {
            title: String::new(),
            tidbit: None,
            register: None,
            state_label: String::new(),
            state: AgentState::Working,
            stage: LifecycleStage::Running,
            severity: Severity::Clear,
            hues: StageHues([196.0; 5]),
            ground: measured::CANVAS,
            theme: CardTheme::UNTHEMED,
            split_channels: false,
            seen: true,
            depth: 1,
            lifted: false,
            focused_space: false,
            mark: None,
            residue: 0,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            spider: None,
            breath: 0.0,
            wash: None,
            crew: Vec::new(),
            bars: None,
        };
        let quiet = PlacedCard {
            rect,
            content: &content,
            geometry: CardGeometry::new(21.0, false),
            crew: crew::CrewBands::default(),
        };
        draw_card(&mut canvas_quiet, &quiet, font);

        let loud_content = CardContent {
            discharge: 1.0,
            title: content.title.clone(),
            tidbit: None,
            register: None,
            state_label: content.state_label.clone(),
            ..content
        };
        let loud = PlacedCard {
            rect,
            content: &loud_content,
            geometry: CardGeometry::new(21.0, false),
            crew: crew::CrewBands::default(),
        };
        draw_card(&mut canvas_loud, &loud, font);

        // Somewhere inside the face, clear of the edges and of the type.
        let mut moved = 0;
        let mut worst_alpha = 0u8;
        for y in 20..64u32 {
            for x in 20..200u32 {
                let at = ((y * 240 + x) * 4) as usize;
                let quiet_alpha = canvas_quiet.rgba8()[at + 3];
                let loud_alpha = canvas_loud.rgba8()[at + 3];
                if loud_alpha != quiet_alpha {
                    moved += 1;
                }
                worst_alpha = worst_alpha.max(loud_alpha);
            }
        }
        assert!(
            moved > 0,
            "a card at full traffic is byte-identical to one at none, so the \
             discharge is not drawn at all"
        );
        // And the loudest pixel of the face is still glass. 200/255 leaves the
        // scene behind measurably present at every point of it; the card's own
        // edge is opaque and is deliberately outside the sampled band.
        assert!(
            worst_alpha < 200,
            "a working pane's face reaches alpha {worst_alpha}, which is a plate \
             rather than glass"
        );
    }

    /// **H7: the scene is measurably visible through a card's face.**
    ///
    /// The clause's own test, and it is a difference rather than a threshold:
    /// sample the face with something behind it and with nothing behind it, and
    /// require the two to differ. A card that occludes what it stands on gives
    /// the same answer both times whatever its alpha says.
    #[test]
    fn what_is_behind_a_card_reaches_through_its_face() {
        let app = pixel_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let mut checked = 0;
        for (layer, frame) in layers.iter().zip(framed(&app)) {
            let (w, h, px) = decode(layer);
            // The middle of the card's own face, clear of its edges, its text
            // and its control rail: a quarter of the way in from the left, on
            // the row its own centre falls on.
            let x = u32::from(frame.x.saturating_sub(layer.rect.x)) * app.host_cell_size.width_px
                + u32::from(frame.width) * app.host_cell_size.width_px / 4;
            let y = u32::from(frame.y.saturating_sub(layer.rect.y)) * app.host_cell_size.height_px
                + u32::from(frame.height) * app.host_cell_size.height_px / 2;
            if x >= w || y >= h {
                continue;
            }
            let at = ((y * w + x) * 4) as usize;
            let alpha = f32::from(px[at + 3]) / 255.0;
            let face = (px[at], px[at + 1], px[at + 2]);
            // Two very different things behind the same pixel of face.
            let composite = |ground: (u8, u8, u8)| {
                (
                    f32::from(face.0) * alpha + f32::from(ground.0) * (1.0 - alpha),
                    f32::from(face.1) * alpha + f32::from(ground.1) * (1.0 - alpha),
                    f32::from(face.2) * alpha + f32::from(ground.2) * (1.0 - alpha),
                )
            };
            let over_void = composite((6, 9, 16));
            let over_star = composite((240, 236, 220));
            let difference = (over_void.0 - over_star.0)
                .abs()
                .max((over_void.1 - over_star.1).abs())
                .max((over_void.2 - over_star.2).abs());
            assert!(
                difference > 40.0,
                "a card's face changed by only {difference:.1} between empty sky and a \
                 star behind it (alpha {alpha:.2}), so nothing is showing through it"
            );
            checked += 1;
        }
        assert!(checked > 0, "no card face was sampled");
    }

    /// **The sheet's corners are transparent too, and that is the fix.**
    ///
    /// This test used to be called `the_sheet_is_an_opaque_rectangle_and_that_is
    /// _the_bug`, and it required the opposite: the sheet painted its backdrop
    /// over every cell of every row, so the rectangle reached the corner at full
    /// alpha. Its own failure message said what to do if that ever became
    /// deliberate, and it has:
    ///
    /// > *the sheet stopped painting a background — if that is deliberate, it
    /// > has converged with the shapes path and one of the two should go*
    ///
    /// A card is glass now (H7): its face is a tenth of an alpha, the panel and
    /// the whole-terminal scene are measurably visible *through* it, and an
    /// opaque plate underneath would make that a lie. So both models are
    /// transparent, and the character card stands down under both — see
    /// [`shape_covers_row`].
    ///
    /// **The two paths have not converged, and neither should go.** What they
    /// differ in is *packaging*, not material: the sheet is one image and one
    /// placement covering the whole tree, and shapes are one image and one
    /// placement per card. That is what buys a per-card arrival, a per-card
    /// carry-forward on an unchanged signature, and a moved card costing one
    /// placement rather than the tree's artwork — and it is what costs a host
    /// one upload per card instead of one. The choice is still real.
    #[test]
    fn the_sheet_paints_no_background_behind_its_cards_either() {
        let app = pixel_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let opaque_corner = framed(&app)
            .into_iter()
            .any(|frame| frame_corner_alphas(&layers[0], frame, app.host_cell_size).contains(&255));
        assert!(
            !opaque_corner,
            "the sheet is painting an opaque rectangle behind its cards again, so \
             nothing shows through the glass"
        );
    }

    /// Cards may overlap, because there is no box to clip.
    ///
    /// Their images do overlap already: each reaches a bloom's width past its own
    /// row and onto its neighbour's. Under the sheet that overlap had to be
    /// resolved *inside one raster* — which is exactly why the tree could only
    /// ever be one picture. As separate placements the terminal composites them,
    /// measured on a real Kitty and recorded in
    /// `data/herdr-card-as-alpha-shape/blend-test/`.
    #[test]
    fn shapes_overlap_instead_of_tiling() {
        let app = shape_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let overlaps = layers.iter().enumerate().any(|(i, a)| {
            layers.iter().skip(i + 1).any(|b| {
                a.rect.x < b.rect.x + b.rect.width
                    && b.rect.x < a.rect.x + a.rect.width
                    && a.rect.y < b.rect.y + b.rect.height
                    && b.rect.y < a.rect.y + a.rect.height
            })
        });
        assert!(
            overlaps,
            "no two cards overlapped, so nothing here proves they can"
        );
    }

    /// A card whose content did not change is not redrawn when a sibling's did.
    ///
    /// The whole point of cutting the tree into objects: moving one card must
    /// cost one card. Under the sheet any change re-rasterised and re-encoded the
    /// entire tree, which is what made the queued motion work unaffordable.
    #[test]
    fn changing_one_card_leaves_its_siblings_untouched() {
        let app = shape_fleet_app();
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let CardsUpdate::Rebuilt(first) =
            build_cards(&app, &cards, sidebar_rect(), app.host_cell_size, &[]).update
        else {
            return; // No face on this machine.
        };
        assert!(matches!(
            build_cards(&app, &cards, sidebar_rect(), app.host_cell_size, &first).update,
            CardsUpdate::Unchanged
        ));

        // Change what exactly one card says, and nothing else about the tree.
        let mut moved = app;
        let pane = cards
            .iter()
            .find_map(|card| card.agent.as_ref())
            .expect("the fleet has no agent card to change");
        let terminal_id = moved.workspaces[0].tabs[0]
            .panes
            .iter()
            .find(|(id, _)| **id == pane.pane_id)
            .map(|(_, state)| state.attached_terminal_id.clone())
            .expect("the changed pane has no terminal");
        let now = moved.state_age_now;
        moved
            .terminals
            .get_mut(&terminal_id)
            .expect("the changed terminal went away")
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([(
                    "doing".to_string(),
                    Some("Rewiring the card's own outline".to_string()),
                )]),
                None,
                now,
            );
        let CardsUpdate::Rebuilt(second) =
            build_cards(&moved, &cards, sidebar_rect(), moved.host_cell_size, &first).update
        else {
            panic!("a tree whose card changed did not rebuild");
        };
        assert_eq!(first.len(), second.len());

        let changed = first
            .iter()
            .zip(&second)
            .filter(|(a, b)| a.signature != b.signature)
            .count();
        assert!(changed > 0, "nothing rebuilt, so the cache is a freeze");
        assert!(
            changed < first.len(),
            "every card rebuilt because one of them changed — that is the sheet's \
             cost with more steps"
        );
        for (a, b) in first.iter().zip(&second) {
            if a.signature == b.signature {
                assert_eq!(
                    a.layer.data_fingerprint, b.layer.data_fingerprint,
                    "an unchanged card was re-encoded anyway"
                );
            }
        }
    }

    /// The thread bound, pinned for one test and released however that test
    /// ends.
    ///
    /// Serialised against the other tests that pin it, so this holds under a
    /// parallel harness as well as the single-threaded one the suite is run
    /// with.
    struct ThreadPin(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    static PIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl ThreadPin {
        fn at(threads: usize) -> Self {
            // A test that panicked while pinned poisoned this; the data it
            // guards is `()`, so there is nothing to be suspicious of.
            let guard = PIN_LOCK.lock().unwrap_or_else(|held| held.into_inner());
            RASTER_THREADS_FOR_TEST.store(threads, std::sync::atomic::Ordering::Relaxed);
            Self(guard)
        }
    }

    impl Drop for ThreadPin {
        fn drop(&mut self) {
            RASTER_THREADS_FOR_TEST.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Everything about a published card that a client can observe.
    ///
    /// The encoded bytes are in it, which is the whole point: two builds that
    /// agree here agree on the pixels the terminal is handed, not merely on the
    /// geometry around them.
    #[derive(PartialEq, Eq)]
    struct Observable<'a> {
        rect: Rect,
        clip: Rect,
        signature: u64,
        content_signature: u64,
        viewport: (i32, i32),
        image: (u32, u32),
        data: &'a [u8],
    }

    fn observable(layer: &SidebarCardLayer) -> Observable<'_> {
        Observable {
            rect: layer.rect,
            clip: layer.clip,
            signature: layer.signature,
            content_signature: layer.content_signature,
            viewport: layer.viewport(),
            image: (layer.layer.image_width, layer.layer.image_height),
            data: layer.layer.data.as_slice(),
        }
    }

    /// Build the tree's cards at a pinned thread count.
    fn built_on(app: &AppState, threads: usize, previous: &[SidebarCardLayer]) -> CardsUpdate {
        let _pin = ThreadPin::at(threads);
        let cards = super::super::compute_workspace_card_areas(app, sidebar_rect());
        build_cards(app, &cards, sidebar_rect(), app.host_cell_size, previous).update
    }

    /// Drawing the cards on many threads produces the same bytes as drawing them
    /// on one.
    ///
    /// This is the contract the parallel path is allowed to exist under, and it
    /// is asserted on the encoded PNG rather than on anything about how the work
    /// was scheduled. A card's pixels are a pure function of the rasteriser, that
    /// card's own content and its own held base, and results land in slots keyed
    /// by index — so neither the number of threads nor the order they finish in
    /// may reach the output. If it ever does, this fails on the bytes.
    #[test]
    fn a_parallel_rebuild_is_byte_identical_to_a_serial_one() {
        let app = shape_fleet_app();
        let CardsUpdate::Rebuilt(serial) = built_on(&app, 1, &[]) else {
            return; // No face on this machine.
        };
        assert!(serial.len() > 1, "one card cannot show that several agree");

        // Past the real bound as well as under it, so the assertion covers a
        // machine wider than this one and a queue that hands several cards to
        // the same thread.
        for threads in [2usize, 3, 6, 16] {
            let CardsUpdate::Rebuilt(parallel) = built_on(&app, threads, &[]) else {
                panic!("a rebuild on {threads} threads produced no cards");
            };
            assert_eq!(
                serial.len(),
                parallel.len(),
                "{threads} threads published a different number of cards"
            );
            for (index, (a, b)) in serial.iter().zip(&parallel).enumerate() {
                assert!(
                    observable(a) == observable(b),
                    "card {index} differs between 1 thread and {threads}"
                );
            }
        }
    }

    /// Threads do not make cards rasterise more often than one thread does.
    ///
    /// The cache is the reason the steady-state cost of this whole path is a
    /// fraction of a millisecond, and parallelising the expensive half would be a
    /// net loss if it cost the cheap half its hit rate. So: a settled tree
    /// reports `Unchanged` at every thread count, and changing exactly one card
    /// rebuilds exactly the cards that changed — no more at six threads than at
    /// one.
    #[test]
    fn threads_do_not_change_how_often_a_card_rasterises() {
        const LADDER: [usize; 4] = [1, 2, 6, 16];
        let mut app = shape_fleet_app();
        let CardsUpdate::Rebuilt(first) = built_on(&app, 1, &[]) else {
            return; // No face on this machine.
        };

        // A settled tree, first: whatever the thread count, a frame that changed
        // nothing must rasterise nothing.
        for threads in LADDER {
            assert!(
                matches!(built_on(&app, threads, &first), CardsUpdate::Unchanged),
                "a settled tree rebuilt on {threads} threads"
            );
        }

        // Change what exactly one card says, and nothing else about the tree.
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let pane = cards
            .iter()
            .find_map(|card| card.agent.as_ref())
            .expect("the fleet has no agent card to change");
        let terminal_id = app.workspaces[0].tabs[0]
            .panes
            .iter()
            .find(|(id, _)| **id == pane.pane_id)
            .map(|(_, state)| state.attached_terminal_id.clone())
            .expect("the changed pane has no terminal");
        let now = app.state_age_now;
        app.terminals
            .get_mut(&terminal_id)
            .expect("the changed terminal went away")
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([(
                    "doing".to_string(),
                    Some("Rewiring the card's own outline".to_string()),
                )]),
                None,
                now,
            );

        let mut rebuilt_counts = Vec::new();
        for threads in LADDER {
            let CardsUpdate::Rebuilt(second) = built_on(&app, threads, &first) else {
                panic!("a tree whose card changed did not rebuild on {threads} threads");
            };
            for (a, b) in first.iter().zip(&second) {
                if a.signature == b.signature {
                    assert_eq!(
                        a.layer.data_fingerprint, b.layer.data_fingerprint,
                        "an unchanged card was re-encoded on {threads} threads"
                    );
                }
            }
            rebuilt_counts.push(
                first
                    .iter()
                    .zip(&second)
                    .filter(|(a, b)| a.signature != b.signature)
                    .count(),
            );
        }
        assert!(
            rebuilt_counts[0] > 0,
            "nothing rebuilt, so the cache is a freeze and this proves nothing"
        );
        assert!(
            rebuilt_counts
                .iter()
                .all(|count| *count == rebuilt_counts[0]),
            "the number of cards rasterised moved with the thread count: {rebuilt_counts:?}"
        );
    }

    /// The thread bound never exceeds the work, the cap, or half the machine.
    ///
    /// The cap is a promise to the fleet sharing this box, so it is asserted
    /// rather than left to be read off the source.
    #[test]
    fn the_raster_thread_bound_stays_inside_its_budget() {
        for work in [0usize, 1, 2, 5, 12, 64] {
            let threads = raster_threads(work);
            assert!(threads >= 1, "{work} cards were given no thread at all");
            assert!(
                threads <= CARD_RASTER_MAX_THREADS,
                "{work} cards took {threads} threads, past the cap"
            );
            assert!(
                threads <= work.max(1),
                "{work} cards took {threads} threads, more than there is work for"
            );
        }
        assert_eq!(raster_threads(1), 1, "one card spawned a thread to draw it");
    }

    /// A tree of exactly `cards` rows, on a panel tall enough to frame all of
    /// them.
    ///
    /// The fleet the other tests use is ten rows, which is what the reference
    /// artwork was drawn against. The rebuild measurement wants twelve, because
    /// twelve is the number every figure in `data/herdr-compute-offload-map/`
    /// was taken at.
    #[cfg(test)]
    fn raster_fleet_app(cards: usize) -> (AppState, Rect) {
        let mut app = shape_fleet_app();
        let now = app.state_age_now;
        while app.workspaces[0].tabs[0].panes.len() < cards {
            let index = app.workspaces[0].tabs[0].panes.len();
            let pane = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
            app.ensure_test_terminals();
            let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
                .attached_terminal_id
                .clone();
            let row = &tests::FLEET[index % tests::FLEET.len()];
            let Some(terminal) = app.terminals.get_mut(&terminal_id) else {
                continue;
            };
            // Distinct per row, so no two cards hash the same and the rebuild
            // really does draw every one of them.
            terminal.set_agent_name(format!("{}-{index}", row.name));
            terminal.state = row.state;
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([
                    ("doing".to_string(), Some(format!("{} {index}", row.doing))),
                    ("project".to_string(), Some(row.project.to_string())),
                    ("context".to_string(), Some(row.context.to_string())),
                    // Hung off the fleet's own root. A worker naming no owner is
                    // not reachable from the tree root and never becomes a row,
                    // so it would be a pane that costs nothing to draw.
                    ("owner".to_string(), Some("firstmate".to_string())),
                ]),
                None,
                now,
            );
            terminal.last_agent_state_change_at = Some(now - std::time::Duration::from_secs(31));
        }
        // Tall enough that every row gets a frame; a row the layout cannot fit
        // is a row that is never rasterised.
        let rect = Rect::new(0, 0, sidebar_rect().width, 8 + 8 * cards as u16);
        (app, rect)
    }

    /// What parallelising the per-card rasterisation actually bought, measured
    /// rather than reasoned about.
    ///
    /// Ignored by default because it prints tables and times things; run it with
    /// `cargo test --release --bin herdr card_raster_cost -- --ignored
    /// --nocapture`. Release matters: a debug build reports several times the
    /// per-card cost and would flatter the speedup.
    ///
    /// Two tables, because the average and the tail are different questions.
    /// The first is the rebuild itself across a thread ladder — the speedup. The
    /// second is a burst: alternating frames where the tree's content changes
    /// and frames where it does not, which is the shape that produces the
    /// stall the parallel path exists to remove, reported as a distribution
    /// rather than a mean.
    #[test]
    #[ignore = "measurement, not an assertion: run with --ignored --nocapture"]
    fn card_raster_cost() {
        const CARDS: usize = 12;
        const RUNS: usize = 60;

        let (app, rect) = raster_fleet_app(CARDS);
        let cell = app.host_cell_size;
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let framed = cards
            .iter()
            .filter(|card| card.card_frame.is_some())
            .count();
        {
            let _pin = ThreadPin::at(1);
            if !matches!(
                build_cards(&app, &cards, rect, cell, &[]).update,
                CardsUpdate::Rebuilt(_)
            ) {
                println!("SKIP: no proportional face on this machine");
                return;
            }
        }
        println!(
            "panel {}x{} cells, host cell {}x{} px, {framed} framed cards, \
             {} cores, bound {} threads",
            rect.width,
            rect.height,
            cell.width_px,
            cell.height_px,
            std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(0),
            raster_threads(framed),
        );

        // A cold rebuild: no held artwork at all, so every card is drawn. This
        // is the frame that costs, and the one the whole change is about.
        println!("\nthreads | med ms | p90 ms | p99 ms |  max ms | speedup");
        println!("--------+--------+--------+--------+---------+--------");
        let mut serial_median = 0.0f64;
        for threads in [1usize, 2, 4, 6, 8, 12, 16] {
            let _pin = ThreadPin::at(threads);
            let mut samples = Vec::with_capacity(RUNS);
            for _ in 0..RUNS {
                let started = std::time::Instant::now();
                let update = build_cards(&app, &cards, rect, cell, &[]).update;
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                assert!(matches!(update, CardsUpdate::Rebuilt(_)));
            }
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let at = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
            if threads == 1 {
                serial_median = at(0.5);
            }
            println!(
                "{threads:>7} | {:>6.2} | {:>6.2} | {:>6.2} | {:>7.2} | {:>5.2}x",
                at(0.5),
                at(0.9),
                at(0.99),
                samples[samples.len() - 1],
                serial_median / at(0.5),
            );
        }

        // The burst, through the whole per-frame pass rather than through
        // `build_cards` alone, because a rasterisation spike is only interesting
        // as a share of a frame. Every other frame changes one agent's `doing`
        // string — which is what a fleet under load actually does — and the
        // frames between are settled, so the run is a realistic mixture of
        // cache hits and rebuilds rather than a worst case held open.
        //
        // The distribution is the deliverable. A mean over this hides exactly
        // the stall the parallel path exists to remove: half these frames cost
        // nothing at all.
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 100, rect.height);
        println!(
            "\nburst of {RUNS} compute_view passes, one card's content changing every other frame"
        );
        println!("threads | med ms | p90 ms | p99 ms |  max ms | rebuilt frames");
        println!("--------+--------+--------+--------+---------+---------------");
        for threads in [1usize, raster_threads(framed)] {
            let (mut burst, _) = raster_fleet_app(CARDS);
            burst.sidebar_width = rect.width;
            let _pin = ThreadPin::at(threads);
            let mut samples = Vec::with_capacity(RUNS);
            let mut rebuilt = 0usize;
            // Off the drawn rows, in layout order, so every mutation lands on a
            // card that is actually rasterised and the two thread counts are
            // handed identical work. Taking them off the pane map instead would
            // hash-order them and include panes that are not rows.
            let terminals: Vec<_> = super::super::compute_workspace_card_areas(&burst, rect)
                .iter()
                .filter_map(|card| card.agent.as_ref())
                .filter_map(|agent| {
                    burst.workspaces[0].tabs[0]
                        .panes
                        .iter()
                        .find(|(id, _)| **id == agent.pane_id)
                        .map(|(_, pane)| pane.attached_terminal_id.clone())
                })
                .collect();
            let mut held: Vec<u64> = Vec::new();
            for frame in 0..RUNS {
                if frame % 2 == 0 {
                    let now = burst.state_age_now;
                    let id = &terminals[(frame / 2) % terminals.len()];
                    if let Some(terminal) = burst.terminals.get_mut(id) {
                        terminal.metadata_tokens.patch(
                            std::collections::HashMap::from([(
                                "doing".to_string(),
                                Some(format!("burst frame {frame}")),
                            )]),
                            None,
                            now,
                        );
                    }
                }
                let started = std::time::Instant::now();
                crate::ui::compute_view_with_cell_size(&mut burst, &runtimes, area, cell);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                let now: Vec<u64> = burst
                    .sidebar_card_layers
                    .iter()
                    .map(|layer| layer.signature)
                    .collect();
                if now != held {
                    rebuilt += 1;
                    held = now;
                }
            }
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let at = |q: f64| samples[((samples.len() as f64 * q) as usize).min(samples.len() - 1)];
            println!(
                "{threads:>7} | {:>6.2} | {:>6.2} | {:>6.2} | {:>7.2} | {rebuilt:>14}",
                at(0.5),
                at(0.9),
                at(0.99),
                samples[samples.len() - 1],
            );
        }
    }

    /// The selection highlight belongs to the card, and it stops at the card's
    /// own border.
    ///
    /// The defect, from the captain's screenshot: the cursor's row was washed
    /// with a flat `surface0` rectangle over *every cell the row owns*, and a
    /// row is wider and taller than the card standing on it. Under a drawn card
    /// that wash was the only part of the selection anyone could see, and what
    /// it looked like was a lit rectangle with a card floating inside it — the
    /// glow spilling past the wireframe border on both sides.
    ///
    /// Two halves, because either one alone is satisfied by a wrong rule.
    /// Painting nothing at all under a shape is only right if the *card* has
    /// taken the selection over, and lifting the card is only right if the row
    /// around it went back to being the panel.
    #[test]
    fn a_drawn_card_carries_the_selection_and_washes_nothing_around_itself() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 100, 46);

        // The same panel twice, differing only in which row the cursor is on.
        // Row 1 is the cursor's row in one and an ordinary row in the other,
        // and row 0 is the active Space in both, so the *only* thing this
        // comparison can see is what selecting a row does to its own cells.
        let panel = |cursor: usize| {
            let mut app = three_rank_pixel_app();
            app.sidebar_card_shapes = true;
            app.mode = crate::app::state::Mode::Navigate;
            app.active = Some(0);
            app.selected = cursor;
            app.sidebar_width = sidebar_rect().width;
            let cell = app.host_cell_size;
            crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
                    .expect("test backend");
            terminal
                .draw(|frame| {
                    super::super::render_sidebar(&app, &runtimes, frame, app.view.sidebar_rect)
                })
                .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            (app, buffer)
        };

        let (selected, on) = panel(1);
        let (_, off) = panel(0);
        if selected.sidebar_card_layers.is_empty() {
            return; // No face on this machine, so no shapes to stand on.
        }
        let row = *selected
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.agent.is_none() && card.ws_idx == 1)
            .expect("the second Space has a row");

        let mut washed: Vec<(u16, u16, ratatui::style::Color)> = Vec::new();
        for y in row.rect.y..row.rect.y + row.rect.height {
            for x in row.rect.x..row.rect.x + row.rect.width {
                if on[(x, y)].bg != off[(x, y)].bg {
                    washed.push((x, y, on[(x, y)].bg));
                }
            }
        }
        washed.truncate(8);
        assert!(
            washed.is_empty(),
            "selecting a row repainted cells under a drawn card, which is a \
             highlight the card's own border cannot contain, from: {washed:?}"
        );

        // And the card itself took it over. Without this the assertion above is
        // passed by a panel that simply stopped showing selection.
        let entries = super::super::workspace_list_entries(&selected);
        let entry = entries
            .iter()
            .find(|entry| matches!(entry, super::super::WorkspaceListEntry::Workspace { ws_idx, .. } if *ws_idx == 1))
            .expect("the second Space is a row of the tree");
        let content = content_for(
            &selected,
            entry,
            &[],
            &crate::ui::sidebar::body_register::BodyRegister::resolve(&selected),
        )
        .expect("the row has a card");
        assert!(
            content.lifted,
            "the cursor's row drew an unlifted card, so nothing at all says \
             which row the cursor is on"
        );
    }

    /// The character card's wash is clipped to the card too.
    ///
    /// Same defect, one path over: with no shapes the row still owns rails and a
    /// gutter the card does not, and a wash over all of it is a highlight
    /// outside the drawn border. Inside the frame it must still change, or the
    /// clip has been turned into a deletion.
    #[test]
    fn a_character_card_is_washed_only_inside_its_own_frame() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 100, 46);
        let panel = |cursor: usize| {
            let mut app = three_rank_pixel_app();
            app.kitty_graphics_enabled = false;
            app.kitty_graphics_capability_confirmed = false;
            app.sidebar_card_shapes = false;
            app.mode = crate::app::state::Mode::Navigate;
            app.active = Some(0);
            app.selected = cursor;
            app.sidebar_width = sidebar_rect().width;
            let cell = app.host_cell_size;
            crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(area.width, area.height))
                    .expect("test backend");
            terminal
                .draw(|frame| {
                    super::super::render_sidebar(&app, &runtimes, frame, app.view.sidebar_rect)
                })
                .expect("draw");
            let buffer = terminal.backend().buffer().clone();
            (app, buffer)
        };

        let (selected, on) = panel(1);
        let (_, off) = panel(0);
        let row = *selected
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.agent.is_none() && card.ws_idx == 1)
            .expect("the second Space has a row");
        let frame = row.card_frame.expect("a 42-column panel draws card shells");

        let mut outside: Vec<(u16, u16)> = Vec::new();
        let mut inside = 0usize;
        for y in row.rect.y..row.rect.y + row.rect.height {
            for x in row.rect.x..row.rect.x + row.rect.width {
                if on[(x, y)].bg == off[(x, y)].bg {
                    continue;
                }
                let within = x >= frame.x
                    && x < frame.x + frame.width
                    && y >= frame.y
                    && y < frame.y + frame.height;
                if within {
                    inside += 1;
                } else {
                    outside.push((x, y));
                }
            }
        }
        outside.truncate(8);
        assert!(
            outside.is_empty(),
            "the selection wash reached cells outside the card's frame \
             {frame:?}, from: {outside:?}"
        );
        assert!(
            inside > 0,
            "selecting the row changed nothing inside the card either"
        );
    }

    /// A card's glow stops at the notification tray, exactly as the sheet's did.
    ///
    /// Both publish at `z: 0` and the tray's badges are their own layer, so a
    /// card reaching into the tray would put its bloom on the badges with no
    /// defined order between them. Cutting the sheet into cards is where that
    /// clamp is easiest to lose: it used to be applied once, to the sheet, and is
    /// now applied to every card.
    #[test]
    fn no_shape_reaches_into_the_notification_tray() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let area = Rect::new(0, 0, 100, 46);
        let mut app = shape_fleet_app();
        app.sidebar_width = 42;
        app.sidebar_signal_tray.enabled = true;
        let cell_size = app.host_cell_size;
        crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
        if !is_available(
            &app,
            super::super::row_fold_width(&app, app.view.sidebar_rect),
        ) {
            return; // No face on this machine.
        }
        let content = super::super::sidebar_content_rect(app.view.sidebar_rect);
        let tray = super::super::tray::tray_rect(&app, content);
        assert!(tray.height > 0, "the tray is off, so this tests nothing");
        assert!(
            !app.sidebar_card_layers.is_empty(),
            "the tree drew no cards"
        );
        for layer in &app.sidebar_card_layers {
            assert!(
                layer.rect.y + layer.rect.height <= tray.y,
                "a card's image reached into the tray at row {}",
                tray.y
            );
        }
    }

    /// Below the card shell's own width the panel is characters, shapes or not.
    ///
    /// The fallback is not a property of the drawing model: a row too narrow for
    /// a card is a bare styled line, and a shape drawn over it would be a third
    /// layout. Both paths read the same [`is_available`].
    #[test]
    fn a_panel_too_narrow_for_a_card_gets_no_shapes() {
        let mut app = shape_fleet_app();
        app.sidebar_width = MIN_FOLD_WIDTH;
        // Claimed by the pass, so the veto under test is the width and not the
        // absence of artwork.
        app.view.sidebar_card_layers_published = true;
        let narrow = Rect::new(0, 0, MIN_FOLD_WIDTH, 46);
        assert!(
            !is_available(&app, super::super::row_fold_width(&app, narrow)),
            "a panel narrower than the card shell still tried to draw pixels"
        );
        assert!(
            !shape_covers_row(&app, super::super::row_fold_width(&app, narrow)),
            "a narrow panel suppressed its character cards and drew no shapes, \
             which is a blank row"
        );
        let cards = super::super::compute_workspace_card_areas(&app, narrow);
        assert!(matches!(
            build_cards(&app, &cards, narrow, app.host_cell_size, &[]).update,
            CardsUpdate::Empty
        ));
    }

    /// Graphics off is the character path, shapes flag or not.
    #[test]
    fn graphics_off_draws_no_shapes_and_hides_no_characters() {
        let mut app = shape_fleet_app();
        app.kitty_graphics_enabled = false;
        app.view.sidebar_card_layers_published = true;
        let fold = super::super::row_fold_width(&app, sidebar_rect());
        assert!(!is_available(&app, fold));
        assert!(
            !shape_covers_row(&app, fold),
            "the character cards were suppressed with no shape to replace them"
        );
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        assert!(matches!(
            build_cards(&app, &cards, sidebar_rect(), app.host_cell_size, &[]).update,
            CardsUpdate::Empty
        ));
    }

    /// A delegating pass suppresses the character cards even though it drew
    /// nothing, because the cards are coming — just not from here.
    ///
    /// This is the one way the skip in `build_cards_inner` could be worse than
    /// the cost it removes, and it is the same trap #95 named for the tray: a
    /// surface that stopped being drawn here is not a surface that stopped
    /// existing, and spelling the two the same way renders the tree twice. A
    /// shape is transparent outside its own glow, so the character card
    /// underneath one shows straight through it.
    #[test]
    fn a_pass_whose_cards_are_drawn_elsewhere_still_stands_the_characters_down() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        let mut drawing = shape_fleet_app();
        let Some(fold) = shape_pass(&mut drawing, &runtimes) else {
            return; // No face on this machine.
        };

        let mut delegating = shape_fleet_app();
        delegating.sidebar_card_graphics_client_rasterized = true;
        delegating.sidebar_width = sidebar_rect().width;
        let cell_size = delegating.host_cell_size;
        crate::ui::compute_view_with_cell_size(&mut delegating, &runtimes, pass_area(), cell_size);

        assert!(
            delegating.sidebar_card_layers.is_empty(),
            "the server drew card pixels no attached viewer would be sent"
        );
        assert!(
            delegating.view.sidebar_card_layers_published,
            "a delegating pass answered `no cards here` and left the tree to \
             draw its character cards under the client's shapes"
        );
        assert!(
            shape_covers_row(&delegating, fold),
            "the character cards were drawn under shapes the client is about to \
             lay over them"
        );

        // The placement stage walks exactly this list — see
        // `kitty_graphics::surface_layer_placement_targets` — so an empty one is
        // also the whole of "nothing stale is offered to a viewer that draws for
        // itself".
    }

    /// Characters are suppressed only where a shape actually got drawn.
    ///
    /// The suppression and the artwork are decided in two different places — the
    /// renderer asks [`shape_covers_row`], the artwork comes out of
    /// [`build_cards`] — and if those two ever disagree in this direction the
    /// tree renders blank: no character card, and no pixel card over it either.
    /// It is the one failure this change can produce that is worse than the bug
    /// it fixes, so it is asserted rather than reasoned about.
    #[test]
    fn characters_are_never_suppressed_without_a_shape_to_replace_them() {
        let app = shape_fleet_app();
        let fold = super::super::row_fold_width(&app, sidebar_rect());

        // Nothing published yet — the state a frame is in before its first
        // build, and the state it falls back to when a build fails.
        assert!(app.sidebar_card_layers.is_empty());
        assert!(
            !shape_covers_row(&app, fold),
            "the character cards were suppressed before any shape existed"
        );

        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let mut drawn = shape_fleet_app();
        let Some(fold) = shape_pass(&mut drawn, &runtimes) else {
            return; // No face on this machine.
        };
        assert!(
            shape_covers_row(&drawn, fold),
            "shapes were drawn and the character cards were drawn under them too"
        );

        // A build that fails publishes nothing, and the characters come straight
        // back. The cell here is the failure the pixel path actually has: a
        // cell-size report large enough to put every card's image past
        // `MAX_IMAGE_PIXELS`.
        let mut failed = shape_fleet_app();
        failed.host_cell_size = HostCellSize {
            width_px: 4000,
            height_px: 4000,
        };
        assert!(
            shape_pass(&mut failed, &runtimes).is_none(),
            "an image well past the pixel ceiling was published anyway"
        );
        let failed_fold = super::super::row_fold_width(&failed, failed.view.sidebar_rect);
        assert!(
            is_available(&failed, failed_fold),
            "the pixel path bowed out before the build could fail, so this \
             tests nothing"
        );
        assert!(
            !shape_covers_row(&failed, failed_fold),
            "a build that published nothing suppressed the character cards anyway"
        );
    }

    /// A pass that will not be sent the shapes keeps drawing its characters.
    ///
    /// The suppression is a property of the *pass* about to be encoded, never of
    /// the shared state: `AppState::sidebar_card_layers` and `host_cell_size` are
    /// the foreground client's, and a second attached client whose own cell size
    /// is unknown is rendered without one and then sent no images at all. Reading
    /// the shared layers there would leave that client's Spaces tree as bare
    /// connectors — every card's glow, tokens, frame and badge suppressed with
    /// nothing drawn over them.
    #[test]
    fn a_client_that_is_sent_no_images_keeps_its_character_cards() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let mut app = shape_fleet_app();
        let Some(fold) = shape_pass(&mut app, &runtimes) else {
            return; // No face on this machine.
        };
        assert!(shape_covers_row(&app, fold));

        // The same server, sizing a frame for a second client: no pane resize
        // and no cell size, which is what the headless server does for every
        // client that is not the foreground one.
        crate::ui::compute_view_without_resizing_panes(&mut app, &runtimes, pass_area());
        assert!(
            !app.sidebar_card_layers.is_empty(),
            "a pass that cannot see pixels threw away the foreground client's \
             artwork, which costs it a re-encode and a re-upload every frame"
        );
        assert!(
            !shape_covers_row(
                &app,
                super::super::row_fold_width(&app, app.view.sidebar_rect)
            ),
            "a client that is sent no images had its character cards suppressed \
             anyway, which draws the tree as bare connectors"
        );
    }

    /// And a pass that drew its character cards is sent no card images.
    ///
    /// The mirror of the case above, and the one a second *Kitty* client hits:
    /// its own cell size is known and graphics are on, so the encode side used
    /// to send it the foreground's shapes even though its own pass had drawn the
    /// character cards — a transparent shape standing over a border, a chip and
    /// a title a few pixels off. Both halves read the one pass fact, so the two
    /// cannot disagree in either direction.
    #[test]
    fn a_pass_that_drew_characters_is_sent_no_card_graphics() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let mut app = shape_fleet_app();
        app.mode = crate::app::Mode::Terminal;
        let Some(fold) = shape_pass(&mut app, &runtimes) else {
            return; // No face on this machine.
        };
        assert!(shape_covers_row(&app, fold));

        // The foreground client: its pass published, so it is sent the shapes.
        let cell_size = app.host_cell_size;
        let mut foreground = crate::kitty_graphics::HostGraphicsCache::default();
        let bytes = crate::kitty_graphics::encode_local_pane_graphics(
            &app,
            &runtimes,
            app.view.tab_surface(),
            cell_size,
            &mut foreground,
            crate::kitty_graphics::EmbeddedSurfaces::ALL,
            None,
        );
        assert!(
            !bytes.is_empty() && !foreground.is_empty(),
            "the client whose pass drew the shapes was sent none of them"
        );

        // A second app client with graphics on and a cell size of its own, whose
        // frame was sized through the non-resizing path.
        crate::ui::compute_view_without_resizing_panes(&mut app, &runtimes, pass_area());
        assert!(!shape_covers_row(
            &app,
            super::super::row_fold_width(&app, app.view.sidebar_rect)
        ));
        let mut second = crate::kitty_graphics::HostGraphicsCache::default();
        let bytes = crate::kitty_graphics::encode_local_pane_graphics(
            &app,
            &runtimes,
            app.view.tab_surface(),
            cell_size,
            &mut second,
            crate::kitty_graphics::EmbeddedSurfaces::ALL,
            None,
        );
        assert!(
            bytes.is_empty() && second.is_empty(),
            "a client that drew its character cards was sent the shapes too, \
             which doubles every border, chip and title a few pixels off"
        );
    }

    /// A host that never answered the capability probe keeps its character
    /// cards.
    ///
    /// The tree's half of the gap PR #101 named: `shape_covers_row` and the
    /// delivery gate were spelled from different conditions, and
    /// `kitty_graphics_capability_confirmed` was in one and not the other. On
    /// every terminal without Kitty Graphics Protocol support — which never
    /// answers the probe, so this is a permanent state and not a startup race —
    /// `update_sidebar_card_layers` published shapes, the character cards stood
    /// down for them, and `server::headless` encoded no graphics at all. The
    /// tree drew as bare connectors: exactly the failure
    /// `a_client_that_is_sent_no_images_keeps_its_character_cards` guards on the
    /// per-client axis, arrived at on the per-host one.
    #[test]
    fn a_host_that_never_confirmed_graphics_keeps_its_character_cards() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();

        // The control: a confirmed host draws shapes and suppresses characters,
        // so the assertions below are about the capability and not about this
        // machine having no face.
        let mut confirmed = shape_fleet_app();
        let Some(fold) = shape_pass(&mut confirmed, &runtimes) else {
            return; // No face on this machine.
        };
        assert!(shape_covers_row(&confirmed, fold));

        let mut app = shape_fleet_app();
        app.kitty_graphics_capability_confirmed = false;
        assert!(
            app.kitty_graphics_enabled && app.host_cell_size.is_known(),
            "the opt-in and the cell are what make this the interesting state"
        );
        assert!(
            shape_pass(&mut app, &runtimes).is_none(),
            "shapes were published for a host no delivery gate will send them to"
        );

        let fold = super::super::row_fold_width(&app, app.view.sidebar_rect);
        assert!(
            !is_available(&app, fold),
            "the pixel path claimed to be live on a host that never confirmed it"
        );
        assert!(
            !shape_covers_row(&app, fold),
            "the character cards were suppressed on a host that is sent no \
             images, which draws the tree as bare connectors"
        );

        // And the encode side agrees, which is what makes this one fact rather
        // than two that happen to match: nothing is withheld from a pass that
        // never published.
        let mut cache = crate::kitty_graphics::HostGraphicsCache::default();
        let bytes = crate::kitty_graphics::encode_local_pane_graphics(
            &app,
            &runtimes,
            app.view.tab_surface(),
            app.host_cell_size,
            &mut cache,
            crate::kitty_graphics::EmbeddedSurfaces::ALL,
            None,
        );
        assert!(
            bytes.is_empty() && cache.is_empty(),
            "a host that drew its character cards had card graphics encoded for it"
        );
    }

    /// The sheet is still sent to a pass that did not build it.
    ///
    /// The withhold above belongs to the shapes path alone, and this is its
    /// paired contrast — a gate collapsed to one term fails one of these two
    /// whichever way it collapses. A sheet is opaque over every cell a row owns,
    /// so it covers the character cards standing under it rather than doubling
    /// them, and the second client of two the same size sees pixel cards exactly
    /// as it did before shapes existed. Withholding it there would be this
    /// branch changing the default path, which it must not do.
    #[test]
    fn the_sheet_still_reaches_a_pass_that_did_not_build_it() {
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let mut app = pixel_fleet_app();
        app.mode = crate::app::Mode::Terminal;
        assert!(!app.sidebar_card_shapes, "this is the flag-off path");
        if shape_pass(&mut app, &runtimes).is_none() {
            return; // No face on this machine.
        }
        let cell_size = app.host_cell_size;

        crate::ui::compute_view_without_resizing_panes(&mut app, &runtimes, pass_area());
        assert!(
            !app.view.sidebar_card_layers_published,
            "the non-resizing pass built the sheet itself, so this tests nothing"
        );
        let mut second = crate::kitty_graphics::HostGraphicsCache::default();
        let bytes = crate::kitty_graphics::encode_local_pane_graphics(
            &app,
            &runtimes,
            app.view.tab_surface(),
            cell_size,
            &mut second,
            crate::kitty_graphics::EmbeddedSurfaces::ALL,
            None,
        );
        assert!(
            !bytes.is_empty() && !second.is_empty(),
            "the default sheet path stopped reaching a second client, which is \
             this branch changing behaviour with its flag off"
        );
    }

    /// Turning the flag on moves no row and changes no height.
    ///
    /// The captain paid for the 68 px base, D-MID density and two-line titles.
    /// This is a change to what a card's *edges* do, and the layout has to come
    /// out the same on both sides of the flag.
    #[test]
    fn the_shapes_path_moves_nothing_the_layout_settled() {
        let sheet = pixel_fleet_app();
        let shapes = shape_fleet_app();
        // Geometry only: two apps built in one test hand out different pane ids,
        // and a pane id is not something the drawing model has any business
        // being compared on.
        let geometry = |app: &AppState| {
            super::super::compute_workspace_card_areas(app, sidebar_rect())
                .into_iter()
                .map(|card| (card.rect, card.card_frame, card.entry_idx))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            geometry(&sheet),
            geometry(&shapes),
            "the drawing model moved the rows it was only supposed to redraw"
        );
        let fold = super::super::row_fold_width(&sheet, sidebar_rect());
        assert_eq!(
            row_height_cells(&sheet, fold),
            row_height_cells(&shapes, fold),
            "height changed with the drawing model"
        );
    }
}

/// A row arriving opens the space it needs, and a row leaving gives it back.
///
/// The captain's ask, in his words: *"these worker panes are being materialized
/// and swiped to the right and then to the left as the existing ones pan up and
/// pan down… that's what pans up and pans down to make room or absorb the
/// room."* Everything here is that sentence turned into an assertion, over the
/// real tree rather than over [`super::motion`]'s arithmetic.
#[cfg(test)]
mod rows_make_room_for_each_other {
    use super::tests::{pixel_fleet_app, sidebar_rect};
    use super::*;
    use std::time::{Duration, Instant};

    /// The composition the flag exists to allow: cards as shapes, rows moving,
    /// and the captain's dissolve still running on the cells underneath.
    fn moving_fleet() -> AppState {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::Slide;
        app.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::Dissolve;
        app.sidebar_animation.row_exit = crate::config::SidebarTokenEmphasis::Dissolve;
        app
    }

    /// Publish the row membership the app loop would, optionally holding one
    /// pane back so it is the only thing arriving on the pass after.
    fn publish(app: &mut AppState, now: Instant, without: Option<crate::layout::PaneId>) {
        let lifecycle = app.sidebar_row_lifecycle();
        let rows: Vec<_> = crate::ui::sidebar_agent_live_entries(app)
            .iter()
            .filter(|entry| Some(entry.pane_id) != without)
            .map(|entry| {
                (
                    crate::anim::ElementId::agent_row(entry.pane_id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(now, crate::anim::Family::AgentRow, &lifecycle, rows);
        let spaces: Vec<_> = app
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    crate::anim::ElementId::workspace_row(&workspace.id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(now, crate::anim::Family::WorkspaceRow, &lifecycle, spaces);
    }

    fn build(
        app: &AppState,
        cards: &[crate::app::state::WorkspaceCardArea],
        previous: &[SidebarCardLayer],
    ) -> Option<Vec<SidebarCardLayer>> {
        build_at(app, cards, sidebar_rect(), previous)
    }

    fn build_at(
        app: &AppState,
        cards: &[crate::app::state::WorkspaceCardArea],
        rect: Rect,
        previous: &[SidebarCardLayer],
    ) -> Option<Vec<SidebarCardLayer>> {
        match build_cards(app, cards, rect, app.host_cell_size, previous).update {
            CardsUpdate::Rebuilt(layers) => Some(layers),
            // No proportional face on this machine, or nothing moved.
            CardsUpdate::Unchanged | CardsUpdate::Empty | CardsUpdate::Delegated => None,
        }
    }

    /// A panel with room to spare, for the one test that has to *add* a row:
    /// [`sidebar_rect`] is already showing as many rows as it can hold, so a
    /// new pane there falls off the bottom instead of pushing anything.
    fn tall_rect() -> Rect {
        Rect::new(0, 0, sidebar_rect().width, 90)
    }

    /// The first agent row that has at least one row under it, so the test has
    /// both an "above" and a "below" to check.
    fn arriving_row(
        cards: &[crate::app::state::WorkspaceCardArea],
    ) -> (usize, crate::layout::PaneId) {
        cards
            .iter()
            .enumerate()
            .take(cards.len().saturating_sub(1))
            .find_map(|(index, card)| card.agent.as_ref().map(|agent| (index, agent.pane_id)))
            .expect("the fleet has no agent row with a sibling below it")
    }

    /// A fleet mid-arrival and the same fleet settled, from one setup.
    ///
    /// `None` when this machine has no proportional face, which is the same
    /// skip every other pixel-card test takes.
    fn mid_and_settled() -> Option<(Vec<SidebarCardLayer>, Vec<SidebarCardLayer>, usize, f32)> {
        let mut app = moving_fleet();
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let (index, pane) = arriving_row(&cards);

        // Everything but one row exists, and settles.
        let now = Instant::now();
        publish(&mut app, now, Some(pane));
        let settled_at = now + Duration::from_secs(2);
        app.anim.advance(settled_at);

        // Then it arrives, and this is its first frame.
        publish(&mut app, settled_at, None);
        let mid = build(&app, &cards, &[])?;

        // And this is the frame after its last.
        app.anim.advance(settled_at + Duration::from_secs(2));
        let settled = build(&app, &cards, &[])?;
        Some((mid, settled, index, row_span_cells(&cards, index)))
    }

    #[test]
    fn a_row_arriving_moves_every_row_below_it_and_none_above() {
        let Some((mid, settled, index, span)) = mid_and_settled() else {
            return;
        };
        assert_eq!(mid.len(), settled.len());
        assert!(index + 1 < mid.len(), "nothing below the arriving row");

        for slot in 0..index {
            assert_eq!(
                mid[slot].viewport(),
                settled[slot].viewport(),
                "row {slot} above the arrival moved"
            );
        }
        assert_eq!(
            mid[index].viewport().1,
            settled[index].viewport().1,
            "the arriving row made room for itself"
        );
        for slot in (index + 1)..mid.len() {
            assert_eq!(
                settled[slot].viewport().1 - mid[slot].viewport().1,
                span as i32,
                "row {slot} did not start where it was standing before the slot \
                 existed"
            );
        }
    }

    /// **F22 on the real render path: no row's placement ever carries a
    /// horizontal offset.**
    ///
    /// This test used to require the opposite. It was called
    /// `the_arriving_row_itself_starts_clear_of_the_panel_and_ends_home`, and it
    /// asserted an arriving row began a whole panel width off to the right and
    /// travelled home. That is exactly what F22 refuses, and the refusal is not
    /// stylistic: a card sliding across the panel is a *finished object being
    /// moved*, and the reference's card **blooms** in place, at its own final
    /// position, once the rail and the branch pointing at it have grown. See
    /// [`super::motion::ArrivalCircuit`].
    ///
    /// `super::motion`'s own `no_row_ever_carries_a_horizontal_entry_offset`
    /// sweeps the arithmetic; this reads the placement the pipeline actually
    /// publishes, mid-arrival and settled.
    #[test]
    fn no_row_is_ever_placed_off_the_column_the_layout_gave_it() {
        let Some((mid, settled, index, _)) = mid_and_settled() else {
            return;
        };
        for (label, strip) in [("mid-arrival", &mid), ("settled", &settled)] {
            for (slot, layer) in strip.iter().enumerate() {
                assert_eq!(
                    layer.viewport().0,
                    layer.rect.x as i32,
                    "row {slot} is placed {} columns off its own rect {label}",
                    layer.viewport().0 - layer.rect.x as i32
                );
            }
        }
        let _ = index;
    }

    /// The cost claim, and the reason motion is affordable at all.
    ///
    /// A pane appearing moves every row below it, which moves those cards'
    /// image rects — so a signature that knew where a card sat would rebuild the
    /// whole tree on the frame a slide *begins*, every time. It does not: the
    /// pixels are the same pixels, and they are carried over.
    #[test]
    fn a_row_appearing_re_places_its_siblings_without_redrawing_one_of_them() {
        let mut app = moving_fleet();
        let before_cards = super::super::compute_workspace_card_areas(&app, tall_rect());
        let Some(before) = build_at(&app, &before_cards, tall_rect(), &[]) else {
            return;
        };

        // A new worker in the fleet, which pushes everything below it down a row
        // and leaves what every other row says untouched. Focus is put back
        // where it was: selection is card *content*, so a split that stole it
        // would legitimately redraw two cards and this test would be measuring
        // that instead of the reflow.
        let focused = app.workspaces[0]
            .focused_pane_id()
            .expect("the fleet has a focused pane");
        let spawned = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        if let Some(tab) = app.workspaces[0].active_tab_mut() {
            tab.layout.focus_pane(focused);
        }
        app.ensure_test_terminals();
        // A pane with no published identity is not a tree row — it renders
        // empty and is left out — so the new worker has to say who it is before
        // it can push anything.
        let terminal_id = app.workspaces[0].tabs[0].panes[&spawned]
            .attached_terminal_id
            .clone();
        let now = app.state_age_now;
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("the new pane has no terminal");
        terminal.set_agent_name("newcomer".to_string());
        terminal.state = crate::detect::AgentState::Working;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([
                ("doing".to_string(), Some("Just arrived".to_string())),
                ("project".to_string(), Some("herdr".to_string())),
                // Under the *first* second mate, so the two groups below it are
                // the ones that have to move.
                ("owner".to_string(), Some("2ndmate-herdr".to_string())),
            ]),
            None,
            now,
        );
        let after_cards = super::super::compute_workspace_card_areas(&app, tall_rect());
        assert_eq!(
            after_cards.len(),
            before_cards.len() + 1,
            "the split did not add a row"
        );
        assert!(
            after_cards
                .iter()
                .any(|card| card.agent.as_ref().is_some_and(|a| a.pane_id == spawned)),
            "the spawned pane did not reach the tree"
        );
        let after = build_at(&app, &after_cards, tall_rect(), &before).expect("a new row rebuilds");

        // Every card that existed before is still the same drawing, wherever it
        // has been pushed to — matched by signature rather than by slot,
        // because an insertion in the middle shifts every slot under it.
        let mut carried = 0usize;
        let mut moved = 0usize;
        for old in &before {
            let Some(new) = after.iter().find(|new| new.signature == old.signature) else {
                panic!("a card was redrawn to arrive back at the pixels it already had");
            };
            assert_eq!(
                new.layer.data_fingerprint, old.layer.data_fingerprint,
                "a card kept its signature but was re-encoded anyway"
            );
            carried += 1;
            moved += usize::from(new.viewport() != old.viewport());
        }
        assert_eq!(carried, before.len());
        assert!(moved > 0, "no card was re-placed, so nothing reflowed");
    }

    /// Motion never lets a card be drawn over the terminal panes.
    ///
    /// A slide deliberately puts a card past the panel's right edge. What stops
    /// that from spilling is the clip box, and that box is the same one every
    /// settled card is already held inside — so nothing about the panel's
    /// footprint changes, in motion or out of it.
    #[test]
    fn a_card_in_motion_is_clipped_to_the_panel_it_belongs_to() {
        let Some((mid, settled, _, _)) = mid_and_settled() else {
            return;
        };
        let bounds = super::super::sidebar_content_rect(sidebar_rect());
        for layer in mid.iter().chain(&settled) {
            assert!(
                layer.clip.x >= bounds.x
                    && layer.clip.x + layer.clip.width <= bounds.x + bounds.width,
                "a card's clip box reached outside the panel: {:?} against {bounds:?}",
                layer.clip
            );
            assert!(
                layer.clip.y >= bounds.y
                    && layer.clip.y + layer.clip.height <= bounds.y + bounds.height,
                "a card's clip box reached outside the panel vertically: {:?}",
                layer.clip
            );
        }
    }

    /// Below the card shell's width the panel is characters, and characters do
    /// not move.
    ///
    /// A glyph cannot leave its cell — the same property [`crate::anim`] is
    /// built around, and the reason the behaviour catalogue resolves colour and
    /// coverage but never position. So there is nothing to slide there, and the
    /// honest fallback is the one the panel already has: a row appears and
    /// disappears on the frame the layout says it does. What this pins is that
    /// turning motion on changes *nothing* about that path, so the fallback
    /// cannot drift away from it.
    #[test]
    fn a_panel_too_narrow_for_a_card_renders_identically_with_motion_on() {
        use ratatui::{backend::TestBackend, Terminal};

        // Narrow enough that the card shell is off, which is the same threshold
        // the pixel path uses.
        let narrow = Rect::new(0, 0, MIN_FOLD_WIDTH, 40);
        let rendered = |motion: crate::config::SidebarRowMotion| -> Vec<String> {
            let mut app = moving_fleet();
            app.sidebar_animation.row_motion = motion;
            app.sidebar_width = narrow.width;
            app.sidebar_max_width = narrow.width;
            let cards = super::super::compute_workspace_card_areas(&app, narrow);
            let (_, pane) = arriving_row(&cards);

            let now = Instant::now();
            publish(&mut app, now, Some(pane));
            let settled = now + Duration::from_secs(2);
            app.anim.advance(settled);
            publish(&mut app, settled, None);
            // Part-way into the arrival, which is the only moment a slide could
            // have moved anything.
            app.anim
                .advance(settled + Duration::from_millis(app.sidebar_animation.row_enter_ms / 2));

            app.view.sidebar_rect = narrow;
            app.view.workspace_card_areas = cards;
            assert!(
                matches!(
                    build_cards(
                        &app,
                        &app.view.workspace_card_areas,
                        narrow,
                        app.host_cell_size,
                        &[]
                    )
                    .update,
                    CardsUpdate::Empty
                ),
                "a panel this narrow drew a card, so this is not the fallback"
            );

            let mut terminal =
                Terminal::new(TestBackend::new(narrow.width, narrow.height)).expect("backend");
            terminal
                .draw(|frame| {
                    super::super::render_sidebar(
                        &app,
                        &crate::terminal::TerminalRuntimeRegistry::new(),
                        frame,
                        narrow,
                    )
                })
                .expect("draws");
            let buffer = terminal.backend().buffer();
            (0..narrow.height)
                .map(|row| {
                    (0..narrow.width)
                        .map(|col| buffer[(col, row)].symbol())
                        .collect::<String>()
                })
                .collect()
        };

        assert_eq!(
            rendered(crate::config::SidebarRowMotion::Slide),
            rendered(crate::config::SidebarRowMotion::None),
            "motion reached the character fallback, which cannot express it"
        );
    }

    /// The tree's connectors travel with the cards they point at.
    ///
    /// A card is pixels and the `├─ ` beside it is a character, drawn by two
    /// different renderers — so "the character path is the authority on where a
    /// row is" once meant the rails stayed at the layout's row while the
    /// artwork slid four rows away from them, and every card in the panel was
    /// visibly detached from its own connector for the whole of an arrival.
    /// They can move together exactly because the offset is quantized to whole
    /// cells once and both read that one number.
    ///
    /// What this pins is the reflow as a whole: with a row's slot still closed,
    /// the tree below it is drawn precisely where it stood before that slot
    /// existed.
    #[test]
    fn the_tree_below_an_arrival_is_drawn_where_the_cards_below_it_are_placed() {
        use ratatui::{backend::TestBackend, Terminal};

        // Wider than the panel so the layout is the desktop one; the rails live
        // in the panel's first few columns whatever the rest of the screen is.
        let area = Rect::new(0, 0, 100, sidebar_rect().height);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::new();
        let strip = |app: &AppState| -> Vec<String> {
            let sidebar = app.view.sidebar_rect;
            let mut terminal =
                Terminal::new(TestBackend::new(area.width, area.height)).expect("backend");
            terminal
                .draw(|frame| super::super::render_sidebar(app, &runtimes, frame, sidebar))
                .expect("draws");
            let buffer = terminal.backend().buffer();
            (0..area.height)
                .map(|row| {
                    (0..RAIL_COLUMNS)
                        .map(|col| buffer[(col, row)].symbol())
                        .collect::<String>()
                })
                .collect()
        };

        let mut app = moving_fleet();
        let cell_size = app.host_cell_size;
        app.mode = crate::app::Mode::Terminal;
        app.sidebar_width = sidebar_rect().width;
        app.sidebar_max_width = sidebar_rect().width;
        let (index, pane) = arriving_row(&super::super::compute_workspace_card_areas(
            &app,
            sidebar_rect(),
        ));

        // Everything but one row exists, and settles.
        let now = Instant::now();
        publish(&mut app, now, Some(pane));
        let settled_at = now + Duration::from_secs(2);
        app.anim.advance(settled_at);

        // Its first frame: its slot is still entirely closed.
        publish(&mut app, settled_at, None);
        crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
        if !app.view.sidebar_card_layers_published {
            // No proportional face on this machine, the same skip every other
            // pixel-card test takes.
            return;
        }
        let mid_cards = app.view.workspace_card_areas.clone();
        let mid = strip(&app);

        let arriving = mid_cards[index];
        // A row's own arrival no longer moves it sideways (F22 — see
        // `no_row_is_ever_placed_off_the_column_the_layout_gave_it`), so "is
        // this the first frame" is read off the thing that *does* move: the row
        // below it, still standing where it was before the slot existed.
        let span = mid_cards
            .get(index + 1)
            .map(|card| -card.motion_cells.1)
            .expect("nothing below the arriving row");
        assert!(
            span > 0,
            "the row below the arrival did not hold its ground, so this is not \
             the arrival's first frame"
        );
        assert_eq!(
            arriving.motion_cells.0, 0,
            "the arriving row travelled sideways"
        );

        app.anim.advance(settled_at + Duration::from_secs(2));
        crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
        let settled = strip(&app);
        assert!(
            app.view.workspace_card_areas[index].motion_cells == (0, 0),
            "the arrival never finished"
        );

        // Above the arrival nothing moved at all.
        for y in 0..usize::from(arriving.rect.y) {
            assert_eq!(mid[y], settled[y], "the tree above the arrival moved");
        }
        // At and below it, the tree is exactly the settled tree read `span`
        // rows further down — connectors, rails and the gaps between them.
        // Bounded by the list rather than the panel: the footer and the tray
        // are not the tree and do not reflow with it.
        let list = super::super::workspace_list_rect(app.view.sidebar_rect);
        // The renderer's own floor, which is one row short of the panel it was
        // given: `list_bottom` in `render_workspace_list`.
        let list_bottom = usize::from(list.y + list.height.saturating_sub(1));
        let bottom = list_bottom
            .saturating_sub(usize::from(u16::try_from(span).expect("a row span")))
            .min(settled.len());
        let mut moved = 0usize;
        for y in usize::from(arriving.rect.y)..bottom {
            assert_eq!(
                mid[y],
                settled[y + span as usize],
                "row {y} of the tree is not where the cards under the arrival \
                 were placed"
            );
            moved += usize::from(mid[y].trim() != settled[y].trim());
        }
        assert!(
            moved > 0,
            "no rail moved, so this proves nothing about the reflow"
        );
    }

    /// The panel's first columns, which is all the tree's rails ever occupy.
    const RAIL_COLUMNS: u16 = 6;

    /// A host that cannot move a row must not be given the phase motion would
    /// have moved it through.
    ///
    /// `row_motion` *synthesizes* an arrival and a departure so a row asked
    /// only to move has a bounded phase to move through. That synthesized
    /// departure is also the one thing that keeps a closed pane's row on
    /// screen: the loop republishes the last pass's rows into
    /// `sidebar_tree_row_memory` whenever a dismount exists, and the tree
    /// re-inserts them. Read off the config alone it existed everywhere —
    /// including on a terminal with no graphics, where a closed pane's row then
    /// sat there for the whole of `row_exit_ms` with nothing playing on it.
    ///
    /// Membership has to actually change for this to show, which is why
    /// `a_panel_too_narrow_for_a_card_renders_identically_with_motion_on`
    /// cannot catch it: it renders the same fleet twice.
    #[test]
    fn a_host_that_cannot_draw_a_pixel_card_retires_a_departed_row_at_once() {
        // Motion and nothing else, so the only life a row has is the one
        // motion invents for it.
        let mut app = moving_fleet();
        app.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::None;
        app.sidebar_animation.row_exit = crate::config::SidebarTokenEmphasis::None;

        // Everything settled, then one pane closes. `still_drawn` is what the
        // tree would put on screen on the frame after.
        let still_drawn = |app: &mut AppState| -> bool {
            app.anim = crate::anim::Animator::default();
            app.sidebar_tree_row_memory.clear();
            let live = crate::ui::sidebar_agent_live_entries(app);
            let (_, pane) = arriving_row(&super::super::compute_workspace_card_areas(
                app,
                sidebar_rect(),
            ));
            let now = Instant::now();
            publish(app, now, None);
            app.anim.advance(now + Duration::from_secs(2));
            app.sidebar_tree_row_memory = live;

            let gone = now + Duration::from_secs(2);
            publish(app, gone, Some(pane));
            let live: Vec<_> = crate::ui::sidebar_agent_live_entries(app)
                .into_iter()
                .filter(|entry| entry.pane_id != pane)
                .collect();
            crate::ui::rows_with_departing(app, live)
                .iter()
                .any(|entry| entry.pane_id == pane)
        };

        assert!(
            app.sidebar_rows_move(),
            "the pixel path is live, so motion is too"
        );
        assert!(app.sidebar_row_lifecycle().dismount.is_some());
        assert!(
            still_drawn(&mut app),
            "a row that is moving out has to still be drawn while it does"
        );

        for label in ["kitty_graphics", "sidebar_card_shapes"] {
            let mut app = moving_fleet();
            app.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::None;
            app.sidebar_animation.row_exit = crate::config::SidebarTokenEmphasis::None;
            match label {
                "kitty_graphics" => app.kitty_graphics_enabled = false,
                _ => app.sidebar_card_shapes = false,
            }
            assert!(
                !app.sidebar_rows_move(),
                "motion reached a host with {label} off"
            );
            assert!(
                app.sidebar_row_lifecycle().dismount.is_none(),
                "with {label} off a row was given a departure it cannot play"
            );
            assert!(
                !still_drawn(&mut app),
                "with {label} off a closed pane's row lingered instead of going \
                 on the next frame"
            );
        }
    }

    /// With motion off nothing reads the engine and every card sits exactly
    /// where the layout put it — the behaviour every Herdr that has not turned
    /// this on keeps.
    #[test]
    fn a_panel_with_motion_off_places_every_card_on_its_own_rect() {
        let mut app = moving_fleet();
        app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::None;
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let (_, pane) = arriving_row(&cards);
        let now = Instant::now();
        publish(&mut app, now, Some(pane));
        app.anim.advance(now + Duration::from_secs(2));
        publish(&mut app, now + Duration::from_secs(2), None);

        let Some(layers) = build(&app, &cards, &[]) else {
            return;
        };
        for layer in &layers {
            assert_eq!(
                layer.viewport(),
                (
                    i32::from(layer.rect.x) - i32::from(layer.clip.x),
                    i32::from(layer.rect.y) - i32::from(layer.clip.y)
                ),
                "a card moved with motion switched off"
            );
        }
    }
}

/// Writes out the *exact bytes* a real Herdr sends, frame by frame through a
/// row arriving and the same row leaving, so a real terminal can be asked what
/// it does with them.
///
/// Nothing here asserts; the assertions are in
/// [`super::rows_make_room_for_each_other`] and they are what CI runs. This
/// exists because the claim is about motion on a screen — that a row's
/// neighbours are *seen* to open a gap for it — and no unit test can show that.
/// The escapes come out of `kitty_graphics::encode_local_pane_graphics`, the one
/// the client actually writes to the host, so what a screenshot shows is what
/// the feature does rather than a reconstruction of it.
///
/// Off unless `HERDR_MOTION_CAPTURE_DIR` is set, exactly like
/// [`super::shape_capture`]. Writes `enter-NN.esc` and `leave-NN.esc` — each
/// self-contained, uploads included, so one frame can be replayed into a fresh
/// terminal — plus `cost.tsv`, which is the same sequence encoded against a
/// cache that *persists* across the frames and is therefore what the running
/// client actually pays per frame of motion.
#[cfg(test)]
mod motion_capture {
    use super::tests::{pixel_fleet_app, sidebar_rect};
    use super::*;
    use std::fmt::Write as _;
    use std::time::{Duration, Instant};

    /// Frames per transition. One more than the engine can resolve at its 50 ms
    /// step over a 320 ms arrival, so the strip cannot be accused of having
    /// smoothed anything the panel would not really draw.
    const STEPS: usize = 8;

    struct Capture {
        dir: String,
        cost: String,
        persistent: crate::kitty_graphics::HostGraphicsCache,
        runtimes: crate::terminal::TerminalRuntimeRegistry,
    }

    impl Capture {
        /// One frame: the panel laid out at `app`'s current animation position,
        /// encoded twice — once standalone for the replay, once against the
        /// running cache for the cost column.
        fn frame(&mut self, app: &mut AppState, name: &str) {
            let cell_size = app.host_cell_size;
            // Wide enough not to trip the mobile layout, and wider than the
            // panel on purpose: everything right of column 42 is the terminal
            // panes, and a sliding card reaching any of it would be the bug the
            // clip box exists to prevent.
            let area = Rect::new(0, 0, 100, sidebar_rect().height);
            crate::ui::compute_view_with_cell_size(app, &self.runtimes, area, cell_size);

            assert!(
                app.view.sidebar_card_layers_published,
                "{name} drew no cards, so there is nothing to capture"
            );
            let mut fresh = crate::kitty_graphics::HostGraphicsCache::default();
            let standalone = crate::kitty_graphics::encode_local_pane_graphics(
                app,
                &self.runtimes,
                app.view.tab_surface(),
                cell_size,
                &mut fresh,
                crate::kitty_graphics::EmbeddedSurfaces::ALL,
                None,
            );
            std::fs::write(format!("{}/{name}.esc", self.dir), &standalone).expect("writes");

            let incremental = crate::kitty_graphics::encode_local_pane_graphics(
                app,
                &self.runtimes,
                app.view.tab_surface(),
                cell_size,
                &mut self.persistent,
                crate::kitty_graphics::EmbeddedSurfaces::ALL,
                None,
            );
            let _ = writeln!(
                self.cost,
                "{name}\t{}\t{}",
                standalone.len(),
                incremental.len()
            );
        }
    }

    /// Publish the row membership the app loop would, holding `without` back,
    /// and keep the departing-row memory the same way `App::observe_agent_rows`
    /// does — otherwise a row whose pane has gone has nothing left to be drawn
    /// from and the exit is invisible rather than absent.
    fn publish(app: &mut AppState, now: Instant, without: Option<crate::layout::PaneId>) {
        let lifecycle = app.sidebar_row_lifecycle();
        let live: Vec<_> = crate::ui::sidebar_agent_live_entries(app)
            .into_iter()
            .filter(|entry| Some(entry.pane_id) != without)
            .collect();
        let rows: Vec<_> = live
            .iter()
            .map(|entry| {
                (
                    crate::anim::ElementId::agent_row(entry.pane_id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(now, crate::anim::Family::AgentRow, &lifecycle, rows);
        let spaces: Vec<_> = app
            .workspaces
            .iter()
            .map(|workspace| {
                (
                    crate::anim::ElementId::workspace_row(&workspace.id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(now, crate::anim::Family::WorkspaceRow, &lifecycle, spaces);
        app.sidebar_tree_row_memory = crate::ui::rows_with_departing(app, live);
    }

    /// The same panel drawn below the card shell's width, with motion on and
    /// with it off, as text.
    fn narrow_render(app: &mut AppState) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let narrow = Rect::new(0, 0, MIN_FOLD_WIDTH, 40);
        let restore = (
            app.sidebar_width,
            app.sidebar_max_width,
            app.sidebar_animation.row_motion,
        );
        let mut out = String::new();
        for motion in [
            crate::config::SidebarRowMotion::Slide,
            crate::config::SidebarRowMotion::None,
        ] {
            app.sidebar_animation.row_motion = motion;
            app.sidebar_width = narrow.width;
            app.sidebar_max_width = narrow.width;
            app.view.sidebar_rect = narrow;
            app.view.workspace_card_areas = super::super::compute_workspace_card_areas(app, narrow);
            let mut terminal =
                Terminal::new(TestBackend::new(narrow.width, narrow.height)).expect("backend");
            terminal
                .draw(|frame| {
                    super::super::render_sidebar(
                        app,
                        &crate::terminal::TerminalRuntimeRegistry::new(),
                        frame,
                        narrow,
                    )
                })
                .expect("draws");
            let buffer = terminal.backend().buffer();
            let _ = writeln!(out, "=== row_motion = {motion:?} ===");
            for row in 0..narrow.height {
                for col in 0..narrow.width {
                    out.push_str(buffer[(col, row)].symbol());
                }
                out.push('\n');
            }
        }
        (
            app.sidebar_width,
            app.sidebar_max_width,
            app.sidebar_animation.row_motion,
        ) = restore;
        out
    }

    #[test]
    fn motion_capture() {
        let dir = std::env::var("HERDR_MOTION_CAPTURE_DIR").unwrap_or_default();
        if dir.is_empty() {
            println!("SKIP: set HERDR_MOTION_CAPTURE_DIR");
            return;
        }

        let mut app = pixel_fleet_app();
        app.mode = crate::app::Mode::Terminal;
        app.sidebar_card_shapes = true;
        // The 42 columns the captain reviews the tree at. The default ceiling is
        // narrower than that, and it is a clamp rather than a preference, so it
        // has to be lifted or the panel comes out 36 wide.
        app.sidebar_max_width = sidebar_rect().width;
        app.sidebar_width = sidebar_rect().width;
        app.sidebar_animation.row_motion = crate::config::SidebarRowMotion::Slide;
        app.sidebar_animation.row_enter = crate::config::SidebarTokenEmphasis::Dissolve;
        app.sidebar_animation.row_enter_ms = 320;
        app.sidebar_animation.row_exit = crate::config::SidebarTokenEmphasis::Dissolve;
        app.sidebar_animation.row_exit_ms = 320;

        let mut capture = Capture {
            dir: dir.clone(),
            cost: String::from("frame\tstandalone_bytes\tincremental_bytes\n"),
            persistent: crate::kitty_graphics::HostGraphicsCache::default(),
            runtimes: crate::terminal::TerminalRuntimeRegistry::new(),
        };

        // The tree as it stands, before the worker exists at all. This is the
        // picture the arrival has to be seen departing from, so it is taken
        // before the pane is created rather than after: a card whose row the
        // engine is not yet tracking is drawn settled, and a "before" frame that
        // already showed the newcomer in place would prove nothing.
        let t0 = Instant::now();
        publish(&mut app, t0, None);
        app.anim.advance(t0 + Duration::from_secs(2));
        let settled = t0 + Duration::from_secs(2);
        capture.frame(&mut app, "enter-00-before");

        // The worker that arrives, under the first second mate so the two groups
        // below it are the ones with room to make.
        let focused = app.workspaces[0]
            .focused_pane_id()
            .expect("the fleet has a focused pane");
        let newcomer = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        if let Some(tab) = app.workspaces[0].active_tab_mut() {
            tab.layout.focus_pane(focused);
        }
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0].tabs[0].panes[&newcomer]
            .attached_terminal_id
            .clone();
        let now = app.state_age_now;
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("the new pane has no terminal");
        terminal.set_agent_name("herdr-row-slide".to_string());
        terminal.state = crate::detect::AgentState::Working;
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([
                (
                    "doing".to_string(),
                    Some("Making the sidebar's rows move".to_string()),
                ),
                ("project".to_string(), Some("herdr".to_string())),
                ("context".to_string(), Some("8%".to_string())),
                ("owner".to_string(), Some("2ndmate-herdr".to_string())),
            ]),
            None,
            now,
        );

        // And it arrives: the pane exists, so the layout already has its slot,
        // and the engine is told about it in the same pass the loop would.
        publish(&mut app, settled, None);
        for step in 0..STEPS {
            let at = settled + Duration::from_millis(320 * step as u64 / (STEPS - 1) as u64);
            app.anim.advance(at);
            capture.frame(&mut app, &format!("enter-{:02}", step + 1));
            if step == STEPS / 2 {
                // The character fallback, taken at the moment a slide would be
                // most visible if it reached that path. It does not.
                let fallback = narrow_render(&mut app);
                std::fs::write(format!("{dir}/fallback.txt"), fallback).expect("writes");
            }
        }
        let done = settled + Duration::from_secs(2);
        app.anim.advance(done);
        capture.frame(&mut app, "enter-99-after");

        // And then it goes: its pane closes, which is the only thing that makes
        // a row leave.
        app.workspaces[0].close_pane(newcomer);
        publish(&mut app, done, None);
        for step in 0..STEPS {
            let at = done + Duration::from_millis(320 * step as u64 / (STEPS - 1) as u64);
            app.anim.advance(at);
            publish(&mut app, at, None);
            capture.frame(&mut app, &format!("leave-{:02}", step + 1));
        }
        app.anim.advance(done + Duration::from_secs(2));
        publish(&mut app, done + Duration::from_secs(2), None);
        capture.frame(&mut app, "leave-99-after");

        std::fs::write(format!("{dir}/cost.tsv"), &capture.cost).expect("writes");

        println!("wrote {} frames to {dir}", STEPS * 2 + 3);
    }
}

/// Writes the real artwork out so a real terminal can be asked what it does
/// with it.
///
/// Nothing here asserts: the assertions live in [`super::a_card_is_its_own_shape`]
/// and they are what CI runs. This exists because the claim being made is about
/// *pixels on a screen* — that a card's glow falls off into what is behind it
/// instead of stopping at a rectangle, and that two overlapping cards blend
/// rather than clip — and the only honest way to check that is to put the cards
/// a real Herdr would send into a real terminal and look.
///
/// Off unless `HERDR_SHAPE_CAPTURE_DIR` is set, exactly like [`dissolve_capture`].
/// Writes `shape-NN.png` per card, `sheet.png` for the same fleet on the other
/// path, and `manifest.tsv` giving each image the cell rect it is placed at, so
/// the harness in `data/herdr-card-as-alpha-shape/` can replay them.
#[cfg(test)]
mod shape_capture {
    use super::tests::{pixel_fleet_app, sidebar_rect};
    use super::*;
    use std::fmt::Write as _;

    #[test]
    fn shape_capture() {
        let out = std::env::var("HERDR_SHAPE_CAPTURE_DIR").unwrap_or_default();
        if out.is_empty() {
            println!("SKIP: set HERDR_SHAPE_CAPTURE_DIR");
            return;
        }
        let rect = sidebar_rect();
        let mut manifest = String::from("file\tx\ty\tw\th\tcell_w\tcell_h\n");

        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        let cell = app.host_cell_size;
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let CardsUpdate::Rebuilt(layers) = build_cards(&app, &cards, rect, cell, &[]).update else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        for (index, layer) in layers.iter().enumerate() {
            let name = format!("shape-{index:02}.png");
            std::fs::write(format!("{out}/{name}"), &layer.layer.data).expect("writes");
            let _ = writeln!(
                manifest,
                "{name}\t{}\t{}\t{}\t{}\t{}\t{}",
                layer.rect.x,
                layer.rect.y,
                layer.rect.width,
                layer.rect.height,
                cell.width_px,
                cell.height_px,
            );
        }

        // The same fleet on the sheet, so the two can be put side by side and
        // the difference is the feature rather than a description of it.
        let sheet_app = pixel_fleet_app();
        let sheet_cards = super::super::compute_workspace_card_areas(&sheet_app, rect);
        if let CardsUpdate::Rebuilt(sheet) =
            build_cards(&sheet_app, &sheet_cards, rect, cell, &[]).update
        {
            std::fs::write(format!("{out}/sheet.png"), &sheet[0].layer.data).expect("writes");
            let _ = writeln!(
                manifest,
                "sheet.png\t{}\t{}\t{}\t{}\t{}\t{}",
                sheet[0].rect.x,
                sheet[0].rect.y,
                sheet[0].rect.width,
                sheet[0].rect.height,
                cell.width_px,
                cell.height_px,
            );
        }

        std::fs::write(format!("{out}/manifest.tsv"), manifest).expect("writes");
        println!("wrote {} shapes to {out}", layers.len());
    }
}

/// What the two card effects do to a card's light.
///
/// Both are read through the real engine and the real catalogue — nothing here
/// constructs an envelope by hand — so what is pinned is the contract a reader
/// sees on screen, not the arithmetic that produces it.
#[cfg(test)]
mod cards_breathe_and_wash {
    use super::tests::{pixel_fleet_app, sidebar_rect};
    use super::*;
    use std::time::{Duration, Instant};

    /// The fleet with card motion configured on and its rows published, so the
    /// engine really is running the breaths.
    ///
    /// The lifecycle is [`AppState::sidebar_row_lifecycle_given_cards`] with the
    /// card gate handed in rather than `AppState::sidebar_row_lifecycle`, whose
    /// gate additionally requires this machine to have a proportional face —
    /// something a container running the suite routinely does not. That is the
    /// only term substituted: the lists a row is given, and the membership it is
    /// published with, are the ones the app loop builds.
    fn breathing_fleet() -> (AppState, Instant) {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        let now = Instant::now();
        let lifecycle = app.sidebar_row_lifecycle_given_cards(true);
        publish_rows(&mut app, &lifecycle, now);
        (app, now)
    }

    /// The app loop's own publish, as [`crate::app::runtime`] makes it — the
    /// real member builders, so each row carries the breath it is playing and is
    /// stepped on that breath's tier rather than the fastest of the three.
    fn publish_rows(app: &mut AppState, lifecycle: &crate::anim::Lifecycle, now: Instant) {
        let live = super::super::sidebar_agent_live_entries(app);
        let agents = super::super::sidebar_agent_row_members(app, &live);
        app.anim
            .observe(now, crate::anim::Family::AgentRow, lifecycle, agents);
        let spaces = super::super::sidebar_space_row_members(app);
        app.anim
            .observe(now, crate::anim::Family::WorkspaceRow, lifecycle, spaces);
    }

    /// Every card the tree would draw, in layout order, as the renderer sees
    /// them.
    fn contents(app: &AppState) -> Vec<CardContent> {
        let cards = super::super::compute_workspace_card_areas(app, sidebar_rect());
        let entries = super::super::workspace_list_entries(app);
        let agents = super::super::sidebar_agent_entries(app);
        cards
            .iter()
            .filter(|card| card.card_frame.is_some())
            .filter_map(|card| entries.get(card.entry_idx))
            .filter_map(|entry| {
                content_for(
                    app,
                    entry,
                    &agents,
                    &crate::ui::sidebar::body_register::BodyRegister::resolve(app),
                )
            })
            .collect()
    }

    /// One card in a given state, or `None` when the fixture has none.
    fn card_in(app: &AppState, state: AgentState) -> Option<CardContent> {
        contents(app).into_iter().find(|card| card.state == state)
    }

    /// The pane behind the first card in `state`, and that card's row key.
    fn agent_in(app: &AppState, state: AgentState) -> Option<crate::layout::PaneId> {
        super::super::sidebar_agent_live_entries(app)
            .into_iter()
            .find(|entry| entry.state == state)
            .map(|entry| entry.pane_id)
    }

    fn set_pane_state(app: &mut AppState, pane: crate::layout::PaneId, state: AgentState) {
        let terminal_id = app.workspaces[0].tabs[0].panes[&pane]
            .attached_terminal_id
            .clone();
        if let Some(terminal) = app.terminals.get_mut(&terminal_id) {
            terminal.state = state;
        }
    }

    /// Publish this frame's washes exactly as the app loop does, and return
    /// whether any are live.
    fn observe_washes(app: &mut AppState, now: Instant) {
        let window = app.sidebar_cards.wash_duration();
        let cards: Vec<_> = super::super::sidebar_agent_live_entries(app)
            .iter()
            .map(|entry| (crate::anim::CardRow::Agent(entry.pane_id), entry.state))
            .collect();
        let members = app.sidebar_card_washes.observe(now, window, cards);
        let lifecycle = crate::app::card_wash::CardWashes::lifecycle(window);
        app.anim
            .observe(now, crate::anim::Family::CardWash, &lifecycle, members);
    }

    /// Sample the ink one card is drawn from, across its whole width.
    ///
    /// [`CardInks`] and not [`CardLight`], because the ink is what reaches the
    /// pixels: the two states either side of a wash's front are mixed as
    /// resolved colour, so asserting on the light before that mix would be
    /// testing a number the rasteriser never sees.
    fn across(card: &CardContent) -> Vec<CardInk> {
        let inks = CardInks::of(card);
        (0..=32).map(|i| inks.at(i as f32 / 32.0)).collect()
    }

    /// The ink at the middle of a card.
    fn mid(card: &CardContent) -> CardInk {
        CardInks::of(card).at(0.5)
    }

    // ---- the breath ------------------------------------------------------

    /// A card at rest breathes, and the breath only ever sets it *back*.
    ///
    /// The captain's words the effect answers to are *"on the back burner —
    /// dimmed or recessed slightly"*, and the two halves of that are separate
    /// claims with separate checks: the card genuinely moves over its loop (it
    /// breathes at all), and it never once goes above its own settled light (it
    /// is recessed rather than attention-seeking). A breath that brightened
    /// would make an idle card periodically outshine a working one, which
    /// inverts the only thing a card's light is for.
    ///
    /// The depth cue used to be the bloom specifically — an ordinary card had
    /// a real halo to recede from, and receding read as depth rather than as a
    /// dimmer. It no longer does: only the focused Space and an arriving card
    /// carry any bloom at all now ([`CardContent::accented`]), so an
    /// unaccented resting card's bloom sits flat at its own settled zero and
    /// the whole of the breath has to read in luminance alone. Asserting the
    /// bloom stays flat here is what would catch the border speaking work
    /// state again.
    #[test]
    fn at_rest_a_cards_breath_only_ever_sets_it_back() {
        let (mut app, now) = breathing_fleet();
        let Some(idle) = card_in(&app, AgentState::Idle) else {
            return;
        };
        let settled = idle.settled_light();

        let mut lums = Vec::new();
        let mut blooms = Vec::new();
        // A full loop of the resting breath, sampled well inside its own frame
        // tier so the walk is of the curve rather than of the quantiser.
        for step in 0..=120 {
            app.anim.advance(now + Duration::from_millis(step * 50));
            let Some(card) = card_in(&app, AgentState::Idle) else {
                return;
            };
            let light = card.arrived_light();
            assert!(
                light.lum <= settled.lum + 1e-6 && light.bloom <= settled.bloom + 1e-6,
                "the breath took a resting card above its own light: {light:?} against {settled:?}"
            );
            assert!(
                light.ink == settled.ink,
                "a breath must not restate the card's own ink: a card losing its \
                 hue reads as losing its stage, not as resting"
            );
            lums.push(light.lum);
            blooms.push(light.bloom);
        }

        let swing = |values: &[f32]| {
            let hi = values.iter().cloned().fold(f32::MIN, f32::max);
            let lo = values.iter().cloned().fold(f32::MAX, f32::min);
            (hi - lo) / hi.max(1e-6)
        };
        let lum_swing = swing(&lums);
        assert!(
            lum_swing > 0.02,
            "the card did not breathe at all: its light moved by {lum_swing:.4}"
        );
        assert!(
            blooms.iter().all(|bloom| *bloom == settled.bloom),
            "an unaccented resting card's bloom moved at all ({blooms:?}) — it \
             has no depth to recede from any more, so this is the border \
             speaking work state again"
        );
        // And it stays readable at the trough. The spec's digestibility
        // condition does not take a break for half of every cycle.
        assert!(
            lums.iter().all(|lum| *lum > settled.lum * 0.8),
            "a resting card dimmed past four fifths of its own light"
        );
    }

    /// A working card breathes on a different rhythm from a resting one — which
    /// is the whole reason there are two entries and not one with a knob.
    #[test]
    fn a_working_card_and_a_resting_one_do_not_move_together() {
        let (mut app, now) = breathing_fleet();
        let (Some(_), Some(_)) = (
            card_in(&app, AgentState::Idle),
            card_in(&app, AgentState::Working),
        ) else {
            return;
        };
        let mut differed = false;
        for step in 0..=60 {
            app.anim.advance(now + Duration::from_millis(step * 50));
            let (Some(idle), Some(working)) = (
                card_in(&app, AgentState::Idle),
                card_in(&app, AgentState::Working),
            ) else {
                return;
            };
            if (idle.breath - working.breath).abs() > 0.05 {
                differed = true;
            }
        }
        assert!(
            differed,
            "the two card states breathed in lockstep, so rhythm is carrying \
             nothing and the state ladder is colour alone"
        );
    }

    /// A fleet in one state, published the way the app loop publishes it, and
    /// the frames its rows report over a second of passes at the render floor.
    fn frames_reported_in_a_second(state: AgentState) -> u32 {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        for terminal in app.terminals.values_mut() {
            terminal.state = state;
        }
        let now = Instant::now();
        let lifecycle = app.sidebar_row_lifecycle_given_cards(true);
        publish_rows(&mut app, &lifecycle, now);
        (1..=125u32)
            .filter(|step| {
                app.anim
                    .advance(now + Duration::from_millis(u64::from(*step) * 8))
            })
            .count() as u32
    }

    /// **A quiet fleet's cards are stepped on the tier `card-rest` declares, not
    /// on `card-live`'s.**
    ///
    /// Every row declares all three breaths, because a card that named only the
    /// one its state wants today would freeze when the state moved. Reading the
    /// tier as `min()` across that declaration stepped every resting card at
    /// 50 ms — twice what it asks for, and `card-rest` says why it asks for
    /// 100: "a five-second breath has nothing at 50 ms that it does not have at
    /// 100, and a resting card is the tree's common case". Each of those extra
    /// steps is a re-raster of the card, a fresh `CardScene` on the wire, and on
    /// a delegating client a re-raster and a Kitty upload again at the far end.
    ///
    /// A quiet fleet is the case that matters: every card in it is a resting
    /// one. A working fleet is measured too, because halving the idle cost by
    /// slowing down the state that asked to be smooth would be no fix at all.
    #[test]
    fn a_quiet_fleets_cards_are_stepped_on_the_resting_tier() {
        let resting = frames_reported_in_a_second(AgentState::Idle);
        assert!(
            (1..=12).contains(&resting),
            "a resting fleet's cards should report about `card-rest`'s tier in a \
             second; {resting} is `card-live`'s"
        );

        let working = frames_reported_in_a_second(AgentState::Working);
        assert!(
            (15..=25).contains(&working),
            "a working fleet's cards must keep the smooth tier they breathe on, got {working}"
        );
    }

    /// **A card switched on after its row is on screen actually breathes.**
    ///
    /// The row is the element and the breath rides it, so the breath is only
    /// declared on that element once `sidebar_card_animation_active` holds —
    /// and every one of that predicate's terms can turn true *after* the row
    /// exists. Turning `card breathing` on from the settings screen is the
    /// shortest way there on a stock config: the wash keeps the card gate open
    /// throughout, so no row is ever retired and recreated, and this is the
    /// whole of what a reader does.
    ///
    /// The failure this pins is silent on every channel except the picture. The
    /// engine still runs, the row still resolves, the tree still lays out, and
    /// the card is still rasterised, encoded and pushed to the client on every
    /// pass — it is simply the same card every time, so what a reader sees is a
    /// panel of cards holding perfectly still with the effect switched on.
    ///
    /// Published through [`AppState::sidebar_row_lifecycle`] rather than
    /// [`breathing_fleet`]'s hand-built one, because the lifecycle going stale
    /// against what that method now answers *is* the bug.
    #[test]
    fn a_card_breathes_when_the_pulse_is_switched_on_after_its_row_is_drawn() {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        let now = Instant::now();

        // A session whose cards start settled. The wash is still on, so the
        // card gate is open and the rows below are never forgotten.
        app.sidebar_cards.pulse = false;
        if !app.sidebar_card_animation_active() {
            // No proportional face on this machine, so there are no pixel cards
            // for a breath to happen to.
            return;
        }
        let lifecycle = app.sidebar_row_lifecycle();
        publish_rows(&mut app, &lifecycle, now);

        // The captain turns `card breathing` on.
        app.sidebar_cards.pulse = true;

        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        // Well past any arrival, so what is measured is the steady breath.
        for step in 0..=125u64 {
            let at = now + Duration::from_secs(3) + Duration::from_millis(step * 40);
            let lifecycle = app.sidebar_row_lifecycle();
            publish_rows(&mut app, &lifecycle, at);
            for card in contents(&app) {
                lo = lo.min(card.breath);
                hi = hi.max(card.breath);
            }
        }
        assert!(hi >= lo, "the fixture drew no cards at all");
        assert!(
            hi - lo > 0.1,
            "every card held perfectly still with the pulse switched on: the \
             breath swung {:.4} over five seconds",
            hi - lo
        );
    }

    /// With the pulse switched off a card is drawn at exactly its settled
    /// light, which is what a host with no card animation already gets.
    #[test]
    fn switching_the_pulse_off_settles_every_card() {
        let (mut app, now) = breathing_fleet();
        app.sidebar_cards.pulse = false;
        app.anim.advance(now + Duration::from_millis(900));
        for card in contents(&app) {
            assert_eq!(card.breath, 0.0);
            assert_eq!(mid(&card), card.settled_light().inks());
        }
    }

    /// **The cost claim, as a test rather than as a measurement.**
    ///
    /// A card that breathes is a card whose artwork changes, and artwork
    /// changing means a rasterisation and an upload. If every frame of every
    /// card's breath cost one, a tree of a dozen cards would put the whole card
    /// path on the frame-time tail — which is the one thing this effect could
    /// plausibly break.
    ///
    /// [`CARD_BREATH_STEPS`] is what stops it: a card whose quantised envelope
    /// has not moved hashes to the same signature and is carried forward with
    /// nothing redrawn. So over two seconds of frames at the render floor, most
    /// card-frames have to be held rather than drawn. The numbers are a band and
    /// not a fixed count, because what is being pinned is that the quantiser is
    /// load-bearing, not what a particular period divides to.
    ///
    /// The band was re-derived when the ladder went from twelve steps to
    /// forty-eight: a finer ladder deliberately holds fewer frames, so a bound
    /// fitted to the old dial would fail on the new one for the intended reason
    /// rather than the guarded one. What has to stay true is that a *majority*
    /// of card-frames are still carried forward — measured at 38% redrawn.
    #[test]
    fn a_breathing_tree_holds_most_of_its_artwork_between_frames() {
        let (mut app, now) = breathing_fleet();
        let rect = sidebar_rect();
        if !is_available(&app, super::super::row_fold_width(&app, rect)) {
            return;
        }
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let mut previous: Vec<SidebarCardLayer> = Vec::new();
        let mut card_frames = 0usize;
        let mut redrawn = 0usize;
        // Two seconds at the app's own 16 ms render floor: the fastest anything
        // could ask these cards for a frame.
        for step in 0..125u64 {
            app.anim.advance(now + Duration::from_millis(step * 16));
            let built = build_cards(&app, &cards, rect, app.host_cell_size, &previous).update;
            let CardsUpdate::Rebuilt(layers) = built else {
                card_frames += previous.len();
                continue;
            };
            for (index, layer) in layers.iter().enumerate() {
                card_frames += 1;
                if previous
                    .get(index)
                    .is_none_or(|held| held.signature != layer.signature)
                {
                    redrawn += 1;
                }
            }
            previous = layers;
        }
        assert!(card_frames > 0, "the fixture drew no cards at all");
        assert!(
            redrawn > 0,
            "no card was ever redrawn, so nothing is breathing and this test              is measuring a still panel"
        );
        let share = redrawn as f32 / card_frames as f32;
        assert!(
            share < 0.5,
            "{redrawn} of {card_frames} card-frames were rasterised ({:.0}%). The              breath is being drawn at the loop's rate rather than at its own              quantised step, which is exactly the frame-time tail this ladder              exists to keep off.",
            share * 100.0
        );
    }

    /// **The snap the captain asked for reaches the pixels.**
    ///
    /// *"a 10% overshoot that kinda pendulums snaps back into place"* —
    /// [`crate::anim::Curve::SnapPendulum`] carries past its target by design
    /// and [`crate::anim::behaviour::Behaviour::strength`] hands that over
    /// intact. The card path used to clamp it away twice, in [`quantize`] and
    /// in [`CardLight::breathed`], so the overshoot existed in the engine and
    /// never once in the artwork.
    ///
    /// Asserted on the *drawn ink's luminance* and not on the envelope,
    /// because a number that survives quantisation and is then flattened on
    /// the way to the colour has still not reached the screen: a card at the
    /// top of its snap has to be measurably further back than the same card
    /// at a full breath.
    ///
    /// Luminance rather than bloom now: bloom is no longer a function of a
    /// card's own work state at all ([`CardContent::accented`]), so an
    /// unaccented working card's bloom sits flat at zero throughout and has
    /// nothing left to bottom out into. The breath still dips luminance on
    /// every card, accented or not, so that channel still carries the claim.
    #[test]
    fn the_snaps_overshoot_survives_the_ladder_and_reaches_the_ink() {
        let (mut app, now) = breathing_fleet();
        if card_in(&app, AgentState::Working).is_none() {
            return;
        }
        let mut peak_breath = f32::MIN;
        let mut deepest_lum = f32::MAX;
        // Four live cycles at a step far finer than the ladder, so what is
        // being walked is the curve rather than the sampling.
        for step in 0..=2_000u64 {
            app.anim.advance(now + Duration::from_millis(step * 5));
            let Some(card) = card_in(&app, AgentState::Working) else {
                return;
            };
            peak_breath = peak_breath.max(card.breath);
            deepest_lum = deepest_lum.min(card.arrived_light().lum);
        }
        assert!(
            peak_breath > 1.0,
            "the working card's envelope peaked at {peak_breath:.4}, so the snap's \
             overshoot is still being clamped away before it is drawn"
        );
        // The whole of the specified overshoot, not a rounded-down remnant. One
        // rung of the ladder is the tolerance, because the ladder is the only
        // thing between the curve and this number.
        let rung = 1.0 / CARD_BREATH_STEPS;
        assert!(
            peak_breath >= 1.10 - rung,
            "the envelope reached {peak_breath:.4}, short of the stated ten per \
             cent overshoot by more than the ladder's own rung of {rung:.4}"
        );
        // And it is visible: a full breath sets luminance back by
        // `BREATH_LUM_DIP`, so an overshoot has to set it back further still.
        let Some(card) = card_in(&app, AgentState::Working) else {
            return;
        };
        let at_full_breath = card.settled_light().lum * (1.0 - BREATH_LUM_DIP);
        assert!(
            deepest_lum < at_full_breath,
            "the card's luminance bottomed out at {deepest_lum:.4}, which is no \
             deeper than the {at_full_breath:.4} a full breath alone reaches — \
             the overshoot is in the envelope but not in the ink"
        );
    }

    /// **A working card never stops moving in the middle of its snap.**
    ///
    /// The failure this pins is not slowness, it is a *stall*: with the
    /// overshoot clamped, every part of the curve above the cap resolved to one
    /// rung, so a card held a single unchanged picture for 312 ms of every
    /// 2,400 ms cycle — dead still at exactly the moment it was supposed to
    /// snap. Both fixes bear on it, the clamp far more than the ladder.
    ///
    /// The bound is stated against the cycle rather than in bare milliseconds,
    /// so it keeps its meaning if the live period is ever retuned: no single
    /// picture may be held for more than a tenth of one breath. A card at the
    /// smooth peak of a *resting* breath legitimately dwells — that is a
    /// stationary point of a sine, not a stall — which is why this asks the
    /// snapping behaviour and not the drifting one.
    #[test]
    fn a_working_card_holds_no_picture_for_a_tenth_of_its_own_cycle() {
        let (mut app, now) = breathing_fleet();
        let Some(first) = card_in(&app, AgentState::Working) else {
            return;
        };
        const SAMPLE_MS: u64 = 5;
        // Asked of the catalogue rather than restated here, so retuning the
        // live breath's period moves the bound with it.
        let Some(cycle_ms) = app
            .anim
            .catalogue()
            .get(crate::anim::behaviour::names::CARD_LIVE)
            .map(|behaviour| behaviour.period.as_millis() as u64)
        else {
            return;
        };
        let budget_ms = cycle_ms / 10;

        let mut held = first.breath;
        let mut held_since = 0u64;
        let mut worst = (0u64, 0u64, held);
        for step in 1..=2_000u64 {
            let at = step * SAMPLE_MS;
            app.anim.advance(now + Duration::from_millis(at));
            let Some(card) = card_in(&app, AgentState::Working) else {
                return;
            };
            if (card.breath - held).abs() <= f32::EPSILON {
                continue;
            }
            let dwell = at - held_since;
            if dwell > worst.1 {
                worst = (held_since, dwell, held);
            }
            held = card.breath;
            held_since = at;
        }
        assert!(
            worst.1 > 0,
            "the working card never changed its picture at all, so this is \
             measuring a still panel rather than a breath"
        );
        assert!(
            worst.1 <= budget_ms,
            "a working card held one unchanged picture for {} ms — more than the \
             {budget_ms} ms tenth of its own {cycle_ms} ms cycle — starting at \
             {} ms, frozen at envelope {:.4}",
            worst.1,
            worst.0,
            worst.2
        );
    }

    // ---- the wash --------------------------------------------------------

    /// Drive one card from `from` to `into` and return the app the moment the
    /// wash starts, with the pane it happened on.
    fn washing(from: AgentState, into: AgentState) -> Option<(AppState, Instant, AgentState)> {
        let (mut app, now) = breathing_fleet();
        let pane = agent_in(&app, from)?;
        observe_washes(&mut app, now);
        set_pane_state(&mut app, pane, into);
        observe_washes(&mut app, now);
        Some((app, now, from))
    }

    /// **The acceptance criterion.** A state change leaves the *whole* card
    /// drawn in the destination's arrived light — never a half-updated band —
    /// once the wash's own window has closed.
    ///
    /// This used to also assert that the destination's ink differed from the
    /// state the card left, back when the border spoke a card's own
    /// `AgentState` directly. It no longer does — see
    /// [`CardContent::accented`] — so an unaccented card genuinely draws the
    /// same thin default at every stage now, and Idle and Working converging
    /// on one ink is the captain's
    /// `herdr-card-border-dot-final-match-20260822` fix working as intended,
    /// not a regression this test should still catch.
    #[test]
    fn a_finished_wash_leaves_the_whole_card_in_the_new_state() {
        let Some((mut app, now, _)) = washing(AgentState::Idle, AgentState::Working) else {
            return;
        };
        let window = app.sidebar_cards.wash_duration();
        app.anim.advance(now + window);
        observe_washes(&mut app, now + window);

        let Some(card) = card_in(&app, AgentState::Working) else {
            return;
        };
        assert!(card.wash.is_none(), "the sweep outlived its own window");
        // The new state's light, still breathing — the breath does not stop
        // because a wash finished, so what every column has to agree on is the
        // destination *as the card is drawn*, not the raw state constant.
        let arrived = card
            .light_at(AgentState::Working)
            .breathed(card.breath)
            .inks();
        for (index, ink) in across(&card).into_iter().enumerate() {
            assert_eq!(
                ink, arrived,
                "column {index} of 32 was left in a state the card is no longer in"
            );
        }
    }

    /// **An ordinary wash no longer has two sides.**
    ///
    /// Before the captain's `herdr-card-border-dot-final-match-20260822` fix
    /// this swept a visible front of bloom across the card as its `AgentState`
    /// crossed a wash — the new state's presence ahead of the front, the old
    /// state's behind it. That was the same "the border speaks work state"
    /// defect the fix retired: a card's own work no longer reaches the border
    /// at all, only whether it is the focused Space or mid its own arrival —
    /// [`CardContent::accented`] — so an unaccented card crossing
    /// Idle→Working, neither end accented, now reads as one flat state
    /// throughout the sweep. Nothing left to be "two sides" of.
    #[test]
    fn a_wash_in_flight_never_shows_two_sides_on_an_unaccented_card() {
        let Some((mut app, now, _)) = washing(AgentState::Idle, AgentState::Working) else {
            return;
        };
        let window = app.sidebar_cards.wash_duration();

        // Somewhere in the middle of the sweep. Sampled across several steps
        // rather than at one, because where the front is at any single instant
        // is the curve's business and not this test's.
        let mut saw_a_wash = false;
        for step in 1..=8 {
            let at = now + window / 12 * step;
            app.anim.advance(at);
            observe_washes(&mut app, at);
            let Some(card) = card_in(&app, AgentState::Working) else {
                return;
            };
            let Some(_) = card.wash else { continue };
            saw_a_wash = true;
            let columns = across(&card);
            let left = columns.first().copied().expect("sampled");
            let right = columns.last().copied().expect("sampled");
            assert!(
                (left.bloom - right.bloom).abs() < 1e-6,
                "an unaccented card's wash showed two different blooms \
                 ({left:?} against {right:?}) — the border is speaking work \
                 state again"
            );
        }
        assert!(
            saw_a_wash,
            "the fixture never crossed a wash at all, so this proved nothing"
        );
    }

    /// A card that has just arrived does not wash, and a card whose state has
    /// not moved does not either. Otherwise every row in the tree would sweep
    /// on the frame Herdr started.
    #[test]
    fn a_card_washes_on_a_change_and_never_on_arrival() {
        let (mut app, now) = breathing_fleet();
        observe_washes(&mut app, now);
        assert!(
            contents(&app).iter().all(|card| card.wash.is_none()),
            "the tree washed every card the first time it saw them"
        );
        observe_washes(&mut app, now + Duration::from_millis(10));
        assert!(contents(&app).iter().all(|card| card.wash.is_none()));
    }

    /// With the wash switched off a state change is simply a card in a
    /// different state, and nothing sweeps.
    #[test]
    fn switching_the_wash_off_leaves_a_state_change_still() {
        let Some((mut app, now, _)) = washing(AgentState::Idle, AgentState::Working) else {
            return;
        };
        app.sidebar_cards.wash = false;
        app.anim.advance(now + Duration::from_millis(100));
        for card in contents(&app) {
            assert!(card.wash.is_none());
            let inks = CardInks::of(&card);
            assert_eq!(inks.at(0.0), inks.at(1.0));
        }
    }

    /// The breath keeps running underneath a wash.
    ///
    /// Both sides of the front are breathed, not just the destination —
    /// otherwise a state change would look like the moment the card's breath
    /// was switched on.
    #[test]
    fn a_card_breathes_on_both_sides_of_a_wash() {
        let Some((mut app, now, from)) = washing(AgentState::Idle, AgentState::Working) else {
            return;
        };
        let at = now + app.sidebar_cards.wash_duration() / 3;
        app.anim.advance(at);
        observe_washes(&mut app, at);
        let Some(card) = card_in(&app, AgentState::Working) else {
            return;
        };
        if card.wash.is_none() || card.breath <= 0.0 {
            return;
        }
        let unbreathed_old = card.light_at(from).inks();
        let right = CardInks::of(&card).at(1.0);
        assert!(
            right.bloom < unbreathed_old.bloom || unbreathed_old.bloom == 0.0,
            "the far side of the front was drawn without the breath the rest of \
             the card has: {right:?} against {unbreathed_old:?}"
        );
    }

    // ---- what reaches the terminal --------------------------------------

    /// The breathing fleet with every agent at rest, which is what a fleet with
    /// nothing happening on it looks like — and the case
    /// [`crate::app::state::PublishedSurfaceRaster`] exists for.
    fn fleet_all_in(state: AgentState) -> (AppState, Instant) {
        let (mut app, now) = breathing_fleet();
        let ids: Vec<_> = app.terminals.keys().cloned().collect();
        for id in ids {
            if let Some(terminal) = app.terminals.get_mut(&id) {
                terminal.state = state;
            }
        }
        (app, now)
    }

    /// Step the tree for `frames` of the tier the animator actually uses, and
    /// count how many card images were *drawn* against how many the terminal was
    /// handed.
    ///
    /// The second number is taken off `data_fingerprint`, which is what the
    /// graphics cache keys an upload on — so it is the count of real uploads
    /// rather than a restatement of the rule under test.
    fn drawn_and_uploaded(app: &mut AppState, start: Instant, frames: u32) -> (u32, u32) {
        let rect = sidebar_rect();
        app.sidebar_width = rect.width;
        let cell = app.host_cell_size;
        let mut layers: Vec<SidebarCardLayer> = Vec::new();
        let (mut drawn, mut uploaded) = (0, 0);
        for step in 1..=frames {
            app.anim.advance(start + Duration::from_millis(50) * step);
            let cards = super::super::compute_workspace_card_areas(app, rect);
            let CardsUpdate::Rebuilt(next) = build_cards(app, &cards, rect, cell, &layers).update
            else {
                continue;
            };
            for (index, layer) in next.iter().enumerate() {
                let was = layers.get(index);
                if was.is_none_or(|was| was.signature != layer.signature) {
                    drawn += 1;
                }
                if was.is_none_or(|was| was.layer.data_fingerprint != layer.layer.data_fingerprint)
                {
                    uploaded += 1;
                }
            }
            layers = next;
        }
        (drawn, uploaded)
    }

    /// A tree of resting cards costs the terminal almost nothing, and a card
    /// that actually changed costs it a whole image on the very next frame.
    ///
    /// The card statement of `PublishedSurfaceRaster`'s whole claim, and the
    /// direct sibling of the tray's
    /// `a_resting_tray_stops_re_uploading_and_a_lit_one_does_not`. One test
    /// rather than two for the reason it gives: a rule that merely slowed the
    /// panel down would satisfy either half alone, and only the two together
    /// say the drift is *bounded*.
    ///
    /// A resting card's breath is a five-second settle whose consecutive frames
    /// differ by a fraction of one 8-bit level. A card whose agent changed state
    /// is a different picture, and waiting even one frame to say so is the
    /// throttle this deliberately is not.
    #[test]
    fn a_resting_card_stops_re_uploading_and_a_changed_one_publishes_at_once() {
        const FRAMES: u32 = 40;
        let rect = sidebar_rect();

        let (mut app, now) = fleet_all_in(AgentState::Idle);
        let (drawn, uploaded) = drawn_and_uploaded(&mut app, now, FRAMES);
        assert!(
            drawn > 0,
            "no card was rasterised at all, so this measured nothing — is there \
             a proportional face?"
        );
        assert!(
            uploaded > 0,
            "a resting tree uploaded nothing across {FRAMES} frames; the drift is \
             bounded, so a breath still has to arrive"
        );
        assert!(
            uploaded * 3 < drawn,
            "a resting tree uploaded {uploaded} of {drawn} rasters; nothing was \
             saved on the fleet's common case"
        );

        // The other half. Everything above holds just as well for a rule that
        // simply drew less often, and this is what tells them apart.
        let cell = app.host_cell_size;
        let settled = now + Duration::from_millis(50) * (FRAMES + 1);
        app.anim.advance(settled);
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let CardsUpdate::Rebuilt(before) = build_cards(&app, &cards, rect, cell, &[]).update else {
            panic!("a cold build drew nothing");
        };

        let pane = agent_in(&app, AgentState::Idle).expect("a resting agent to change");
        set_pane_state(&mut app, pane, AgentState::Blocked);
        app.anim.advance(settled + Duration::from_millis(50));
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let CardsUpdate::Rebuilt(after) = build_cards(&app, &cards, rect, cell, &before).update
        else {
            panic!("an agent changing state moved nothing at all");
        };
        let republished = after
            .iter()
            .zip(&before)
            .filter(|(after, before)| after.layer.data_fingerprint != before.layer.data_fingerprint)
            .count();
        assert!(
            republished > 0,
            "an agent changed state and not one of {} cards was handed to the \
             terminal again; this is a throttle, not a bound on what the screen \
             may drift",
            after.len()
        );
    }

    /// The whole delegated round trip, which is the path the change is for: the
    /// server lays out and ships a `CardScene`, the client draws it, and what
    /// reaches the terminal is counted at the client's end.
    ///
    /// A resting card's scene is a *different* scene on nearly every frame tier
    /// — the breath is a continuous envelope quantized to 48 levels — so
    /// `previous_card_layers` alone stops none of them, and each one used to buy
    /// the terminal ten fresh card images for a change of a fraction of an 8-bit
    /// level. This is the card statement of what
    /// `decode_and_rasterise_tray_scene`'s own doc says about the tray.
    ///
    /// The two counts come off the same list the client keeps, so nothing here
    /// is a restatement of the rule: `signature` is what the rasteriser redrew,
    /// `data_fingerprint` is what the graphics cache uploads on.
    #[test]
    fn a_delegating_client_stops_re_uploading_a_resting_tree() {
        const FRAMES: u32 = 30;
        let (mut app, now) = fleet_all_in(AgentState::Idle);
        let rect = sidebar_rect();
        app.sidebar_width = rect.width;
        let cell = app.host_cell_size;
        // The server half: laid out, shipped, and not one pixel drawn here.
        app.sidebar_card_graphics_client_rasterized = true;

        let mut client: Vec<SidebarCardLayer> = Vec::new();
        let (mut scenes, mut drawn, mut uploaded) = (0u32, 0u32, 0u32);
        let mut last: Option<Vec<u8>> = None;
        for step in 1..=FRAMES {
            app.anim.advance(now + Duration::from_millis(50) * step);
            let cards = super::super::compute_workspace_card_areas(&app, rect);
            let Some(scene) = build_card_scene(&app, &cards, rect, cell) else {
                continue;
            };
            let bytes = encode_card_scene(&scene).expect("encode CardScene");
            if last.as_ref() == Some(&bytes) {
                // The server only sends a scene that moved.
                continue;
            }
            last = Some(bytes.clone());
            scenes += 1;

            let decoded = decode_card_scene(&bytes).expect("decode CardScene");
            let Ok(Some(next)) = rasterise_card_scene(
                &decoded,
                None,
                cell,
                crate::kitty_graphics::HostTerminalKind::Rio,
                false,
                &client,
            ) else {
                continue;
            };
            for (index, layer) in next.iter().enumerate() {
                let was = client.get(index);
                if was.is_none_or(|was| was.signature != layer.signature) {
                    drawn += 1;
                }
                if was.is_none_or(|was| was.layer.data_fingerprint != layer.layer.data_fingerprint)
                {
                    uploaded += 1;
                }
            }
            client = next;
        }

        assert!(
            drawn > 0,
            "the client drew no cards at all — is there a proportional face?"
        );
        assert!(
            scenes * 4 > FRAMES,
            "only {scenes} of {FRAMES} frames shipped a scene, so this measured \
             an idle wire rather than the gate"
        );
        assert!(
            uploaded > 0,
            "the client uploaded nothing across {scenes} scenes; the drift is \
             bounded, so a breath still has to arrive"
        );
        assert!(
            uploaded * 3 < drawn,
            "a delegating client uploaded {uploaded} of {drawn} card rasters to \
             its terminal; the resting fleet's churn is still on the wire to Rio"
        );
    }

    /// Every viewer draws the cards itself, so this pass lays them out and stops.
    ///
    /// The card sibling of `a_delegated_tray_tracks_the_animation_without_rasterising_it`.
    /// What must survive the skip is everything that is not pixels: the row
    /// offsets the character connectors are drawn at, and the answer that says
    /// the character cards stand down — the cards are coming, just not from
    /// here.
    #[test]
    fn a_delegated_pass_lays_the_cards_out_and_rasterises_none_of_them() {
        let (mut app, now) = fleet_all_in(AgentState::Idle);
        let rect = sidebar_rect();
        app.sidebar_width = rect.width;
        app.sidebar_card_graphics_client_rasterized = true;
        app.anim.advance(now + Duration::from_millis(50));

        let cards = super::super::compute_workspace_card_areas(&app, rect);
        assert!(!cards.is_empty(), "the fixture drew no rows");
        let build = build_cards(&app, &cards, rect, app.host_cell_size, &[]);
        assert!(
            matches!(build.update, CardsUpdate::Delegated),
            "the server drew cards for a fleet whose every viewer draws its own"
        );
        assert_eq!(
            build.motion.len(),
            cards.len(),
            "the layout stopped with the pixels, so the character connectors \
             lost the offsets they are drawn at"
        );

        // And the same pass with nobody delegating draws them, so what is being
        // measured is the flag and not a fixture that could never draw at all.
        app.sidebar_card_graphics_client_rasterized = false;
        assert!(
            matches!(
                build_cards(&app, &cards, rect, app.host_cell_size, &[]).update,
                CardsUpdate::Rebuilt(_)
            ),
            "the fixture cannot draw cards either way, so the skip proved nothing"
        );
    }

    /// A card measured against a layer drawn for another box is not this card.
    ///
    /// `card_layer` counts the placement's grid out of the rect, and nothing
    /// downstream re-derives it — `aim_at` moves a layer, it does not resize
    /// one. So a standing layer whose pixels are within the tolerance is still
    /// only this surface's image if it was drawn for this surface's box, and the
    /// rect is checked beside the pixels for exactly that reason.
    /// A full stack has to stay a stack: eight contour lines inside a card that
    /// is only so deep, with a gap between each pair and clear air at the
    /// centre. If [`RESIDUE_INSET`] and [`RESIDUE_STEP`] ever grow past this,
    /// the deepest rings run into each other and the last absorptions stop
    /// being countable — which is the failure the eight-ring cap exists to
    /// avoid in the first place.
    #[test]
    fn a_full_ring_stack_fits_inside_the_card() {
        // Every card height the real path produces on the cell sizes this crate
        // is tested at, and then some.
        for height in [24.0_f32, 36.0, 48.0, 63.0, 84.0, 120.0] {
            let stack = RingStack::new(crate::app::residue::MAX_RINGS as u8, height)
                .expect("a full stack is not empty");
            let deepest = stack.inset + f32::from(stack.count - 1) * stack.step;
            assert!(
                deepest + stack.half_w < height / 2.0,
                "at height {height} the oldest ring reaches {deepest} px in, past the card's own \
                 centre line at {}",
                height / 2.0
            );
            assert!(
                stack.step > stack.half_w * 2.0,
                "at height {height} rings {} px apart cannot be told apart at {} px wide",
                stack.step,
                stack.half_w * 2.0
            );
        }
    }

    /// The stack is sampled off the card's own signed distance, so this is the
    /// whole of what the pixel loop asks it. Each ring answers at its own depth
    /// and nowhere else, and the alphas fall with age.
    #[test]
    fn every_ring_answers_at_its_own_depth_and_the_older_ones_answer_fainter() {
        let stack = RingStack::new(6, 63.0).expect("six rings is not empty");
        let mut seen = Vec::new();
        for age in 0..6 {
            let depth = stack.inset + age as f32 * stack.step;
            let alpha = stack
                .at(-depth)
                .unwrap_or_else(|| panic!("ring {age} drew nothing at its own depth"));
            seen.push(alpha);
        }
        for pair in seen.windows(2) {
            assert!(
                pair[1] < pair[0],
                "an older ring drew at least as strongly as a newer one: {seen:?}"
            );
        }

        // Outside the card, at the card's edge, and past the last ring: all
        // nothing. A ring must never leak onto a neighbouring row, and a
        // seven-ring stack must not draw an eighth.
        assert_eq!(stack.at(4.0), None, "a ring drew outside the card");
        assert_eq!(
            stack.at(0.0),
            None,
            "a ring drew on the card's own boundary"
        );
        let past = stack.inset + 6.0 * stack.step;
        assert_eq!(stack.at(-past), None, "a stack of six drew a seventh ring");
    }

    /// A mate that has absorbed nothing takes the branch it always took.
    #[test]
    fn a_card_with_no_residue_has_no_ring_stack_at_all() {
        assert!(RingStack::new(0, 63.0).is_none());
        assert!(RingStack::new(1, 63.0).is_some());
    }

    /// The point of the whole feature, in pixels: six absorbed workers and none
    /// are two different cards, and the difference is inside the card rather
    /// than over its edge or its text.
    #[test]
    fn a_mate_that_has_absorbed_workers_draws_a_different_card_from_one_that_has_not() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        let geometry = CardGeometry::new(21.0, false);
        let rect = RoundRect {
            x: 4.0,
            y: 4.0,
            w: 220.0,
            h: 63.0,
            r: geometry.radius,
        };
        let painted = |residue: u8| {
            let content = residue_test_content(residue);
            let mut sheet = Canvas::new(240, 76);
            draw_card(
                &mut sheet,
                &PlacedCard {
                    rect,
                    content: &content,
                    geometry: CardGeometry::new(21.0, false),
                    crew: crew::CrewBands::default(),
                },
                font,
            );
            sheet.rgba8().to_vec()
        };
        let bare = painted(0);
        let six = painted(6);
        assert_ne!(
            bare, six,
            "six absorbed workers drew the same card as none — the residue is not reaching the \
             pixels"
        );

        // Where it differs: only inside the card, and only in the band the ring
        // stack occupies. A difference at the boundary would mean the residue
        // is being mistaken for the card's own stroke; one outside it would
        // mean a ring is drawing on the neighbouring row.
        let mut differing_depths: Vec<f32> = Vec::new();
        for y in 0..76u32 {
            for x in 0..240u32 {
                let i = ((y * 240 + x) * 4) as usize;
                if bare[i..i + 4] != six[i..i + 4] {
                    differing_depths.push(-rect.distance(x as f32 + 0.5, y as f32 + 0.5));
                }
            }
        }
        assert!(
            !differing_depths.is_empty(),
            "the two cards differ but not at any pixel — impossible"
        );
        let stack = RingStack::new(6, rect.h).expect("six rings is not empty");
        let shallowest = stack.inset - stack.half_w - 1.0;
        let deepest = stack.inset + 5.0 * stack.step + stack.half_w + 1.0;
        for depth in &differing_depths {
            assert!(
                *depth >= shallowest && *depth <= deepest,
                "residue changed a pixel {depth} px in from the card's edge, outside the ring \
                 band {shallowest}..={deepest}"
            );
        }
    }

    /// A card carried forward on a stale signature would never show the sixth
    /// absorption: nothing else about the card moves when a ring is added.
    #[test]
    fn adding_a_ring_changes_the_cards_signature() {
        let signature = |residue: u8| {
            let mut hasher = DefaultHasher::new();
            residue_test_content(residue).hash_into(&mut hasher);
            hasher.finish()
        };
        assert_ne!(signature(0), signature(1));
        assert_ne!(signature(5), signature(6));
    }

    fn residue_test_content(residue: u8) -> CardContent {
        CardContent {
            title: "2ndmate-explore".into(),
            tidbit: None,
            register: None,
            state_label: "idle".into(),
            state: AgentState::Idle,
            stage: LifecycleStage::Done,
            severity: Severity::Clear,
            hues: StageHues([0.0; 5]),
            ground: measured::CANVAS,
            theme: CardTheme::UNTHEMED,
            split_channels: true,
            seen: true,
            depth: 1,
            lifted: false,
            focused_space: false,
            mark: None,
            residue,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            // This fixture isolates the residue rings, so it carries no spider.
            spider: None,
            breath: 0.0,
            wash: None,
            crew: Vec::new(),
            bars: None,
        }
    }

    #[test]
    fn a_layer_drawn_for_another_box_is_not_carried_forward_however_close_its_pixels() {
        let (mut app, now) = fleet_all_in(AgentState::Idle);
        let rect = sidebar_rect();
        app.sidebar_width = rect.width;
        let cell = app.host_cell_size;
        app.anim.advance(now + Duration::from_millis(50));
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let CardsUpdate::Rebuilt(first) = build_cards(&app, &cards, rect, cell, &[]).update else {
            panic!("the fixture drew no cards — is there a proportional face?");
        };

        // Standing layers holding exactly the pixels this tree just published,
        // carrying bytes no rasterisation would ever arrive at. Whether the
        // marker survives is which branch ran.
        let marked = |nudge_rect: bool| -> Vec<SidebarCardLayer> {
            first
                .iter()
                .map(|layer| {
                    let mut marked = layer.clone();
                    if nudge_rect {
                        marked.rect.height += 1;
                    }
                    // No signature this tree can plan to, so the gate is always
                    // what decides rather than the held-image match.
                    marked.signature = marked.signature.wrapping_add(1);
                    marked.layer = crate::app::state::GraphicsLayer::new(
                        layer.layer.format,
                        layer.layer.image_width,
                        layer.layer.image_height,
                        vec![0xA5; 64],
                        layer.layer.render,
                    );
                    marked
                })
                .collect()
        };

        let survived = |standing: &[SidebarCardLayer]| -> usize {
            let CardsUpdate::Rebuilt(built) =
                build_cards(&app, &cards, rect, cell, standing).update
            else {
                panic!("a tree whose every held signature mismatches reported nothing to do");
            };
            built
                .iter()
                .zip(standing)
                .filter(|(built, standing)| {
                    built.layer.data_fingerprint == standing.layer.data_fingerprint
                })
                .count()
        };

        assert!(
            survived(&marked(false)) > 0,
            "no card was carried forward at all, so this test cannot tell the two branches apart"
        );
        assert_eq!(
            survived(&marked(true)),
            0,
            "a layer drawn for a taller box was handed to the terminal as this box's image"
        );
    }
}
// Throwaway measurement probes written for herdr-glow-cause-scout.
// Append verbatim to the END of src/ui/sidebar/image_card.rs on fork/master
// (measured at aeb46d50) and run with:
//   cargo test --bin herdr glow_probe -- --nocapture --test-threads=1
//   cargo test --release --bin herdr probe_i_cost -- --nocapture
// They reuse the crate's own test fixtures (tests::pixel_fleet_app, 42-column
// sidebar_rect, 10x21 px cell) and the real build_cards() path, so what they
// measure is the actual PNG bytes the server puts on the wire.

/// **The sheet paints nothing over the tree's own lines.**
///
/// The captain, on the fleet he actually ran: *"the trunk line from firstmate
/// does not visually touch the firstmate root node"*, and the same gap where
/// every second mate's connector arrived at its card. Both were the sheet's
/// doing rather than the tree's. The character renderer drew the whole line —
/// [`RAIL_INK_COLUMN_FRACTION`] already put it in the right column — but the
/// sheet painted an opaque backdrop over every cell a row owned, so the two
/// stretches of that line which crossed a card's own cells were covered.
/// `Rasteriser::draw_tree_joins` put them back in pixels.
///
/// **Both the damage and the repair are gone.** A card is glass (H7) and the
/// sheet paints no backdrop at all, so the character rails are simply on screen
/// where the renderer drew them. What is worth pinning now is the *absence*: the
/// sheet must not paint in the rail's own column outside a card, or it is
/// covering the tree's line again by a different route.
#[cfg(test)]
mod the_sheet_leaves_the_trees_lines_alone {
    use super::tests::{sidebar_rect, three_rank_pixel_app};
    use super::*;
    /// The captain's own cell, and the one every geometry constant here was
    /// measured against.
    const CELL: (u32, u32) = (10, 21);

    /// One published sheet, decoded, with everything needed to ask a question
    /// about a pixel of it.
    struct Sheet {
        origin: Rect,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
        backdrop: Rgb,
    }

    impl Sheet {
        /// Whether anything was drawn at this pixel beyond the ground the sheet
        /// lays down over a row's cells. The ground is the *whole* question: a
        /// gap in the tree is exactly a run of pixels where the backdrop is all
        /// there is.
        fn inked(&self, x: u32, y: u32) -> bool {
            if x >= self.width || y >= self.height {
                return false;
            }
            let at = ((y * self.width + x) * 4) as usize;
            let (r, g, b, a) = (
                self.pixels[at],
                self.pixels[at + 1],
                self.pixels[at + 2],
                self.pixels[at + 3],
            );
            a > 0
                && (r.abs_diff(self.backdrop.0) as u16
                    + g.abs_diff(self.backdrop.1) as u16
                    + b.abs_diff(self.backdrop.2) as u16)
                    > 12
        }

        /// The pixel column a rail's ink stands in for a card at `frame`, and
        /// the pixel rows that card's cells cover.
        fn rail(&self, frame: Rect) -> (u32, u32, u32, u32) {
            let left = u32::from(frame.x.saturating_sub(self.origin.x)) * CELL.0;
            let top = u32::from(frame.y.saturating_sub(self.origin.y)) * CELL.1;
            let ink = left + (CELL.0 as f32 * RAIL_INK_COLUMN_FRACTION) as u32;
            (left, ink, top, top + u32::from(frame.height) * CELL.1)
        }
    }

    /// The sheet a fleet of three ranks publishes, and the frames it was drawn
    /// for. `None` on a machine with no proportional face, where there is no
    /// pixel path to read at all.
    fn sheet() -> Option<(Sheet, Vec<Rect>)> {
        let app = three_rank_pixel_app();
        assert!(
            !app.sidebar_card_shapes,
            "the joins are the sheet's to draw, so this fixture must be on the sheet"
        );
        let rect = sidebar_rect();
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let frames: Vec<Rect> = cards.iter().filter_map(|card| card.card_frame).collect();
        assert!(frames.len() >= 3, "the fixture lost a rank: {frames:?}");
        let CardsUpdate::Rebuilt(layers) =
            build_cards(&app, &cards, rect, app.host_cell_size, &[]).update
        else {
            return None;
        };
        let layer = layers.into_iter().next()?;
        let decoder = png::Decoder::new(layer.layer.data.as_slice());
        let mut reader = decoder.read_info().expect("a layer that is not a PNG");
        let mut pixels = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut pixels).expect("a PNG with no frame");
        pixels.truncate(info.buffer_size());
        assert_eq!(info.color_type, png::ColorType::Rgba);
        Some((
            Sheet {
                origin: layer.rect,
                width: info.width,
                height: info.height,
                pixels,
                backdrop: backdrop_rgb(&app),
            },
            frames,
        ))
    }

    /// **Nothing the sheet draws lands in the tree's rail column** outside the
    /// cards themselves.
    ///
    /// Swept over the gutter under every card — the rows between one card's
    /// bottom edge and the next row's top — which is exactly where the tree's
    /// trunk runs and exactly where the old backdrop used to reach. A sheet
    /// painting there is a sheet covering the line again.
    #[test]
    fn the_sheet_paints_nothing_in_the_rail_column_under_a_card() {
        let Some((sheet, frames)) = sheet() else {
            return;
        };
        let mut checked = 0;
        for frame in &frames {
            let (left, ink, _, bottom) = sheet.rail(*frame);
            // The gutter under this card: from its own drawn bottom to the foot
            // of the cells it was given.
            for y in bottom.saturating_sub(2)..bottom {
                for x in left..=ink {
                    assert!(
                        !sheet.inked(x, y),
                        "the sheet painted at ({x}, {y}), in the column the tree's \
                         own trunk runs down"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no gutter was swept");
    }

    /// And nothing is drawn where no line goes. A card with nothing under it
    /// keeps its gutter clear — otherwise the deepest card in every group would
    /// trail a stub into the row below it.
    #[test]
    fn a_card_with_nothing_under_it_grows_no_stub() {
        let Some((sheet, frames)) = sheet() else {
            return;
        };
        let leaf = *frames.last().expect("the fixture drew no cards");
        let (_, ink, _, bottom) = sheet.rail(leaf);
        assert!(
            !sheet.inked(ink, bottom - 1),
            "the last card in the tree left a rail hanging out of its bottom"
        );
    }
}

/// The GPU compute path draws the same cards the CPU path draws.
///
/// This is the whole safety argument for `crate::gpu`. A client picks a backend
/// from what its machine happens to have, so two people looking at the same
/// fleet must not be looking at two different pictures of it — and the CPU path
/// has to stay a working answer, not a theoretical one, on every machine that
/// cannot or should not use a GPU.
///
/// Both tests go through [`rasterise_card_scene`], the real client entry point,
/// rather than reaching into the pass: what has to agree is the bytes a client
/// hands its terminal, not an intermediate buffer.
#[cfg(test)]
mod the_gpu_draws_what_the_cpu_draws {
    use super::tests::{pixel_fleet_app, sidebar_rect};
    use super::*;
    use std::sync::atomic::Ordering;

    /// A fleet's cards as the client's own path produces them, drawn fresh with
    /// no previous layers to carry forward.
    fn client_cards(app: &AppState, scene: &CardScene) -> Vec<SidebarCardLayer> {
        rasterise_card_scene(
            scene,
            None,
            app.host_cell_size,
            crate::kitty_graphics::HostTerminalKind::Kitty,
            true,
            &[],
        )
        .expect("the client rasterised the scene")
        .expect("the scene produced cards")
    }

    /// A real fleet's `CardScene`, or `None` on a machine with no face to set
    /// the cards in.
    fn scene() -> Option<(AppState, CardScene)> {
        let app = pixel_fleet_app();
        let rect = sidebar_rect();
        let cards = super::super::compute_workspace_card_areas(&app, rect);
        let scene = build_card_scene(&app, &cards, rect, app.host_cell_size)?;
        (!scene.placed.is_empty()).then_some((app, scene))
    }

    /// Draw a fleet on the CPU, draw it again on the GPU, and compare the bytes
    /// that would reach the terminal.
    ///
    /// Skips — loudly — on a machine with no adapter, because there is no
    /// comparison to make there. It does not *pass* quietly: the tile counter is
    /// the positive control, and `HERDR_GPU_TEST_REQUIRE=1` turns the skip into
    /// a failure for a run whose whole point was to exercise a real device.
    #[test]
    fn the_gpu_and_the_cpu_agree_on_every_card_byte() {
        let Some((app, scene)) = scene() else {
            println!("SKIP: no proportional face on this machine");
            return;
        };

        let cpu = {
            let _gate = crate::gpu::ForceEnabled::new(false);
            client_cards(&app, &scene)
        };

        let before = crate::gpu::bloom::TILES_COMPOSED.load(Ordering::Relaxed);
        let gpu = {
            let _gate = crate::gpu::ForceEnabled::new(true).ignoring_the_cost();
            client_cards(&app, &scene)
        };
        let composed = crate::gpu::bloom::TILES_COMPOSED.load(Ordering::Relaxed) - before;

        if composed == 0 {
            assert!(
                std::env::var("HERDR_GPU_TEST_REQUIRE").is_err(),
                "HERDR_GPU_TEST_REQUIRE is set but no tile reached a GPU: {}",
                crate::gpu::bloom::adapter_description()
                    .unwrap_or_else(|| "no adapter".to_string())
            );
            println!("SKIP: no GPU adapter composed a tile on this machine");
            return;
        }

        assert_eq!(
            cpu.len(),
            gpu.len(),
            "the two backends drew a different number of cards"
        );
        for (index, (cpu, gpu)) in cpu.iter().zip(&gpu).enumerate() {
            assert_eq!(cpu.rect, gpu.rect, "card {index} was placed differently");
            assert_eq!(
                cpu.layer.data,
                gpu.layer.data,
                "card {index} came out different on the GPU ({} bytes vs {})",
                cpu.layer.data.len(),
                gpu.layer.data.len()
            );
        }
    }

    /// With the gate open but the shipped threshold in force, the cards are
    /// still exactly the CPU's cards — whichever way the batch actually went.
    ///
    /// The fallback is the point. `compose` declines for four different reasons
    /// and none of them is an error the caller can see in the pixels, so the
    /// invariant that has to hold is not "the GPU ran" but "it did not matter
    /// whether it ran".
    #[test]
    fn declining_the_gpu_is_never_a_different_card() {
        let Some((app, scene)) = scene() else {
            println!("SKIP: no proportional face on this machine");
            return;
        };

        let cpu = {
            let _gate = crate::gpu::ForceEnabled::new(false);
            client_cards(&app, &scene)
        };
        let shipped = {
            let _gate = crate::gpu::ForceEnabled::new(true);
            client_cards(&app, &scene)
        };

        assert_eq!(cpu.len(), shipped.len());
        for (index, (cpu, shipped)) in cpu.iter().zip(&shipped).enumerate() {
            assert_eq!(
                cpu.layer.data, shipped.layer.data,
                "card {index} changed when the GPU gate was opened"
            );
        }
    }

    /// What the two backends actually cost on this machine, on a real fleet's
    /// cards.
    ///
    /// Ignored, because it is a measurement and not an assertion: the numbers
    /// are a property of whatever adapter and CPU it happens to run on, and
    /// there is no threshold here that would mean the same thing on two boxes.
    /// Run it with
    ///
    /// ```text
    /// cargo test --release --bin herdr the_two_backends_cost -- --ignored --nocapture
    /// ```
    ///
    /// The fleet is repeated up to `WORTH_A_DISPATCH` so the batch is the size
    /// the GPU path is actually for — a frame where the whole tree was redrawn —
    /// rather than the handful of cards the fixture happens to hold.
    #[test]
    #[ignore]
    fn the_two_backends_cost() {
        let Some((app, scene)) = scene() else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        let font = font::card_font(None).expect("a face");
        let cell = app.host_cell_size;
        let rasteriser = Rasteriser {
            font,
            title_metrics: font.metrics(TITLE_PX),
            tidbit_metrics: font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
            cell_size: cell,
            cell_w: f32::from(cell.width_px as u16),
            cell_h: f32::from(cell.height_px as u16),
            crew_bands: crew::CrewBands::of(font, TITLE_PX, f32::from(cell.height_px as u16)),
            field: scene.field,
            bounds: scene.bounds,
            bloom_floor: scene.bloom_floor,
            backdrop: scene.backdrop,
            rail: None,
            dissolve: None,
            host_terminal_kind: crate::kitty_graphics::HostTerminalKind::Kitty,
            host_graphics_is_local: true,
        };

        // (image size, the one splat that lights it) for every card, repeated
        // until the batch is a real frame's worth of work.
        let mut plans: Vec<(u32, u32, BloomSplat)> = Vec::new();
        for (frame, wire) in &scene.placed {
            let content = CardContent::from(wire.clone());
            let Some(rect) = rasteriser.card_rect(*frame) else {
                continue;
            };
            let Some((width, height)) = rasteriser.image_size_px(rect) else {
                continue;
            };
            let card = rasteriser.place(*frame, &content, rect);
            if let Some(splat) = plan_bloom(&card, width, height) {
                plans.push((width, height, splat));
            }
        }
        assert!(!plans.is_empty(), "the fixture planned no card blooms");
        let one_round = plans.len();
        // A twelve-card frame's worth of pixels, which is what the GPU path is
        // for: the frame where the tree's content changed and every card is
        // redrawn at once.
        const A_WHOLE_TREE: u64 = 500_000;
        while plans
            .iter()
            .map(|(w, h, _)| u64::from(*w) * u64::from(*h))
            .sum::<u64>()
            < A_WHOLE_TREE
        {
            for index in 0..one_round {
                let (w, h, splat) = &plans[index];
                plans.push((*w, *h, splat.clone()));
            }
        }

        let cpu = |plans: &[(u32, u32, BloomSplat)]| {
            let mut out = Vec::with_capacity(plans.len());
            for (width, height, splat) in plans {
                let mut canvas = Canvas::new(*width, *height);
                let mut field = BloomField::new(*width, *height);
                lay_splat(&mut field, splat);
                field.composite(&mut canvas);
                out.push(canvas);
            }
            out
        };
        let tiles: Vec<crate::gpu::bloom::Tile> = plans
            .iter()
            .map(|(width, height, splat)| crate::gpu::bloom::Tile {
                width: *width,
                height: *height,
                splats: vec![splat.for_gpu()],
            })
            .collect();

        let best = |mut run: Box<dyn FnMut()>| {
            let mut best = f64::INFINITY;
            for _ in 0..12 {
                let at = std::time::Instant::now();
                run();
                best = best.min(at.elapsed().as_secs_f64() * 1000.0);
            }
            best
        };

        let cpu_ms = best(Box::new(|| {
            std::hint::black_box(cpu(&plans));
        }));
        let _gate = crate::gpu::ForceEnabled::new(true);
        let gpu_ms = best(Box::new(|| {
            std::hint::black_box(crate::gpu::bloom::compose(&tiles, bloom_curve()).is_ok());
        }));

        let pixels: u64 = plans
            .iter()
            .map(|(w, h, _)| u64::from(*w) * u64::from(*h))
            .sum();
        println!(
            "cards={} pixels={pixels} cpu(1 thread)={cpu_ms:.2}ms gpu={gpu_ms:.2}ms speedup={:.2}x\nadapter={}",
            plans.len(),
            cpu_ms / gpu_ms,
            crate::gpu::bloom::adapter_description().unwrap_or_else(|| "none".into())
        );
    }

    /// A pre-bloomed canvas the wrong size for the image is refused, and the
    /// card is drawn on the CPU instead of over the wrong pixels.
    ///
    /// `Canvas::from_rgba8` guards the length and `rasterise` guards the shape;
    /// this holds the second one, which is the only thing standing between a
    /// mismatched batch and a card drawn into someone else's image.
    #[test]
    fn a_prebloom_of_the_wrong_size_is_refused() {
        let Some((app, scene)) = scene() else {
            println!("SKIP: no proportional face on this machine");
            return;
        };
        let expected = {
            let _gate = crate::gpu::ForceEnabled::new(false);
            client_cards(&app, &scene)
        };

        let font = font::card_font(None).expect("a face");
        let cell = app.host_cell_size;
        let rasteriser = Rasteriser {
            font,
            title_metrics: font.metrics(TITLE_PX),
            tidbit_metrics: font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
            cell_size: cell,
            cell_w: f32::from(cell.width_px as u16),
            cell_h: f32::from(cell.height_px as u16),
            crew_bands: crew::CrewBands::of(font, TITLE_PX, f32::from(cell.height_px as u16)),
            field: scene.field,
            bounds: scene.bounds,
            bloom_floor: scene.bloom_floor,
            backdrop: scene.backdrop,
            rail: None,
            dissolve: None,
            host_terminal_kind: crate::kitty_graphics::HostTerminalKind::Kitty,
            host_graphics_is_local: true,
        };
        let placed: Vec<(Rect, CardContent)> = scene
            .placed
            .iter()
            .cloned()
            .map(|(rect, wire)| (rect, CardContent::from(wire)))
            .collect();
        let one = &placed[0..1];
        let rect = rasteriser
            .card_rect(placed[0].0)
            .expect("the first card has an image");

        let honest = rasteriser
            .rasterise(one, rect, None)
            .expect("the CPU drew the first card");
        let with_junk = rasteriser
            .rasterise(one, rect, Canvas::from_rgba8(4, 4, vec![9; 64]))
            .expect("a mismatched prebloom still draws the card");

        assert_eq!(
            honest.rgba8(),
            with_junk.rgba8(),
            "a prebloom of the wrong size was drawn over instead of refused"
        );
        assert!(
            !expected.is_empty(),
            "the fixture published no cards, so this tests nothing"
        );
    }
}

/// The failure spider survives the pixel path.
///
/// The defect this whole module's [`spider`] submodule exists to fix: the marker
/// shipped as a character cell, and every one of the captain's own frames is a
/// pixel card drawn over exactly those cells, so it had never once been on his
/// screen. These assert against *published layer bytes* rather than against the
/// drawing functions, because "the creature is correct" and "the creature
/// reaches a card" are two different claims and only the second one was ever in
/// doubt.
#[cfg(test)]
mod the_spider_reaches_the_pixel_card {
    use super::tests::{pixel_fleet_app, sidebar_rect};
    use super::*;

    /// The fleet on the shapes path, with `marked` rows carrying an open defect
    /// and a failed lifecycle, and their spiders mounted and settled.
    ///
    /// The lifecycle comes from [`crate::app::runtime::failure_spider_lifecycle`]
    /// rather than a copy: a fixture with its own stage table would keep passing
    /// after the real one changed.
    fn marked_fleet(marked: &[usize], defect: &str) -> AppState {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        app.view.sidebar_card_layers_published = true;
        let now = std::time::Instant::now();
        app.state_age_now = now;

        // The tree's own order, not the pane map's: a `HashMap`'s iteration
        // order is not stable across processes, and a fixture that marked a
        // different row each run would be a flake rather than a test.
        let panes: Vec<crate::layout::PaneId> = super::super::sidebar_agent_entries(&app)
            .iter()
            .map(|entry| entry.pane_id)
            .collect();
        let mut members: Vec<(crate::anim::ElementId, crate::anim::behaviour::DriveInputs)> =
            Vec::new();
        for index in marked {
            let Some(pane) = panes.get(*index) else {
                continue;
            };
            let terminal_id = app.workspaces[0].tabs[0].panes[pane]
                .attached_terminal_id
                .clone();
            let terminal = app
                .terminals
                .get_mut(&terminal_id)
                .expect("a test terminal");
            terminal.state = AgentState::Blocked;
            terminal.metadata_tokens.patch(
                std::collections::HashMap::from([
                    (
                        crate::app::lifecycle::STAGE_TOKEN.to_string(),
                        Some("failed".to_string()),
                    ),
                    (
                        crate::quality_streak::DEFECT_TOKEN.to_string(),
                        Some(defect.to_string()),
                    ),
                ]),
                None,
                now,
            );
            members.push((
                crate::anim::ElementId::failure_spider(crate::anim::CardRow::Agent(*pane)),
                crate::anim::behaviour::DriveInputs::default(),
            ));
        }

        let lifecycle = crate::app::runtime::failure_spider_lifecycle();
        app.anim
            .observe(now, crate::anim::Family::FailureSpider, &lifecycle, members);
        // Past the climb, so the spider under test is the settled one rather
        // than a frame of its arrival.
        app.anim.advance(
            now + crate::anim::behaviour::FAILURE_SPIDER_CLIMB_PERIOD
                + std::time::Duration::from_millis(50),
        );
        app
    }

    /// Every published card's own bytes, in layout order, or `None` on a
    /// machine with no proportional face and so no pixel path to test.
    fn layers(app: &AppState) -> Option<Vec<Vec<u8>>> {
        let cards = super::super::compute_workspace_card_areas(app, sidebar_rect());
        match build_cards(app, &cards, sidebar_rect(), app.host_cell_size, &[]).update {
            CardsUpdate::Rebuilt(layers) => {
                Some(layers.into_iter().map(|l| l.layer.data).collect())
            }
            _ => None,
        }
    }

    /// How many of a tree's cards carry a spider, or `None` on a machine with
    /// no pixel path to ask.
    fn spiders_in(app: &AppState) -> Option<usize> {
        let cards = super::super::compute_workspace_card_areas(app, sidebar_rect());
        let scene = build_card_scene(app, &cards, sidebar_rect(), app.host_cell_size)?;
        Some(
            scene
                .placed
                .iter()
                .filter(|(_, content)| content.spider.is_some())
                .count(),
        )
    }

    #[test]
    fn a_marked_card_is_drawn_differently_from_the_same_card_unmarked() {
        let Some(unmarked) = layers(&marked_fleet(&[], "-")) else {
            return;
        };
        let marked = layers(&marked_fleet(&[1], "S1")).expect("the same fleet still draws");
        assert_eq!(
            unmarked.len(),
            marked.len(),
            "marking a row changed how many cards the tree has"
        );
        let differing: Vec<usize> = unmarked
            .iter()
            .zip(&marked)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(index, _)| index)
            .collect();
        assert!(
            !differing.is_empty(),
            "an open defect changed nothing about the pixels — the spider is still \
             invisible on the pixel path, which is the whole defect"
        );
        // One row and no other. A marker that repainted its neighbours would
        // be the card's own stage leaking into the tree. Which index it lands
        // on is deliberately not asserted: the tree's layout order is the
        // sidebar's business, and pinning it here would make this test fail for
        // a reason that has nothing to do with the spider.
        assert_eq!(
            differing.len(),
            1,
            "marking one row changed {} cards: {differing:?}",
            differing.len()
        );
    }

    #[test]
    fn the_marker_is_drawn_at_the_severity_the_fleet_published() {
        let Some(loud) = layers(&marked_fleet(&[1], "S1")) else {
            return;
        };
        let quiet = layers(&marked_fleet(&[1], "S4")).expect("the same fleet still draws");
        assert_ne!(
            loud, quiet,
            "S1 and S4 drew the same tree — the defect ladder reaches no pixel"
        );
    }

    #[test]
    fn a_dash_closes_the_defect_and_takes_the_marker_with_it() {
        let Some(open) = layers(&marked_fleet(&[1], "S2")) else {
            return;
        };
        let closed = layers(&marked_fleet(&[1], "-")).expect("the same fleet still draws");
        assert_ne!(open, closed, "closing the defect changed nothing");
        // The same row, still failed, still blocked, and carrying no marker:
        // only the fleet can know a defect is closed, and `-` is it saying so.
        // Counted off the scene rather than off the pixels, because the two
        // trees differ in the *stage* too and a byte comparison could not tell
        // which of the two changes it was looking at.
        assert_eq!(spiders_in(&marked_fleet(&[1], "S2")), Some(1));
        assert_eq!(spiders_in(&marked_fleet(&[1], "-")), Some(0));
    }

    /// The delegating path: a client that rasterises its own cards has to draw
    /// the same marker, or the captain's Windows box loses the spider again for
    /// a different reason.
    #[test]
    fn the_marker_crosses_the_wire_to_a_client_that_draws_its_own_cards() {
        let app = marked_fleet(&[1], "S1");
        let cards = super::super::compute_workspace_card_areas(&app, sidebar_rect());
        let Some(scene) = build_card_scene(&app, &cards, sidebar_rect(), app.host_cell_size) else {
            return;
        };
        let bytes = encode_card_scene(&scene).expect("a scene that encodes");
        let decoded = decode_card_scene(&bytes).expect("a scene that decodes");
        assert_eq!(
            decoded, scene,
            "the scene did not survive its own round trip"
        );
        let carried = decoded
            .placed
            .iter()
            .filter(|(_, content)| content.spider.is_some())
            .count();
        assert_eq!(
            carried, 1,
            "the marked row's spider did not cross the wire — a delegating client \
             would draw the tree with no marker on it"
        );
    }
}

/// **The worker list reaches pixels.**
///
/// [`crate::ui::sidebar::a_space_card_carries_its_own_workers`] pins which rows
/// are in which card's list; this pins that the card actually *draws* them —
/// under its own content, one fixed step apart, and in the order the two beats
/// of an arrival happen. A list that resolved correctly and painted nothing
/// would pass every one of those tests.
#[cfg(test)]
mod a_card_draws_the_workers_it_carries {
    use super::*;

    const CELL_H: f32 = 21.0;
    const BANDS: crew::CrewBands = crew::CrewBands {
        divider: CELL_H,
        row: CELL_H * 2.0,
    };

    fn member(name: &str, tier: u8, arrival: crew::CrewArrival) -> crew::CrewMember {
        crew::CrewMember {
            name: name.to_string(),
            detail: Some("holding the line".to_string()),
            tier,
            arrival,
            pulse: 0.0,
            spider: None,
        }
    }

    fn content(crew: Vec<crew::CrewMember>) -> CardContent {
        let mut hues = [0.0; 5];
        for (slot, stage) in hues.iter_mut().zip(LifecycleStage::ALL) {
            *slot = stage.hue(
                &crate::app::state::Palette::catppuccin(),
                &crate::terminal_theme::TerminalTheme::default(),
            );
        }
        CardContent {
            title: "herdr".into(),
            tidbit: Some("gas giant · 2,470 files".into()),
            register: Some(Caption {
                text: "streak 11 · 17 revs".into(),
                tone: CaptionTone::Register,
            }),
            state_label: "idle".into(),
            state: AgentState::Idle,
            stage: LifecycleStage::Running,
            severity: Severity::Clear,
            hues: StageHues(hues),
            ground: Rgb(9, 17, 28),
            theme: CardTheme::UNTHEMED,
            split_channels: true,
            seen: true,
            depth: 0,
            lifted: false,
            focused_space: false,
            mark: None,
            residue: 0,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            breath: 0.0,
            spider: None,
            wash: None,
            crew,
            bars: None,
        }
    }

    /// The card, drawn at exactly the height [`Rasteriser::place`] would give
    /// it: its own content block plus whatever its list is currently taking.
    fn drawn(content: &CardContent, font: &CardFont) -> (Canvas, f32) {
        let head = card_height_px(
            font.metrics(TITLE_PX),
            font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
        );
        let crew_px = crew::drawn_extent_px(BANDS, &content.crew);
        let rect = RoundRect {
            x: 4.0,
            y: 4.0,
            w: 320.0,
            h: head + crew_px,
            r: 3.0,
        };
        let mut canvas = Canvas::new(340, (rect.y + rect.h).ceil() as u32 + 8);
        draw_card(
            &mut canvas,
            &PlacedCard {
                rect,
                content,
                geometry: CardGeometry::new(CELL_H, false),
                crew: BANDS,
            },
            font,
        );
        (canvas, 4.0 + head)
    }

    /// One band's own pixels, so two cards can be compared over exactly the rows
    /// one worker occupies.
    fn band(canvas: &Canvas, from: f32, to: f32) -> Vec<u8> {
        let px = canvas.rgba8();
        let width = canvas.width() as usize;
        let y0 = (from.ceil() as usize).min(canvas.height() as usize);
        let y1 = (to.floor() as usize).min(canvas.height() as usize);
        px[y0 * width * 4..y1 * width * 4].to_vec()
    }

    /// The leftmost pixel column in `band` that differs between two cards.
    ///
    /// The card's own face fills every row of its box, worker list included, so
    /// "is there ink here" cannot see a row at all. What can is the *difference*
    /// a row makes: the leftmost column where a card carrying it and the same
    /// card not carrying it part company is exactly that row's own left edge.
    fn leftmost_difference(a: &Canvas, b: &Canvas, from: f32, to: f32) -> Option<u32> {
        let (wa, wb) = (a.width(), b.width());
        assert_eq!(
            wa, wb,
            "two canvases of different widths cannot be compared"
        );
        let (pa, pb) = (a.rgba8(), b.rgba8());
        let y0 = (from.ceil() as u32).min(a.height());
        let y1 = (to.floor() as u32).min(a.height());
        (0..wa).find(|x| {
            (y0..y1).any(|y| {
                let at = ((y * wa + x) * 4) as usize;
                pa[at..at + 4] != pb[at..at + 4]
            })
        })
    }

    /// **A card with a crew draws something under its own content; the same card
    /// without one draws nothing there.**
    #[test]
    fn the_list_is_drawn_below_the_cards_own_block() {
        let Some(font) = font::card_font(None) else {
            return; // No proportional face on this machine.
        };
        let bare = content(Vec::new());
        let crewed = content(vec![member("fm/direct", 0, crew::CrewArrival::SETTLED)]);
        let (bare_canvas, head) = drawn(&bare, font);
        let (crewed_canvas, crewed_head) = drawn(&crewed, font);
        assert_eq!(
            head, crewed_head,
            "the head band moved when a worker arrived"
        );

        // The bare card ends where the crewed one's list begins, so its own
        // image simply has no rows there: the band is off the bottom of it.
        assert!(
            bare_canvas.height() < (head + BANDS.divider).ceil() as u32,
            "a card with no workers reserved a band for a list it does not have"
        );
        // And the crewed one draws a row there — the leftmost column at which it
        // differs from the same card whose row has not bloomed yet. See
        // [`leftmost_difference`] for why presence alone cannot say this: the
        // card's own face fills every row of its box, list included.
        let unbloomed = content(vec![member(
            "fm/direct",
            0,
            crew::CrewArrival {
                open: 1.0,
                bloom: 0.0,
            },
        )]);
        let (unbloomed, _) = drawn(&unbloomed, font);
        let first = head + BANDS.divider;
        assert!(
            leftmost_difference(&crewed_canvas, &unbloomed, first, first + BANDS.row).is_some(),
            "a card with a worker drew nothing in its own list's band"
        );
    }

    /// **A second mate's row is drawn one fixed step right of a direct one.**
    ///
    /// Measured as the leftmost column at which a card carrying the row differs
    /// from the same card whose row has not bloomed yet — the row's own left
    /// edge as drawn, not as computed.
    #[test]
    fn a_via_mate_row_starts_one_step_right_of_a_direct_one() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        let unbloomed = crew::CrewArrival {
            open: 1.0,
            bloom: 0.0,
        };
        let left_edge = |tier: u8| {
            let shown = content(vec![member("fm/worker", tier, crew::CrewArrival::SETTLED)]);
            let hidden = content(vec![member("fm/worker", tier, unbloomed)]);
            let (shown, head) = drawn(&shown, font);
            let (hidden, _) = drawn(&hidden, font);
            let first = head + BANDS.divider;
            leftmost_difference(&shown, &hidden, first, first + BANDS.row)
                .expect("a worker row drew nothing at all")
        };
        let direct = left_edge(0);
        let via = left_edge(1);
        assert!(
            via > direct,
            "the second mate's row is not stepped in: {direct} vs {via}"
        );
    }

    /// **A panel with no row motion draws every worker settled.**
    ///
    /// Herdr's answer to the mockup's `prefers-reduced-motion`: `row_motion` is
    /// `none` out of the box, and a panel that does not move rows does not open
    /// a track or fade anything in — it draws the list at the state it ends in.
    /// Read through the real `crew_for`, so this is the branch the renderer
    /// takes rather than a restatement of it.
    #[test]
    fn a_panel_with_no_row_motion_draws_every_worker_settled() {
        let app = crate::ui::sidebar::a_space_card_carries_its_own_workers::crewed_fleet();
        assert!(
            !app.sidebar_rows_move(),
            "the fixture has to be a panel with motion off"
        );
        let entries = super::super::workspace_list_entries(&app);
        let agents = super::super::sidebar_agent_entries(&app);
        let crew = crew_for(
            &app,
            &entries,
            0,
            &agents,
            app.sidebar_rows_move(),
            usize::MAX,
        );
        assert_eq!(crew.len(), 3, "the Space lost a worker");
        for member in &crew {
            assert_eq!(
                member.arrival,
                crew::CrewArrival::SETTLED,
                "{} is mid-gesture on a panel that moves nothing",
                member.name
            );
        }
    }

    /// **The track opens before any ink appears.**
    ///
    /// A row part-way through its push takes its full height and draws nothing
    /// of itself: two such rows differing in name and tier produce byte-identical
    /// bands, where the same two settled rows do not. This is the captain's
    /// *"the row below finishes sliding into place before the new worker's
    /// content starts to fade in"*, in pixels rather than in keyframes.
    #[test]
    fn a_pushing_row_takes_its_height_before_it_takes_any_ink() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        let unbloomed = crew::CrewArrival {
            open: 1.0,
            bloom: 0.0,
        };
        let closed = crew::CrewArrival {
            open: 0.0,
            bloom: 0.0,
        };
        let two = |second: crew::CrewMember| {
            content(vec![
                member("fm/first", 0, crew::CrewArrival::SETTLED),
                second,
            ])
        };

        // The height half: an unopened track takes none of it, a fully opened
        // one takes all of it, and neither depends on the ink.
        assert_eq!(
            crew::drawn_extent_px(BANDS, &two(member("fm/new", 0, closed)).crew) + BANDS.row,
            crew::drawn_extent_px(
                BANDS,
                &two(member("fm/new", 0, crew::CrewArrival::SETTLED)).crew
            ),
            "an unopened track took height"
        );
        assert_eq!(
            crew::drawn_extent_px(BANDS, &two(member("fm/new", 0, unbloomed)).crew),
            crew::drawn_extent_px(
                BANDS,
                &two(member("fm/new", 0, crew::CrewArrival::SETTLED)).crew
            ),
            "a fully opened track took less than a settled one"
        );

        // The ink half.
        let banded = |row: crew::CrewMember| {
            let (canvas, head) = drawn(&two(row), font);
            let second = head + BANDS.divider + BANDS.row;
            band(&canvas, second, second + BANDS.row)
        };
        assert_eq!(
            banded(member("fm/new", 0, unbloomed)),
            banded(member("fm/entirely-different", 1, unbloomed)),
            "a row drew its ink while its own track was still the only thing moving"
        );
        assert_ne!(
            banded(member("fm/new", 0, crew::CrewArrival::SETTLED)),
            banded(member(
                "fm/entirely-different",
                1,
                crew::CrewArrival::SETTLED
            )),
            "the fixture cannot tell two settled rows apart, so it cannot see ink at all"
        );
    }
}

/// The theme's own card colours: that they reach the pixels a card is actually
/// drawn from, and — the half that protects everyone who authored nothing —
/// that a card with no `[theme.custom]` block still draws exactly the measured
/// family it always did.
///
/// # Why these are pixel tests and not palette tests
///
/// Because the palette chain was already proved to resolve, and the cards were
/// still the wrong colour. `config.theme.custom` → `refresh_sidebar_palette` →
/// `sidebar_palette` is exercised by
/// `custom_theme_overrides_survive_auto_switch_into_the_sidebar` in
/// `src/app/mod.rs` and lands the mockup's exact hex values in the palette —
/// and none of it ever reached this module, because the pixel card resolved
/// its stroke, its dots and its ink from [`measured`] and never asked the
/// palette anything. A test that stops at the palette cannot see that, which
/// is why every assertion here reads a drawn pixel.
///
/// The sampled evidence, off the captain's own screen on 2026-08-25: the live
/// worker dot was `#7fe2e4` — [`measured::STROKE_A`] to the byte — with
/// `accent = "#5ad1ff"` set in his config, and the resting card border was
/// `#4f6b6c`, which is that same constant walked to the `Queued` endpoint of
/// the one-hue ramp.
#[cfg(test)]
mod the_theme_reaches_the_card {
    use super::*;

    /// `#5ad1ff` — the "Rio Window, Assembled" mockup's own `--cyan`, and what
    /// the captain's `[theme.custom] accent` is set to.
    const MOCKUP_CYAN: Rgb = Rgb(0x5a, 0xd1, 0xff);
    /// `#16233a` — the mockup's `--edge`, its `.card` border and its
    /// `hr.divider`.
    const MOCKUP_EDGE: Rgb = Rgb(0x16, 0x23, 0x3a);
    /// `#e6edf3` — the mockup's `--ink`.
    const MOCKUP_INK: Rgb = Rgb(0xe6, 0xed, 0xf3);

    fn themed() -> CardTheme {
        CardTheme {
            accent: Some(MOCKUP_CYAN),
            edge: Some(MOCKUP_EDGE),
            face: Some(Rgb(0x0a, 0x12, 0x20)),
            ink: Some(MOCKUP_INK),
            ok: Some(Rgb(0x3d, 0xdc, 0x84)),
            warn: Some(Rgb(0xff, 0xb4, 0x54)),
        }
    }

    fn content(theme: CardTheme, focused_space: bool, crew: Vec<crew::CrewMember>) -> CardContent {
        CardContent {
            title: "herdr".to_string(),
            tidbit: None,
            register: None,
            state_label: String::new(),
            state: AgentState::Working,
            stage: LifecycleStage::Running,
            severity: Severity::Clear,
            hues: StageHues([196.0; 5]),
            ground: Rgb(0x04, 0x07, 0x0c),
            theme,
            // The captain's own setting, and the default: one measured hue
            // family, not the five-hue lifecycle channel. This is the branch
            // that was hardcoded.
            split_channels: false,
            seen: true,
            depth: 0,
            lifted: false,
            focused_space,
            mark: None,
            residue: 0,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            spider: None,
            breath: 0.0,
            wash: None,
            crew,
            bars: None,
        }
    }

    fn worker(pulse: f32) -> crew::CrewMember {
        crew::CrewMember {
            name: "fm/worker-card-redesign".to_string(),
            detail: Some("nested cards, tiered indent".to_string()),
            tier: 0,
            arrival: crew::CrewArrival::SETTLED,
            pulse,
            spider: None,
        }
    }

    /// The canvas the fixture draws on, and the card's own rect inside it.
    /// Named so [`border_strip`] can address the stroke by the same numbers
    /// [`drawn`] drew it at.
    const SHEET_W: usize = 280;
    const CARD_X: usize = 10;
    const CARD_Y: usize = 10;
    const CARD_W: usize = 250;
    const CARD_H: usize = 120;

    /// The card's own top border, corners excluded.
    ///
    /// # Why a strip and not the whole canvas
    ///
    /// Because "no pixel anywhere is near colour X" is a claim a card full of
    /// antialiased type cannot honour. The measured ramp's own edge is
    /// `#557475`, a desaturated teal, and a half-covered pixel of grey caption
    /// ink lands at `#5e6a75` — nine away on red, ten on green, zero on blue,
    /// and so inside any tolerance loose enough to admit the stroke's own
    /// antialiasing. That is a false positive about *text*, not a fact about
    /// the border. Three rows of the card's own top edge are stroke and
    /// nothing else: no glyph is set there, and the corners are excluded so
    /// the arc's own coverage ramp cannot dilute it either.
    fn border_strip(pixels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for y in CARD_Y..CARD_Y + 3 {
            for x in CARD_X + CARD_H / 2..CARD_X + CARD_W - CARD_H / 2 {
                let i = (y * SHEET_W + x) * 4;
                out.extend_from_slice(&pixels[i..i + 4]);
            }
        }
        out
    }

    /// Draw one card and hand back its pixels.
    ///
    /// Returns `None` when the machine has no proportional face, which is the
    /// same gate [`is_available`] puts on the whole path — a card is not drawn
    /// at all there, so there is nothing for these tests to be true or false
    /// about.
    fn drawn(content: &CardContent) -> Option<Vec<u8>> {
        let font = font::card_font(None)?;
        let geometry = CardGeometry::new(21.0, false);
        let mut canvas = Canvas::new(SHEET_W as u32, 150);
        draw_card(
            &mut canvas,
            &PlacedCard {
                rect: RoundRect {
                    x: CARD_X as f32,
                    y: CARD_Y as f32,
                    w: CARD_W as f32,
                    h: CARD_H as f32,
                    r: geometry.radius,
                },
                content,
                geometry,
                crew: crew::CrewBands {
                    divider: 21.0,
                    row: 34.0,
                },
            },
            font,
        );
        Some(canvas.rgba8().to_vec())
    }

    /// How close a pixel has to sit to a colour to count as that colour.
    ///
    /// Tight. A card's ink is antialiased and blended, so nothing is required
    /// to land on the target exactly — but the two colours these tests have to
    /// tell apart, `#5ad1ff` and `#7fe2e4`, are 37 apart on green and 39 on
    /// blue, so a tolerance any looser than this would call one the other and
    /// pass on a card that never changed.
    const NEAR: u32 = 12;

    fn paints_near(pixels: &[u8], target: Rgb) -> bool {
        pixels.chunks_exact(4).any(|c| {
            u32::from(c[0]).abs_diff(u32::from(target.0)) <= NEAR
                && u32::from(c[1]).abs_diff(u32::from(target.1)) <= NEAR
                && u32::from(c[2]).abs_diff(u32::from(target.2)) <= NEAR
                && c[3] > 40
        })
    }

    /// **The bug, as a test.** A themed card draws its worker dot and its
    /// accented border in the theme's own accent, and draws no pixel at the
    /// hardcoded constant it used to be locked to.
    ///
    /// The negative half is the whole point: the dot was `#7fe2e4` on the
    /// captain's real screen with `#5ad1ff` in his config, so a test that only
    /// checked the cyan was present would have passed against the bug — the
    /// two are close enough to sit in the same "cyan family" by eye, and
    /// eyeballing them is exactly what cost the time this test exists to save.
    #[test]
    fn a_themed_card_draws_its_accent_and_not_the_measured_one() {
        let Some(pixels) = drawn(&content(themed(), true, vec![worker(0.0)])) else {
            return;
        };
        assert!(
            paints_near(&pixels, MOCKUP_CYAN),
            "a themed card drew no pixel at its own accent"
        );
        assert!(
            !paints_near(&pixels, measured::STROKE_A),
            "a themed card is still drawing the hardcoded measured cyan"
        );
    }

    /// **The regression guard for the captain's decision D-c.** A card whose
    /// theme authored nothing draws the measured family, exactly as it did
    /// before any of this existed.
    ///
    /// Every built-in theme leaves [`CardTheme`] empty, so this is the branch
    /// every user who never wrote a `[theme.custom]` block is on. If this ever
    /// fails, a preference nobody expressed has started repainting their
    /// panel.
    #[test]
    fn an_unthemed_card_still_draws_the_measured_family() {
        let Some(pixels) = drawn(&content(CardTheme::UNTHEMED, true, vec![worker(0.0)])) else {
            return;
        };
        assert!(
            paints_near(&pixels, measured::STROKE_A),
            "an unthemed card stopped drawing the measured cyan"
        );
        assert!(
            !paints_near(&pixels, MOCKUP_CYAN),
            "an unthemed card drew a colour no theme asked for"
        );
    }

    /// A resting card's border is the authored `--edge` **flat**, not that
    /// colour walked down the one-hue ramp a second time.
    ///
    /// `--edge` is a stated colour in the mockup — `.card { border: 1px solid
    /// var(--edge) }` — so restating it would land somewhere neither the theme
    /// nor the reference asked for. Unthemed, the same call still walks the
    /// accent to `Queued`, which is what the sampled `#4f6b6c` was.
    #[test]
    fn a_resting_card_takes_the_authored_edge_flat() {
        assert_eq!(themed().edge(), MOCKUP_EDGE);
        let mix = crate::anim::cell::one_hue_stage_mix(LifecycleStage::Queued);
        assert_eq!(
            CardTheme::UNTHEMED.edge(),
            measured::STROKE_A.restate(mix.saturation, mix.luminance),
            "an unthemed resting card's border moved off the measured ramp"
        );
    }

    /// Nothing about an unthemed card's light changed. The one-hue branch is
    /// reached through [`CardTheme`] now, and it has to come out the other side
    /// bit for bit — including the HSL round trip `restate(1.0, 1.0)` performs
    /// on the accented card, which is why that call is still made.
    #[test]
    fn the_unthemed_one_hue_light_is_unchanged() {
        for accented in [false, true] {
            let mix = crate::anim::cell::one_hue_stage_mix(if accented {
                LifecycleStage::Running
            } else {
                LifecycleStage::Queued
            });
            assert_eq!(
                CardLight::of(
                    Severity::Clear,
                    196.0,
                    measured::CANVAS,
                    false,
                    accented,
                    CardTheme::UNTHEMED,
                )
                .ink,
                measured::STROKE_A.restate(mix.saturation, mix.luminance),
                "the one-hue ink moved for accented={accented}"
            );
        }
    }

    /// `[theme.custom]` is what [`CardTheme::resolve`] reads, and a theme that
    /// authored nothing resolves to [`CardTheme::UNTHEMED`].
    ///
    /// The resolved palette is deliberately *not* the source: it always has an
    /// `accent` — Catppuccin's is `#89b4fa` — so reading it would repaint every
    /// default user's cards blue.
    #[test]
    fn only_an_authored_custom_block_themes_a_card() {
        let mut app = AppState::test_new();
        assert_eq!(
            CardTheme::resolve(&app),
            CardTheme::UNTHEMED,
            "a theme that authored nothing themed the cards anyway"
        );
        app.theme_runtime.custom = Some(crate::config::CustomThemeColors {
            accent: Some("#5ad1ff".to_string()),
            surface0: Some("#16233a".to_string()),
            text: Some("#e6edf3".to_string()),
            ..Default::default()
        });
        let resolved = CardTheme::resolve(&app);
        assert_eq!(resolved.accent(), MOCKUP_CYAN);
        assert_eq!(resolved.edge(), MOCKUP_EDGE);
        assert_eq!(resolved.ink(), MOCKUP_INK);
        // Untouched roles still fall through to the measurement rather than to
        // some neutral of their own.
        assert_eq!(resolved.badge_ok(), measured::BADGE_OK);
        assert_eq!(resolved.face(), measured::GLASS_FACE);
    }

    /// All six roles at once, spelled as the `[theme.custom]` block someone
    /// converging on the mockup would actually write.
    ///
    /// The three-role case above proves the mechanism; this proves the
    /// *mapping* — that `surface0` is the card's edge and not its fill, that
    /// `yellow` is the warn badge and not the healthy one, and so on. A
    /// mechanism that worked while the roles were crossed would put every
    /// colour on screen and none of them where the mockup puts it.
    #[test]
    fn the_mockups_own_six_roles_land_where_the_mockup_puts_them() {
        let mut app = AppState::test_new();
        app.theme_runtime.custom = Some(crate::config::CustomThemeColors {
            accent: Some("#5ad1ff".to_string()),   // --cyan
            surface0: Some("#16233a".to_string()), // --edge
            panel_bg: Some("#0a1220".to_string()), // --panel
            text: Some("#e6edf3".to_string()),     // --ink
            green: Some("#3ddc84".to_string()),    // --ok
            yellow: Some("#ffb454".to_string()),   // --amber
            ..Default::default()
        });
        let t = CardTheme::resolve(&app);
        assert_eq!(t.accent(), Rgb(0x5a, 0xd1, 0xff), "accent");
        assert_eq!(t.edge(), Rgb(0x16, 0x23, 0x3a), "edge");
        assert_eq!(t.face(), Rgb(0x0a, 0x12, 0x20), "face");
        assert_eq!(t.ink(), Rgb(0xe6, 0xed, 0xf3), "ink");
        assert_eq!(t.badge_ok(), Rgb(0x3d, 0xdc, 0x84), "badge ok");
        assert_eq!(t.badge_warn(), Rgb(0xff, 0xb4, 0x54), "badge warn");
    }

    /// A role authored as a reset alias is *not* an authored colour.
    ///
    /// `panel_bg = "reset"` is in the configuration docs' own example, and it
    /// means "this surface has no colour of its own". [`parse_color`] turns it
    /// into `Color::Reset` and `resolve_color_rgb` answers `None` for that, so
    /// it has to land on the measurement rather than on whatever a resolved
    /// `Reset` would otherwise composite to.
    ///
    /// [`parse_color`]: crate::config::parse_color
    #[test]
    fn a_reset_alias_leaves_the_measurement_standing() {
        let mut app = AppState::test_new();
        app.theme_runtime.custom = Some(crate::config::CustomThemeColors {
            panel_bg: Some("reset".to_string()),
            green: Some("transparent".to_string()),
            ..Default::default()
        });
        let resolved = CardTheme::resolve(&app);
        assert_eq!(resolved.face(), measured::GLASS_FACE);
        assert_eq!(resolved.badge_ok(), measured::BADGE_OK);
    }

    /// A card's signature moves when its theme does.
    ///
    /// Without this the sheet is carried forward on a stale hash and the panel
    /// keeps the old theme's ink until something *else* about a card changes —
    /// which on a settled fleet is never. The same trap `hues` and `ground`
    /// are already hashed against.
    #[test]
    fn a_themes_change_moves_the_cards_signature() {
        let hash_of = |content: &CardContent| {
            let mut hasher = DefaultHasher::new();
            content.hash_into(&mut hasher);
            hasher.finish()
        };
        assert_ne!(
            hash_of(&content(CardTheme::UNTHEMED, false, Vec::new())),
            hash_of(&content(themed(), false, Vec::new())),
            "a card carried its own signature across a theme change"
        );
    }

    /// A worker's dot breath moves its card's signature too.
    ///
    /// Same trap, one level down: `crew` is hashed member by member, and a
    /// `pulse` left out of [`crew::CrewMember::hash_into`] is a dot that is
    /// computed every frame and drawn once.
    #[test]
    fn a_dots_breath_moves_the_cards_signature() {
        let hash_of = |pulse: f32| {
            let mut hasher = DefaultHasher::new();
            content(themed(), false, vec![worker(pulse)]).hash_into(&mut hasher);
            hasher.finish()
        };
        assert_ne!(
            hash_of(0.0),
            hash_of(1.0),
            "a breathing dot hashed the same at both ends of its own swing"
        );
    }

    /// A resting card — every card but the one the panel is accenting — draws
    /// its border in the theme's `--edge`, and the dashed rule inside it in the
    /// same colour.
    ///
    /// Separate from the accented case because they are two colours in the
    /// mockup and were one here: `.card` is `1px solid var(--edge)` while
    /// `.card.active` is `--cyan` at an alpha. Before this they were the same
    /// hardcoded constant at two rungs of one ramp, which is why the captain's
    /// resting borders sampled `#4f6b6c` — a dim teal — where the mockup asks
    /// for a dark navy.
    #[test]
    fn a_resting_themed_card_draws_its_border_in_the_authored_edge() {
        let Some(pixels) = drawn(&content(themed(), false, vec![worker(0.0)])) else {
            return;
        };
        let border = border_strip(&pixels);
        assert!(
            paints_near(&border, MOCKUP_EDGE),
            "a resting themed card drew its border in something other than its own edge colour"
        );
        assert!(
            !paints_near(&border, CardTheme::UNTHEMED.edge()),
            "a resting themed card is still drawing the measured ramp's own edge"
        );
    }

    /// A dot at the bottom of its breath is dimmer than one at the top, and
    /// the row's type is not.
    ///
    /// The mockup animates `.wk-dot` alone — `opacity: 1` to `0.4` — so a
    /// pass that dimmed the whole row would be the list going quiet rather
    /// than its work being live.
    #[test]
    fn the_breath_dims_the_dot_and_leaves_the_row_alone() {
        let Some(lit) = drawn(&content(themed(), false, vec![worker(0.0)])) else {
            return;
        };
        let Some(dipped) = drawn(&content(themed(), false, vec![worker(1.0)])) else {
            return;
        };
        assert!(
            paints_near(&lit, MOCKUP_CYAN),
            "the fixture drew no dot to breathe with"
        );
        assert!(
            !paints_near(&dipped, MOCKUP_CYAN),
            "a dot at the bottom of its breath is still at full strength"
        );
        // The name is drawn in the card's ink and never in the accent, so it
        // has to survive the swing untouched.
        assert!(
            paints_near(&lit, MOCKUP_INK) && paints_near(&dipped, MOCKUP_INK),
            "the breath reached the row's own type"
        );
    }
}
