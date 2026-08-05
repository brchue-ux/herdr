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

/// One card's finished pixels, and the cells it covers.
pub(crate) struct SidebarCardLayer {
    /// The cell rect the sheet is placed at. Chosen by the tree's own geometry,
    /// so the sheet is exactly as large as the cards plus the reach of their
    /// bloom.
    pub rect: Rect,
    /// What the sheet was built from. A frame whose signature is unchanged
    /// keeps the sheet it already has and re-encodes nothing.
    pub signature: u64,
    pub layer: crate::app::state::GraphicsLayer,
}

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

fn text_column(
    font: &CardFont,
    geometry: &CardGeometry,
    width: f32,
    height: f32,
    state_label: &str,
) -> TextColumn {
    let chip_px = (TITLE_PX * measured::TIDBIT_SIZE_MUL).max(9.0);
    let chip_metrics = font.metrics(chip_px);
    let label = state_label.to_uppercase();
    let chip_width = font.width(&label, chip_px) + chip_px * 1.5;
    let chip_height = (chip_metrics.line_height * 1.25).max(chip_px * 1.55);
    let left = geometry.text_inset();
    let right = width - geometry.pad_right;
    let chip_gap = geometry.pad * 0.7;
    TextColumn {
        left,
        right,
        chip_px,
        chip_width,
        chip_height,
        // The chip is dropped rather than overlapped when the card cannot hold
        // it and a readable title at once. On a correctly measured cell this is
        // never reached; it was reached on every card of a 3440-wide window,
        // because the card was being laid out in a two-pixel cell.
        chip_fits: chip_height < height - 2.0 && right - chip_width - chip_gap > left,
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
    let column = text_column(font, geometry, width, height, &content.state_label);
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

/// What one pass over the tree's cards concluded about the sheet.
///
/// Three outcomes rather than an `Option`, because "keep what you have" and
/// "there is nothing to draw" are opposites and an `Option<Layer>` spells them
/// the same way — which is how a stale sheet outlives the rows it was a picture
/// of.
pub(crate) enum SheetUpdate {
    /// Nothing the sheet is a picture of moved. Keep it, encode nothing.
    Unchanged,
    Rebuilt(SidebarCardLayer),
    /// The pixel path is not live, or the tree has no agent cards in it.
    Empty,
}

/// Build the sheet for the tree's current cards.
///
/// `previous` is the sheet the last frame produced. A frame whose content
/// signature matches it reports [`SheetUpdate::Unchanged`]: nothing is
/// rasterised and nothing is re-encoded, which is what makes a fleet whose
/// cards change about once every ninety seconds cost about that often rather
/// than sixty times a second.
pub(crate) fn build_sheet(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
    previous: Option<&SidebarCardLayer>,
) -> SheetUpdate {
    match build_sheet_inner(app, cards, sidebar_area, cell_size, previous) {
        Ok(Some(layer)) => SheetUpdate::Rebuilt(layer),
        Ok(None) => SheetUpdate::Unchanged,
        Err(()) => SheetUpdate::Empty,
    }
}

/// `Ok(Some)` is a new sheet, `Ok(None)` is the one already held, `Err` is none
/// at all.
fn build_sheet_inner(
    app: &AppState,
    cards: &[crate::app::state::WorkspaceCardArea],
    sidebar_area: Rect,
    cell_size: HostCellSize,
    previous: Option<&SidebarCardLayer>,
) -> Result<Option<SidebarCardLayer>, ()> {
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

    // The sheet spans the cards plus the reach of their bloom, clamped into the
    // panel. A placement whose rect leaves the panel would be clipped by the
    // pipeline anyway; keeping it inside means the clip never has to run.
    let bloom_cells_x = ((BASE_HEIGHT_PX * BLOOM_REACH) / cell_w).ceil() as u16;
    let bloom_cells_y = ((BASE_HEIGHT_PX * BLOOM_REACH) / cell_h).ceil() as u16;
    let min_x = placed.iter().map(|(r, _)| r.x).min().unwrap_or(bounds.x);
    let min_y = placed.iter().map(|(r, _)| r.y).min().unwrap_or(bounds.y);
    let max_x = placed
        .iter()
        .map(|(r, _)| r.x.saturating_add(r.width))
        .max()
        .unwrap_or(bounds.x);
    let max_y = placed
        .iter()
        .map(|(r, _)| r.y.saturating_add(r.height))
        .max()
        .unwrap_or(bounds.y);
    let sheet_x = min_x.saturating_sub(bloom_cells_x).max(bounds.x);
    let sheet_y = min_y.saturating_sub(bloom_cells_y).max(bounds.y);
    let sheet_right = max_x
        .saturating_add(bloom_cells_x)
        .min(bounds.x.saturating_add(bounds.width));
    let sheet_bottom = max_y.saturating_add(bloom_cells_y).min(bloom_floor);
    let sheet_rect = Rect::new(
        sheet_x,
        sheet_y,
        sheet_right.saturating_sub(sheet_x),
        sheet_bottom.saturating_sub(sheet_y),
    );
    if sheet_rect.width == 0 || sheet_rect.height == 0 {
        return Err(());
    }

    let mut hasher = DefaultHasher::new();
    cell_size.width_px.hash(&mut hasher);
    cell_size.height_px.hash(&mut hasher);
    sheet_rect.x.hash(&mut hasher);
    sheet_rect.y.hash(&mut hasher);
    sheet_rect.width.hash(&mut hasher);
    sheet_rect.height.hash(&mut hasher);
    for (frame, content) in &placed {
        frame.x.hash(&mut hasher);
        frame.y.hash(&mut hasher);
        frame.width.hash(&mut hasher);
        frame.height.hash(&mut hasher);
        content.hash_into(&mut hasher);
    }
    backdrop.0.hash(&mut hasher);
    backdrop.1.hash(&mut hasher);
    backdrop.2.hash(&mut hasher);
    let signature = hasher.finish();
    if let Some(previous) = previous {
        if previous.signature == signature && previous.rect == sheet_rect {
            return Ok(None);
        }
    }

    let width_px = u32::from(sheet_rect.width) * cell_size.width_px;
    let height_px = u32::from(sheet_rect.height) * cell_size.height_px;
    // A sheet larger than this is a sidebar nobody has — 8 megapixels is a
    // panel over a thousand pixels wide and seven thousand tall. The guard is
    // here so a nonsense cell-size report cannot turn into a huge allocation:
    // at four bytes a pixel for the sheet and eight more for the bloom field,
    // this ceiling is about 96 MB, held only while the sheet is being built.
    const MAX_SHEET_PIXELS: u32 = 8_000_000;
    if width_px == 0 || height_px == 0 || width_px.saturating_mul(height_px) > MAX_SHEET_PIXELS {
        return Err(());
    }

    let mut sheet = Canvas::new(width_px, height_px);
    let title_metrics = font.metrics(TITLE_PX);
    let tidbit_metrics = font.metrics(TITLE_PX * measured::TIDBIT_SIZE_MUL);
    let cards: Vec<PlacedCard<'_>> = placed
        .iter()
        .map(|(frame, content)| {
            let geometry = CardGeometry::new(content.depth, cell_h, content.mark.is_some());
            // The card is drawn at the height its tier asked for, centred in
            // the cells the row was given. The leftover is the gutter — this is
            // where the measured 0.19 h sibling gap comes back, and it is also
            // what makes the tier scale visible again after the row height was
            // rounded up to a whole number of cells.
            let cell_top = f32::from(frame.y.saturating_sub(sheet_rect.y)) * cell_h;
            let cell_height = f32::from(frame.height) * cell_h;
            let wanted =
                tier_height_px(content.depth, title_metrics, tidbit_metrics).min(cell_height);
            PlacedCard {
                rect: RoundRect {
                    x: f32::from(frame.x.saturating_sub(sheet_rect.x)) * cell_w,
                    y: cell_top + (cell_height - wanted) / 2.0,
                    w: f32::from(frame.width) * cell_w,
                    h: wanted,
                    r: geometry.radius,
                },
                content,
                geometry,
            }
        })
        .collect();

    // Backdrop first, over exactly the cells each row owns. The sheet is
    // otherwise transparent, so this is what covers the character card standing
    // underneath — including in the gutter, where the card itself does not
    // reach — while leaving the tree's connectors and everything outside a row
    // showing through.
    for (frame, _) in &placed {
        fill_row_backdrop(&mut sheet, frame, sheet_rect, cell_w, cell_h, backdrop);
    }

    let mut bloom = BloomField::new(width_px, height_px);
    for card in &cards {
        lay_bloom(&mut bloom, card);
    }
    bloom.composite(&mut sheet);

    for card in &cards {
        draw_card(&mut sheet, card, font);
        if card.content.lifted {
            // Selection is a change of intensity, never of hue — the same rule
            // the character card's lifted glow ramp follows.
            lift(&mut sheet, card);
        }
    }

    let data = encode_png(&sheet).ok_or(())?;
    Ok(Some(SidebarCardLayer {
        rect: sheet_rect,
        signature,
        layer: crate::app::state::GraphicsLayer::new(
            crate::api::schema::PaneGraphicsFormat::Png,
            width_px,
            height_px,
            data,
            crate::api::schema::PaneGraphicsPlacementParams {
                viewport_col: 0,
                viewport_row: 0,
                grid_cols: u32::from(sheet_rect.width),
                grid_rows: u32::from(sheet_rect.height),
                // Over the text: the sheet is opaque exactly where a card is
                // and transparent everywhere else, so the tree's connectors and
                // its Space rows keep showing through while the character card
                // under each pixel card is covered.
                z: 0,
            },
        ),
    }))
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
    fn pixel_fleet_app() -> AppState {
        let mut app = fleet_app();
        app.kitty_graphics_enabled = true;
        app.host_cell_size = HostCellSize {
            width_px: 10,
            height_px: 21,
        };
        app
    }

    fn sidebar_rect() -> Rect {
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
            .sidebar_card_layer
            .as_ref()
            .expect("compute_view drew the tree's cards");
        let signature = sheet.signature;
        assert!(sheet.rect.width > 0 && sheet.rect.height > 0);

        // A pass with no cell size — a virtual client, or a background frame —
        // leaves the foreground client's sheet alone. Clearing it here would
        // cost the next real frame a re-encode and a re-upload.
        crate::ui::compute_view_without_resizing_panes(&mut app, &runtimes, area);
        assert_eq!(
            app.sidebar_card_layer.as_ref().map(|sheet| sheet.signature),
            Some(signature),
            "a pass that cannot see pixels threw away the sheet"
        );

        // Graphics off puts the panel back on characters and takes the sheet
        // with it, so nothing is left on the host to delete later.
        app.kitty_graphics_enabled = false;
        crate::ui::compute_view_with_cell_size(&mut app, &runtimes, area, cell_size);
        assert!(app.sidebar_card_layer.is_none());
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
                .sidebar_card_layer
                .as_ref()
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
    fn real_text_column(font: &CardFont, sidebar_width: u16, cell_w: f32, depth: u8) -> TextColumn {
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
        )
    }

    /// Every panel width a card is drawn on, against every cell width
    /// `HostCellSize::is_plausible` lets through.
    fn card_widths() -> impl Iterator<Item = (u16, f32, u8)> {
        [34u16, 36, 38, 40, 42, 50, 60]
            .into_iter()
            .flat_map(|sidebar_width| {
                [5.0f32, 6.0, 7.0, 8.0, 10.0, 12.0, 16.0, 24.0]
                    .into_iter()
                    .flat_map(move |cell_w| {
                        (0..3u8).map(move |depth| (sidebar_width, cell_w, depth))
                    })
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
        let Some(font) = font::card_font(None) else {
            return;
        };
        for (sidebar_width, cell_w, depth) in card_widths() {
            let column = real_text_column(font, sidebar_width, cell_w, depth);
            for title in REAL_FLEET_TITLES {
                for line in wrap(font, title, TITLE_PX, column.available(), TITLE_LINES) {
                    if font.width(&line, TITLE_PX) <= column.available() + 0.5 {
                        continue;
                    }
                    assert!(
                        !line.contains(' '),
                        "{line:?} overruns its {:.1}px column with a break available, at \
                         sidebar {sidebar_width}, cell {cell_w}, depth {depth}",
                        column.available()
                    );
                }
            }
        }
    }

    /// And on the widths the fleet runs, not even a single unbreakable word
    /// overruns — so nothing is clipped at all.
    #[test]
    fn no_real_title_word_overruns_its_column_at_the_widths_the_fleet_runs() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        for (sidebar_width, cell_w, depth) in card_widths() {
            if sidebar_width < 38 || cell_w < 8.0 {
                continue;
            }
            let column = real_text_column(font, sidebar_width, cell_w, depth);
            for title in REAL_FLEET_TITLES {
                for line in wrap(font, title, TITLE_PX, column.available(), TITLE_LINES) {
                    assert!(
                        font.width(&line, TITLE_PX) <= column.available() + 0.5,
                        "{line:?} would be clipped at sidebar {sidebar_width}, cell {cell_w}, \
                         depth {depth}"
                    );
                }
            }
        }
    }

    /// And on any panel and cell the captain actually runs, no word is even
    /// dropped: the whole title is set, on two lines, at 14 px.
    ///
    /// 38 columns and an 8 px cell is the floor this is claimed at, and 8 px is
    /// `HostCellSize::FALLBACK` — the narrowest cell Herdr will ever *assume*.
    /// Below that pair the longest real `doing` string does not physically fit
    /// two 14 px lines, and the card's answer is to set the words it can rather
    /// than to shrink the type. His panel is 42 columns on a 12 px cell, which
    /// clears this floor with room to spare.
    #[test]
    fn every_real_fleet_title_is_set_whole_at_the_widths_the_fleet_runs() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        for (sidebar_width, cell_w, depth) in card_widths() {
            if sidebar_width < 38 || cell_w < 8.0 {
                continue;
            }
            let column = real_text_column(font, sidebar_width, cell_w, depth);
            for title in REAL_FLEET_TITLES {
                let lines = wrap(font, title, TITLE_PX, column.available(), TITLE_LINES);
                let set = lines.join(" ");
                assert_eq!(
                    set.split_whitespace().collect::<Vec<_>>(),
                    title.split_whitespace().collect::<Vec<_>>(),
                    "dropped words at sidebar {sidebar_width}, cell {cell_w}, depth {depth}: \
                     {set:?} from {title:?}"
                );
            }
        }
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
            );
            let without = text_column(
                font,
                &CardGeometry::new(depth, 16.0, false),
                width,
                height,
                WIDEST_STATE_LABEL,
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
        );
        let top_with = text_column(
            font,
            &CardGeometry::new(0, 16.0, true),
            42.0 * 12.0,
            68.0,
            WIDEST_STATE_LABEL,
        );
        assert!(top.available() - top_with.available() >= measured::PLATE_MAX_PX);
    }

    /// The chip is at the card's right edge, so it is the first thing a card
    /// too narrow for its content stops drawing. On his 3440-wide window every
    /// chip vanished; on a card measured in a real cell none may.
    #[test]
    fn the_state_chip_is_drawn_at_every_tier_and_width_a_card_is_drawn_at() {
        let Some(font) = font::card_font(None) else {
            return;
        };
        for (sidebar_width, cell_w, depth) in card_widths() {
            let column = real_text_column(font, sidebar_width, cell_w, depth);
            assert!(
                column.chip_fits,
                "no chip at sidebar {sidebar_width}, cell {cell_w}, depth {depth}"
            );
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
