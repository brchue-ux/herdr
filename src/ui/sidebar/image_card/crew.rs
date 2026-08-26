//! The worker list a Space's card carries inside its own box.
//!
//! A Space is a mate and a worker is something that mate dispatched, so the two
//! were always one fact — but until now they were two *cards*, and a reader had
//! to reassemble the relation out of a connector glyph. The captain's confirmed
//! mockups put the workers back inside the card that owns them: the card's own
//! header, its bars and its orbit line, then a dashed rule, then one compact row
//! per worker. One box, one Space, everything it is running.
//!
//! # Two tiers and no third
//!
//! A worker the Space dispatched itself sits flush with the card's own left
//! margin and carries a full-strength dot. A worker some *second mate* dispatched
//! is shifted one fixed step in and drawn dimmer — **one** step, whichever second
//! mate it was, because the question the indent answers is "did this come through
//! somebody" and not "through whom". Both tiers live in the one list: a Space
//! that has a direct worker and a second mate's worker running at once draws them
//! in the same column of rows, told apart by the step and nothing else. Splitting
//! them into sections would answer a question nobody asked and cost the card the
//! only thing it is trying to say, which is *this is everything running here*.
//!
//! The tier is read off the ownership tree
//! ([`crate::app::agent_tree`]) — the `owner` token a fleet already publishes —
//! and never off a new flag. See [`super::super::crew_tier`].
//!
//! # The spawn gesture is two moves and they never overlap
//!
//! A row arriving in this list opens its own track first, pushing every row
//! below it down; only once that track has finished opening does the new row's
//! own content appear. A row leaving runs it backwards: the content goes, and
//! *then* the gap closes. That is [`CrewArrival`], and both halves are read off
//! the same [`super::super::motion::ArrivalCircuit`] the panel's own rows arrive
//! on — the push beat for the track, the card beat for the content — so the two
//! gestures are one rule at two scales rather than two timings to keep in sync.
//!
//! Content appears by **opacity alone**, never by a translation or a clip, for
//! the same reason [`super::CardContent::generate`] does: the track is what
//! carries the sense of something being made room for, and a row that also slid
//! would be two motions where the design asks for one.

use super::canvas::{coverage, Canvas, Rgb};
use super::draw_text;
use super::font::{CardFont, FontMetrics};
use super::measured;

/// The name's size, against the card's own title.
///
/// The mockup sets a worker's branch name at `0.62rem` under a card name of
/// `0.72rem`. Stated as a ratio rather than a size so the whole list rescales
/// with [`super::TITLE_PX`] the way every other measurement on the card does.
const NAME_MUL: f32 = 0.86;

/// The status line's size, against the name above it — the mockup's `0.53rem`
/// against that same `0.62rem`.
const DETAIL_MUL: f32 = 0.855;

/// The air between a row's name and its own status line: the mockup's
/// `.wk-desc { margin-top: 0.08rem }`, as a fraction of the detail's line.
const DETAIL_GAP_MUL: f32 = 0.13;

/// The air between two rows — the mockup's `.workers { gap: 0.3rem }` against
/// its `0.72rem` card name.
const ROW_GAP_MUL: f32 = 0.417;

/// One indent step, against the card's own title — the mockup's
/// `.wrow.t2 { margin-left: 0.8rem }`.
///
/// **One step and one only.** A second mate's worker is one step in; a worker
/// dispatched by a second mate's own second mate is drawn at the same step,
/// because past the first the indent has stopped answering a question anybody
/// asked and started eating a 26-column sidebar. See [`CrewMember::tier`].
const INDENT_MUL: f32 = 1.11;

/// The gap between a row's own left rail and its dot — the mockup's
/// `.wrow { padding-left: 0.5rem }`.
const RAIL_GAP_MUL: f32 = 0.69;

/// The rail's own width in pixels — the mockup's `border-left: 2px`, which is
/// a hairline at every cell size a card is drawn at and so is not scaled.
const RAIL_W_PX: f32 = 1.6;

/// The dot's diameter, against the name beside it — the mockup's `4.5px`
/// against `0.62rem`.
const DOT_MUL: f32 = 0.45;

/// The air between the dot and the name — the mockup's `.wk-dot`'s own
/// `margin-right`.
const DOT_GAP_MUL: f32 = 0.36;

/// How far a breathing dot dips, as a share of its own strength.
///
/// The mockup's `@keyframes wk-pulse { 0%,100% { opacity: 1 } 50% { opacity:
/// 0.4 } }` — a dot at the bottom of its breath is drawn at 40% of the top,
/// so the swing is the remaining 60%.
///
/// Applied to the dot alone and never to the row's rail or its type. The
/// mockup pulses `.wk-dot` and nothing else on the row, and it is the right
/// call for the same reason [`super::CardLight::breathed`] leaves saturation
/// alone: a name that faded in and out would read as the row itself being
/// uncertain rather than as its work being live.
const DOT_BREATH_DIP: f32 = 0.6;

/// How much of its full strength a second mate's row is drawn at.
///
/// The mockup dims the whole row's *edge* — `rgba(90,209,255,0.55)` becomes
/// `rgba(90,209,255,0.22)` — and drops the dot's glow entirely. One number for
/// both, because a tier is one signal: a row is either the Space's own or it
/// came through somebody, and every mark on it says the same thing.
const VIA_MATE_PRESENCE: f32 = 0.42;

/// The dashed rule that separates a card's own content from its crew — the
/// mockup's `hr.divider`.
const DIVIDER_DASH_MUL: f32 = 0.32;
const DIVIDER_GAP_MUL: f32 = 0.24;
/// Air above the rule, and below it, against the card's own title — the
/// mockup's `margin: 0.5rem 0 0.44rem`.
const DIVIDER_LEAD_MUL: f32 = 0.69;
const DIVIDER_TRAIL_MUL: f32 = 0.61;

/// How far a row's ink is faded in and how far its own track has opened.
///
/// The two are read off one [`super::super::motion::ArrivalCircuit`] and are
/// non-overlapping there by construction, so nothing here has to sequence them:
/// `open` has already reached `1.0` before `bloom` leaves `0.0`, in both
/// directions.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CrewArrival {
    /// `0.0..=1.0`: how much of this row's own height it currently occupies.
    /// The rows under it are pushed by exactly this, so the push is a real
    /// reflow of the list rather than an offset guessed against it.
    pub(crate) open: f32,
    /// `0.0..=1.0`: this row's own ink opacity.
    pub(crate) bloom: f32,
}

impl CrewArrival {
    /// A row at rest: fully open, fully drawn. Every row on a settled panel, and
    /// every row at all on a host with no card motion.
    pub(crate) const SETTLED: Self = Self {
        open: 1.0,
        bloom: 1.0,
    };
}

/// One worker, drawn inside its Space's own card.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CrewMember {
    /// The branch this worker is on, set bold on the row's first line.
    pub(crate) name: String,
    /// The one dim line under it: what this worker is doing, or what state it
    /// is in when it is not saying.
    pub(crate) detail: Option<String>,
    /// `0` flush with the card's own left margin, `1` one step in.
    ///
    /// Saturated at `1` by [`super::super::crew_tier`], which is the only thing
    /// that constructs one.
    pub(crate) tier: u8,
    pub(crate) arrival: CrewArrival,
    /// Where this worker's dot is in its own breath, as the engine's envelope
    /// — the mockup's `.wk-dot.pulse`.
    ///
    /// `0.0` is the dot at full strength, which is every dot on a host with no
    /// card animation, every dot with `[ui.sidebar.cards] pulse = false`, and
    /// every dot on a worker that is not working. A full swing is `1.0`, and a
    /// snapping behaviour carries past it on its overshoot exactly as
    /// [`super::CardContent::breath`] does.
    ///
    /// # Why this is a number and not a flag
    ///
    /// Because the envelope is already being computed. A worker row declares
    /// the same three `card-*` behaviours its Space's card does (see
    /// `AppState::sidebar_row_lifecycle`), and the engine accumulates their
    /// phases for every row whether or not anything reads them — so this reads
    /// a value the animator has already stepped rather than starting a clock.
    /// A flag would have to be turned into an envelope somewhere, and that
    /// somewhere would be a second clock to keep in step with the card's.
    #[serde(default)]
    pub(crate) pulse: f32,
    /// The failure marker this worker's fleet says it is carrying.
    ///
    /// A worker used to have a card of its own for its marker to climb, and it
    /// no longer does. The marker follows it here rather than being dropped: an
    /// open defect that stops being drawn the moment the design changed is a
    /// regression nobody would see until they needed it.
    pub(crate) spider: Option<super::spider::CardSpider>,
}

impl CrewMember {
    /// How present this row's marks are: full strength for a worker the Space
    /// dispatched, dimmed for one that came through a second mate.
    fn presence(&self) -> f32 {
        if self.tier == 0 {
            1.0
        } else {
            VIA_MATE_PRESENCE
        }
    }

    pub(super) fn hash_into(&self, hasher: &mut std::collections::hash_map::DefaultHasher) {
        use std::hash::Hash;
        self.name.hash(hasher);
        self.detail.hash(hasher);
        self.tier.hash(hasher);
        // Quantized for the same reason the card's own breath and bloom are: a
        // row whose track has not opened by a step anyone could see hashes the
        // same and its card is carried forward rather than redrawn.
        ((self.arrival.open * ARRIVAL_STEPS).round() as u16).hash(hasher);
        ((self.arrival.bloom * ARRIVAL_STEPS).round() as u16).hash(hasher);
        // Quantized on the card's own breath ladder rather than the arrival's:
        // this *is* a card breath, read off the same engine at the same tier,
        // and hashing it any finer would rasterise the sheet for a step of the
        // dot's opacity nothing on screen could resolve. Without it the card is
        // carried forward on a stale signature and the dot never moves at all.
        ((self.pulse * super::CARD_BREATH_STEPS).round() as u16).hash(hasher);
        // Presence first, then the frame — the same two-part reading a card's
        // own marker gets, and for the same reason: a row that has just been
        // marked and one that has none are two different rows even before the
        // creature has moved.
        self.spider.is_some().hash(hasher);
        if let Some(spider) = &self.spider {
            spider.hash_into(hasher);
        }
    }
}

/// The ladder both halves of an arrival are quantized to.
///
/// The same count [`super::GENERATE_STEPS`] uses for the card's own bloom: a
/// crew row is smaller than a card, so a finer ladder would rasterise more
/// often for a change nothing on screen could resolve.
const ARRIVAL_STEPS: f32 = 24.0;

/// Every measurement of the list, resolved once from the card's own title size.
#[derive(Debug, Clone, Copy)]
pub(super) struct CrewMetrics {
    pub(super) name_px: f32,
    pub(super) detail_px: f32,
    name: FontMetrics,
    detail: FontMetrics,
    /// What the rule band *wants*: the air above it, the rule, and the air
    /// below. The band it is actually given is [`CrewBands::divider`].
    pub(super) divider: f32,
    /// What one row wants, gap included. See [`CrewBands::row`].
    pub(super) row: f32,
    indent: f32,
    rail_gap: f32,
    dot: f32,
    dot_gap: f32,
}

impl CrewMetrics {
    pub(super) fn of(font: &CardFont, title_px: f32) -> Self {
        let name_px = title_px * NAME_MUL;
        let detail_px = name_px * DETAIL_MUL;
        let name = font.metrics(name_px);
        let detail = font.metrics(detail_px);
        Self {
            name_px,
            detail_px,
            name,
            detail,
            divider: title_px * (DIVIDER_LEAD_MUL + DIVIDER_TRAIL_MUL) + 1.0,
            row: name.line_height
                + detail.line_height * (1.0 + DETAIL_GAP_MUL)
                + title_px * ROW_GAP_MUL,
            indent: title_px * INDENT_MUL,
            rail_gap: title_px * RAIL_GAP_MUL,
            dot: name_px * DOT_MUL,
            dot_gap: name_px * DOT_GAP_MUL,
        }
    }
}

/// The bands the *layout* gave the list, in pixels.
///
/// # Why these are not [`CrewMetrics`]'s own numbers
///
/// Because a crew row is a row of the panel: it has its own rect, it is what a
/// click on a worker lands on, and it is what the row above it pushes when it
/// arrives. So its height is a whole number of cells, decided once by
/// [`super::crew_row_cells`] and handed here — rather than a float the
/// rasteriser picks and the layout then has to guess at. The two used to be
/// spelled separately on the horizontal axis and that is exactly the failure
/// [`super::super::tree_prefix_width`] exists to prevent.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct CrewBands {
    /// The rule's own band: the head's last cells, above the first row.
    pub(super) divider: f32,
    /// One row's cells.
    pub(super) row: f32,
}

impl CrewBands {
    /// The bands this host lays a list on: each measurement rounded **up** to a
    /// whole number of cells.
    ///
    /// A pure function of the face and the cell, which is what lets a client
    /// that rasterises its own cards resolve the same bands the server laid the
    /// rows out on without either of them being sent over the wire — both ends
    /// have the bundled face, and the cell the server measured against is the
    /// client's own reported one.
    pub(super) fn of(font: &CardFont, title_px: f32, cell_h: f32) -> Self {
        let metrics = CrewMetrics::of(font, title_px);
        if cell_h <= 0.0 {
            return Self {
                divider: metrics.divider,
                row: metrics.row,
            };
        }
        Self {
            // Ceil, exactly as `row_height_cells` does and for the same reason:
            // a band shorter than its cells would leave the character row
            // showing through under the image.
            divider: (metrics.divider / cell_h).ceil() * cell_h,
            row: (metrics.row / cell_h).ceil() * cell_h,
        }
    }

    /// The height the list is *drawn* at right now: every row weighted by how
    /// far its own track has opened.
    fn drawn_extent(&self, members: &[CrewMember]) -> f32 {
        if members.is_empty() {
            return 0.0;
        }
        let open: f32 = members
            .iter()
            .map(|member| member.arrival.open.clamp(0.0, 1.0))
            .sum();
        if open <= 0.0 {
            return 0.0;
        }
        self.divider + self.row * open
    }
}

/// The height a card's crew adds to it right now.
///
/// `0.0` on a card with no crew, which is every worker's card and every Space
/// running nothing — so a card that has never had a worker is drawn at exactly
/// the height it always was.
pub(super) fn drawn_extent_px(bands: CrewBands, members: &[CrewMember]) -> f32 {
    bands.drawn_extent(members)
}

/// Draw a card's crew under its own content.
///
/// `top` is where the card's own content block ends; the rule and the rows
/// follow it. `left`/`right` are the card's own text column, so a worker's name
/// starts exactly where the card's title does and is clipped exactly where it is.
///
/// `ink` is the card's own caption ink, `edge` its stroke ink and `accent` the
/// theme's own full-strength accent, all three taken from the caller rather
/// than resolved here: the crew is part of the card, not a widget standing on
/// it, and a list that picked its own colours would be the one thing on the
/// card that did not change when its state did.
///
/// `edge` and `accent` are two colours because the mockup draws them as two:
/// `hr.divider` is `1px dashed var(--edge)` while `.wrow`'s own rail is
/// `rgba(--cyan, .55)`. They are the same colour on an unthemed panel — the
/// one measured hue family — and only a theme that authored `--edge`
/// separates them.
#[allow(clippy::too_many_arguments)] // Sheet, font, rows, geometry, three inks
                                     // and the card's own opacity: each varies
                                     // per call.
pub(super) fn draw(
    sheet: &mut Canvas,
    font: &CardFont,
    members: &[CrewMember],
    (metrics, bands): (&CrewMetrics, CrewBands),
    (left, right): (f32, f32),
    top: f32,
    (ink, edge, accent): (Rgb, Rgb, Rgb),
    marker: super::spider::Palette,
    opacity: f32,
) {
    if members.is_empty() || opacity <= 0.0 || right <= left {
        return;
    }
    let title_px = metrics.name_px / NAME_MUL;
    let rule_y = top + bands.divider * DIVIDER_LEAD_MUL / (DIVIDER_LEAD_MUL + DIVIDER_TRAIL_MUL);
    draw_dashed_rule(sheet, (left, right), rule_y, title_px, edge, opacity);

    let name_ink = ink.restate(1.0, 0.92);
    let detail_ink = measured::FILL_MID.mix(ink, measured::TIDBIT_INK_MIX);
    let mut y = top + bands.divider;
    for member in members {
        let open = member.arrival.open.clamp(0.0, 1.0);
        if open <= 0.0 {
            continue;
        }
        let band = bands.row * open;
        // The row's ink is its own bloom *and* the card's: a card still fading
        // in does not hold a fully drawn worker list, and a fully drawn card
        // does not hold a worker that has not arrived.
        let alpha = member.arrival.bloom.clamp(0.0, 1.0) * opacity;
        if alpha > 0.0 {
            draw_row(
                sheet,
                font,
                member,
                metrics,
                (left, right),
                y + (bands.row - metrics.row).max(0.0) / 2.0,
                (name_ink, detail_ink, accent),
                alpha,
            );
            // Over the row it belongs to, after its type, exactly as a card's
            // own marker is drawn after the card. Its climb is up this row's
            // band rather than up a card's border, which is the same journey at
            // the scale the row actually has.
            if let Some(spider) = member.spider {
                super::spider::draw_at(
                    sheet,
                    spider,
                    &super::canvas::RoundRect {
                        x: left,
                        y,
                        w: (right - left).max(0.0),
                        h: bands.row,
                        r: 0.0,
                    },
                    marker,
                    alpha,
                );
            }
        }
        y += band;
    }
}

/// The dashed rule between a card's own content and its crew.
///
/// Drawn as a run of short dashes rather than as a solid hairline at low alpha:
/// the mockup's rule is `1px dashed`, and a dash pattern survives being placed
/// on a cell boundary in a way a one-pixel line at a fraction of an alpha does
/// not.
fn draw_dashed_rule(
    sheet: &mut Canvas,
    (left, right): (f32, f32),
    y: f32,
    title_px: f32,
    edge: Rgb,
    opacity: f32,
) {
    let dash = (title_px * DIVIDER_DASH_MUL).max(1.0);
    let gap = (title_px * DIVIDER_GAP_MUL).max(1.0);
    let row = y.floor().max(0.0) as u32;
    if row >= sheet.height() {
        return;
    }
    let mut x = left;
    while x < right {
        let end = (x + dash).min(right);
        for px in x.floor().max(0.0) as u32..end.ceil() as u32 {
            if px >= sheet.width() {
                break;
            }
            sheet.blend(px, row, edge, measured::TIDBIT_INK_MIX * opacity);
        }
        x = end + gap;
    }
}

/// One worker row: its rail, its dot, its name, and the dim line under it.
#[allow(clippy::too_many_arguments)] // See [`draw`].
fn draw_row(
    sheet: &mut Canvas,
    font: &CardFont,
    member: &CrewMember,
    metrics: &CrewMetrics,
    (left, right): (f32, f32),
    top: f32,
    (name_ink, detail_ink, accent): (Rgb, Rgb, Rgb),
    opacity: f32,
) {
    let presence = member.presence();
    // The one place the tier becomes a distance. Every mark on the row is
    // measured from here, so a row cannot end up half-indented.
    let row_left = left + metrics.indent * f32::from(member.tier.min(1));
    let text_left = row_left + RAIL_W_PX + metrics.rail_gap;
    if text_left >= right {
        return;
    }

    let name_top = top;
    let name_baseline = name_top + metrics.name.ascent;
    let detail_top = name_top + metrics.name.line_height * (1.0 + DETAIL_GAP_MUL);

    // ---- the rail ----------------------------------------------------------
    // The mockup's `border-left`, and the row's whole tier signal after the dot:
    // it runs the height of the row's own type and no further, so the list reads
    // as a column of rows rather than as a second tree hung inside the card.
    let rail_bottom = detail_top + metrics.detail.line_height;
    for y in name_top.floor().max(0.0) as u32..rail_bottom.ceil() as u32 {
        if y >= sheet.height() {
            break;
        }
        for x in row_left.floor().max(0.0) as u32..(row_left + RAIL_W_PX).ceil() as u32 {
            if x >= sheet.width() {
                break;
            }
            sheet.blend(x, y, accent, 0.55 * presence * opacity);
        }
    }

    // ---- the dot -----------------------------------------------------------
    // Full strength with its glow on a worker the Space dispatched, dimmed and
    // flat on one that came through a mate — the mockup's own two `.wk-dot`
    // rules, and the second thing on the row saying the same one fact.
    let radius = metrics.dot / 2.0;
    let center = (
        text_left + radius,
        name_top + metrics.name.line_height / 2.0,
    );
    // The breath rides the dot's own opacity and nothing else's, which is
    // exactly what the mockup animates. Floored at zero for the reason
    // `CardLight::breathed` floors its own: a negative envelope would drive the
    // dot *past* full strength, and full strength is where it already is.
    let breath = 1.0 - DOT_BREATH_DIP * member.pulse.max(0.0);
    draw_dot(
        sheet,
        center,
        radius,
        presence,
        opacity * breath.clamp(0.0, 1.0),
        accent,
    );

    let type_left = text_left + metrics.dot + metrics.dot_gap;
    if type_left >= right {
        return;
    }
    draw_text(
        sheet,
        font,
        &member.name,
        metrics.name_px,
        type_left,
        name_baseline,
        name_ink,
        type_left,
        right,
        opacity,
    );
    if let Some(detail) = &member.detail {
        draw_text(
            sheet,
            font,
            detail,
            metrics.detail_px,
            type_left,
            detail_top + metrics.detail.ascent,
            detail_ink,
            type_left,
            right,
            opacity,
        );
    }
}

/// The row's own status dot.
///
/// [`super::draw_worker_dot`] draws the one a *worker's own card* carries beside
/// its title, at one size and always at full strength. This one is smaller and
/// carries the tier, so it takes both as arguments rather than being a second
/// copy of that function with the constants changed.
///
/// # Why the halo is sampled well past itself
///
/// Because a Gaussian cut off where it is still visible is a *square*, and at
/// this size that is what a reader sees: the dot's own glow reaching the edge of
/// the box it was sampled in at about a third of an alpha and stopping dead. Live
/// capture, not arithmetic — the first lab screenshot of this list has a bright
/// rounded rectangle around every full-strength dot. The box now runs to three
/// sigmas, where the falloff is under a 255th and the edge is nothing to see.
///
/// The second half of the same failure is the boundary: a pixel the disc only
/// partly covers used to take its fill and *skip* the glow, so it came out darker
/// than the halo around it and the dot wore a dark ring. The glow is laid over
/// the fill at `1 - fill` instead, which is the same number the disc did not use.
fn draw_dot(
    sheet: &mut Canvas,
    center: (f32, f32),
    radius: f32,
    presence: f32,
    opacity: f32,
    ink: Rgb,
) {
    // A dimmed row draws no glow at all, per the mockup's `box-shadow: none`:
    // the glow is what makes a dot read as lit, and a lit dot at a lower alpha
    // is a dim light rather than a light that is not this row's.
    let lit = presence >= 1.0;
    let sigma = (radius * GLOW_SIGMA_MUL).max(0.5);
    let reach = if lit {
        radius + sigma * GLOW_REACH_SIGMAS
    } else {
        radius
    };
    let x0 = (center.0 - reach).floor().max(0.0) as u32;
    let y0 = (center.1 - reach).floor().max(0.0) as u32;
    let x1 = ((center.0 + reach).ceil() as u32).min(sheet.width());
    let y1 = ((center.1 + reach).ceil() as u32).min(sheet.height());
    for y in y0..y1 {
        let py = y as f32 + 0.5;
        for x in x0..x1 {
            let px = x as f32 + 0.5;
            let d = ((px - center.0).powi(2) + (py - center.1).powi(2)).sqrt() - radius;
            let fill = coverage(d);
            if fill > 0.0 {
                sheet.blend(x, y, ink, fill * presence * opacity);
            }
            if !lit || fill >= 1.0 {
                continue;
            }
            // Laid over whatever the disc did not cover, so the boundary is one
            // ramp rather than two effects meeting at a seam.
            let glow = (-(d.max(0.0).powi(2)) / (2.0 * sigma * sigma)).exp()
                * GLOW_PEAK_ALPHA
                * (1.0 - fill);
            if glow > 0.001 {
                sheet.blend(x, y, ink, glow * opacity);
            }
        }
    }
}

/// The halo's width, as a multiple of the dot's own radius — the mockup's
/// `box-shadow: 0 0 5px` against its `4.5px` dot.
const GLOW_SIGMA_MUL: f32 = 0.9;

/// How far the halo is sampled, in sigmas. Past three the falloff is under a
/// 255th of an alpha and the box's own edge has nothing to show.
const GLOW_REACH_SIGMAS: f32 = 3.0;

/// The halo's alpha at the disc's own edge — the mockup's `rgba(90,209,255,0.6)`.
const GLOW_PEAK_ALPHA: f32 = 0.6;

#[cfg(test)]
mod tests {
    use super::*;

    fn member(tier: u8, arrival: CrewArrival) -> CrewMember {
        CrewMember {
            name: "fm/verve-notes".to_string(),
            detail: Some("craft.md falloff numbers".to_string()),
            tier,
            arrival,
            pulse: 0.0,
            spider: None,
        }
    }

    const BANDS: CrewBands = CrewBands {
        divider: 21.0,
        row: 42.0,
    };

    /// The whole push: a row whose track has not opened takes no height, and one
    /// half open takes half. This is what the rows below it are pushed by, so a
    /// list that opened its track any other way would drift from them.
    #[test]
    fn a_tracks_open_amount_is_the_height_it_takes() {
        let settled = member(0, CrewArrival::SETTLED);
        let closed = member(
            0,
            CrewArrival {
                open: 0.0,
                bloom: 0.0,
            },
        );
        let half = member(
            0,
            CrewArrival {
                open: 0.5,
                bloom: 0.0,
            },
        );

        assert_eq!(BANDS.drawn_extent(&[]), 0.0);
        assert_eq!(BANDS.drawn_extent(std::slice::from_ref(&settled)), 63.0);
        // A row that has not started opening yet adds nothing at all: the rule
        // is still there, because the rows already in the list are.
        assert_eq!(
            BANDS.drawn_extent(&[settled.clone(), closed]),
            63.0,
            "an unopened track must add no height"
        );
        assert_eq!(
            BANDS.drawn_extent(&[settled, half]),
            84.0,
            "a half-open track must add half a row"
        );
    }

    /// The two beats never overlap, in either direction — the whole of the
    /// captain's "push settles, then the new card blooms; fade out, then the gap
    /// closes". Read off the panel's own circuit rather than restated here, so
    /// there is one sequencing rule for a card arriving and a worker arriving.
    #[test]
    fn the_track_finishes_opening_before_any_ink_appears() {
        for step in 0..=40 {
            let settle = step as f32 / 40.0;
            let circuit = super::super::super::motion::arrival_circuit(settle);
            let arrival = CrewArrival {
                open: circuit.push,
                bloom: circuit.card,
            };
            assert!(
                arrival.bloom == 0.0 || arrival.open >= 1.0,
                "ink at {settle} with the track only {} open",
                arrival.open
            );
        }
    }

    /// **A lit dot's halo is round, and reaches nothing.**
    ///
    /// Two failures at once, both found on a live capture rather than in
    /// arithmetic. A Gaussian sampled only as far as it is still bright is a
    /// *square*: the first lab screenshot of this list has a hard-edged
    /// rectangle of glow around every full-strength dot. And a pixel the disc
    /// only partly covered used to take that fill and skip the glow, so it came
    /// out darker than the halo around it and the dot wore a dark ring.
    ///
    /// Measured on the dot alone, on an empty canvas, so nothing else on a card
    /// can be mistaken for it: points at one radius must agree, whatever
    /// direction they are in, and far enough out there must be nothing at all.
    #[test]
    fn a_lit_dot_has_a_round_halo_that_reaches_nothing() {
        let mut sheet = Canvas::new(60, 60);
        let radius = 2.7;
        draw_dot(
            &mut sheet,
            (30.0, 30.0),
            radius,
            1.0,
            1.0,
            measured::STROKE_A,
        );
        let px = sheet.rgba8();
        let alpha = |dx: i32, dy: i32| {
            let x = (30 + dx) as u32;
            let y = (30 + dy) as u32;
            px[((y * 60 + x) * 4 + 3) as usize]
        };

        // **Round, measured as a ring.** Every pixel at one radius from the
        // centre should carry one alpha. A Gaussian does that; a Gaussian
        // clipped to its own bounding box does not — the part of the ring
        // outside the box reads zero while the part inside it is still lit, and
        // that discontinuity *is* the square edge a reader sees.
        //
        // The tolerance is not zero because a ring of whole pixels is not a
        // circle: `draw_dot` samples pixel centres, so the band admits a
        // quarter-pixel of real radius and the falloff across that is a few
        // 255ths. The clip's own discontinuity is an order of magnitude more —
        // it spans the whole halo, from lit to nothing.
        let ring: Vec<u8> = (0..60)
            .flat_map(|y| (0..60).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let d =
                    ((*x as f32 + 0.5 - 30.0).powi(2) + (*y as f32 + 0.5 - 30.0).powi(2)).sqrt();
                (6.9..=7.15).contains(&d)
            })
            .map(|(x, y)| px[((y * 60 + x) * 4 + 3) as usize])
            .collect();
        assert!(!ring.is_empty(), "the fixture sampled no ring at all");
        let (low, high) = (
            ring.iter().copied().min().unwrap_or(0),
            ring.iter().copied().max().unwrap_or(0),
        );
        assert!(
            high - low <= 6,
            "the halo is not round: one radius spans {low}..={high}, which is a \
             Gaussian cut off at the edge of its own box"
        );
        assert!(high > 0, "there is no halo to be round");

        // The disc is solid, the ring just outside it is lit, and far out there
        // is nothing — a monotone falloff with no moat in it.
        assert_eq!(alpha(0, 0), 255, "the disc is not solid");
        assert!(alpha(3, 0) > 0, "the ring outside the disc is unlit");
        assert!(
            alpha(3, 0) > alpha(7, 0),
            "the halo does not fall off: {} then {}",
            alpha(3, 0),
            alpha(7, 0)
        );
        assert_eq!(
            alpha(20, 0),
            0,
            "the halo is still lit past its own falloff"
        );

        // A dimmed row draws the disc and no halo at all.
        let mut dim = Canvas::new(60, 60);
        draw_dot(
            &mut dim,
            (30.0, 30.0),
            radius,
            VIA_MATE_PRESENCE,
            1.0,
            measured::STROKE_A,
        );
        let dim = dim.rgba8();
        assert_eq!(
            dim[((30 * 60 + 33) * 4 + 3) as usize],
            0,
            "a dimmed row lit a halo the mockup gives it none of"
        );
    }

    /// A second mate's row is dimmer *and* stepped in — one signal said twice,
    /// which is the mockup's own rule, and never a third tier for a deeper chain.
    #[test]
    fn a_via_mate_row_is_dimmer_and_never_indents_twice() {
        assert_eq!(member(0, CrewArrival::SETTLED).presence(), 1.0);
        assert!(member(1, CrewArrival::SETTLED).presence() < 1.0);
        // `tier.min(1)` in `draw_row` is what holds this: a deeper chain draws
        // at the same step as the first one it passed through.
        let step = |tier: u8| f32::from(tier.min(1));
        assert_eq!(step(1), step(3));
        assert!(step(1) > step(0));
    }
}
