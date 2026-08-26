//! The failure spider, drawn as pixels on a card.
//!
//! The pixel-path twin of [`crate::ui::sidebar::render_failure_spiders`]. That
//! one draws a single emoji cell and returns early the moment a pixel card is
//! going to cover the row it would have stood on, which is every frame on a
//! host running `kitty_graphics` + `sidebar_card_shapes` — so until this module
//! existed the marker was invisible on exactly the setup it matters most on.
//!
//! # One creature, two renderers, one row model
//!
//! Nothing here decides *whether* a spider exists, what hue it takes, or how
//! loud it is. Those three come from the same places the character marker reads
//! them from and may not be re-derived:
//!
//! - existence: [`crate::app::lifecycle::RowSignal::defect`], the fleet's own
//!   `defect` token resolved by [`crate::quality_streak::defect_mark`];
//! - hue: the row's [`crate::anim::cell::LifecycleStage`], through the card's
//!   own resolved [`super::StageHues`], so a marker on a running card is drawn
//!   in the running hue rather than repainting the card's stage;
//! - intensity: [`crate::quality_streak::DefectMark::intensity`], through
//!   [`crate::anim::cell::marker_ink`] — the same function and the same
//!   `MARKER_FULL_REACH` ceiling the character marker was measured against on a
//!   live render.
//!
//! What *is* decided here is the drawing: a pixel card has room for a creature
//! where a character cell has room for a pictograph, and the captain asked for
//! the creature.
//!
//! # Where the drawing comes from
//!
//! The captain's own pixel-art reference, measured before anything was drawn,
//! and the concept demo he approved. What the measurements say, and what they
//! shaped in [`SPRITE`]:
//!
//! - the bounding box is essentially square (0.975 w/h) and about 30 % inked;
//! - **the body is its own outline.** There is no separate outline colour: the
//!   silhouette is near-black punched into a lighter field, and every accent is
//!   inset into that black;
//! - eight legs, four a side, one pair raised above the body, and three visible
//!   segments per leg — an accent joint near the body, a dark middle, a bright
//!   outer segment, a warm tip;
//! - the eyes are six dots in two columns of three, which at any size a sidebar
//!   can give merge into two paired bars. They are drawn as the bars, because
//!   that is what the reference *reduces to*, not as six dots that would land on
//!   two pixels;
//! - measured bloom in the reference is zero, so nothing here glows.
//!
//! The one deliberate departure is the dark tone. The reference sits on a field
//! lighter than its own body; a Herdr card is darker than the reference's field,
//! so a leg at the reference's own luma would be a black line on a black card.
//! The legs take the reference's *lift* tone rather than its deep tone, which
//! keeps the "silhouette is the outline" idiom while leaving the limbs readable
//! on the panel they actually stand on.

use super::canvas::{Canvas, Rgb};
use super::PlacedCard;

/// The creature, one character per pixel.
///
/// Twenty-five by twenty-six, which is about what it is drawn at: the card is
/// [`super::BASE_HEIGHT_PX`] tall on every rank and the spider takes
/// [`HEIGHT_FRACTION`] of it, so on a normal host one sprite pixel is close to
/// one device pixel. That is on purpose. The reference measures as a 49 × 50
/// illustration, and a mechanical reduction of it to sidebar size is mud: that
/// was rendered and looked at, and it did not read. So the creature is
/// *authored* at the size it is drawn, with each feature given a whole pixel to
/// be in.
///
/// | token | what it is |
/// |---|---|
/// | `.` | nothing |
/// | `#` | body deep — the near-black core |
/// | `+` | body lift — the silhouette's own edge |
/// | `o` | a leg's dark middle segment |
/// | `p` | the abdomen plate |
/// | `g` | accent, mid: the joint where a leg meets the body |
/// | `G` | accent, hot: the bright outer leg segment and the mouthparts |
/// | `w` | the warm tip of a leg |
/// | `e` | an eye |
///
/// `g`, `G` are the **lifecycle hue** and carry the whole colour ladder. `w` and
/// `e` are fixed warm inks, exactly as the approved concept holds its own `4`
/// channel fixed: they are the creature's material, not its signal, and a
/// spider whose every mark moved with the stage would be a colour swatch rather
/// than an animal that is going through something.
const SPRITE: [&str; 26] = [
    ".......w.........w.......",
    ".......GG.......GG.......",
    ".......GGo.....oGG.......",
    ".......oooo...oooo.......",
    "........ooog.gooo........",
    "....ooo..oo+++oo..ooo....",
    "...Gooooog+###+goooooG...",
    "..GGG.ooog#e#e#gooo.GGG..",
    ".wwG.....g#e#e#g.....Gww.",
    ".........+#e#e#+.........",
    ".........+#####+.........",
    "...oooooo+#####+oooooo...",
    "..Goooooog+###+gooooooG..",
    ".GGGo......+++......oGGG.",
    ".wG......g+ppp+g......Gw.",
    "ww......og+ppp+go......ww",
    "......ooooGGGGGoooo......",
    ".....oooo.++p++.oooo.....",
    ".....oo....g.g....oo.....",
    "....ooo...........ooo....",
    "....oo.............oo....",
    "....Go.............oG....",
    "....GG.............GG....",
    "....GG.............GG....",
    "....Gw.............wG....",
    ".....w.............w.....",
];

const SPRITE_H: usize = SPRITE.len();
const SPRITE_W: usize = 25;

/// How tall the spider is drawn, as a fraction of the card it rides.
///
/// **A fraction of the card and not of the rank**, which is the whole of the
/// captain's *"full size on every rank — no shrink at worker depth"*: a card is
/// [`super::card_height_px`] tall whatever its depth, so one fraction of it is
/// one size everywhere, by construction rather than by a table that could grow a
/// rung. `a_worker_card_gets_exactly_the_same_spider_as_a_mate` holds it.
const HEIGHT_FRACTION: f32 = 0.62;

/// How much of the spider hangs above the card's top stroke when it is resting.
///
/// Half, so it *straddles* the border rather than sitting on the card's face —
/// the same placement the character marker takes, and for the same reason: the
/// face is the crowded surface the sidebar's width rules protect and the border
/// is not. Clamped against the image's own top edge by [`draw`], because a
/// card's image only carries a bloom margin above it and a spider drawn past
/// that would simply lose its head.
const STRADDLE: f32 = 0.5;

/// How far the stage-4 collapse flattens the creature, and how far it spreads.
///
/// `0.8` is the approved concept's own number (`scale(1, 1 - squash * 0.8)`).
/// The spread has no counterpart there and is added deliberately: a thing that
/// has been squashed goes *wider* as it goes flatter, and without it the collapse
/// reads as the spider shrinking — which is the "just a tint" outcome the
/// captain ruled out, one axis over.
const SQUASH_DEPTH: f32 = 0.8;
const SQUASH_SPREAD: f32 = 0.28;

/// How far the resting pulse pulls the accents back toward the panel at its
/// trough.
///
/// Short of the character marker's own [`FAILURE_SPIDER_PULSE_DEPTH`] because
/// the two are dimming different things: there it swings one glyph's whole ink,
/// here it swings the accent cells inset into a body that stays put, so the
/// creature never loses its shape at the bottom of a breath. The same rule that
/// behaviour's doc records — *"has to stay legible as a spider at the trough"*.
///
/// [`FAILURE_SPIDER_PULSE_DEPTH`]: crate::anim::behaviour
const PULSE_FADE: f32 = 0.45;

/// Steps of the pulse the artwork is rebuilt at.
///
/// The spider is the only thing on a card that moves while the card itself is
/// settled, so this is what its frame rate costs. Coarser than
/// [`super::CARD_BREATH_STEPS`] on purpose: the breath moves a whole card's
/// light, where this moves a handful of accent pixels, and a ladder fine enough
/// to be worth paying for on the first is not on the second.
const PULSE_STEPS: f32 = 16.0;

/// Steps of the climb the artwork is rebuilt at.
///
/// Finer than the pulse, because a marker *travelling* is the one thing here
/// whose position is legible — the same argument [`super::CARD_WASH_STEPS`]
/// makes for a front crossing a card.
const CLIMB_STEPS: f32 = 32.0;

/// The near-black the body's core is drawn in.
const BODY_DEEP: Rgb = Rgb(10, 12, 22);
/// The lift on the silhouette's own edge.
const BODY_LIFT: Rgb = Rgb(30, 36, 52);
/// A leg's dark middle segment. See this module's note on the tone departure.
const LEG_DARK: Rgb = Rgb(34, 40, 56);
/// The abdomen plate — the reference's one grey.
const PLATE: Rgb = Rgb(52, 60, 74);
/// The warm tip of a leg. The reference's amber, held across every stage.
const WARM: Rgb = Rgb(205, 140, 20);
/// An eye. The approved concept's own fixed cream.
const EYE: Rgb = Rgb(255, 243, 208);

/// How far the accent's mid tone sits from its hot one, toward the body.
const ACCENT_MID_MIX: f32 = 0.45;

/// The failure spider riding one card, as that card's content carries it.
///
/// Four resolved numbers rather than a borrow of the engine, for the reason
/// [`super::CardWashFrame`] copies its behaviour out of the catalogue: a card's
/// content owns its whole appearance, and this has to survive being serialized
/// to a client that rasterises the card for itself.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CardSpider {
    /// How far along its climb, in `0.0..=1.0`. `1.0` is arrived and resting.
    climb: f32,
    /// This frame of the resting pulse, in `0.0..=1.0`. `1.0` while climbing:
    /// the climb is its own motion and the pulse does not run under it.
    pulse: f32,
    /// The fleet's published defect intensity, in `0.0..=1.0`.
    intensity: f32,
    /// The stage-4 collapse, in `0.0..=1.0`.
    squash: f32,
}

impl CardSpider {
    /// The card-signature contribution of one spider.
    ///
    /// Every field goes in as bits, and all four are already quantized by
    /// [`resolve`] — so a spider whose pulse has not moved by a step anyone
    /// could see hashes the same and its card is carried forward without being
    /// redrawn. That is the whole of what keeps a failing card from rasterising
    /// on every frame for the sake of a few accent pixels.
    pub(super) fn hash_into(&self, hasher: &mut impl std::hash::Hasher) {
        for value in [self.climb, self.pulse, self.intensity, self.squash] {
            std::hash::Hash::hash(&value.to_bits(), hasher);
        }
    }
}

/// A spider from four resolved numbers, with no engine behind them.
///
/// [`resolve`] is the only way a real card gets one, and it needs an `AppState`
/// and a running animation. `super::bench` has neither: it stands a synthetic
/// fleet up in a shipped binary, and a quarter of that fleet has to carry a
/// spider or the benchmark would be drawing cards with the marker's whole pixel
/// path skipped.
pub(super) fn synthetic_for_bench(
    climb: f32,
    pulse: f32,
    intensity: f32,
    squash: f32,
) -> CardSpider {
    CardSpider {
        climb,
        pulse,
        intensity,
        squash,
    }
}

/// Resolve the spider on one row, or `None` when that row has none.
///
/// A **pure read** of the engine and of the row's own published signal, exactly
/// as [`super::breath`] is: no clock, no mutation, and every number it returns
/// is quantized where it is read so the value that reaches the card's signature
/// and the value that reaches its pixels are the same value.
pub(super) fn resolve(
    app: &crate::app::state::AppState,
    row: crate::anim::CardRow,
    signal: crate::app::lifecycle::RowSignal,
) -> Option<CardSpider> {
    use crate::anim::cell::{CellExtent, CellPos};
    let defect = signal.defect?;
    let id = crate::anim::ElementId::failure_spider(row);
    let frame = app.anim.frame(
        &id,
        Some(crate::anim::behaviour::names::FAILURE_SPIDER_PULSE),
    )?;
    let (climb, pulse) = match frame.phase {
        // Arrived. The climb is done and the pulse is what is running.
        crate::anim::Phase::Idle => {
            let pulse = frame
                .behaviour
                .map(|behaviour| {
                    behaviour.strength(CellPos::new(0, 0), CellExtent::new(1, 1), frame.progress)
                })
                .unwrap_or(1.0);
            (1.0, super::quantize(pulse.clamp(0.0, 1.0), PULSE_STEPS))
        }
        // Climbing, or retreating — which the engine hands back as the climb
        // counting down, so one number covers both and nothing here has to know
        // which direction it is in.
        crate::anim::Phase::Mount | crate::anim::Phase::Dismount => (
            super::quantize(frame.progress.clamp(0.0, 1.0), CLIMB_STEPS),
            1.0,
        ),
        crate::anim::Phase::Retired => return None,
    };
    Some(CardSpider {
        climb,
        pulse,
        intensity: defect.intensity(),
        // The collapse is the *state* the green stage is, not a transition into
        // it: the arrival is carried by the row's own stage change, which the
        // card already washes. See [`SQUASH_DEPTH`].
        squash: f32::from(u8::from(
            signal.stage == crate::anim::cell::LifecycleStage::Done,
        )),
    })
}

/// Where the spider's centre is on its climb, in the coordinates of the image
/// `card` is drawn into.
///
/// The tail of the character marker's own waypoint walk
/// ([`crate::ui::sidebar::failure_spider_waypoints`]) — up the card's left
/// border to the top, then across the top border to centre — and deliberately
/// only the tail. The first two legs of that path are on the tree's trunk and
/// branch, which are *outside* this card's image: a shape is only as large as
/// its own card and the reach of its own bloom, so a climb that started down the
/// trunk would spend its first half drawn on pixels that do not exist.
///
/// Each leg takes a share of the climb proportional to its own length, which is
/// the same rule the character path uses, so a tall card's climb up its own
/// border is not rushed relative to the jog to centre.
fn centre_at(rect: &super::RoundRect, size: (f32, f32), climb: f32) -> (f32, f32) {
    let (w, h) = size;
    let rest_y = (rect.y - h * STRADDLE).max(0.0) + h / 2.0;
    let border_x = rect.x.max(w / 2.0);
    let centre_x = rect.x + rect.w / 2.0;
    let start = (border_x, rect.y + rect.h);
    let knee = (border_x, rest_y);
    let rest = (centre_x, rest_y);

    let up = (start.1 - knee.1).abs();
    let across = (rest.0 - knee.0).abs();
    let total = up + across;
    if total <= 0.0 {
        return rest;
    }
    let travelled = climb.clamp(0.0, 1.0) * total;
    if travelled <= up {
        let t = if up > 0.0 { travelled / up } else { 1.0 };
        return (start.0, start.1 + (knee.1 - start.1) * t);
    }
    let t = if across > 0.0 {
        ((travelled - up) / across).clamp(0.0, 1.0)
    } else {
        1.0
    };
    (knee.0 + (rest.0 - knee.0) * t, rest.1)
}

/// Draw this card's spider, if it has one.
///
/// Called by [`super::Rasteriser::rasterise`] after the card and its lift, so
/// the creature stands on the finished card rather than under its stroke — it
/// is a marker *on* the card, the same way the character one is drawn over the
/// border rather than into it.
pub(super) fn draw(sheet: &mut Canvas, card: &PlacedCard<'_>) {
    let content = card.content;
    let Some(spider) = content.spider else {
        return;
    };
    draw_at(
        sheet,
        spider,
        &card.rect,
        Palette {
            ground: content.ground,
            hue: content.hues.of(content.stage),
        },
        content.generate,
    );
}

/// The two colours a marker resolves itself against: the card's own ground, and
/// the hue of the stage its work is at.
///
/// Taken as a pair rather than off a [`super::CardContent`] because a marker no
/// longer only ever rides a card. A worker drawn inside its Space's own box
/// (see [`super::crew`]) has no card of its own, and the alternative to this was
/// dropping its failure marker on the floor — which is exactly the class of
/// silent loss the marker exists to prevent.
#[derive(Debug, Clone, Copy)]
pub(super) struct Palette {
    pub(super) ground: Rgb,
    pub(super) hue: f32,
}

/// Draw one marker over `rect`, at `opacity`.
pub(super) fn draw_at(
    sheet: &mut Canvas,
    spider: CardSpider,
    rect: &super::RoundRect,
    palette: Palette,
    opacity: f32,
) {
    let opacity = opacity.clamp(0.0, 1.0);
    if opacity <= 0.0 {
        return;
    }
    let full_h = rect.h * HEIGHT_FRACTION;
    let full_w = full_h * SPRITE_W as f32 / SPRITE_H as f32;
    if full_h < 1.0 || full_w < 1.0 {
        return;
    }
    let (cx, cy) = centre_at(rect, (full_w, full_h), spider.climb);
    let squash = spider.squash.clamp(0.0, 1.0);
    let draw_h = full_h * (1.0 - SQUASH_DEPTH * squash);
    let draw_w = full_w * (1.0 + SQUASH_SPREAD * squash);
    // Centre-anchored, which is the border line itself when the spider is at
    // rest ([`STRADDLE`]): the creature flattens *onto* the edge it was
    // standing on. Anchoring the bottom instead pushes the flattened sliver
    // half a spider down onto the card's face, which reads as a smear on the
    // title rather than as something that has been stepped on — caught on a
    // real render of this build, not by the assertion below.
    let origin = (cx - draw_w / 2.0, cy - draw_h / 2.0);

    let ground = palette.ground;
    let hot = Rgb::from_tuple(crate::anim::cell::marker_ink(
        palette.hue,
        spider.intensity,
        ground.as_tuple(),
    ));
    // The pulse swings the accents and leaves the body alone. See [`PULSE_FADE`].
    let hot = hot.mix(ground, (1.0 - spider.pulse.clamp(0.0, 1.0)) * PULSE_FADE);
    let mid = hot.mix(BODY_DEEP, ACCENT_MID_MIX);

    let cell_w = draw_w / SPRITE_W as f32;
    let cell_h = draw_h / SPRITE_H as f32;
    for (sy, row) in SPRITE.iter().enumerate() {
        for (sx, token) in row.chars().enumerate() {
            let ink = match token {
                '#' => BODY_DEEP,
                '+' => BODY_LIFT,
                'o' => LEG_DARK,
                'p' => PLATE,
                'g' => mid,
                'G' => hot,
                'w' => WARM,
                'e' => EYE,
                _ => continue,
            };
            let x0 = origin.0 + sx as f32 * cell_w;
            let y0 = origin.1 + sy as f32 * cell_h;
            fill(sheet, (x0, y0, x0 + cell_w, y0 + cell_h), ink, opacity);
        }
    }
}

/// Fill one sprite pixel's rectangle, area-weighted.
///
/// A sprite pixel is close to a device pixel but never exactly one — the card's
/// height is a font measurement, not a multiple of 26 — so a rectangle that
/// simply rounded to whole pixels would drop or double whole rows of the
/// creature depending on where it landed. The coverage is the overlap area,
/// which is what keeps the legs the same weight wherever the spider is standing
/// on its climb.
fn fill(sheet: &mut Canvas, rect: (f32, f32, f32, f32), ink: Rgb, opacity: f32) {
    let (x0, y0, x1, y1) = rect;
    if x1 <= 0.0 || y1 <= 0.0 {
        return;
    }
    let px0 = x0.max(0.0).floor() as u32;
    let py0 = y0.max(0.0).floor() as u32;
    let px1 = (x1.max(0.0).ceil() as u32).min(sheet.width());
    let py1 = (y1.max(0.0).ceil() as u32).min(sheet.height());
    for y in py0..py1 {
        let cover_y = (y1.min(y as f32 + 1.0) - y0.max(y as f32)).clamp(0.0, 1.0);
        if cover_y <= 0.0 {
            continue;
        }
        for x in px0..px1 {
            let cover_x = (x1.min(x as f32 + 1.0) - x0.max(x as f32)).clamp(0.0, 1.0);
            if cover_x <= 0.0 {
                continue;
            }
            sheet.blend(x, y, ink, cover_x * cover_y * opacity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{CardContent, CardGeometry, ControlRail, StageHues};
    use super::*;
    use crate::anim::cell::{LifecycleStage, Severity};
    use crate::detect::AgentState;

    /// A card with nothing on it but a spider, so the ink a canvas comes back
    /// with is the creature and only the creature.
    fn content(stage: LifecycleStage, spider: Option<CardSpider>) -> CardContent {
        // The real theme's own stage hues, off a default app, so the ladder
        // under test is the one a card actually draws.
        let app = crate::app::state::AppState::test_new();
        let mut hues = [0.0; 5];
        for (slot, stage) in hues.iter_mut().zip(LifecycleStage::ALL) {
            *slot = stage.hue(&app.sidebar_palette, &app.host_terminal_theme);
        }
        CardContent {
            cut_above: false,
            title: String::new(),
            tidbit: None,
            register: None,
            state_label: String::new(),
            state: AgentState::Idle,
            stage,
            severity: Severity::Clear,
            hues: StageHues(hues),
            ground: Rgb(9, 17, 28),
            theme: super::super::CardTheme::UNTHEMED,
            split_channels: true,
            seen: true,
            depth: 0,
            lifted: false,
            focused_space: false,
            mark: None,
            // Nothing on this card but the creature: no absorbed workers either.
            residue: 0,
            controls: ControlRail::default(),
            generate: 1.0,
            discharge: 0.0,
            breath: 0.0,
            spider,
            wash: None,
            crew: Vec::new(),
            bars: None,
        }
    }

    fn spider_at(climb: f32, intensity: f32, squash: f32) -> CardSpider {
        CardSpider {
            climb,
            pulse: 1.0,
            intensity,
            squash,
        }
    }

    /// The card every drawing test here uses: the real card height, drawn one
    /// card's worth down its own image so there is a bloom margin above it for
    /// the spider to straddle into.
    const CARD_H: f32 = 54.4;
    const CARD_TOP: f32 = 25.0;

    fn placed<'a>(content: &'a CardContent) -> PlacedCard<'a> {
        PlacedCard {
            clip_top: 0.0,
            rect: super::super::canvas::RoundRect {
                x: 20.0,
                y: CARD_TOP,
                w: 400.0,
                h: CARD_H,
                r: 6.0,
            },
            content,
            geometry: CardGeometry::new(21.0, false),
            crew: crate::ui::sidebar::image_card::crew::CrewBands::default(),
        }
    }

    /// Draw one card's spider into an empty canvas and hand back the canvas.
    fn drawn(content: &CardContent) -> Canvas {
        let mut canvas = Canvas::new(460, 120);
        draw(&mut canvas, &placed(content));
        canvas
    }

    /// The bounding box of everything a canvas has ink in, as
    /// `(x0, y0, x1, y1)` half-open, plus how many pixels carry any.
    fn ink_bounds(canvas: &Canvas) -> Option<(u32, u32, u32, u32, usize)> {
        let px = canvas.rgba8();
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut count = 0usize;
        for y in 0..canvas.height() {
            for x in 0..canvas.width() {
                let i = ((y * canvas.width() + x) * 4 + 3) as usize;
                if px[i] == 0 {
                    continue;
                }
                count += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
        (count > 0).then_some((x0, y0, x1, y1, count))
    }

    #[test]
    fn a_row_with_no_open_defect_draws_nothing_at_all() {
        let content = content(LifecycleStage::Failed, None);
        assert!(
            ink_bounds(&drawn(&content)).is_none(),
            "a card with no spider on it put ink on the canvas anyway"
        );
    }

    #[test]
    fn a_settled_spider_straddles_the_cards_top_border_at_its_centre() {
        let content = content(LifecycleStage::Failed, Some(spider_at(1.0, 1.0, 0.0)));
        let (x0, y0, x1, y1, _) = ink_bounds(&drawn(&content)).expect("a marked card draws");
        let card_centre = 20.0 + 400.0 / 2.0;
        let drawn_centre = (x0 + x1) as f32 / 2.0;
        assert!(
            (drawn_centre - card_centre).abs() <= 2.0,
            "the spider settled at {drawn_centre} rather than the card's centre {card_centre}"
        );
        assert!(
            f32::from(u16::try_from(y0).unwrap()) < CARD_TOP,
            "the spider drew no part of itself above the card's top border"
        );
        assert!(
            f32::from(u16::try_from(y1).unwrap()) > CARD_TOP,
            "the spider drew no part of itself below the card's top border"
        );
    }

    #[test]
    fn the_climb_arrives_at_the_top_centre_from_the_cards_own_left_border() {
        let setting_out = content(LifecycleStage::Failed, Some(spider_at(0.0, 1.0, 0.0)));
        let arrived = content(LifecycleStage::Failed, Some(spider_at(1.0, 1.0, 0.0)));
        let (sx0, sy0, sx1, _, _) =
            ink_bounds(&drawn(&setting_out)).expect("a climbing card draws");
        let (ax0, ay0, ax1, _, _) = ink_bounds(&drawn(&arrived)).expect("an arrived card draws");
        assert!(
            (sx0 + sx1) < (ax0 + ax1),
            "a spider setting out was not left of the one that has arrived"
        );
        assert!(
            sy0 > ay0,
            "a spider setting out was not below the one that has arrived"
        );
    }

    #[test]
    fn the_climb_is_monotone_and_never_leaves_the_image() {
        let mut previous: Option<(u32, u32)> = None;
        for step in 0..=20 {
            let t = step as f32 / 20.0;
            let content = content(LifecycleStage::Failed, Some(spider_at(t, 1.0, 0.0)));
            let canvas = drawn(&content);
            let (x0, y0, x1, y1, _) = ink_bounds(&canvas).expect("every step of a climb draws");
            assert!(
                x1 <= canvas.width() && y1 <= canvas.height(),
                "the spider left the image at t={t}"
            );
            if let Some((px, py)) = previous {
                assert!(
                    x0 >= px.saturating_sub(1),
                    "the climb went backwards at t={t}"
                );
                assert!(y0 <= py + 1, "the climb went back down at t={t}");
            }
            previous = Some((x0, y0));
        }
    }

    /// The captain's *"full size on every rank — no shrink at worker depth"*.
    #[test]
    fn a_worker_card_gets_exactly_the_same_spider_as_a_mate() {
        let mut mate = content(LifecycleStage::Failed, Some(spider_at(1.0, 1.0, 0.0)));
        mate.depth = 0;
        let mut worker = content(LifecycleStage::Failed, Some(spider_at(1.0, 1.0, 0.0)));
        worker.depth = 2;
        assert_eq!(
            ink_bounds(&drawn(&mate)),
            ink_bounds(&drawn(&worker)),
            "a worker's spider is not the same size as a first mate's"
        );
    }

    /// The captain's *"green bugs should be squashed"*, and the approved
    /// concept's stage-4 collapse.
    #[test]
    fn the_green_stage_collapses_the_spider_on_its_vertical_axis() {
        let open = content(LifecycleStage::Failed, Some(spider_at(1.0, 1.0, 0.0)));
        let solved = content(LifecycleStage::Done, Some(spider_at(1.0, 1.0, 1.0)));
        let (ox0, oy0, ox1, oy1, _) = ink_bounds(&drawn(&open)).expect("an open defect draws");
        let (sx0, sy0, sx1, sy1, _) = ink_bounds(&drawn(&solved)).expect("a solved defect draws");
        assert!(
            (sy1 - sy0) * 3 < (oy1 - oy0),
            "the solved spider is {} tall against {} — that is a tint, not a collapse",
            sy1 - sy0,
            oy1 - oy0
        );
        assert!(
            (sx1 - sx0) > (ox1 - ox0),
            "a squashed spider went narrower rather than spreading"
        );
        let open_centre = (oy0 + oy1) as f32 / 2.0;
        let solved_centre = (sy0 + sy1) as f32 / 2.0;
        assert!(
            (open_centre - solved_centre).abs() <= 2.0,
            "the collapse slid the spider off the border it stands on: {open_centre} -> \
             {solved_centre}"
        );
    }

    #[test]
    fn the_hue_is_the_rows_stage_and_the_loudness_is_the_defects_own() {
        let ink = |stage, intensity| {
            let content = content(stage, Some(spider_at(1.0, intensity, 0.0)));
            let canvas = drawn(&content);
            let px = canvas.rgba8().to_vec();
            px
        };
        assert_ne!(
            ink(LifecycleStage::Failed, 1.0),
            ink(LifecycleStage::Running, 1.0),
            "the spider drew the same pixels at two different lifecycle stages"
        );
        assert_ne!(
            ink(LifecycleStage::Failed, 1.0),
            ink(LifecycleStage::Failed, 0.25),
            "the spider drew the same pixels at two different defect severities"
        );
    }

    #[test]
    fn the_pulse_moves_the_accents_and_leaves_the_body_alone() {
        let bright = content(
            LifecycleStage::Failed,
            Some(CardSpider {
                climb: 1.0,
                pulse: 1.0,
                intensity: 1.0,
                squash: 0.0,
            }),
        );
        let trough = content(
            LifecycleStage::Failed,
            Some(CardSpider {
                climb: 1.0,
                pulse: 0.0,
                intensity: 1.0,
                squash: 0.0,
            }),
        );
        assert_ne!(
            drawn(&bright).rgba8().to_vec(),
            drawn(&trough).rgba8().to_vec(),
            "the resting pulse changed nothing on screen"
        );
        assert_eq!(
            ink_bounds(&drawn(&bright)).map(|b| (b.0, b.1, b.2, b.3)),
            ink_bounds(&drawn(&trough)).map(|b| (b.0, b.1, b.2, b.3)),
            "the pulse changed the creature's shape rather than its accents"
        );
    }

    /// The sprite is a table, and a typo in it is a hole in the drawing.
    #[test]
    fn every_row_of_the_sprite_is_the_declared_width_and_only_known_tokens() {
        for (index, row) in SPRITE.iter().enumerate() {
            assert_eq!(
                row.chars().count(),
                SPRITE_W,
                "sprite row {index} is not {SPRITE_W} pixels wide"
            );
            for token in row.chars() {
                assert!(
                    ".#+opgGwe".contains(token),
                    "sprite row {index} carries an unknown token {token:?}"
                );
            }
        }
        // Mirror symmetry, which the reference has and a hand-authored table
        // loses one pixel at a time.
        for (index, row) in SPRITE.iter().enumerate() {
            let forward: Vec<char> = row.chars().collect();
            let back: Vec<char> = row.chars().rev().collect();
            assert_eq!(forward, back, "sprite row {index} is not symmetric");
        }
    }
}
