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

mod canvas;
mod font;
mod measured;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::layout::Rect;

use canvas::{coverage, Canvas, Rgb, RoundRect};
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
/// **Rank is carried by width, and only by width.** A row's right edge steps in
/// by rank in `super::rank_right_inset`, and that step is now the whole signal.
/// So there is nothing left for a depth to scale: the height, the padding, the
/// stroke, the radius, the plate and the bloom's sigma are all fractions of this
/// one number on every rank, and a card's size says *what it is* in one
/// dimension rather than in two that disagreed about which of depth and rank
/// they were reading.
const BASE_HEIGHT_PX: f32 = 68.0;

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
/// Below this a bloom pixel cannot move an 8-bit channel: the peak amount is
/// [`measured::BLOOM_PEAK`] = 0.19 and it carries about +33 levels over the
/// canvas, so 0.002 is roughly a third of one level. It is also the number
/// [`BLOOM_REACH_SIGMAS`] is derived from — see there.
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
/// tier, at every cell size, on every card. For the two-lobe field this draws it
/// is **3.64 σ**; 3.7 rounds up so the cut sits under the floor rather than on
/// it.
///
/// The consequence is that the truncation is no longer visible by construction
/// rather than by measurement — the profile has already stopped painting before
/// the reach cuts it — and `card_glow_falls_to_nothing_before_it_is_cut` holds
/// it there if any of the shape constants move.
const BLOOM_REACH_SIGMAS: f32 = 3.7;

/// The narrowest a bloom's near lobe is ever drawn, in pixels.
///
/// A sigma under a pixel or two is not a gradient, it is a stroke with a fringe.
const BLOOM_SIGMA_MIN_PX: f32 = 1.6;

/// The height a card gets *nominally*, before its content pushes it taller.
///
/// Every ratio in the measured table is a fraction of this and not of the drawn
/// height — see [`CardGeometry::new`]. Named because the bloom needs it in two
/// places that must agree exactly: the field [`lay_bloom`] paints, and the image
/// [`card_image_rect`] sizes to hold it.
///
/// The same on every rank since the tiers were retired ([`BASE_HEIGHT_PX`]);
/// still a function because the cell height is a floor on it, and a host with
/// tall cells is a host where one cell is already more than the base.
fn nominal_height_px(cell_height: f32) -> f32 {
    BASE_HEIGHT_PX.max(cell_height)
}

/// The sigma of the near lobe of a card's bloom, in pixels.
fn bloom_sigma_px(cell_height: f32) -> f32 {
    (measured::BLOOM_SIGMA * nominal_height_px(cell_height)).max(BLOOM_SIGMA_MIN_PX)
}

/// How far a card's bloom is carried past its stroke, in pixels.
fn bloom_reach_px(cell_height: f32) -> f32 {
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
pub(crate) fn is_available(app: &AppState, fold_width: u16) -> bool {
    app.kitty_graphics_enabled
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
/// The sheet answers `false` and keeps drawing the character card underneath,
/// because the sheet is opaque over every cell a row owns and covers it. A shape
/// is transparent everywhere outside its own glow, so anything drawn beneath it
/// would show *through* it — the character card's border, its chip and its
/// title, doubled a few pixels off the pixel card's own. Not drawing them is
/// what makes the transparency mean "the panel is behind this card".
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
    app.view.sidebar_card_layers_published
        && app.sidebar_card_shapes
        && is_available(app, fold_width)
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

/// The ink a D-MID card carries: two title lines and the tidbit under them.
fn content_block_px(metrics: FontMetrics, tidbit_metrics: FontMetrics) -> f32 {
    let title = metrics.line_height * (TITLE_LEADING * (TITLE_LINES as f32 - 1.0) + 1.0);
    let tidbit = tidbit_metrics.line_height * (1.0 + TIDBIT_GAP);
    title + tidbit
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
    tidbit: Option<String>,
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
    /// Whether the two channels are switched on. Off draws the reference's own
    /// single hue family with its intensity following the stage, which is what
    /// shipped before the split — see [`crate::config::SidebarCardsConfig`].
    split_channels: bool,
    seen: bool,
    depth: u8,
    lifted: bool,
    /// The project mark, once there are any. See [`CardMark`].
    mark: Option<CardMark>,
    /// This frame of the card's breath, quantized to [`CARD_BREATH_STEPS`].
    /// `0.0` is the card at its own settled light, which is what a host with no
    /// card animation draws, and `1.0` is a full breath — but a snapping
    /// behaviour carries *past* `1.0` on its overshoot and this holds that too.
    /// See [`quantize`] for why the ladder has no ceiling.
    breath: f32,
    /// The state change crossing the card right now, if one is.
    wash: Option<CardWashFrame>,
}

impl CardContent {
    fn hash_into(&self, hasher: &mut DefaultHasher) {
        self.title.hash(hasher);
        self.tidbit.hash(hasher);
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
        self.split_channels.hash(hasher);
        self.seen.hash(hasher);
        self.depth.hash(hasher);
        self.lifted.hash(hasher);
        self.mark.is_some().hash(hasher);
        // Both quantized before they reach here, so a card whose light has not
        // moved by a step anyone could see hashes the same and is carried
        // forward without being redrawn. This is the whole of what keeps a tree
        // of breathing cards from rasterising on every frame.
        ((self.breath * CARD_BREATH_STEPS).round() as u16).hash(hasher);
        self.wash.map(CardWashFrame::step).hash(hasher);
    }

    /// The light of one stage on this card, at this card's severity.
    ///
    /// The two channels are supplied from two different places and meet only in
    /// [`CardLight::of`]: the stage decides which of the five hues is handed
    /// over, the severity decides how far off the panel it is placed, and
    /// neither is consulted about the other's number.
    fn light_of(&self, stage: LifecycleStage) -> CardLight {
        CardLight::of(
            stage,
            self.severity,
            self.hues.of(stage),
            self.ground,
            self.split_channels,
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
        return Rgb::from_hsl(h, s, l);
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
    /// The light one stage at one severity is drawn in, over `ground`.
    ///
    /// The one place the two channels meet, and they meet by being handed to
    /// different arguments of one function that never crosses them: `hue` goes
    /// only into the hue slot, `severity` only into the saturation and contrast
    /// slots. `stage` is consulted a second time for the bloom, which is the
    /// depth cue and not either channel — see [`CardLight::bloom`].
    fn of(
        stage: LifecycleStage,
        severity: Severity,
        hue: f32,
        ground: Rgb,
        split_channels: bool,
    ) -> Self {
        let ink = if split_channels {
            Rgb::from_tuple(crate::anim::cell::signal_ink(
                hue,
                severity,
                ground.as_tuple(),
            ))
        } else {
            // The reference's own answer to "what carries state without a
            // rainbow": one hue, with saturation and light muted by stage.
            let (sat, lum) = match stage {
                LifecycleStage::Running | LifecycleStage::Waiting | LifecycleStage::Failed => {
                    (1.0, 1.0)
                }
                LifecycleStage::Done => (
                    (1.0 + measured::MUTED_SAT) / 2.0,
                    (1.0 + measured::MUTED_LUM) / 2.0,
                ),
                LifecycleStage::Queued => (measured::MUTED_SAT, measured::MUTED_LUM),
            };
            measured::STROKE_A.restate(sat, lum)
        };
        Self {
            ink,
            lum: 1.0,
            bloom: presence(stage),
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

    /// The stroke's two ends and the bloom's, at this light.
    ///
    /// The measured gradient is reproduced as a *travel around* this card's own
    /// ink rather than as the two sampled cyans: half the measured hue swing
    /// either side, at the measured saturation ratio. So a card keeps the
    /// left-to-right gradient the reference has, centred on whatever hue its
    /// stage supplies.
    fn inks(self) -> CardInk {
        let (h, s, l) = self.ink.to_hsl();
        let l = l * self.lum;
        let stroke_a = Rgb::from_hsl(h - measured::HUE_TRAVEL / 2.0, s, l);
        let stroke_b = Rgb::from_hsl(
            h + measured::HUE_TRAVEL / 2.0,
            s * measured::STROKE_B_SAT_RATIO,
            l,
        );
        let bloomed = |c: Rgb| c.restate(measured::BLOOM_SAT_MUL, measured::BLOOM_LUM_MUL);
        CardInk {
            stroke_a,
            stroke_b,
            bloom_a: bloomed(stroke_a),
            bloom_b: bloomed(stroke_b),
            bloom: self.bloom,
        }
    }
}

/// How far off the panel a card at this stage stands, before the breath moves
/// it.
///
/// The depth channel, kept on stage because that is what the visual target puts
/// there: work in flight comes forward, a queue sits flat in the panel, and a
/// finished card is part-way back. `Failed` stands as far forward as `Running` —
/// a failure is not on the back burner.
fn presence(stage: LifecycleStage) -> f32 {
    match stage {
        LifecycleStage::Running | LifecycleStage::Waiting | LifecycleStage::Failed => 1.0,
        LifecycleStage::Done => 0.35,
        LifecycleStage::Queued => 0.0,
    }
}

/// The colours one column of a card is drawn from.
///
/// The stroke runs a gradient from its own left end to its own right one and
/// the bloom runs the same gradient in a more saturated form, so a column needs
/// both ends rather than one colour — the *column's* position in that gradient
/// is a separate axis from where the state wash has reached.
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
const CARD_BREATH_STEPS: f32 = 48.0;

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
            radius: (measured::RADIUS * nominal).max(2.0),
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
fn wrap(font: &CardFont, text: &str, px: f32, avail: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if font.width(&candidate, px) <= avail || current.is_empty() {
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
    let inks = card.inks();
    if inks.peak_bloom() <= 0.0 {
        return;
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
    let x1 = ((rect.x + rect.w + reach).ceil() as u32).min(bloom.width);
    let y1 = ((rect.y + rect.h + reach).ceil() as u32).min(bloom.height);

    // The profile is a function of distance alone, so it is a curve rather than
    // a calculation: sampled once per half pixel out to the reach and read back
    // by index. Two exponentials per pixel over a card and the ground around it
    // is most of what drawing a card costs otherwise.
    //
    // The card's own bloom strength is deliberately *not* baked into it any
    // more: it now varies across the card — the breath swings it and a state
    // wash carries two different values either side of its front — so it is a
    // per-column multiplier applied where the profile is read.
    const PROFILE_STEPS_PER_PX: f32 = 8.0;
    let profile: Vec<f32> = (0..=((reach * PROFILE_STEPS_PER_PX).ceil() as usize))
        .map(|step| {
            let d = step as f32 / PROFILE_STEPS_PER_PX;
            let near = (-(d * d) / (2.0 * near_sigma * near_sigma)).exp();
            let far = (-(d * d) / (2.0 * far_sigma * far_sigma)).exp();
            measured::BLOOM_PEAK
                * (measured::BLOOM_NEAR_WEIGHT * near + measured::BLOOM_FAR_WEIGHT * far)
        })
        .collect();
    // The bloom's colour runs the stroke's own gradient, so like the stroke it
    // depends on the column and nothing else.
    let columns: Vec<(Rgb, f32)> = (x0..x1)
        .map(|x| {
            let t = card.column_t(x);
            let ink = inks.at(t);
            (ink.bloom_a.mix(ink.bloom_b, t), ink.bloom)
        })
        .collect();

    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for (column, x) in (x0..x1).enumerate() {
            let d = rect.distance(x as f32 + 0.5, py);
            if d <= 0.0 {
                continue;
            }
            let Some(amount) = profile.get((d * PROFILE_STEPS_PER_PX) as usize) else {
                continue;
            };
            let (color, bloom_mul) = columns[column];
            let amount = *amount * bloom_mul;
            if amount > BLOOM_PAINT_FLOOR {
                bloom.lighten(x, y, color, amount);
            }
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

/// Padding inside the state chip, each side, as a multiple of its type size.
///
/// Trimmed from 0.75 in the reality pass. The chip is the single widest thing
/// competing with the title for a narrow card's one horizontal budget — wider
/// than both pads and the gap put together — so its own air is the first place
/// to look when a real title will not fit.
const CHIP_SIDE_PAD: f32 = 0.55;

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
    /// The card's right pad, before the chip has taken its share.
    right: f32,
    chip_px: f32,
    chip_width: f32,
    chip_height: f32,
    chip_fits: bool,
    chip_gap: f32,
}

impl TextColumn {
    /// Where the text has to stop once the chip has been placed.
    fn text_right(&self) -> f32 {
        self.right - self.chip_width - self.chip_gap
    }

    /// The width the title and the tidbit are set in.
    fn available(&self) -> f32 {
        let right = if self.chip_fits {
            self.text_right()
        } else {
            self.right
        };
        (right - self.left).max(0.0)
    }
}

/// Whether `title` sets whole — every word, no line overrunning — in `avail`.
fn title_sets_whole(font: &CardFont, title: &str, avail: f32) -> bool {
    if avail <= 1.0 {
        return false;
    }
    let lines = wrap(font, title, TITLE_PX, avail, TITLE_LINES);
    let words = lines
        .iter()
        .map(|line| line.split_whitespace().count())
        .sum::<usize>();
    words == title.split_whitespace().count()
        && lines
            .iter()
            .all(|line| font.width(line, TITLE_PX) <= avail + 0.5)
}

/// The chip yields to the title, never the other way round.
///
/// The card's one absolute is that a title is never shortened and never shrunk.
/// The chip is the widest thing competing with it — about a quarter of a narrow
/// card — so on a card that cannot hold both, the chip is what goes. It is
/// dropped only when dropping it actually makes the title whole: on a card too
/// narrow for the title either way there is nothing to buy, and the state is
/// worth more than one extra word.
fn text_column(
    font: &CardFont,
    geometry: &CardGeometry,
    width: f32,
    height: f32,
    state_label: &str,
    title: &str,
) -> TextColumn {
    let chip_px = (TITLE_PX * measured::TIDBIT_SIZE_MUL).max(9.0);
    let chip_metrics = font.metrics(chip_px);
    let label = state_label.to_uppercase();
    let chip_width = font.width(&label, chip_px) + chip_px * CHIP_SIDE_PAD * 2.0;
    let chip_height = (chip_metrics.line_height * 1.25).max(chip_px * 1.55);
    let left = geometry.text_inset();
    let right = width - geometry.pad_right;
    let chip_gap = geometry.pad * CHIP_GAP_MUL;

    let with_chip = right - chip_width - chip_gap - left;
    let without_chip = right - left;
    let room_for_chip = chip_height < height - 2.0 && with_chip > 0.0;
    let chip_costs_a_word =
        !title_sets_whole(font, title, with_chip) && title_sets_whole(font, title, without_chip);

    TextColumn {
        left,
        right,
        chip_px,
        chip_width,
        chip_height,
        chip_fits: room_for_chip && !chip_costs_a_word,
        chip_gap,
    }
}

/// Draw one card's body, plate, chip and text over whatever is already there.
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

    // One pass over the card, reading the same distance for the fill, the inner
    // glow and the stroke. The bloom is already down; the body is opaque, so it
    // masks itself out of it exactly as the measured build does.
    let x0 = ox.floor().max(0.0) as u32;
    let y0 = oy.floor().max(0.0) as u32;
    let x1 = ((ox + width).ceil() as u32).min(sheet.width());
    let y1 = ((oy + height).ceil() as u32).min(sheet.height());
    let inner_sigma = (measured::FILL_INNER_SIGMA * height).max(1.0);
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
    let inks = card.inks();
    let columns: Vec<(Rgb, Rgb)> = (x0..x1)
        .map(|x| {
            let t = card.column_t(x);
            let ink = inks.at(t);
            (
                measured::FILL_MID
                    .mix(measured::FILL_TRAVEL_A.mix(measured::FILL_TRAVEL_B, t), 0.5),
                ink.stroke_a.mix(ink.stroke_b, t),
            )
        })
        .collect();

    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for (column, x) in (x0..x1).enumerate() {
            let px = x as f32 + 0.5;
            let d = rect.distance(px, py);
            let (fill, gradient) = columns[column];

            let body = coverage(d);
            if body > 0.0 {
                sheet.blend(x, y, fill, body);
                // The fill is not a vertical ramp: it is a symmetric inner glow
                // from both strokes in the local stroke hue.
                if d > -inner_reach {
                    let inner = (-(d * d) / (2.0 * inner_sigma * inner_sigma)).exp()
                        * measured::FILL_EDGE_ALPHA;
                    if inner > 0.001 {
                        sheet.blend(x, y, gradient, inner * body);
                    }
                }
            }

            let stroke = coverage(d.abs() - half_stroke);
            if stroke > 0.0 {
                sheet.blend(x, y, gradient, stroke);
            }
        }
    }

    // ---- the icon slot ---------------------------------------------------
    // Drawn only when there is a mark to put in it — `CardGeometry::new` has
    // already collapsed the slot to nothing when there is not, so this is the
    // same code path either way and the plate simply has no size.
    let plate = geometry.plate.min(height - geometry.pad * 2.0).max(0.0);
    let plate_x = ox + geometry.pad;
    let plate_y = oy + (height - plate) / 2.0;
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
                    sheet.blend(x, y, top.mix(bottom, v), inside);
                }
                let line = coverage(d.abs() - hairline);
                if line > 0.0 {
                    sheet.blend(x, y, edge, line);
                }
            }
        }
    }

    // ---- the chip --------------------------------------------------------
    let column = text_column(
        font,
        geometry,
        width,
        height,
        &content.state_label,
        &content.title,
    );
    let mut text_right = ox + column.right;
    let chip_px = column.chip_px;
    let chip_metrics = font.metrics(chip_px);
    let label = content.state_label.to_uppercase();
    let chip_w = column.chip_width;
    let chip_h = column.chip_height;
    let text_left = ox + column.left;
    if column.chip_fits {
        let ink = chip_ink(content);
        let fill = measured::FILL_MID.mix(ink, 0.16);
        let edge = fill.mix(ink, 0.50);
        let chip_x = text_right - chip_w;
        let chip_y = oy + (height - chip_h) / 2.0;
        let chip_rect = RoundRect {
            x: chip_x,
            y: chip_y,
            w: chip_w,
            h: chip_h,
            r: chip_h / 2.0,
        };
        let hairline = (geometry.stroke * 0.6).max(0.8) / 2.0;
        for y in chip_y.floor().max(0.0) as u32..((chip_y + chip_h).ceil() as u32) {
            let py = y as f32 + 0.5;
            for x in chip_x.floor().max(0.0) as u32..((chip_x + chip_w).ceil() as u32) {
                let px = x as f32 + 0.5;
                let d = chip_rect.distance(px, py);
                let inside = coverage(d);
                if inside > 0.0 {
                    sheet.blend(x, y, fill, inside);
                }
                let line = coverage(d.abs() - hairline);
                if line > 0.0 {
                    sheet.blend(x, y, edge, line);
                }
            }
        }
        let baseline = chip_y + (chip_h - chip_metrics.line_height) / 2.0 + chip_metrics.ascent;
        let label_x = chip_x + (chip_w - font.width(&label, chip_px)) / 2.0;
        draw_text(
            sheet,
            font,
            &label,
            chip_px,
            label_x,
            baseline,
            ink,
            chip_x,
            chip_x + chip_w,
        );
        text_right = ox + column.text_right();
    }

    // ---- title and tidbit ------------------------------------------------
    // The same number the fit tests measure, not a second one derived here.
    let avail = column.available();
    if avail <= 1.0 {
        return;
    }
    let title_metrics = font.metrics(TITLE_PX);
    let tidbit_px = TITLE_PX * measured::TIDBIT_SIZE_MUL;
    let tidbit_metrics = font.metrics(tidbit_px);
    let lines = wrap(font, &content.title, TITLE_PX, avail, TITLE_LINES);
    let leading = title_metrics.line_height * TITLE_LEADING;

    let title_block = leading * (lines.len().max(1) as f32 - 1.0) + title_metrics.line_height;
    let tidbit_block = content
        .tidbit
        .as_ref()
        .map(|_| tidbit_metrics.line_height * (1.0 + TIDBIT_GAP))
        .unwrap_or(0.0);
    let block_top = oy + (height - title_block - tidbit_block) / 2.0;

    let ink = measured::INK.restate(1.0, (0.55 + 0.45 * lum).min(1.0));
    for (index, line) in lines.iter().enumerate() {
        let baseline = block_top + leading * index as f32 + title_metrics.ascent;
        draw_text(
            sheet, font, line, TITLE_PX, text_left, baseline, ink, text_left, text_right,
        );
    }

    if let Some(tidbit) = &content.tidbit {
        let baseline = block_top
            + title_block
            + tidbit_metrics.line_height * TIDBIT_GAP
            + tidbit_metrics.ascent;
        let tidbit_ink = measured::FILL_MID.mix(ink, measured::TIDBIT_INK_MIX);
        draw_text(
            sheet, font, tidbit, tidbit_px, text_left, baseline, tidbit_ink, text_left, text_right,
        );
    }
}

/// Set `text` with its baseline at `(x, baseline)`, clipped to `[left, right)`.
///
/// The clip is the only thing standing in for an ellipsis: a word wider than
/// its column is drawn and cut at the column edge rather than shortened, which
/// is the behaviour the captain asked for over a mid-word ellipsis.
#[allow(clippy::too_many_arguments)] // Text, size, origin, ink and clip bounds:
                                     // every one of these varies per call site and none of them groups with another.
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
) {
    let left = left.floor() as i32;
    let right = right.ceil() as i32;
    font.draw(text, px, x, baseline, |gx, gy, coverage| {
        if gx < left || gx >= right || gx < 0 || gy < 0 {
            return;
        }
        sheet.blend(gx as u32, gy as u32, ink, coverage);
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
/// stores, so this reads it and never writes or shortens it. With no `doing`
/// published the card falls back to the same name the character row shows,
/// which is what keeps a plain shell pane from drawing an empty card.
fn title_text(entry: &AgentPanelEntry) -> String {
    entry
        .tokens
        .get("doing")
        .cloned()
        .or_else(|| entry.agent_label.clone())
        .or_else(|| entry.pane_label.clone())
        .unwrap_or_else(|| entry.primary_label.clone())
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
fn breath_behaviour(state: AgentState, severity: Severity) -> &'static str {
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
) -> Option<CardContent> {
    match entry {
        super::WorkspaceListEntry::Workspace {
            ws_idx,
            worktree_child,
            ..
        } => {
            let workspace = app.workspaces.get(*ws_idx)?;
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
            let stage = crate::app::lifecycle::stage(
                tokens
                    .get(crate::app::lifecycle::STAGE_TOKEN)
                    .map(String::as_str),
                state,
            );
            let severity = crate::app::lifecycle::severity(
                tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            );
            let breath = breath(app, &row, state, severity);
            Some(CardContent {
                title: tokens.get("doing").cloned().unwrap_or(label),
                tidbit: tidbit_parts(tokens.get("project"), tokens.get("context"), age),
                state_label: crate::ui::status::state_label(state, seen).to_string(),
                state,
                stage,
                severity,
                hues: StageHues::resolve(app),
                ground: backdrop_rgb(app),
                split_channels: app.sidebar_cards.stage_hue,
                seen,
                depth: entry.depth(),
                lifted: app.active == Some(*ws_idx),
                mark: None,
                breath,
                wash: wash(app, crate::anim::CardRow::Space(workspace.id.clone())),
            })
        }
        super::WorkspaceListEntry::Agent { entry_idx, .. } => {
            let detail = agents.get(*entry_idx)?;
            let age = detail
                .last_agent_state_change_at
                .map(|at| app.state_age_now.saturating_duration_since(at));
            let row = crate::anim::ElementId::agent_row(detail.pane_id);
            let stage = crate::app::lifecycle::stage(
                detail
                    .tokens
                    .get(crate::app::lifecycle::STAGE_TOKEN)
                    .map(String::as_str),
                detail.state,
            );
            let severity = crate::app::lifecycle::severity(
                detail
                    .tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            );
            let breath = breath(app, &row, detail.state, severity);
            Some(CardContent {
                title: title_text(detail),
                tidbit: tidbit_line(detail, age),
                state_label: super::agent_status_label(detail).to_string(),
                state: detail.state,
                stage,
                severity,
                hues: StageHues::resolve(app),
                ground: backdrop_rgb(app),
                split_channels: app.sidebar_cards.stage_hue,
                seen: detail.seen,
                depth: entry.depth(),
                lifted: app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id),
                mark: None,
                breath,
                wash: wash(app, crate::anim::CardRow::Agent(detail.pane_id)),
            })
        }
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
/// `previous` is what the last frame produced. A frame whose content signature
/// matches it reports [`CardsUpdate::Unchanged`]: nothing is rasterised and
/// nothing is re-encoded, which is what makes a fleet whose cards change about
/// once every ninety seconds cost about that often rather than sixty times a
/// second.
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
    let update = match build_cards_inner(app, cards, sidebar_area, cell_size, previous, &mut motion)
    {
        Ok(Some(layers)) => CardsUpdate::Rebuilt(layers),
        Ok(None) => CardsUpdate::Unchanged,
        Err(()) => CardsUpdate::Empty,
    };
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

    // How far through its own arrival or departure each drawn row is, gathered
    // in layout order beside the cards themselves so the two lists index
    // together. Only when rows are configured to move: with motion off nothing
    // reads the engine here at all and every card is placed exactly where the
    // layout put it, which is what the panel has always done.
    let moving = app.sidebar_rows_move();
    let mut placed: Vec<(Rect, CardContent)> = Vec::new();
    let mut lives: Vec<super::motion::RowLife> = Vec::new();
    // Which input card each entry of `placed` came from, so the resolved
    // offsets can be handed back on the caller's own indexing. A card without a
    // frame or without content is skipped here and stays at rest.
    let mut placed_from: Vec<usize> = Vec::new();
    for (index, card) in cards.iter().enumerate() {
        let Some(frame) = card.card_frame else {
            continue;
        };
        if frame.width == 0 || frame.height == 0 {
            continue;
        }
        let Some(entry) = entries.get(card.entry_idx) else {
            continue;
        };
        let Some(content) = content_for(app, entry, &agents) else {
            continue;
        };
        if moving {
            lives.push(super::motion::RowLife {
                // The distance to the next row's own top, so the span a row
                // opens and closes is its height *and* the gap the layout puts
                // after it. Taken off the layout rather than recomputed from
                // `row_gap`, so the two can never disagree about what a row
                // occupies.
                height_px: row_span_cells(cards, index) * cell_h,
                settle: row_settle(app, card),
            });
        }
        placed.push((frame, content));
        placed_from.push(index);
    }
    if placed.is_empty() {
        return Err(());
    }
    let panel_px = f32::from(bounds.width) * cell_w;
    let offsets = if moving {
        super::motion::cell_offsets(
            &super::motion::row_offsets(&lives, panel_px),
            cell_w,
            cell_h,
        )
    } else {
        vec![(0, 0); placed.len()]
    };
    // Published before anything is drawn, so the offsets the connectors follow
    // are the ones the placement was planned from even on the frame the
    // rasterisation fails.
    for (slot, offset) in placed_from.iter().zip(&offsets) {
        if let Some(cell) = motion.get_mut(*slot) {
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
) -> Result<Option<Vec<SidebarCardLayer>>, ()> {
    let placement = compute_card_placement(app, cards, sidebar_area, cell_size, motion)?;

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
        dissolve: sheet_dissolve(app, cell_size),
        host_terminal_kind: app.host_terminal_kind,
        host_graphics_is_local: app.host_graphics_is_local,
    };

    if app.sidebar_card_shapes {
        return rasteriser.shapes(&placement.placed, &placement.offsets, previous);
    }
    rasteriser.sheet(&placement.placed, previous)
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
    mark: Option<CardMark>,
    breath: f32,
}

impl From<&CardContent> for CardContentWire {
    fn from(content: &CardContent) -> Self {
        Self {
            title: content.title.clone(),
            tidbit: content.tidbit.clone(),
            state_label: content.state_label.clone(),
            state: content.state,
            stage: content.stage,
            severity: content.severity,
            hues: content.hues,
            ground: content.ground,
            split_channels: content.split_channels,
            seen: content.seen,
            depth: content.depth,
            lifted: content.lifted,
            mark: content.mark,
            breath: content.breath,
        }
    }
}

impl From<CardContentWire> for CardContent {
    fn from(wire: CardContentWire) -> Self {
        Self {
            title: wire.title,
            tidbit: wire.tidbit,
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
            breath: wire.breath,
            wash: None,
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
        dissolve: None,
        host_terminal_kind,
        host_graphics_is_local,
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
    clamp_bloomed(
        frame,
        (reach / cell_w).ceil() as u16,
        (reach / cell_h).ceil() as u16,
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
    dissolve: Option<DissolveFrame<'a>>,
    /// The foreground client's detected host terminal, from `AppState`. See
    /// `crate::kitty_graphics::preferred_card_pixel_format`.
    host_terminal_kind: crate::kitty_graphics::HostTerminalKind,
    host_graphics_is_local: bool,
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
        let mut layer = self.finish(sheet_rect, held, signature, content_signature, || {
            self.rasterise(placed, sheet_rect, true)
        })?;
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

        // The expensive half, and the only part that is parallel.
        let mut drawn = self.draw_shapes(&planned, &sources, placed);

        let clip = self.clip();
        let mut layers = Vec::with_capacity(planned.len());
        for (index, planned) in planned.iter().enumerate() {
            let mut layer = match &sources[index] {
                // Untouched, or moved and nothing more. The bytes are copied but
                // the drawing is not redone, and the drawing is the expensive
                // half by an order of magnitude. This is the case every frame of
                // a slide takes.
                ShapeSource::Held(slot) => SidebarCardLayer::clone(&previous[*slot]),
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
    ) -> Vec<Option<Result<SidebarCardLayer, ()>>> {
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
                out[index] = Some(self.draw_one(index, planned, sources, placed));
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
                                local.push((index, self.draw_one(index, planned, sources, placed)));
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
            || self.rasterise(one, planned.rect, false),
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
    fn finish(
        &self,
        rect: Rect,
        held: Option<UndissolvedSheet>,
        signature: u64,
        content_signature: u64,
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
        // Picked once per image rather than per client: the same rasterised
        // bytes back every attached client's placement of this image, so a
        // single "is the host terminal local and known-fast" answer — the
        // foreground client's, detected at attach — is what all of them get.
        // See `preferred_card_pixel_format`.
        let format = crate::kitty_graphics::preferred_card_pixel_format(
            canvas_is_fully_opaque(canvas),
            self.host_terminal_kind,
            self.host_graphics_is_local,
        );
        let data = encode_canvas(canvas, format).ok_or(())?;
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
            layer: card_layer(format, width_px, height_px, data, rect),
        })
    }

    /// Draw `placed` into an image covering `rect`.
    ///
    /// `paint_backdrop` is true only for the sheet, which has to cover the
    /// character card standing under it. A shape leaves those pixels transparent
    /// — that is the whole point of it — which is sound because the character
    /// content beneath a shape is not drawn at all.
    fn rasterise(
        &self,
        placed: &[(Rect, CardContent)],
        rect: Rect,
        paint_backdrop: bool,
    ) -> Result<Canvas, ()> {
        let width_px = u32::from(rect.width) * self.cell_size.width_px;
        let height_px = u32::from(rect.height) * self.cell_size.height_px;
        // An image larger than this is a sidebar nobody has — 8 megapixels is a
        // panel over a thousand pixels wide and seven thousand tall. The guard is
        // here so a nonsense cell-size report cannot turn into a huge allocation:
        // at four bytes a pixel for the canvas and eight more for the bloom
        // field, this ceiling is about 96 MB, held only while it is being built.
        const MAX_IMAGE_PIXELS: u32 = 8_000_000;
        if width_px == 0 || height_px == 0 || width_px.saturating_mul(height_px) > MAX_IMAGE_PIXELS
        {
            return Err(());
        }

        let mut canvas = Canvas::new(width_px, height_px);
        let cards: Vec<PlacedCard<'_>> = placed
            .iter()
            .map(|(frame, content)| self.place(*frame, content, rect))
            .collect();

        if paint_backdrop {
            // Over exactly the cells each row owns. The sheet is otherwise
            // transparent, so this is what covers the character card standing
            // underneath — including in the gutter, where the card itself does
            // not reach — while leaving the tree's connectors and everything
            // outside a row showing through.
            for (frame, _) in placed {
                fill_row_backdrop(
                    &mut canvas,
                    frame,
                    rect,
                    self.cell_w,
                    self.cell_h,
                    self.backdrop,
                );
            }
        }

        let mut bloom = BloomField::new(width_px, height_px);
        for card in &cards {
            lay_bloom(&mut bloom, card);
        }
        bloom.composite(&mut canvas);

        for card in &cards {
            draw_card(&mut canvas, card, self.font);
            if card.content.lifted {
                // Selection is a change of intensity, never of hue — the same
                // rule the character card's lifted glow ramp follows.
                lift(&mut canvas, card);
            }
        }
        Ok(canvas)
    }

    /// One card's rounded rect, in the coordinates of an image covering `rect`.
    fn place<'c>(&self, frame: Rect, content: &'c CardContent, rect: Rect) -> PlacedCard<'c> {
        let geometry = CardGeometry::new(self.cell_h, content.mark.is_some());
        // The card is drawn at the one height every card is drawn at, centred in
        // the cells the row was given. The leftover is the gutter — this is where
        // the measured 0.19 h sibling gap comes back after the row height was
        // rounded up to a whole number of cells.
        let cell_top = f32::from(frame.y.saturating_sub(rect.y)) * self.cell_h;
        let cell_height = f32::from(frame.height) * self.cell_h;
        let wanted = card_height_px(self.title_metrics, self.tidbit_metrics).min(cell_height);
        // The left border stands where the tree's rails have their ink, not
        // where the card's first cell begins. See [`RAIL_INK_COLUMN_FRACTION`].
        let left =
            (f32::from(frame.x.saturating_sub(rect.x)) + RAIL_INK_COLUMN_FRACTION) * self.cell_w;
        PlacedCard {
            rect: RoundRect {
                x: left,
                y: cell_top + (cell_height - wanted) / 2.0,
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

/// Fill the cells one row owns with the ground its card floats on.
///
/// Opaque, and over the whole row rather than only under the card, because this
/// is the one thing standing between the pixel card and the character card
/// drawn beneath it: a gutter left transparent would show a sliver of the
/// character card's closing rule between every pair of pixel cards.
fn fill_row_backdrop(
    sheet: &mut Canvas,
    frame: &Rect,
    sheet_rect: Rect,
    cell_w: f32,
    cell_h: f32,
    backdrop: Rgb,
) {
    let x0 = (f32::from(frame.x.saturating_sub(sheet_rect.x)) * cell_w) as u32;
    let y0 = (f32::from(frame.y.saturating_sub(sheet_rect.y)) * cell_h) as u32;
    let x1 = (x0 + (f32::from(frame.width) * cell_w) as u32).min(sheet.width());
    let y1 = (y0 + (f32::from(frame.height) * cell_h) as u32).min(sheet.height());
    for y in y0..y1 {
        for x in x0..x1 {
            sheet.blend(x, y, backdrop, 1.0);
        }
    }
}

/// Lift the selected card, without recolouring it.
fn lift(sheet: &mut Canvas, card: &PlacedCard<'_>) {
    let rect = card.rect;
    for y in rect.y.max(0.0) as u32..((rect.y + rect.h).ceil() as u32).min(sheet.height()) {
        for x in rect.x.max(0.0) as u32..((rect.x + rect.w).ceil() as u32).min(sheet.width()) {
            let inside = coverage(rect.distance(x as f32 + 0.5, y as f32 + 0.5));
            if inside > 0.0 {
                sheet.blend(x, y, measured::STROKE_A, 0.07 * inside);
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

/// Whether every pixel is fully opaque — the gate `preferred_card_pixel_format`
/// needs before it will hand a canvas to an alpha-losing raw format. Herdr's
/// cards are translucent by design (gutters, glow falloff, rounded corners),
/// so this is expected to come back `false` for most real card sheets; a
/// single-image API payload or a full-bleed background wash are the cases
/// it exists for.
fn canvas_is_fully_opaque(sheet: &Canvas) -> bool {
    sheet.rgba8().chunks_exact(4).all(|pixel| pixel[3] == 255)
}

/// Encodes a finished canvas in whichever format the host terminal is fast
/// at (`preferred_card_pixel_format`), falling back to PNG for anything else.
fn encode_canvas(
    sheet: &Canvas,
    format: crate::api::schema::PaneGraphicsFormat,
) -> Option<Vec<u8>> {
    match format {
        crate::api::schema::PaneGraphicsFormat::Png => encode_png(sheet),
        crate::api::schema::PaneGraphicsFormat::Rgba => Some(sheet.rgba8().to_vec()),
        crate::api::schema::PaneGraphicsFormat::Rgb => Some(rgba_to_rgb(sheet.rgba8())),
    }
}

/// Drops the alpha byte from each pixel. The canvas is always fully opaque
/// where a card draws and transparent elsewhere; RGB has no way to carry
/// that transparency, so this format is only ever selected for a terminal
/// (`f=24` kitty) that composites the sheet the same way the RGBA path does
/// — see `preferred_local_pixel_format`.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.chunks_exact(4) {
        out.extend_from_slice(&pixel[..3]);
    }
    out
}

fn encode_png(sheet: &Canvas) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, sheet.width(), sheet.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // Fast rather than default: this runs on the render thread when a card's
    // content changes, and the content is flat fills and text, which is where
    // the cheap filters already get most of the ratio.
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(sheet.rgba8()).ok()?;
    drop(writer);
    Some(out)
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
    fn rgba_to_rgb_drops_every_fourth_byte() {
        let rgba = vec![1, 2, 3, 255, 4, 5, 6, 128];
        assert_eq!(rgba_to_rgb(&rgba), vec![1, 2, 3, 4, 5, 6]);
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
        assert_eq!(encoded, rgba_to_rgb(canvas.rgba8()));
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
            CardsUpdate::Empty => SheetUpdate::Empty,
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
            .filter(|entry| content_for(&app, entry, &agents).is_some())
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
        // Tall enough that the fleet's ten uniform-height rows still reach the
        // tray exactly: since height stopped stepping down by rank, every row
        // below the top tier is now as tall as the top tier's, so the fixture
        // needs more room than it did when depth 1 and 2 rows were shorter.
        let area = Rect::new(0, 0, 100, 52);

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

        let Some((sheet, tray, last_card)) = sheet_for(true) else {
            return;
        };
        assert!(tray.height > 0, "the fixture drew no tray");
        assert!(
            sheet.y + sheet.height <= tray.y,
            "the card sheet reached {} rows into the tray",
            (sheet.y + sheet.height).saturating_sub(tray.y)
        );

        // The tray is the only thing that moves the floor. With it off the
        // sheet still blooms past its last card, which is what shipped: the
        // footer the `new` button sits on is characters, not a placement.
        let Some((sheet_off, tray_off, last_card_off)) = sheet_for(false) else {
            return;
        };
        assert_eq!(tray_off, Rect::default(), "the tray drew while disabled");
        assert!(
            sheet_off.y + sheet_off.height > last_card_off,
            "turning the tray off clamped the bloom to the last card anyway"
        );
        // And the clamp costs the tray-on sheet only the bloom it cannot have:
        // it still reaches every row up to the tray's edge.
        assert_eq!(
            sheet.y + sheet.height,
            tray.y,
            "the sheet stopped short of the tray rather than at it"
        );
        assert_eq!(
            last_card, tray.y,
            "the fixture's tree did not reach the tray"
        );
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
        // Three ranks, narrowing to the right the way `rank_right_inset` does.
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
        // two 14 px lines, a tidbit and the measured padding, which is why
        // that is the number.
        assert_eq!(height, BASE_HEIGHT_PX);
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
            let lines = wrap(font, title, TITLE_PX, avail, TITLE_LINES);
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

    /// The widest state label a chip ever carries, which is the one that leaves
    /// the title the least room.
    const WIDEST_STATE_LABEL: &str = "BLOCKED";

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
            WIDEST_STATE_LABEL,
            title,
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
                    let column = real_text_column(&font, sidebar_width, cell_w, depth, title);
                    for line in wrap(&font, title, TITLE_PX, column.available(), TITLE_LINES) {
                        if font.width(&line, TITLE_PX) <= column.available() + 0.5 {
                            continue;
                        }
                        assert!(
                            !line.contains(' '),
                            "{line:?} overruns its {:.1}px column with a break available, in \
                             {face} at sidebar {sidebar_width}, cell {cell_w}, depth {depth}",
                            column.available()
                        );
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
                for title in REAL_FLEET_TITLES {
                    let column = real_text_column(&font, sidebar_width, cell_w, depth, title);
                    let lines = wrap(&font, title, TITLE_PX, column.available(), TITLE_LINES);
                    let set = lines.join(" ");
                    assert_eq!(
                        set.split_whitespace().collect::<Vec<_>>(),
                        title.split_whitespace().collect::<Vec<_>>(),
                        "dropped words in {face} at sidebar {sidebar_width}, cell {cell_w}, \
                         depth {depth}: {set:?} from {title:?}"
                    );
                    for line in &lines {
                        assert!(
                            font.width(line, TITLE_PX) <= column.available() + 0.5,
                            "{line:?} would be clipped in {face} at sidebar {sidebar_width}, \
                             cell {cell_w}, depth {depth}"
                        );
                    }
                }
            }
        }
    }

    /// And that guarantee is bought by the chip, not by the title.
    ///
    /// On a card too narrow for both, the chip is what goes — the title is the
    /// one thing on the card that is never shortened and never shrunk. This
    /// pins the direction of that trade so a later change cannot quietly
    /// reverse it and start clipping words again.
    #[test]
    fn the_chip_yields_to_the_title_and_never_the_other_way_round() {
        let mut ever_yielded = false;
        for (face, font) in font::all_available_faces() {
            for (sidebar_width, cell_w, depth) in card_widths() {
                for title in REAL_FLEET_TITLES {
                    let column = real_text_column(&font, sidebar_width, cell_w, depth, title);
                    let with_chip = column.text_right() - column.left;
                    let without_chip = column.right - column.left;
                    let where_ = format!(
                        "{face} at sidebar {sidebar_width}, cell {cell_w}, depth {depth}, \
                         {title:?}"
                    );

                    if column.chip_fits {
                        // A chip is only kept when keeping it costs no words.
                        assert!(
                            title_sets_whole(&font, title, with_chip)
                                || !title_sets_whole(&font, title, without_chip),
                            "the chip was kept at the title's expense in {where_}"
                        );
                    } else {
                        ever_yielded = true;
                        // And is only given up when giving it up buys the title
                        // — otherwise the card simply had no room for one.
                        let bought_the_title = title_sets_whole(&font, title, without_chip)
                            && !title_sets_whole(&font, title, with_chip);
                        let never_had_room = with_chip <= 0.0
                            || column.chip_height
                                >= card_height_px(
                                    font.metrics(TITLE_PX),
                                    font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
                                ) - 2.0;
                        assert!(
                            bought_the_title || never_had_room,
                            "the chip vanished for nothing in {where_}"
                        );
                    }
                }
            }
        }
        assert!(
            ever_yielded,
            "no width in the sweep exercised the chip standing down, so the trade is untested"
        );
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
            WIDEST_STATE_LABEL,
            REAL_FLEET_TITLES[0],
        );
        let without = text_column(
            font,
            &CardGeometry::new(16.0, false),
            width,
            height,
            WIDEST_STATE_LABEL,
            REAL_FLEET_TITLES[0],
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

    /// The chip is at the card's right edge, so it is the first thing a card
    /// too narrow for its content stops drawing. On his 3440-wide window every
    /// chip vanished, because the card was being laid out in a two-pixel cell.
    /// On the fleet's real geometry none may, in any face and at any tier.
    #[test]
    fn the_state_chip_is_drawn_on_the_fleets_own_geometry() {
        for (face, font) in font::all_available_faces() {
            for depth in 0..3u8 {
                for title in REAL_FLEET_TITLES {
                    let column = real_text_column(
                        &font,
                        FLEET_SIDEBAR_COLUMNS,
                        FLEET_CELL_WIDTH_PX,
                        depth,
                        title,
                    );
                    assert!(
                        column.chip_fits,
                        "no chip in {face} at the fleet's own geometry, depth {depth}, {title:?}"
                    );
                    // And it is not bought with a word.
                    assert!(
                        title_sets_whole(&font, title, column.available()),
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
        assert_eq!(wrap(font, "Ship PR", TITLE_PX, 400.0, TITLE_LINES).len(), 1);
    }

    /// With the split switched off the card is exactly what it was: the
    /// reference's one hue family, with state carried by saturation and bloom.
    ///
    /// This is the invariant the card shipped with, kept as the `stage_hue =
    /// false` contract rather than deleted, so the fallback really is the old
    /// look and not an untested branch nobody has drawn.
    #[test]
    fn without_the_split_a_card_stays_inside_the_measured_hue_family() {
        for stage in LifecycleStage::ALL {
            let light = CardLight::of(stage, Severity::Critical, 0.0, measured::CANVAS, false);
            assert!((0.0..=1.0).contains(&light.lum));
            assert!((0.0..=1.0).contains(&light.bloom));
            // Desaturating toward grey is allowed; rotating the hue is not, and
            // no severity may rotate it either — the whole channel is off.
            let Rgb(r, g, b) = light.ink;
            assert!(
                b >= r && g >= r,
                "{stage:?} moved the stroke out of the blue-cyan family: {r},{g},{b}"
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
                    CardLight::of(stage, severity, hue, ground, true)
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
                let (_, sat, light) = CardLight::of(stage, severity, hue, ground, true)
                    .ink
                    .to_hsl();
                assert!(
                    (light - placed).abs() < 0.02,
                    "{stage:?} is placed at lightness {light:.3} where {severity:?} \
                     asks for {placed:.3}: the stage channel reached into the \
                     severity channel"
                );
                let first = CardLight::of(LifecycleStage::Queued, severity, hues[0], ground, true)
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
                    let (h, _, l) = CardLight::of(stage, severity, hue, ground, true)
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
                    let ink = CardLight::of(stage, severity, hue, ground, true).ink;
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

    /// The title is the fleet's own words. Herdr picks which token to read and
    /// never edits the value.
    #[test]
    fn the_title_is_the_published_doing_string_verbatim() {
        let mut entry = AgentPanelEntry::test_new("worker");
        assert_eq!(title_text(&entry), "worker");
        let doing = "Investigateing killed Okta corpus and Herdr work sessions";
        entry.tokens.insert("doing".to_string(), doing.to_string());
        assert_eq!(title_text(&entry), doing);
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
    fn framed(app: &AppState) -> Vec<Rect> {
        super::super::compute_workspace_card_areas(app, sidebar_rect())
            .into_iter()
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
            if content_for(&app, entry, &agents).is_none() {
                continue;
            }
            // The pixel band this row owns inside the sheet.
            let y0 = (u32::from(frame.y.saturating_sub(sheet.rect.y)) as f32 * cell_h) as u32;
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
    /// at the captain's 42-column width. This is [`super::rank_right_inset`],
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
            if content_for(&app, entry, &agents).is_none() {
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
    fn a_card_glow_falls_to_nothing_before_it_is_cut() {
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
                assert!(
                    outermost <= 2,
                    "a card's glow was still worth alpha {outermost} where it was \
                     truncated — that is a step in open panel, not a falloff. The \
                     reach has to be re-derived whenever the bloom's sigma, its far \
                     lobe or its weights move",
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no glow edge was checked");
    }

    /// Whether this layer runs into the panel's own boundary on `side`, in the
    /// order [`a_card_glow_falls_to_nothing_before_it_is_cut`] scans them.
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

    /// The sheet's corners *are* opaque, which is what makes the test above
    /// worth running.
    ///
    /// A check both models satisfied would be checking nothing. The sheet paints
    /// its backdrop over every cell of every row, so the rectangle reaches the
    /// corner at full alpha and the glow terminates against it.
    #[test]
    fn the_sheet_is_an_opaque_rectangle_and_that_is_the_bug() {
        let app = pixel_fleet_app();
        let Some(layers) = built(&app) else {
            return; // No face on this machine.
        };
        let opaque_corner = framed(&app)
            .into_iter()
            .any(|frame| frame_corner_alphas(&layers[0], frame, app.host_cell_size).contains(&255));
        assert!(
            opaque_corner,
            "the sheet stopped painting a background — if that is deliberate, it \
             has converged with the shapes path and one of the two should go"
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
            true,
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
            true,
        );
        assert!(
            bytes.is_empty() && second.is_empty(),
            "a client that drew its character cards was sent the shapes too, \
             which doubles every border, chip and title a few pixels off"
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
            true,
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
            CardsUpdate::Unchanged | CardsUpdate::Empty => None,
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

    #[test]
    fn the_arriving_row_itself_starts_clear_of_the_panel_and_ends_home() {
        let Some((mid, settled, index, _)) = mid_and_settled() else {
            return;
        };
        let panel = i32::from(super::super::sidebar_content_rect(sidebar_rect()).width);
        assert!(
            mid[index].viewport().0 >= panel,
            "an arrival that starts on screen reads as a jump: {:?} against a \
             {panel}-column panel",
            mid[index].viewport()
        );
        assert_eq!(settled[index].viewport().0, mid[index].rect.x as i32);
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
        assert_ne!(
            arriving.motion_cells.0, 0,
            "the arriving row is not travelling, so this is not its first frame"
        );
        let span = mid_cards
            .get(index + 1)
            .map(|card| -card.motion_cells.1)
            .expect("nothing below the arriving row");
        assert!(
            span > 0,
            "the row below the arrival did not hold its ground"
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
                true,
            );
            std::fs::write(format!("{}/{name}.esc", self.dir), &standalone).expect("writes");

            let incremental = crate::kitty_graphics::encode_local_pane_graphics(
                app,
                &self.runtimes,
                app.view.tab_surface(),
                cell_size,
                &mut self.persistent,
                true,
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
    /// The lifecycle is built from [`crate::config::SidebarCardsConfig`]'s own
    /// answer rather than from `AppState::sidebar_row_lifecycle`, which
    /// additionally requires this machine to have a proportional face —
    /// something a container running the suite routinely does not. The
    /// behaviours declared are the same ones either way, and the read path
    /// under test is the same read path.
    fn breathing_fleet() -> (AppState, Instant) {
        let mut app = pixel_fleet_app();
        app.sidebar_card_shapes = true;
        let now = Instant::now();
        let mut lifecycle = crate::anim::Lifecycle::still();
        for behaviour in app.sidebar_cards.pulse_behaviours() {
            lifecycle = lifecycle.with_idle(*behaviour);
        }
        publish_rows(&mut app, &lifecycle, now);
        (app, now)
    }

    fn publish_rows(app: &mut AppState, lifecycle: &crate::anim::Lifecycle, now: Instant) {
        let agents: Vec<_> = super::super::sidebar_agent_live_entries(app)
            .iter()
            .map(|entry| {
                (
                    crate::anim::ElementId::agent_row(entry.pane_id),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect();
        app.anim
            .observe(now, crate::anim::Family::AgentRow, lifecycle, agents);
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
            .filter_map(|entry| content_for(app, entry, &agents))
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
        let bloom_swing = swing(&blooms);
        assert!(
            lum_swing > 0.02,
            "the card did not breathe at all: its light moved by {lum_swing:.4}"
        );
        assert!(
            bloom_swing > lum_swing * 2.0,
            "the depth cue is the point: the bloom swung {bloom_swing:.3} against \
             the ink's {lum_swing:.3}, so this reads as a dimmer rather than as \
             a card settling back into the panel"
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
    /// Asserted on the *drawn ink* and not on the envelope, because a number
    /// that survives quantisation and is then flattened on the way to the
    /// colour has still not reached the screen: a card at the top of its snap
    /// has to be measurably further back than the same card at a full breath.
    #[test]
    fn the_snaps_overshoot_survives_the_ladder_and_reaches_the_ink() {
        let (mut app, now) = breathing_fleet();
        if card_in(&app, AgentState::Working).is_none() {
            return;
        }
        let mut peak_breath = f32::MIN;
        let mut deepest_bloom = f32::MAX;
        // Four live cycles at a step far finer than the ladder, so what is
        // being walked is the curve rather than the sampling.
        for step in 0..=2_000u64 {
            app.anim.advance(now + Duration::from_millis(step * 5));
            let Some(card) = card_in(&app, AgentState::Working) else {
                return;
            };
            peak_breath = peak_breath.max(card.breath);
            deepest_bloom = deepest_bloom.min(card.arrived_light().bloom);
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
        // And it is visible: a full breath sets the bloom back by
        // `BREATH_BLOOM_DIP`, so an overshoot has to set it back further still.
        let Some(card) = card_in(&app, AgentState::Working) else {
            return;
        };
        let at_full_breath = card.settled_light().bloom * (1.0 - BREATH_BLOOM_DIP);
        assert!(
            deepest_bloom < at_full_breath,
            "the card's bloom bottomed out at {deepest_bloom:.4}, which is no \
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
    /// changed, not a highlight that passes over and leaves nothing behind.
    ///
    /// Checked at the end of the sweep across every column of the card: all of
    /// them are the new state's light, none of them have gone back. A band
    /// would end with the card exactly as it started, which is the thing this
    /// is not.
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
        // And the state it left is genuinely gone from the card, rather than
        // the two states having happened to resolve alike.
        assert_ne!(
            arrived,
            card.light_at(AgentState::Idle).breathed(card.breath).inks()
        );
    }

    /// And while it is crossing, the card really is two states: the new one
    /// behind the front and the one it left ahead of it.
    ///
    /// This is what makes the change legible *as a change* rather than as a
    /// card that is suddenly a different colour, and it is the reason the wash
    /// has to remember the state the card came from at all.
    #[test]
    fn a_wash_in_flight_is_new_on_its_left_and_old_on_its_right() {
        let Some((mut app, now, from)) = washing(AgentState::Idle, AgentState::Working) else {
            return;
        };
        let window = app.sidebar_cards.wash_duration();
        let left_behind = presence(crate::app::lifecycle::stage(None, from));
        let arriving = presence(crate::app::lifecycle::stage(None, AgentState::Working));

        // Somewhere in the middle of the sweep. Sampled across several steps
        // rather than at one, because where the front is at any single instant
        // is the curve's business and not this test's.
        let mut saw_two_sides = false;
        for step in 1..=8 {
            let at = now + window / 12 * step;
            app.anim.advance(at);
            observe_washes(&mut app, at);
            let Some(card) = card_in(&app, AgentState::Working) else {
                return;
            };
            let Some(_) = card.wash else { continue };
            let columns = across(&card);
            let left = columns.first().copied().expect("sampled");
            let right = columns.last().copied().expect("sampled");
            // Read as the *bloom strength*: it is the number the two states
            // differ in most, and it is what the halo — the depth cue the whole
            // effect is about — is laid from.
            if left.bloom > right.bloom + 1e-3 {
                saw_two_sides = true;
                assert!(
                    left.bloom <= arriving + 1e-6,
                    "the left of the card overshot the state it was arriving at"
                );
                assert!(
                    right.bloom <= left_behind + 1e-6,
                    "the right of the card had already passed the state it is \
                     still meant to be in"
                );
                // And every column between the two is on the ramp between them,
                // in order: a front, not two halves.
                for pair in columns.windows(2) {
                    assert!(
                        pair[0].bloom >= pair[1].bloom - 1e-3,
                        "the front doubled back on itself inside the card"
                    );
                }
            }
        }
        assert!(
            saw_two_sides,
            "the card was never two states at once, so nothing swept across it"
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
}
// Throwaway measurement probes written for herdr-glow-cause-scout.
// Append verbatim to the END of src/ui/sidebar/image_card.rs on fork/master
// (measured at aeb46d50) and run with:
//   cargo test --bin herdr glow_probe -- --nocapture --test-threads=1
//   cargo test --release --bin herdr probe_i_cost -- --nocapture
// They reuse the crate's own test fixtures (tests::pixel_fleet_app, 42-column
// sidebar_rect, 10x21 px cell) and the real build_cards() path, so what they
// measure is the actual PNG bytes the server puts on the wire.
