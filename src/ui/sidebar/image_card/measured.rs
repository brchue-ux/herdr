//! The card's sampled constants.
//!
//! Every number here came out of the captain's reference image with a pixel
//! probe rather than out of somebody's eye — the sampling passes and the
//! resulting table live in `data/herdr-card-measured-restyle/`. They are
//! reproduced verbatim, with the measurement each one is in the comment beside
//! it, so a change to one of them is visibly a change to a *measurement* and
//! has to be justified against the reference rather than against taste.
//!
//! Ratios are against `h`, the card's own height, which is what lets the same
//! table serve all three tiers without a second set of numbers.

use super::canvas::Rgb;

/// The darkest clean canvas the reference's cards sit on: `#09111C`, lum 16.1.
///
/// Only a fallback here. Herdr composites against the *host* terminal's
/// background, which it measures, so this is what the card floats on only when
/// the host answered neither OSC 11 nor the palette query.
pub(super) const CANVAS: Rgb = Rgb(9, 17, 28);

/// Card stroke, left edge: `#7FE2E4`, H 180.9° S 60% L 65%, lum 205.
pub(super) const STROKE_A: Rgb = Rgb(127, 226, 228);
/// Card stroke, right edge: `#7EA5D1`, H 212.1° S 51% L 66%, lum 160.
///
/// The gradient is **per card**, not tree-wide: every card in the reference
/// runs H 181 at its own left edge and H 212 at its own right edge regardless
/// of where it sits, so it is normalised to card width and not to panel
/// position.
pub(super) const STROKE_B: Rgb = Rgb(126, 165, 209);

/// Geometric stroke width, as a fraction of `h`. Measured FWHM was 3 px on a
/// 61 px card (0.049 h) including antialiasing; the geometric core is ~2 px.
pub(super) const STROKE_W: f32 = 0.033;

/// Corner radius: an 8 px arc on a 61 px card.
pub(super) const RADIUS: f32 = 0.13;

/// Card fill at the centre: `#1E323E`, lum 46.6 — **2.9× the canvas**, which is
/// the number that killed "near-black with a subtle gradient".
pub(super) const FILL_MID: Rgb = Rgb(30, 50, 62);

/// The fill is not a linear vertical ramp. It is a symmetric *inner glow* from
/// both strokes in the local stroke hue, at this alpha and this falloff.
pub(super) const FILL_EDGE_ALPHA: f32 = 0.10;
pub(super) const FILL_INNER_SIGMA: f32 = 0.09;

/// Peak outward excess as a fraction of the stroke's own excess over canvas:
/// +33 lum immediately outside the stroke where the stroke's excess is 173.
pub(super) const BLOOM_PEAK: f32 = 0.19;
/// Gaussian sigma of the bloom, as a fraction of `h` — 11.5 px on a 61 px card,
/// reading zero by 26–28 px.
pub(super) const BLOOM_SIGMA: f32 = 0.19;
/// The bloom is *more saturated* than the stroke: its excess is (8,40,39) where
/// the stroke's R/G is 0.52, so red is scaled by this on the way out.
pub(super) const BLOOM_RED_MUL: f32 = 0.40;

/// Two lobes rather than one: a single gaussian fitted to the peak undershoots
/// the tail and one fitted to the tail undershoots the peak.
pub(super) const BLOOM_NEAR_WEIGHT: f32 = 0.82;
pub(super) const BLOOM_FAR_WEIGHT: f32 = 0.18;
pub(super) const BLOOM_FAR_SIGMA_MUL: f32 = 2.2;

/// Padding: 9 px on a 61 px card at left, top and bottom; 13 px at the right.
pub(super) const PAD: f32 = 0.148;
pub(super) const PAD_RIGHT: f32 = 0.21;

/// The icon container: a 42 × 43 px square on a 61 px card, with its own
/// vertical gradient and a hairline.
pub(super) const PLATE: f32 = 0.70;
pub(super) const PLATE_RADIUS: f32 = 0.10;
/// Gap between the plate and the text ink: 13 px on a 61 px card.
pub(super) const PLATE_GAP: f32 = 0.21;

/// The measured 0.70 h plate makes a full-width *top* card the narrowest text
/// column in the whole tree — 291 px against 315 px at depth 2. Capping it is a
/// named deviation from the measurement, taken in
/// `data/herdr-card-iteration-2/` and kept here.
pub(super) const PLATE_MAX_PX: f32 = 44.0;

/// Ink: `#CEDCE9`, L 86%.
pub(super) const INK: Rgb = Rgb(206, 220, 233);

/// How far the tidbit line sits from full ink. Measured as a *design* choice in
/// the density pass, not off the reference: 52% ink is where the second line
/// reads as caption rather than as a second sentence.
pub(super) const TIDBIT_INK_MIX: f32 = 0.52;
/// The tidbit's type size, as a fraction of the title's.
pub(super) const TIDBIT_SIZE_MUL: f32 = 0.72;

/// An inactive card in the reference is the same hue as an active one and
/// differs only by saturation and bloom — S 14.5% where an active card is
/// 59.6%, lum 111 against 196, and no bloom at all. It never moves hue to
/// signal anything, which is the reference answering the state question itself.
pub(super) const MUTED_SAT: f32 = 0.145 / 0.596;
pub(super) const MUTED_LUM: f32 = 111.5 / 196.5;

/// Fill hue travel across the card: the same 180→212 the stroke runs, at about
/// a quarter of the amplitude. Left (30,60,71) → right (35,61,86).
pub(super) const FILL_TRAVEL_A: Rgb = Rgb(30, 60, 71);
pub(super) const FILL_TRAVEL_B: Rgb = Rgb(35, 61, 86);
