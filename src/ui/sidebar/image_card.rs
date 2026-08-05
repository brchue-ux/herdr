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

use crate::app::state::AppState;
use crate::detect::AgentState;
use crate::kitty_graphics::HostCellSize;
use crate::ui::sidebar::AgentPanelEntry;

/// The base card height, in pixels, at depth 0.
///
/// REC-TIGHT from `data/herdr-card-iteration-2/`: the tier ratios were
/// originally calibrated for type that grew with the box, and once the title
/// stopped scaling the 96 px top card was mostly air.
const BASE_HEIGHT_PX: f32 = 68.0;

/// What each depth scales the base by.
///
/// A 65% step: 1.00 / 0.65 / 0.42. Applied to everything on the card *except*
/// the title's type size — see [`TITLE_PX`].
const TIER_SCALE: [f32; 3] = [1.00, 0.65, 0.4225];

/// The title's type size, in pixels, at every tier.
///
/// Fixed rather than scaled, and never smaller: the fit ladder in
/// `data/herdr-card-iteration-2/` measured the legibility floor at two numbers
/// rather than one — about 14 px at Light 300 and about 10 px at Medium 500 —
/// because below the floor the stem is thinner than a device pixel and the
/// rasteriser hands back grey instead of ink. The card is set at the Light
/// floor, which is the comfortable one.
///
/// It is also why the tier scale cannot own the card's height. Two lines at
/// this size plus the tidbit is about 54 px of block, so a card at the 0.42
/// tier's nominal 29 px could not hold its own title. The tier scale therefore
/// sets a *floor* the card is never shorter than, and the content sets the
/// height when it needs more. See [`tier_height_px`].
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
/// Cut from 0.55 in the reality pass. It is air, and air is the only thing a
/// card below the top tier has left to give: with the title's size fixed, this
/// gap and [`MIN_VERTICAL_PAD_PX`] are the whole difference between a tier that
/// reads smaller and one that does not. See [`tier_height_px`].
const TIDBIT_GAP: f32 = 0.35;

/// The narrowest panel the image card draws on, in columns.
///
/// The same threshold the character card shell uses, deliberately: below it a
/// row is a bare styled line rather than a box, and a pixel card drawn over a
/// row that is not a box would be a third layout. `MIN_FOLD_WIDTH` is 32, which
/// is a 34-column sidebar.
pub(crate) const MIN_FOLD_WIDTH: u16 = super::card::MIN_FOLD_WIDTH;

/// A card's own bloom reaches this far past its stroke, as a fraction of the
/// card's height — measured dead by 26–28 px on a 61 px card.
const BLOOM_REACH: f32 = 0.45;

/// One finished image and the cells it covers — one card's shape, or the whole
/// tree's sheet.
///
/// `Clone` so a card whose content did not change can be carried into the next
/// frame's list when a *sibling* did. That copies the encoded bytes — a few
/// kilobytes of flat-fill PNG — and skips the rasterisation, which is the
/// expensive half by roughly an order of magnitude.
#[derive(Clone)]
pub(crate) struct SidebarCardLayer {
    /// The cell rect this image is placed at. Chosen by the tree's own geometry:
    /// for a shape, exactly its own card plus the reach of that card's bloom;
    /// for the sheet, every card plus the reach of theirs.
    pub rect: Rect,
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
        && font::card_font(app.sidebar_card_font.as_deref()).is_some()
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

/// The tier a row at `depth` is drawn at.
fn tier(depth: u8) -> f32 {
    TIER_SCALE[usize::from(depth).min(TIER_SCALE.len() - 1)]
}

/// The height a card at `depth` wants, in pixels.
///
/// `max(tier floor, what the content needs)`. The tier's nominal height is a
/// floor and not a ceiling because the title's size is fixed: a card is allowed
/// to be taller than its tier when two lines of 14 px type and a tidbit will
/// not fit in it, and that is the case at every tier below the first. The tier
/// still reads — the padding, the plate, the chip, the stroke and the radius
/// all scale with it — it just cannot squeeze the words.
///
/// # Why the ladder is not 1.00 / 0.65 / 0.42 on screen
///
/// It cannot be, and the arithmetic says so rather than the implementation.
/// Two 14 px lines at 1.25 leading is about 2.25 line heights, the tidbit under
/// them adds its own line and the gap above it, and the whole block will not go
/// below roughly 0.85 of the 68 px base without shrinking type the captain
/// fixed at 14 px. Tier 1's nominal is 0.65 of base and tier 2's is 0.42, so
/// *both* land on that floor and both come out the same height. The step that
/// survives is the one between the top tier and everything under it, which is
/// the one that carries the meaning — a worker is visibly shorter than the
/// first mate. Anything more needs either a smaller title or a card that drops
/// the tidbit below the top tier, and both are the captain's call, not this
/// function's.
fn tier_height_px(depth: u8, metrics: FontMetrics, tidbit_metrics: FontMetrics) -> f32 {
    let nominal = BASE_HEIGHT_PX * tier(depth);
    nominal.max(content_floor_px(metrics, tidbit_metrics))
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
/// This, and not the measured 0.148 h padding, is what sets a card's height
/// once the content stops fitting the tier: the measured padding is what a card
/// *wants*, and at the top tier it gets it — 68 px is almost exactly two 14 px
/// lines, a tidbit and 0.148 h on each side, which is why the captain's base
/// height is that number. Below the top tier the same block no longer fits the
/// nominal at all, so the padding gives way first and the height only grows
/// once it has nothing left to give.
///
/// Cut from 5 px in the reality pass, for the reason spelled out on
/// [`tier_height_px`]: the floor this sets is what a sub-top-tier card's height
/// actually is, so every pixel of it is a pixel the tier scale does not get.
const MIN_VERTICAL_PAD_PX: f32 = 3.0;

/// Rows a card at `depth` occupies, or `None` when the pixel path is not live.
///
/// This is the one place the pixel design reaches back into the character
/// layout. Everything else about a row — where it starts, what it can be
/// clicked to select, whether it scrolls off — is unchanged, but its *height*
/// has to come from the card being drawn or the image would not fill its cells.
pub(crate) fn row_height_cells(app: &AppState, depth: u8, fold_width: u16) -> Option<u16> {
    if !is_available(app, fold_width) {
        return None;
    }
    let font = font::card_font(app.sidebar_card_font.as_deref())?;
    let cell_height = f32::from(u16::try_from(app.host_cell_size.height_px).ok()?);
    if cell_height <= 0.0 {
        return None;
    }
    let wanted = tier_height_px(
        depth,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardMark {}

/// What one card says.
struct CardContent {
    title: String,
    tidbit: Option<String>,
    state_label: String,
    state: AgentState,
    seen: bool,
    depth: u8,
    lifted: bool,
    /// The project mark, once there are any. See [`CardMark`].
    mark: Option<CardMark>,
}

impl CardContent {
    fn hash_into(&self, hasher: &mut DefaultHasher) {
        self.title.hash(hasher);
        self.tidbit.hash(hasher);
        self.state_label.hash(hasher);
        (self.state as u8).hash(hasher);
        self.seen.hash(hasher);
        self.depth.hash(hasher);
        self.lifted.hash(hasher);
        self.mark.is_some().hash(hasher);
    }
}

/// The ground the cards float on.
///
/// The reference's own canvas is `#09111C`, but Herdr paints no global
/// background: every colour it draws composites against whatever the host
/// terminal is using, which is the RGB it measures with OSC 11. So the ground
/// under a card is the host's background when the host told us one, and the
/// panel's own background after that; the measured canvas is only the last
/// resort, for a host that answered neither.
///
/// It matters because the bloom is *lift*: the reference has no drop shadow
/// anywhere, and its cards float by being brighter than the ground rather than
/// by casting onto it. A bloom with nothing under it to lift is invisible.
fn backdrop_rgb(app: &AppState) -> Rgb {
    if let Some(background) = app.host_terminal_theme.background {
        return Rgb(background.r, background.g, background.b);
    }
    crate::ui::color::resolve_color_rgb(app.palette.panel_bg, &app.host_terminal_theme)
        .map(|rgb| Rgb(rgb.0, rgb.1, rgb.2))
        .unwrap_or(measured::CANVAS)
}

/// The chip's ink per state.
///
/// The reference's own cards never move hue to signal anything — its inactive
/// card is the same hue at 24% of the saturation — so the family stays inside
/// H 181–210 and state is carried by saturation and lightness within it. These
/// are the values the density and icon passes were rendered and reviewed at.
fn chip_ink(state: AgentState, seen: bool) -> Rgb {
    let (h, s, l) = match (state, seen) {
        (AgentState::Blocked, _) => (181.0, 0.75, 0.72),
        (AgentState::Working, _) => (192.0, 0.62, 0.66),
        (AgentState::Idle, false) => (205.0, 0.40, 0.52),
        (AgentState::Idle, true) => (210.0, 0.16, 0.42),
        (AgentState::Unknown, _) => (210.0, 0.10, 0.36),
    };
    Rgb::from_hsl(h, s, l)
}

/// How much saturation and light a card keeps.
///
/// The reference's answer to "what carries state without a rainbow": an
/// inactive card is the same hue with S 14.5% where an active one is 59.6%, at
/// 57% of the luminance, and with no bloom at all.
fn card_intensity(state: AgentState) -> (f32, f32, f32) {
    match state {
        AgentState::Working | AgentState::Blocked => (1.0, 1.0, 1.0),
        AgentState::Idle => (
            (1.0 + measured::MUTED_SAT) / 2.0,
            (1.0 + measured::MUTED_LUM) / 2.0,
            0.35,
        ),
        AgentState::Unknown => (measured::MUTED_SAT, measured::MUTED_LUM, 0.0),
    }
}

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
    /// Resolved against the tier's *nominal* height rather than the card's
    /// drawn height.
    ///
    /// Every ratio in the measured table is a fraction of `h`, and the card's
    /// drawn height is `max(nominal, content)` — so measuring the padding and
    /// the plate against the drawn height would silently undo the tier scale
    /// on exactly the cards the content pushed taller. The nominal is what the
    /// tier means; the extra height is slack, and slack belongs to the gap
    /// around the content, not to the chrome.
    ///
    /// `has_mark` collapses the icon slot. An empty plate is not a placeholder,
    /// it is a box; and at 0.70 h plus its gap it was the single widest thing
    /// on the card that carried no information, taking that width from the one
    /// thing that does. The slot keeps its measured size for the day something
    /// goes in it and is worth nothing until then.
    fn new(depth: u8, cell_height: f32, has_mark: bool) -> Self {
        let nominal = (BASE_HEIGHT_PX * tier(depth)).max(cell_height);
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
            bloom_sigma: (measured::BLOOM_SIGMA * nominal).max(1.6),
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
    /// The stroke's two ends and the bloom's, after this card's state has had
    /// its say about saturation and light.
    fn inks(&self) -> (Rgb, Rgb, Rgb, Rgb, f32, f32) {
        let (sat, lum, bloom_mul) = card_intensity(self.content.state);
        let stroke_a = measured::STROKE_A.restate(sat, lum);
        let stroke_b = measured::STROKE_B.restate(sat, lum);
        let red = |c: Rgb| Rgb((f32::from(c.0) * measured::BLOOM_RED_MUL) as u8, c.1, c.2);
        (
            stroke_a,
            stroke_b,
            red(stroke_a),
            red(stroke_b),
            bloom_mul,
            lum,
        )
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
    let (_, _, bloom_a, bloom_b, bloom_mul, _) = card.inks();
    if bloom_mul <= 0.0 {
        return;
    }
    let rect = card.rect;
    let reach = rect.h * BLOOM_REACH;
    let near_sigma = card.geometry.bloom_sigma;
    let far_sigma = near_sigma * measured::BLOOM_FAR_SIGMA_MUL;

    let x0 = (rect.x - reach).floor().max(0.0) as u32;
    let y0 = (rect.y - reach).floor().max(0.0) as u32;
    let x1 = ((rect.x + rect.w + reach).ceil() as u32).min(bloom.width);
    let y1 = ((rect.y + rect.h + reach).ceil() as u32).min(bloom.height);

    // The profile is a function of distance alone, so it is a curve rather than
    // a calculation: sampled once per half pixel out to the reach and read back
    // by index. Two exponentials per pixel over a card and the ground around it
    // is most of what drawing a card costs otherwise.
    const PROFILE_STEPS_PER_PX: f32 = 8.0;
    let profile: Vec<f32> = (0..=((reach * PROFILE_STEPS_PER_PX).ceil() as usize))
        .map(|step| {
            let d = step as f32 / PROFILE_STEPS_PER_PX;
            let near = (-(d * d) / (2.0 * near_sigma * near_sigma)).exp();
            let far = (-(d * d) / (2.0 * far_sigma * far_sigma)).exp();
            measured::BLOOM_PEAK
                * (measured::BLOOM_NEAR_WEIGHT * near + measured::BLOOM_FAR_WEIGHT * far)
                * bloom_mul
        })
        .collect();
    // The bloom's colour runs the stroke's own gradient, so like the stroke it
    // depends on the column and nothing else.
    let columns: Vec<Rgb> = (x0..x1)
        .map(|x| {
            let t = (((x as f32 + 0.5) - rect.x) / rect.w).clamp(0.0, 1.0);
            bloom_a.mix(bloom_b, t)
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
            if *amount > 0.002 {
                bloom.lighten(x, y, columns[column], *amount);
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
    let (stroke_a, stroke_b, _, _, _, lum) = card.inks();
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
    let columns: Vec<(Rgb, Rgb)> = (x0..x1)
        .map(|x| {
            let t = (((x as f32 + 0.5) - ox) / width).clamp(0.0, 1.0);
            (
                measured::FILL_MID
                    .mix(measured::FILL_TRAVEL_A.mix(measured::FILL_TRAVEL_B, t), 0.5),
                stroke_a.mix(stroke_b, t),
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
        let ink = chip_ink(content.state, content.seen);
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
            Some(CardContent {
                title: tokens.get("doing").cloned().unwrap_or(label),
                tidbit: tidbit_parts(tokens.get("project"), tokens.get("context"), age),
                state_label: crate::ui::status::state_label(state, seen).to_string(),
                state,
                seen,
                depth: entry.depth(),
                lifted: app.active == Some(*ws_idx),
                mark: None,
            })
        }
        super::WorkspaceListEntry::Agent { entry_idx, .. } => {
            let detail = agents.get(*entry_idx)?;
            let age = detail
                .last_agent_state_change_at
                .map(|at| app.state_age_now.saturating_duration_since(at));
            Some(CardContent {
                title: title_text(detail),
                tidbit: tidbit_line(detail, age),
                state_label: super::agent_status_label(detail).to_string(),
                state: detail.state,
                seen: detail.seen,
                depth: entry.depth(),
                lifted: app.is_active_pane(detail.ws_idx, detail.tab_idx, detail.pane_id),
                mark: None,
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
) -> CardsUpdate {
    match build_cards_inner(app, cards, sidebar_area, cell_size, previous) {
        Ok(Some(layers)) => CardsUpdate::Rebuilt(layers),
        Ok(None) => CardsUpdate::Unchanged,
        Err(()) => CardsUpdate::Empty,
    }
}

/// `Ok(Some)` is new artwork, `Ok(None)` is what is already held, `Err` is none
/// at all.
fn build_cards_inner(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
    previous: &[SidebarCardLayer],
) -> Result<Option<Vec<SidebarCardLayer>>, ()> {
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

    let mut placed: Vec<(Rect, CardContent)> = Vec::new();
    for card in cards {
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
        placed.push((frame, content));
    }
    if placed.is_empty() {
        return Err(());
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
    let extents: Vec<(u8, Rect)> = placed
        .iter()
        .map(|(frame, content)| (content.depth, *frame))
        .collect();
    let field_rect = dissolve_field_rect(
        &extents,
        (cell_w, cell_h),
        (title_metrics, tidbit_metrics),
        bounds,
        bloom_floor,
    )
    .ok_or(())?;

    let rasteriser = Rasteriser {
        font,
        title_metrics,
        tidbit_metrics,
        cell_size,
        cell_w,
        cell_h,
        field: field_rect,
        bounds,
        bloom_floor,
        backdrop,
        dissolve: sheet_dissolve(app, cell_size),
    };

    if app.sidebar_card_shapes {
        return rasteriser.shapes(&placed, previous);
    }
    rasteriser.sheet(&placed, previous)
}

/// The cells one card's own image covers: its frame plus the reach of its own
/// bloom, clamped into the panel.
///
/// Its *own* bloom and not the tree's largest, because the reach is a fraction
/// of the card's drawn height (see [`lay_bloom`]) and a worker's card is two
/// thirds of a mate's. Giving every card the top tier's margin would make every
/// smaller card's image bigger than it needs to be, and the margin is
/// transparent padding that still has to be encoded and uploaded.
fn card_image_rect(
    depth: u8,
    frame: Rect,
    cell: (f32, f32),
    metrics: (FontMetrics, FontMetrics),
    bounds: Rect,
    bloom_floor: u16,
) -> Option<Rect> {
    let (cell_w, cell_h) = cell;
    let (title_metrics, tidbit_metrics) = metrics;
    let drawn =
        tier_height_px(depth, title_metrics, tidbit_metrics).min(f32::from(frame.height) * cell_h);
    let reach = drawn * BLOOM_REACH;
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
/// [`tier_height_px`] floors a card at what its content needs, so a proportional
/// face with a tall line height draws the top tier past [`BASE_HEIGHT_PX`] and
/// gives it a larger margin than a constant read off that base.
fn dissolve_field_rect(
    cards: &[(u8, Rect)],
    cell: (f32, f32),
    metrics: (FontMetrics, FontMetrics),
    bounds: Rect,
    bloom_floor: u16,
) -> Option<Rect> {
    cards
        .iter()
        .filter_map(|(depth, frame)| {
            card_image_rect(*depth, *frame, cell, metrics, bounds, bloom_floor)
        })
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
            hash_placed(&mut hasher, frame, content);
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
        if previous
            .is_some_and(|previous| previous.signature == signature && previous.rect == sheet_rect)
        {
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
        let layer = self.finish(sheet_rect, held, signature, content_signature, || {
            self.rasterise(placed, sheet_rect, true)
        })?;
        Ok(Some(vec![layer]))
    }

    /// One transparent image per card.
    ///
    /// The card is the unit of everything here: its own rect, its own signature,
    /// its own placement. A card whose content did not change is carried forward
    /// without being rasterised or re-encoded even when a sibling changed, which
    /// is the property the queued motion work needs — moving one card must cost
    /// one card, not the tree.
    fn shapes(
        &self,
        placed: &[(Rect, CardContent)],
        previous: &[SidebarCardLayer],
    ) -> Result<Option<Vec<SidebarCardLayer>>, ()> {
        // Measured first, drawn second. Deciding whether anything moved before
        // rasterising anything is what makes a settled panel free: the common
        // frame walks this list, matches every entry, and returns having encoded
        // nothing.
        let planned: Vec<PlannedShape> = placed
            .iter()
            .map(|(frame, content)| self.plan(*frame, content))
            .collect::<Option<_>>()
            .ok_or(())?;

        if planned.len() == previous.len()
            && planned.iter().zip(previous).all(|(planned, previous)| {
                planned.signature == previous.signature && planned.rect == previous.rect
            })
        {
            return Ok(None);
        }

        let mut layers = Vec::with_capacity(planned.len());
        for (index, planned) in planned.iter().enumerate() {
            // Positional rather than by identity: a card only keeps its slot
            // while the tree's shape is unchanged, and a tree that reordered has
            // moved every rect it reordered — which the signature already caught.
            let previous = previous.get(index);
            if let Some(held) = previous.filter(|previous| {
                previous.signature == planned.signature && previous.rect == planned.rect
            }) {
                // Untouched. The bytes are copied but the drawing is not redone,
                // and the drawing is the expensive half by an order of magnitude.
                layers.push(SidebarCardLayer::clone(held));
                continue;
            }
            let held = previous
                .filter(|previous| {
                    previous.content_signature == planned.content_signature
                        && previous.rect == planned.rect
                })
                .and_then(|previous| previous.undissolved.clone());
            // One card, drawn into an image that is only as large as that card
            // and the reach of its own bloom, with no background painted
            // anywhere. Everything outside the glow stays at alpha zero.
            let one = &placed[index..index + 1];
            layers.push(self.finish(
                planned.rect,
                held,
                planned.signature,
                planned.content_signature,
                || self.rasterise(one, planned.rect, false),
            )?);
        }
        Ok(Some(layers))
    }

    /// What one card's image will be, before anything is drawn.
    fn plan(&self, frame: Rect, content: &CardContent) -> Option<PlannedShape> {
        let rect = self.card_rect(frame, content)?;
        let mut hasher = DefaultHasher::new();
        self.hash_common(&mut hasher, rect);
        hash_placed(&mut hasher, &frame, content);
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
        let data = match self.dissolve {
            Some(dissolve) => {
                let mut canvas = Canvas::clone(&base.0);
                dissolve.apply(&mut canvas, self.dissolve_origin(rect), self.field_px());
                encode_png(&canvas)
            }
            None => encode_png(&base.0),
        }
        .ok_or(())?;
        Ok(SidebarCardLayer {
            rect,
            signature,
            content_signature,
            // Only while a transition is running: a settled panel keeps no
            // second copy of artwork it is not about to take apart.
            undissolved: self.dissolve.map(|_| base),
            layer: card_layer(width_px, height_px, data, rect),
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
        let geometry = CardGeometry::new(content.depth, self.cell_h, content.mark.is_some());
        // The card is drawn at the height its tier asked for, centred in the
        // cells the row was given. The leftover is the gutter — this is where the
        // measured 0.19 h sibling gap comes back, and it is also what makes the
        // tier scale visible again after the row height was rounded up to a whole
        // number of cells.
        let cell_top = f32::from(frame.y.saturating_sub(rect.y)) * self.cell_h;
        let cell_height = f32::from(frame.height) * self.cell_h;
        let wanted =
            tier_height_px(content.depth, self.title_metrics, self.tidbit_metrics).min(cell_height);
        PlacedCard {
            rect: RoundRect {
                x: f32::from(frame.x.saturating_sub(rect.x)) * self.cell_w,
                y: cell_top + (cell_height - wanted) / 2.0,
                w: f32::from(frame.width) * self.cell_w,
                h: wanted,
                r: geometry.radius,
            },
            content,
            geometry,
        }
    }

    /// The cells one card's own image covers, from the same [`card_image_rect`]
    /// the dissolve field is built out of.
    fn card_rect(&self, frame: Rect, content: &CardContent) -> Option<Rect> {
        card_image_rect(
            content.depth,
            frame,
            (self.cell_w, self.cell_h),
            (self.title_metrics, self.tidbit_metrics),
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
    fn hash_common(&self, hasher: &mut DefaultHasher, rect: Rect) {
        self.cell_size.width_px.hash(hasher);
        self.cell_size.height_px.hash(hasher);
        rect.x.hash(hasher);
        rect.y.hash(hasher);
        rect.width.hash(hasher);
        rect.height.hash(hasher);
        self.backdrop.0.hash(hasher);
        self.backdrop.1.hash(hasher);
        self.backdrop.2.hash(hasher);
    }
}

/// What one card's image will be, decided before any pixel is drawn.
struct PlannedShape {
    rect: Rect,
    signature: u64,
    content_signature: u64,
}

/// One card's frame and content, fed into a signature.
fn hash_placed(hasher: &mut DefaultHasher, frame: &Rect, content: &CardContent) {
    frame.x.hash(hasher);
    frame.y.hash(hasher);
    frame.width.hash(hasher);
    frame.height.hash(hasher);
    content.hash_into(hasher);
}

/// The placement a finished image is published as.
fn card_layer(
    width_px: u32,
    height_px: u32,
    data: Vec<u8>,
    sheet_rect: Rect,
) -> crate::app::state::GraphicsLayer {
    crate::app::state::GraphicsLayer::new(
        crate::api::schema::PaneGraphicsFormat::Png,
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
        ) {
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
    struct FleetRow {
        name: &'static str,
        owner: Option<&'static str>,
        doing: &'static str,
        state: AgentState,
        project: &'static str,
        context: &'static str,
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

    const FLEET: &[FleetRow] = &[
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
        let area = Rect::new(0, 0, 100, 46);

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
            tier_height_px(0, title, tidbit) > BASE_HEIGHT_PX,
            "the fixture's face is not tall enough to test anything"
        );
        let cell = (10.0f32, 21.0f32);
        let bounds = Rect::new(0, 0, 40, 60);
        let bloom_floor = bounds.y + bounds.height;
        let cards = [
            (0u8, Rect::new(1, 2, 38, 8)),
            (1u8, Rect::new(3, 10, 36, 6)),
            (2u8, Rect::new(5, 16, 34, 5)),
        ];

        let field = dissolve_field_rect(&cards, cell, (title, tidbit), bounds, bloom_floor)
            .expect("a tree of three cards has a field");
        for (depth, frame) in cards {
            let rect = card_image_rect(depth, frame, cell, (title, tidbit), bounds, bloom_floor)
                .expect("a card with a frame has an image");
            assert_eq!(
                field.union(rect),
                field,
                "the card at depth {depth} reaches outside the field it is \
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

    /// The tier scale has to actually shrink something, or the three depths are
    /// one card drawn three times.
    #[test]
    fn each_tier_is_smaller_than_the_one_above_it() {
        for depth in 1..TIER_SCALE.len() {
            assert!(
                tier(depth as u8) < tier(depth as u8 - 1),
                "tier {depth} did not shrink"
            );
        }
        // Past the deepest tier the scale holds rather than vanishing: the tree
        // caps display depth, and a card at the cap is still a card.
        assert_eq!(tier(9), tier(2));
    }

    /// The settled table's own arithmetic does not survive contact with a title
    /// that may not shrink: two lines at 14 px plus a tidbit is more block than
    /// the 0.65 and 0.42 tiers have nominal height for. The tier is therefore a
    /// floor, and this is the test that says so out loud — if someone later
    /// makes the height literal, the titles start clipping and this fails.
    ///
    /// The consequence, spelled out because it is the one place the settled
    /// table could not be implemented as written: the top tier lands on its
    /// settled 68 px, and every deeper tier is floored by the title rather than
    /// by its own scale, so the 0.65 and 0.42 steps show in the chrome and not
    /// in the height.
    #[test]
    fn a_tier_is_a_floor_and_the_title_is_allowed_to_push_past_it() {
        // The real face's numbers at 14 px: a line box of exactly the em size,
        // and the tidbit at 0.72 of it.
        let title = metrics(TITLE_PX);
        let tidbit = metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);
        let block = content_block_px(title, tidbit);

        for depth in 0..3u8 {
            let height = tier_height_px(depth, title, tidbit);
            assert!(
                height >= block + MIN_VERTICAL_PAD_PX * 2.0,
                "depth {depth} at {height}px cannot hold {block}px of title and tidbit"
            );
            assert!(
                height >= BASE_HEIGHT_PX * tier(depth),
                "depth {depth} fell below its tier floor"
            );
        }
        assert!(
            tier_height_px(0, title, tidbit) > tier_height_px(2, title, tidbit),
            "the top tier stopped reading taller than the deepest"
        );
        // The top tier is the settled base and reaches it by its own nominal,
        // not by the title floor: 68 px is two 14 px lines, a tidbit and the
        // measured padding, which is why that is the number.
        assert_eq!(tier_height_px(0, title, tidbit), BASE_HEIGHT_PX);
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
        let geometry = CardGeometry::new(depth, 16.0, false);
        let height = tier_height_px(
            depth,
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
                                >= tier_height_px(
                                    depth,
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
    /// should be" is what the captain could see and this is what it cost: at
    /// the top tier the collapsed slot hands the title back more than the width
    /// of the state chip.
    #[test]
    fn collapsing_the_empty_plate_gives_real_width_back_to_the_title() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        for depth in 0..3u8 {
            let height = tier_height_px(
                depth,
                font.metrics(TITLE_PX),
                font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL),
            );
            let width = 42.0 * 12.0;
            let with_plate = text_column(
                font,
                &CardGeometry::new(depth, 16.0, true),
                width,
                height,
                WIDEST_STATE_LABEL,
                REAL_FLEET_TITLES[0],
            );
            let without = text_column(
                font,
                &CardGeometry::new(depth, 16.0, false),
                width,
                height,
                WIDEST_STATE_LABEL,
                REAL_FLEET_TITLES[0],
            );
            assert!(
                without.available() > with_plate.available(),
                "depth {depth} gained nothing by collapsing the slot"
            );
        }
        // At the top tier specifically, where the plate was capped at its
        // widest, the gain is worth more than the chip.
        let top = text_column(
            font,
            &CardGeometry::new(0, 16.0, false),
            42.0 * 12.0,
            68.0,
            WIDEST_STATE_LABEL,
            REAL_FLEET_TITLES[0],
        );
        let top_with = text_column(
            font,
            &CardGeometry::new(0, 16.0, true),
            42.0 * 12.0,
            68.0,
            WIDEST_STATE_LABEL,
            REAL_FLEET_TITLES[0],
        );
        assert!(top.available() - top_with.available() >= measured::PLATE_MAX_PX);
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

    /// The reference never moves hue to signal state: it changes saturation and
    /// bloom and nothing else. A future edit that reaches for a red card fails
    /// here first.
    #[test]
    fn state_changes_intensity_and_bloom_but_never_hue() {
        let hue_of = |c: Rgb| {
            let restated = c.restate(1.0, 1.0);
            (restated.0, restated.1, restated.2)
        };
        let base = hue_of(measured::STROKE_A);
        for state in [
            AgentState::Working,
            AgentState::Blocked,
            AgentState::Idle,
            AgentState::Unknown,
        ] {
            let (sat, lum, bloom) = card_intensity(state);
            assert!((0.0..=1.0).contains(&sat));
            assert!((0.0..=1.0).contains(&lum));
            assert!((0.0..=1.0).contains(&bloom));
            let restated = measured::STROKE_A.restate(sat, lum);
            // Desaturating toward grey is allowed; rotating the hue is not.
            let (r, g, b) = (restated.0, restated.1, restated.2);
            assert!(
                b >= r && g >= r,
                "{state:?} moved the stroke out of the blue-cyan family: {r},{g},{b} from \
                 {base:?}"
            );
        }
    }

    /// A card's chrome is measured against the tier's nominal height, so a card
    /// the content pushed taller keeps the padding and plate its tier asked
    /// for rather than inflating them.
    #[test]
    fn chrome_scales_with_the_tier_not_with_the_drawn_height() {
        let base = CardGeometry::new(0, 16.0, true);
        let deep = CardGeometry::new(2, 16.0, true);
        assert!(deep.pad < base.pad);
        assert!(deep.plate < base.plate);
        assert!(deep.radius < base.radius);
        assert!(deep.stroke <= base.stroke);
    }

    /// The plate cap is the named deviation from the measured 0.70 h: without
    /// it the top card has the narrowest text column in the tree.
    #[test]
    fn the_plate_is_capped_so_the_top_card_is_not_the_narrowest_column() {
        const {
            assert!(measured::PLATE * BASE_HEIGHT_PX > measured::PLATE_MAX_PX);
        }
        assert_eq!(
            CardGeometry::new(0, 16.0, true).plate,
            measured::PLATE_MAX_PX
        );
    }

    /// No mark, no slot — and every pixel the slot was taking goes to the text
    /// column rather than to a wider gap.
    #[test]
    fn a_card_with_no_mark_reserves_no_icon_slot() {
        let marked = CardGeometry::new(0, 16.0, true);
        let bare = CardGeometry::new(0, 16.0, false);
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
    use super::tests::{pixel_fleet_app, sidebar_rect};
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

    /// The cards a build published, or `None` when this machine has no
    /// proportional face and there is no pixel path to test.
    fn built(app: &AppState) -> Option<Vec<SidebarCardLayer>> {
        let cards = super::super::compute_workspace_card_areas(app, sidebar_rect());
        match build_cards(app, &cards, sidebar_rect(), app.host_cell_size, &[]) {
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
            // Every card's glow reaches at most `BLOOM_REACH` of the tallest a
            // card is ever drawn, plus a pixel for the antialiasing ramp.
            let reach = box_h * BLOOM_REACH + 1.0;

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
            build_cards(&app, &cards, sidebar_rect(), app.host_cell_size, &[])
        else {
            return; // No face on this machine.
        };
        assert!(matches!(
            build_cards(&app, &cards, sidebar_rect(), app.host_cell_size, &first),
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
            build_cards(&moved, &cards, sidebar_rect(), moved.host_cell_size, &first)
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
            build_cards(&app, &cards, narrow, app.host_cell_size, &[]),
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
            build_cards(&app, &cards, sidebar_rect(), app.host_cell_size, &[]),
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
        );
        assert!(
            !bytes.is_empty() && !second.is_empty(),
            "the default sheet path stopped reaching a second client, which is \
             this branch changing behaviour with its flag off"
        );
    }

    /// Turning the flag on moves no row and changes no tier.
    ///
    /// The captain paid for the 68 px base, the 65% tier step, D-MID density and
    /// two-line titles. This is a change to what a card's *edges* do, and the
    /// layout has to come out the same on both sides of the flag.
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
        for depth in 0..3u8 {
            let fold = super::super::row_fold_width(&sheet, sidebar_rect());
            assert_eq!(
                row_height_cells(&sheet, depth, fold),
                row_height_cells(&shapes, depth, fold),
                "tier {depth} changed height with the drawing model"
            );
        }
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
        let CardsUpdate::Rebuilt(layers) = build_cards(&app, &cards, rect, cell, &[]) else {
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
        if let CardsUpdate::Rebuilt(sheet) = build_cards(&sheet_app, &sheet_cards, rect, cell, &[])
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
