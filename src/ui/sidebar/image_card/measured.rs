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

/// The hue band the whole tree column's ink stays inside, in degrees.
///
/// H1, measured off the reference rather than chosen: over its entire tree
/// column, **99.94% of chromatic pixels above L25 sit inside 175–265°, and
/// 99.7% of those in a single 15° bucket at 195°**. One hue family; everything
/// else in the panel is brightness.
///
/// It is a *clamp* on [`HUE_TRAVEL`] and not a replacement for it — see
/// `CardLight::inks`. The travel is a property of a card's shape and runs its
/// full width everywhere inside the band; it gives way only at the band's two
/// edges, which is where a card at the cold end of the measured family would
/// otherwise put its left border outside the tree's own colour.
pub(super) const HUE_BAND: (f32, f32) = (175.0, 265.0);

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
///
/// **Capped by [`RADIUS_MAX_PX`], which is what actually decides it now.** The
/// 0.13 h here was measured off an earlier sampling pass; the reference the card
/// is drawn against has sharp corners — F6, *no radius above 3 px* — and 0.13 h
/// is an 8 px arc, which is a rounded card. The ratio stays on record because it
/// is a measurement, and the cap stays over it because the reference is.
pub(super) const RADIUS: f32 = 0.13;

/// The largest corner arc the card ever draws, in pixels.
///
/// F6's own number. A pane in the reference is a sharp-cornered rectangle with
/// at most a hairline break at the corner; anything above this reads as a
/// rounded plate, which is the material the glass treatment replaces.
pub(super) const RADIUS_MAX_PX: f32 = 3.0;

/// The card's face, as glass rather than as a plate.
///
/// `rgba(122, 196, 222, .10)` — sampled off the reference's own mate pane. It is
/// a *tint over what is behind it*, not a fill: at [`GLASS_FACE_ALPHA`] the
/// starfield, and in herdr the whole-terminal scene, is measurably visible
/// through the card. That is H7, and it is the load-bearing quality of the
/// material — a card that occludes the sky is the tree covering the system
/// rather than hanging in front of it.
pub(super) const GLASS_FACE: Rgb = Rgb(122, 196, 222);

/// How much of the face's tint reaches the pixel. The reference's own `.10`.
pub(super) const GLASS_FACE_ALPHA: f32 = 0.10;

/// How far down and right the second face sits, in pixels.
///
/// The reference's own 3 px. It is what makes the pane read as an object with a
/// front — a single boundary at any alpha reads as a painted rectangle, and no
/// amount of edge brightness fixes that.
pub(super) const GLASS_THICKNESS_PX: f32 = 3.0;

/// The back face's share of the front's own alpha.
///
/// Under half, so the thickness is a shadow of the pane's own material rather
/// than a second pane. Any higher and the offset copy competes with the card it
/// is behind; any lower and it disappears against the panel.
pub(super) const GLASS_BACK_ALPHA: f32 = 0.45;

/// The back face's edge, as a share of the front edge's alpha.
///
/// The one line that actually draws the thickness, so it is the strongest part
/// of the back face — but still well under the front's, which is what keeps the
/// front reading as the front.
pub(super) const GLASS_BACK_EDGE_ALPHA: f32 = 0.35;

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
/// terminal.
///
/// Since confirmed twice, and the second one is the one the captain asked for.
///
/// First off the wire: a real server on this build, driven through a real PTY
/// client at the captain's 42-column sidebar and a 10×21 px cell, decoding
/// the actual `\x1b_G` Kitty graphics APC blocks off the wire (not the
/// in-process PNG fixture) and compositing them source-over in linear light
/// the way a terminal does. Two adjacent same-hued idle cards at the real
/// 17 px ink-to-ink gap: the gutter's four centre rows read exactly the
/// panel background, `(30, 30, 46)`, with zero measured excess.
///
/// Then on real pixels, which is what "zero" had never been checked against:
/// a real kitty under Xvfb attached to a lab server on this build, at the same
/// 42 columns and the same 10 × 21 px cell the server logs for that client —
/// so the terminal decoded, scaled and composited the graphics itself. Nine
/// same-hued idle cards, 48 gutters over six frames of the breath, measured
/// off screenshots: every gutter row is **byte-identical to bare panel**, a
/// Chebyshev excess of 0 on every channel. Not "0.0% by a ratio" — no pixel
/// between two cards differs from the panel at all.
///
/// The same rig run against the glow this replaced (`BLOOM_SIGMA` 0.07 h, two
/// lobes, peak 0.19) reads a Chebyshev excess of 21 in the same gutters, which
/// is what makes the zero a measurement rather than a rig that cannot see
/// bleed.
///
/// One caveat belongs with the number, because it decides whether it holds on
/// a given host. The gutter is not a constant: `row_height_cells` rounds a
/// card's wanted height *up* to whole cells and `place` centres the card in
/// them, so the gap between two cards is the rounding leftover,
/// `(-card_height_px).rem_euclid(cell_height)`. That is a property of the
/// host's cell, not of this table, and it does not vary smoothly — at a 21 px
/// cell it is 16 px and this field is past its reach there (Chebyshev 0), at
/// 20 px it is 12 px (Chebyshev 3), and at 18 px the grid leaves only 4 px.
/// In that last case the gutter's midpoint sits inside one sigma, and a hot
/// narrow core is the wrong shape for it: measured on the same rig, wire-C
/// reads *brighter* there than the dim wide glow it replaced, 49 against 40.
///
/// That is not a reason to widen this field again — it would give up the
/// 21-to-0 win at the captain's own cell to improve a case that is already
/// bad. Closing it means moving the gap itself, which is the `row_gap` lever
/// the captain refused in `glow-narrower-field` to keep card density. His
/// call, not a retune of this table.
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
///
/// Cut from 44.0 with the card, on the same 20% as `BASE_HEIGHT_PX`: this is an
/// absolute pixel ceiling on a slot whose measured size is a fraction of the
/// nominal height, so leaving it where it was would have quietly stopped it
/// being a cap at all — 0.70 × 54.4 is 38.1 px, already under the old 44 — and
/// the deviation this constant exists to record would have evaporated rather
/// than been withdrawn. `the_plate_is_capped_so_the_card_is_not_the_narrowest_column`
/// is the const assertion that catches that.
pub(super) const PLATE_MAX_PX: f32 = 35.2;

/// Ink: `#CEDCE9`, L 86%.
pub(super) const INK: Rgb = Rgb(206, 220, 233);

/// How far the tidbit line sits from full ink. Measured as a *design* choice in
/// the density pass, not off the reference: 52% ink is where the second line
/// reads as caption rather than as a second sentence.
pub(super) const TIDBIT_INK_MIX: f32 = 0.52;
/// The tidbit's type size, as a fraction of the title's.
pub(super) const TIDBIT_SIZE_MUL: f32 = 0.72;

/// The Space badge's healthy ink: `--ok: #3ddc84`, the flight-deck circuit
/// mockup's own token.
pub(super) const BADGE_OK: Rgb = Rgb(61, 220, 132);
/// The Space badge's warn ink: `--amber: #ffb454`, same mockup, `.badge.warn`.
pub(super) const BADGE_WARN: Rgb = Rgb(255, 180, 84);
/// The badge pill's own fill, as a share of its ink: the mockup's
/// `rgba(*, .14)` on both `.badge` and `.badge.warn`.
pub(super) const BADGE_FILL_ALPHA: f32 = 0.14;
/// The badge pill's border, as a share of its ink: the mockup's `~.3` on both.
pub(super) const BADGE_EDGE_ALPHA: f32 = 0.30;
/// The badge's type size, as a fraction of the caption's own. The mockup sets
/// `.badge` at `0.56rem` against `.card-name`'s `0.72rem` — 78%, applied here
/// to the card's own caption size rather than its title, since the badge sits
/// no taller than the control rail beside it.
pub(super) const BADGE_SIZE_MUL: f32 = 0.78;
/// The badge's horizontal padding, as a fraction of its own text height. The
/// mockup's `0.36rem` against a `0.56rem` badge line is 64%.
pub(super) const BADGE_PAD_MUL: f32 = 0.64;
/// The badge's vertical padding, as a fraction of its own text height. The
/// mockup's `0.06rem` against a `0.56rem` badge line is 11% — a pill that
/// hugs its text, not a tall chip.
pub(super) const BADGE_VPAD_MUL: f32 = 0.11;

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
