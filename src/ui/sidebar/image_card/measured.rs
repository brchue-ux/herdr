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
///
/// A card's hue is its own now — it carries which stage its work is at — so the
/// renderer runs this gradient as a *travel around* whatever hue that is, using
/// [`HUE_TRAVEL`] and [`STROKE_B_SAT_RATIO`] below. The sampled pair stays on
/// record because those two are derived from it and
/// `the_gradient_ratios_reproduce_the_sampled_pair` checks that they still
/// reproduce it.
#[allow(dead_code)] // the measurement [`HUE_TRAVEL`] and [`STROKE_B_SAT_RATIO`] come from
pub(super) const STROKE_B: Rgb = Rgb(126, 165, 209);

/// The hue travel across one card, in degrees: H 181.2 to H 211.8 in the sample.
///
/// Half either side of the card's own hue, so a stage's hue is the card's
/// midpoint rather than its left edge — the gradient is a property of the
/// card's *shape*, not a statement about which stage it is at.
pub(super) const HUE_TRAVEL: f32 = 30.6;

/// How saturated the card's right edge is against its left.
///
/// Only this ratio is carried from the pair, because their lightness is within
/// a point of each other. Taken off the sampled RGB rather than off the
/// percentages quoted beside them — `#7EA5D1` is HSL S 47.5% against `#7FE2E4`'s
/// 65.1%, not the 51/60 the sampling pass wrote down, and the pixels are what
/// the card is drawn from. `the_gradient_ratios_reproduce_the_sampled_pair` is
/// what keeps this tied to them.
pub(super) const STROKE_B_SAT_RATIO: f32 = 0.73;

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

/// Peak outward excess as a fraction of the stroke's own excess over canvas.
/// Sampled at +33 lum against the stroke's 173 (0.19), the reference value a
/// card standing alone was drawn at.
///
/// # Why this is 0.38 and not the sampled 0.19
///
/// At the captain's real 42-column sidebar the minimal gap between two cards
/// is a fixed 17 px, and at the previously shipped 0.07h sigma the gutter
/// between two same-hued cards still measured a mean 57–67% of the
/// brightness against a card's own stroke — a visibly hazy, not-quite-dark
/// seam, not the "clean, legible boundary" asked for. Raising the peak while
/// tightening [`BLOOM_SIGMA`] and dropping the far lobe ([`BLOOM_FAR_WEIGHT`])
/// is not a tradeoff: measured by compositing the real card layers the way
/// the terminal does, this brighter, tighter "wire" retune reads **0.0%
/// bleed in every one of nine real gaps**, and because `lay_bloom`'s cost
/// scales with the field's reach², the smaller reach this pairs with also
/// costs *less* to rasterise than the wider, dimmer glow it replaces.
///
/// The captain picked this over the softer alternative that still left 16%
/// residual bleed, explicitly flagging "zero" bleed as a qualified claim
/// measured only through the in-process render fixture, not yet a live
/// terminal — verify that before treating it as settled.
pub(super) const BLOOM_PEAK: f32 = 0.38;
/// Gaussian sigma of the bloom's near lobe, as a fraction of the tier's
/// **nominal** height — not the height the card is drawn at. See
/// [`super::CardGeometry::new`] for why every ratio in this table is against the
/// nominal, and [`super::BLOOM_REACH_SIGMAS`] for what went wrong when one
/// constant was not.
///
/// Narrowed from 0.07h (itself narrowed from the sampled 0.19h — see
/// [`BLOOM_PEAK`]) to 0.030h alongside the peak increase and the move to a
/// single lobe ([`BLOOM_FAR_WEIGHT`] `= 0.0`): a hot, thin core rather than a
/// soft halo, which is what let this candidate clear zero measured bleed at
/// the real 17 px minimal gap instead of the 57–67% the wider glow left.
pub(super) const BLOOM_SIGMA: f32 = 0.030;
/// The bloom is *more saturated* than the stroke: its excess is (8,40,39) where
/// the stroke's R/G is 0.52.
///
/// Quoted as a saturation and a lightness ratio rather than as the red-channel
/// multiplier it was sampled as (`R × 0.40`), because scaling one named channel
/// is only "more saturated" for a colour whose red channel happens to be its
/// minimum — true of the measured cyan, false the moment a card's hue is its
/// own. These two reproduce the sampled bloom to within a level on that cyan and
/// mean the same thing on every other hue: `#7FE2E4` restated by them lands on
/// `#31E1E4` against the sampled `#32E2E4`.
pub(super) const BLOOM_SAT_MUL: f32 = 1.18;
pub(super) const BLOOM_LUM_MUL: f32 = 0.78;

/// Single lobe now, not two: the far lobe that used to carry the tail past
/// the near lobe's own falloff is exactly what let the previous shipped glow
/// still reach a same-hued neighbour at the real 17 px minimal gap. Dropping
/// it entirely ([`BLOOM_FAR_WEIGHT`] `= 0.0`) in favour of a single tight,
/// bright core is what took the measured gutter bleed to 0.0% — a two-lobe
/// fit that undershoots either the peak or the tail is no longer the
/// tradeoff being made once the tail itself is the thing bleeding into the
/// gutter.
pub(super) const BLOOM_NEAR_WEIGHT: f32 = 1.0;
pub(super) const BLOOM_FAR_WEIGHT: f32 = 0.0;
/// Irrelevant while [`BLOOM_FAR_WEIGHT`] is `0.0` — there is no far lobe for
/// this to scale. Left in place rather than removed so a future two-lobe
/// retune has the constant to come back to.
pub(super) const BLOOM_FAR_SIGMA_MUL: f32 = 1.5;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The two derived gradient ratios still reproduce the pair they came from.
    ///
    /// [`HUE_TRAVEL`] and [`STROKE_B_SAT_RATIO`] replaced the second sampled
    /// stroke in the renderer, so this is what keeps them honest: run the
    /// measured left edge through them and the measured right edge comes back.
    #[test]
    fn the_gradient_ratios_reproduce_the_sampled_pair() {
        let (h, s, l) = STROKE_A.to_hsl();
        let a = Rgb::from_hsl(h - HUE_TRAVEL / 2.0, s, l);
        let b = Rgb::from_hsl(h + HUE_TRAVEL / 2.0, s * STROKE_B_SAT_RATIO, l);
        // The travel is re-centred, so the sample's own two ends sit half a
        // travel either side of where they were measured — what has to survive
        // is the *span* between them and the saturation ratio across it.
        let (ha, sa, _) = a.to_hsl();
        let (hb, sb, _) = b.to_hsl();
        assert!((hb - ha - HUE_TRAVEL).abs() < 1.0, "{ha} to {hb}");
        assert!((sb / sa - STROKE_B_SAT_RATIO).abs() < 0.02);

        let (measured_a, measured_b) = (STROKE_A.to_hsl(), STROKE_B.to_hsl());
        assert!(
            (measured_b.0 - measured_a.0 - HUE_TRAVEL).abs() < 1.0,
            "the sampled pair no longer spans {HUE_TRAVEL}°"
        );
        assert!((measured_b.1 / measured_a.1 - STROKE_B_SAT_RATIO).abs() < 0.02);
    }

    /// And the bloom ratios reproduce the sampled bloom on the sampled stroke.
    #[test]
    fn the_bloom_ratios_reproduce_the_sampled_bloom() {
        let bloomed = STROKE_A.restate(BLOOM_SAT_MUL, BLOOM_LUM_MUL);
        // The sample: red scaled to 0.40 of itself on the measured cyan.
        let sampled = Rgb((f32::from(STROKE_A.0) * 0.40) as u8, STROKE_A.1, STROKE_A.2);
        let gap = |a: u8, b: u8| i32::from(a).abs_diff(i32::from(b));
        assert!(
            gap(bloomed.0, sampled.0) <= 2
                && gap(bloomed.1, sampled.1) <= 2
                && gap(bloomed.2, sampled.2) <= 2,
            "{bloomed:?} against the sampled {sampled:?}"
        );
    }
}
