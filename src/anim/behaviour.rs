//! What a named behaviour *is*, expressed as data.
//!
//! A behaviour is four independent choices — which cells the effect reaches
//! ([`Field`]), how it combines with time ([`Shape`]), how that time is paced
//! ([`Curve`]), and what the resulting per-cell amount does to the cell
//! ([`Paint`]) — plus how fast it loops and what live signal, if any, drives
//! its strength. Every name in the catalogue is one row of those choices. That
//! is the whole extensibility claim: a new named behaviour is a value, not a
//! branch, and nothing in the render pipeline learns its name.
//!
//! Four properties this module is responsible for holding:
//!
//! - **Resolving a cell is a pure function.** `cell(pos, extent, progress, …)`
//!   allocates nothing, reads no clock, and touches no state, so a render pass
//!   can call it once per cell without any of them being able to disagree.
//! - **A behaviour never widens or narrows what it decorates.** It resolves to
//!   colour, attributes, coverage, and — for decoration only — a glyph of the
//!   same display width. Never a width, never a position. Dropping every frame
//!   leaves the element identical, and no substitution can move a column; see
//!   [`super::cell::CellPaint::glyph_over`], which is where that is enforced.
//! - **A live metric scales a behaviour; it never *is* one.** [`Drive`] maps a
//!   `0.0..=1.0` signal onto a strength or a rate, so the same named behaviour
//!   works with a metric bound or without one, and the metric's own shape stays
//!   the sampler's business rather than being re-derived here.
//! - **Cost is declared, not discovered.** Each behaviour states the shortest
//!   spacing between two frames anyone could tell apart, so the loop can arm
//!   exactly the deadlines that change a cell and no others.

use std::collections::HashMap;
use std::time::Duration;

use super::cell::{AttrPatch, CellExtent, CellPaint, CellPos, Ink, InkPalette};
use crate::ui::color::mix_rgb;

/// Loop period every built-in idle behaviour is written against unless it says
/// otherwise. Long enough to read as breathing rather than as flashing.
const DEFAULT_PERIOD: Duration = Duration::from_millis(1_600);

/// Default spacing between two frames anyone could tell apart.
///
/// Matches the app's existing animation interval exactly, so a behaviour that
/// does not ask for anything smoother costs precisely what the sidebar pulse
/// already costs today. Behaviours whose motion actually reads at a finer step
/// — a band travelling across a span, a reveal — say so themselves.
const DEFAULT_FRAME_INTERVAL: Duration = crate::app::ANIMATION_INTERVAL;

/// Frame spacing for behaviours whose motion genuinely resolves finer.
///
/// Three times the app's minimum render interval: fine enough that a travelling
/// edge reads as moving rather than stepping, coarse enough that a configured
/// animation cannot turn the loop into a busy spin.
const SMOOTH_FRAME_INTERVAL: Duration = Duration::from_millis(50);

/// Which cells an effect reaches, and in what order.
///
/// The value a field gives a cell is *where that cell sits along the effect*,
/// in `0.0..=1.0` — not how bright it is. [`Shape`] turns that plus the clock
/// into an amount.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Field {
    /// Every cell at once.
    ///
    /// A uniform field has no geometry, so [`Shape`] has nothing to act on and
    /// the amount is the paced progress itself. This is what makes `fade` and
    /// `pulse` reach every cell equally instead of saturating part-way.
    Uniform,
    /// Along a straight axis.
    Linear { axis: Axis, reverse: bool },
    /// Outward from the element's centre, or inward toward it.
    Radial { inward: bool },
    /// Pseudo-random per cell, stable for a given cell and seed.
    ///
    /// Stability is the whole point: a dissolve whose cells reshuffled every
    /// frame is noise, not a dissolve.
    Scatter { seed: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Horizontal,
    Vertical,
}

impl Field {
    fn value(self, pos: CellPos, extent: CellExtent) -> f32 {
        let (u, v) = extent.normalize(pos);
        match self {
            Self::Uniform => 0.0,
            Self::Linear { axis, reverse } => {
                let along = match axis {
                    Axis::Horizontal => u,
                    Axis::Vertical => v,
                };
                if reverse {
                    1.0 - along
                } else {
                    along
                }
            }
            Self::Radial { inward } => {
                // Chebyshev rather than Euclidean distance: on a cell grid it
                // produces a clean rectangular front, which is what reads as a
                // pane collapsing rather than as a blurry circle.
                let distance = (u - 0.5).abs().max((v - 0.5).abs()) * 2.0;
                if inward {
                    1.0 - distance
                } else {
                    distance
                }
            }
            Self::Scatter { seed } => {
                let mut hash = u64::from(seed)
                    ^ (u64::from(pos.col) << 32)
                    ^ u64::from(pos.row).wrapping_mul(0x9E37_79B9);
                hash ^= hash >> 33;
                hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
                hash ^= hash >> 33;
                (hash >> 40) as f32 / 16_777_216.0
            }
        }
    }

    /// Cells between this field's first and last position, for turning a length
    /// expressed in cells into the `0.0..=1.0` the field itself speaks in.
    ///
    /// Never zero, so a one-cell element divides cleanly instead of blowing up.
    /// A field with no single axis to measure along reports one step rather
    /// than inventing a geometry: a length in cells has no meaning on a scatter.
    fn span(self, extent: CellExtent) -> f32 {
        let cells = match self {
            Self::Linear {
                axis: Axis::Horizontal,
                ..
            } => extent.cols,
            Self::Linear {
                axis: Axis::Vertical,
                ..
            } => extent.rows,
            Self::Uniform | Self::Radial { .. } | Self::Scatter { .. } => 2,
        };
        f32::from(cells.saturating_sub(1).max(1))
    }
}

/// How a cell's place in the field combines with the clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Shape {
    /// A leading edge sweeps through; everything behind it stays at full
    /// amount. Reveals, wipes, typewriters, dissolves, collapses.
    ///
    /// `softness` is how much of the field the edge takes to go from nothing to
    /// full. At `0.0` it is a hard step — one cell on, the next off — which is
    /// exactly what a typewriter is.
    Front { softness: f32 },
    /// A band peaks as it passes each cell and leaves it as it was. Shimmers,
    /// highlights, travelling charges.
    ///
    /// `width` is the band's own extent as a fraction of the field. It travels
    /// from fully before the first cell to fully past the last, so a looping
    /// band never wraps visibly.
    Band { width: f32 },
    /// The field offsets each cell's phase through the curve. Waves.
    ///
    /// `spread` is how many whole cycles separate the first cell from the last.
    Phase { spread: f32 },
    /// A peak with a short leading edge and a long trailing one. Charges,
    /// scanners, anything that should read as *moving* rather than as pulsing.
    ///
    /// Asymmetric on purpose, and that asymmetry is what carries the sub-cell
    /// position. A symmetric band gives a cell the same amount whether the
    /// effect is arriving or leaving, so nothing downstream can tell which; a
    /// comet's `head_cells` is a linear rise ahead of the peak, so a cell's
    /// amount *is* how far into that cell the peak has come. At the default
    /// one-cell rise a glyph ramp resolves that fraction directly — eight
    /// positions inside a cell the grid could otherwise only light or not.
    ///
    /// Both lengths are **in cells**, not in fractions of the field, which is
    /// the one place this enum departs from the others. A comet drawn over
    /// three cells and one drawn over thirty should have the same-sized head,
    /// because the head's size is what the sub-cell reading depends on; a
    /// fraction would silently stop meaning a cell the moment the element was
    /// drawn at a different width. The peak travels from one head-length before
    /// the first cell to one tail-length past the last, so both ends of the
    /// travel are dark and a looping comet never wraps visibly.
    Comet { head_cells: f32, tail_cells: f32 },
}

/// A glyph substitution that resolves a position finer than one cell.
///
/// The ramp's steps are positions *inside* a cell: an amount rising from
/// `floor` to `1.0` walks it end to end, so an effect whose amount is a
/// continuous function of where it is reads as travelling through a cell rather
/// than as arriving in it. Three cells and an eight-step ramp is twenty-four
/// distinguishable positions where the cell grid alone offers three.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GlyphRamp {
    /// Emptiest first. Every entry must be one column wide;
    /// `every_glyph_a_behaviour_can_ask_for_is_one_column_wide` is the check.
    pub(crate) steps: &'static [char],
    /// Amount below which the cell keeps the glyph it settled to. Above zero so
    /// a cell the effect has barely reached is left alone rather than flickering
    /// its lightest step on and off.
    pub(crate) floor: f32,
}

impl GlyphRamp {
    fn glyph(self, amount: f32) -> Option<char> {
        if self.steps.is_empty() || amount < self.floor {
            return None;
        }
        let span = (1.0 - self.floor).max(1e-3);
        let position = ((amount - self.floor) / span).clamp(0.0, 1.0);
        let last = self.steps.len() - 1;
        let index = (position * last as f32).round() as usize;
        self.steps.get(index.min(last)).copied()
    }
}

/// A discharge: the one thing a *shape* can say that no styling can.
///
/// This is the reason [`super::cell::CellPaint`] carries a glyph at all. A
/// charge crackles — it forks, breaks, and jumps — and there is no colour,
/// brightness, or attribute on a `─` that reads as an arc, because the
/// information "something is arcing here" lives in the mark's shape. Above
/// `above` the cell draws an arc struck through the line instead of the line,
/// re-rolled every `flicker` of the loop.
///
/// The roll also pulls the cell's amount down by up to `jitter`, because a
/// discharge whose shape flickered while its brightness swept smoothly past
/// would read as a marching decoration rather than as something electrical.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Crackle {
    /// Arcs to choose between. One column each, and deliberately drawn from the
    /// same box-drawing family as the line they strike through, so a discharge
    /// reads as happening *to* the connector rather than as a foreign glyph
    /// parked on top of it.
    pub(crate) arcs: &'static [char],
    /// Amount at or above which a cell arcs rather than showing its ramp step.
    pub(crate) above: f32,
    /// Fraction of one loop between re-rolls. Small enough to read as a
    /// crackle, large enough that consecutive frames are not pure noise.
    pub(crate) flicker: f32,
    /// Deepest a roll may pull a cell's amount, in `0.0..=1.0`.
    pub(crate) jitter: f32,
    pub(crate) seed: u32,
}

impl Crackle {
    /// One pseudo-random draw, stable for a cell within a flicker bucket.
    ///
    /// Bucketed rather than continuous so the discharge holds a shape for a
    /// few frames and then jumps, which is what crackling looks like; a fresh
    /// draw every frame is indistinguishable from static.
    fn roll(self, pos: CellPos, progress: f32) -> u64 {
        let bucket = (progress / self.flicker.max(1e-3)).floor();
        let mut hash = u64::from(self.seed)
            ^ u64::from(pos.col).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ u64::from(pos.row).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
            ^ (bucket as i64 as u64).wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        hash ^= hash >> 29;
        hash
    }

    fn arc(self, roll: u64) -> Option<char> {
        if self.arcs.is_empty() {
            return None;
        }
        self.arcs
            .get((roll >> 40) as usize % self.arcs.len())
            .copied()
    }

    /// The factor this roll scales the cell's amount by, in
    /// `1.0 - jitter ..= 1.0`.
    fn dim(self, roll: u64) -> f32 {
        let unit = ((roll >> 16) & 0xFFFF) as f32 / 65_535.0;
        1.0 - self.jitter.clamp(0.0, 1.0) * unit
    }
}

/// How progress is paced through its span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Curve {
    Linear,
    /// Fast, then settling. The pacing for anything arriving.
    EaseOut,
    /// Slow at both ends. The pacing for anything moving between two rests.
    EaseInOut,
    /// Up and back down, peaking at the halfway point. Loops seamlessly.
    Triangle,
    /// Up and back down on a cosine. Loops seamlessly and with no corner at
    /// the peak, which is what separates breathing from ticking.
    Sine,
    /// Slow, then a ramp into a snap, then a pendulum that settles.
    ///
    /// The one curve in this enum that is a *stated* motion character rather
    /// than a piece of arithmetic, and the numbers in [`SNAP_OVERSHOOT`] and
    /// [`SNAP_REVERSE`] are the specification, not taste: exponential
    /// acceleration into a snap, roughly ten percent past the target, then
    /// about five percent back the other way before it comes to rest.
    ///
    /// It is the only curve that **exceeds 1.0** — that overshoot is the whole
    /// point, so a consumer reading [`Behaviour::strength`] gets it intact and
    /// the colour path clamps at the one place it mixes. It also returns to
    /// exactly `0.0` at the end of its span, so it loops without a seam.
    SnapPendulum,
    /// The same snap, ending where it landed instead of releasing back to rest.
    ///
    /// Not a second motion character: [`snap`] is the stated one and both
    /// curves are it, so [`SNAP_OVERSHOOT`] and [`SNAP_REVERSE`] cannot drift
    /// apart between them. What this one drops is the release, and the release
    /// exists for exactly one reason — a *loop* must come back to where it
    /// started or it has a seam in it. A bounded arrival has nothing to loop
    /// back to, and playing the release on one would undo the arrival: a card's
    /// state wash would sweep across, taint the card, and then untaint it.
    ///
    /// Ends at exactly `1.0`, because the pendulum's second lobe is a whole
    /// sine hump and closes on zero.
    SnapArrival,
}

/// Steepness of the snap's exponential ramp.
///
/// Four is where the ramp reads as accelerating rather than as a slow linear
/// slide: the first half of the rise covers under a fifth of the distance.
const SNAP_RAMP_K: f32 = 4.0;

/// Fraction of one span the ramp occupies, before the snap lands.
const SNAP_RISE: f32 = 0.42;

/// Fraction of one span the pendulum swings through after the snap.
const SNAP_RING: f32 = 0.26;

/// How far past the target the snap carries, as a fraction of the travel.
const SNAP_OVERSHOOT: f32 = 0.10;

/// How far back the other way the pendulum swings after the overshoot.
const SNAP_REVERSE: f32 = 0.05;

/// Normalised exponential acceleration over `0.0..=1.0`.
fn exp_in(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    ((SNAP_RAMP_K * t).exp() - 1.0) / (SNAP_RAMP_K.exp() - 1.0)
}

/// The swing after the snap lands, as an offset from the target.
///
/// One lobe [`SNAP_OVERSHOOT`] past it, then one [`SNAP_REVERSE`] back the
/// other way. Both lobes are whole sine humps, so the swing starts at rest,
/// ends at rest, and never has a corner in it — a pendulum, not a bounce.
fn pendulum(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    if u < 0.5 {
        SNAP_OVERSHOOT * (std::f32::consts::TAU * u).sin()
    } else {
        -SNAP_REVERSE * (std::f32::consts::TAU * (u - 0.5)).sin()
    }
}

/// The snap itself, over its own `0.0..=1.0`: the exponential ramp, then the
/// pendulum that settles it.
///
/// The captain's sentence and nothing else — *"exponential acceleration, starts
/// slow then ramps into a snap, a 10% overshoot that pendulums back, maybe 5%
/// in reverse"*. Both snap curves are this function; they differ only in what
/// they do after it, which is why the numbers live here once.
fn snap(u: f32) -> f32 {
    // The rise and the ring re-expressed as fractions of the snap alone rather
    // than of a whole [`Curve::SnapPendulum`] span, so the two curves resolve
    // the same shape at the same place in it.
    const RISE: f32 = SNAP_RISE / (SNAP_RISE + SNAP_RING);
    let u = u.clamp(0.0, 1.0);
    if u < RISE {
        exp_in(u / RISE)
    } else {
        1.0 + pendulum((u - RISE) / (1.0 - RISE))
    }
}

impl Curve {
    fn apply(self, progress: f32) -> f32 {
        let p = progress.clamp(0.0, 1.0);
        match self {
            Self::Linear => p,
            Self::EaseOut => 1.0 - (1.0 - p) * (1.0 - p),
            Self::EaseInOut => p * p * (3.0 - 2.0 * p),
            Self::Triangle => 1.0 - (2.0 * p - 1.0).abs(),
            Self::Sine => (1.0 - (std::f32::consts::TAU * p).cos()) / 2.0,
            Self::SnapPendulum => {
                let settled = SNAP_RISE + SNAP_RING;
                if p < settled {
                    snap(p / settled)
                } else {
                    // The release back to rest is the same acceleration played
                    // forward, so the return reads as deliberate rather than as
                    // the effect being switched off.
                    1.0 - exp_in((p - settled) / (1.0 - settled).max(1e-3))
                }
            }
            Self::SnapArrival => snap(p),
        }
    }
}

/// What a per-cell amount does to the cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Paint {
    /// Foreground moves toward this ink as the amount rises.
    pub(crate) fg: Option<Ink>,
    /// Background moves toward this ink as the amount rises.
    pub(crate) bg: Option<Ink>,
    /// How far that move goes at full amount, in `0.0..=1.0`. Partial on
    /// purpose for anything that has to stay readable at its extreme.
    pub(crate) depth: f32,
    /// The amount is the cell's coverage: `0.0` is not yet here, `1.0` is
    /// fully arrived. Reveals set this; steady-state emphasis does not.
    pub(crate) reveal: bool,
    /// Attributes applied once the amount crosses this threshold.
    pub(crate) attrs_above: Option<(f32, AttrPatch)>,
    /// Sub-cell glyph ramp. Decoration only — a call site drawing information
    /// ignores it, so setting this can never garble a label.
    pub(crate) glyphs: Option<GlyphRamp>,
    /// Discharge shape, for decoration whose whole point is that it is not
    /// smooth. Overrides [`Paint::glyphs`] on the cells it reaches.
    pub(crate) crackle: Option<Crackle>,
}

impl Paint {
    const fn tint(ink: Ink, depth: f32) -> Self {
        Self {
            fg: Some(ink),
            bg: None,
            depth,
            reveal: false,
            attrs_above: None,
            glyphs: None,
            crackle: None,
        }
    }

    const fn reveal() -> Self {
        Self {
            fg: None,
            bg: None,
            depth: 1.0,
            reveal: true,
            attrs_above: None,
            glyphs: None,
            crackle: None,
        }
    }

    /// Touches nothing: no ink, no coverage, no attributes, no glyph.
    ///
    /// Every cell comes back [`CellPaint::is_settled`], so this is how a
    /// behaviour can exist purely to give an element a *phase* — see
    /// [`names::STILL`].
    const fn inert() -> Self {
        Self {
            fg: None,
            bg: None,
            depth: 0.0,
            reveal: false,
            attrs_above: None,
            glyphs: None,
            crackle: None,
        }
    }

    /// A travelling mark: it inks the foreground toward `ink` and walks the
    /// horizontal block ramp, so where it has reached is legible to a fraction
    /// of a cell rather than only to the cell.
    const fn charge(ink: Ink, depth: f32, floor: f32) -> Self {
        Self {
            fg: Some(ink),
            bg: None,
            depth,
            reveal: false,
            attrs_above: None,
            glyphs: Some(GlyphRamp {
                steps: &CHARGE_BLOCKS,
                floor,
            }),
            crackle: None,
        }
    }
}

/// Where a behaviour's strength or rate comes from.
///
/// A live signal scales a behaviour; it never replaces one. That separation is
/// what lets the same catalogue entry be used with a metric bound or without,
/// and it keeps the metric's own smoothing the sampler's business.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Drive {
    /// A constant. `Fixed(1.0)` is the behaviour's natural strength or rate.
    Fixed(f32),
    /// The element's live work-volume level, mapped onto `at_rest..=at_full`.
    ///
    /// `at_rest` is deliberately settable above zero: an element that is quiet
    /// is not an element that should vanish, and a floor is how a behaviour
    /// says "still visible, just calm".
    Activity { at_rest: f32, at_full: f32 },
}

impl Drive {
    pub(crate) fn value(self, inputs: DriveInputs) -> f32 {
        match self {
            Self::Fixed(value) => value,
            Self::Activity { at_rest, at_full } => {
                at_rest + (at_full - at_rest) * inputs.activity.clamp(0.0, 1.0)
            }
        }
    }

    /// True when this drive reads a live signal, so a caller knows whether the
    /// element's frame can change without the clock moving.
    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Activity { .. })
    }
}

/// The live signals a behaviour's drives can read.
///
/// One struct rather than separate arguments so a new signal is a new field
/// here and a default at every call site that does not have it yet, rather than
/// a signature change through the whole render pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct DriveInputs {
    /// How hard the thing behind this element is working, in `0.0..=1.0`.
    /// Sourced from [`crate::app::pane_activity`]; `0.0` for anything with no
    /// work-volume signal of its own.
    pub(crate) activity: f32,
}

/// One named behaviour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Behaviour {
    pub(crate) field: Field,
    pub(crate) shape: Shape,
    pub(crate) curve: Curve,
    pub(crate) paint: Paint,
    /// How long one loop of the unbounded idle phase takes at rate `1.0`.
    pub(crate) period: Duration,
    /// Shortest spacing between two frames anyone could tell apart.
    pub(crate) frame_interval: Duration,
    /// Scales [`Paint::depth`].
    pub(crate) depth_drive: Drive,
    /// Multiplies the loop rate. `Fixed(1.0)` is the stated period.
    pub(crate) rate_drive: Drive,
}

impl Behaviour {
    /// True when every cell resolves identically, so a caller may resolve once
    /// and style a whole span with the result.
    ///
    /// This is not an optimisation detail a caller may ignore: it is what keeps
    /// an unswept behaviour costing exactly one span, the way the sidebar draws
    /// today.
    pub(crate) fn is_uniform(&self) -> bool {
        self.field == Field::Uniform
    }

    /// True when this behaviour can change what it draws without the clock
    /// moving, because one of its drives reads a live signal.
    pub(crate) fn is_metric_reactive(&self) -> bool {
        self.depth_drive.is_live() || self.rate_drive.is_live()
    }

    /// How far through its loop this behaviour is after `cycles` whole turns.
    ///
    /// Takes accumulated turns rather than elapsed time on purpose: a rate that
    /// changes must bend the loop, not jump it, and only an accumulated phase
    /// can do that. See [`super::Animator`], which does the accumulating.
    fn loop_progress(cycles: f32) -> f32 {
        cycles.rem_euclid(1.0)
    }

    /// The amount at one cell, in `0.0..=1.0`.
    fn amount(&self, pos: CellPos, extent: CellExtent, progress: f32) -> f32 {
        if self.field == Field::Uniform {
            return self.curve.apply(progress);
        }
        let field = self.field.value(pos, extent);
        match self.shape {
            Shape::Front { softness } => {
                // Pinned at the ends rather than left to the arithmetic: a
                // front that has finished covers everything, and one that has
                // not started covers nothing. Without this the last cell of a
                // wipe lands a rounding step short of arrived and stays
                // fractionally dimmed forever.
                let shaped = self.curve.apply(progress);
                if shaped >= 1.0 {
                    return 1.0;
                }
                if shaped <= 0.0 {
                    return 0.0;
                }
                let softness = softness.max(1e-3);
                let edge = shaped * (1.0 + softness);
                ((edge - field) / softness).clamp(0.0, 1.0)
            }
            Shape::Band { width } => {
                let width = width.clamp(1e-3, 2.0);
                let half = width / 2.0;
                let centre = self.curve.apply(progress) * (1.0 + width) - half;
                (1.0 - (centre - field).abs() / half).clamp(0.0, 1.0)
            }
            Shape::Phase { spread } => self
                .curve
                .apply((progress + field * spread).rem_euclid(1.0)),
            Shape::Comet {
                head_cells,
                tail_cells,
            } => {
                let span = self.field.span(extent);
                let head = (head_cells / span).clamp(1e-3, 2.0);
                let tail = (tail_cells / span).clamp(1e-3, 2.0);
                // One head-length before the first cell to one tail-length past
                // the last: the offsets are the two lengths that actually reach
                // back into the field, so both ends of the travel are genuinely
                // dark and a charge neither pops into existence nor vanishes
                // still lit.
                let peak = self.curve.apply(progress) * (1.0 + head + tail) - head;
                let behind = peak - field;
                if behind >= 0.0 {
                    (1.0 - behind / tail).clamp(0.0, 1.0)
                } else {
                    (1.0 + behind / head).clamp(0.0, 1.0)
                }
            }
        }
    }

    /// How strongly this behaviour reaches one cell, before that becomes
    /// colour, coverage, or a glyph.
    ///
    /// For a call site whose own drawing is not a patch over what the settled
    /// pass produced — a status icon that has to keep meaning what it means and
    /// therefore blends toward *its own* colour rather than the behaviour's.
    /// Takes the same progress [`Behaviour::cell`] does, and normalises it the
    /// same way, so the two can never disagree about where the effect is.
    ///
    /// Deliberately not the crackled amount: a discharge's jitter belongs to
    /// the mark it draws, and a caller asking for the envelope wants the
    /// envelope.
    pub(crate) fn strength(&self, pos: CellPos, extent: CellExtent, progress: f32) -> f32 {
        self.amount(pos, extent, Self::normalized(progress))
    }

    /// Progress folded into `0.0..=1.0`, whether it arrived as a bounded phase
    /// or as an unbounded loop's accumulated turns.
    fn normalized(progress: f32) -> f32 {
        if (0.0..=1.0).contains(&progress) {
            progress
        } else {
            Self::loop_progress(progress)
        }
    }

    /// What this behaviour does to one cell, this frame.
    ///
    /// `progress` is `0.0..=1.0` through a bounded phase, or the accumulated
    /// turn count of an unbounded one — both are handled, because a looping
    /// behaviour takes the fractional part and a bounded one is already in
    /// range.
    pub(crate) fn cell(
        &self,
        pos: CellPos,
        extent: CellExtent,
        progress: f32,
        inputs: DriveInputs,
        palette: InkPalette,
    ) -> CellPaint {
        let progress = Self::normalized(progress);
        let mut amount = self.amount(pos, extent, progress);
        let depth = (self.paint.depth * self.depth_drive.value(inputs)).clamp(0.0, 1.0);
        let mut paint = CellPaint::default();

        // Shape is resolved before colour so a discharge's own jitter reaches
        // the brightness too: a crackle that only changed glyphs would read as
        // a decoration marching over a smooth ramp.
        if let Some(crackle) = self.paint.crackle {
            if amount >= crackle.above {
                let roll = crackle.roll(pos, progress);
                paint.glyph = crackle.arc(roll);
                amount *= crackle.dim(roll);
            }
        }
        if paint.glyph.is_none() {
            if let Some(ramp) = self.paint.glyphs {
                paint.glyph = ramp.glyph(amount);
            }
        }

        // Clamped once, here, because [`Curve::SnapPendulum`] deliberately
        // overshoots past 1.0 and a mix fraction above one is not a brighter
        // colour, it is an extrapolation past the ink. The overshoot still
        // reaches a consumer that wants it, through
        // [`Behaviour::strength`], which is the un-clamped envelope.
        let mixed = (amount * depth).clamp(0.0, 1.0);
        if self.paint.reveal {
            // Depth scales how far a reveal gets rather than how bright it is:
            // a reveal driven to half strength arrives half-way and stays, which
            // is the honest reading of "this element is only half here".
            paint.coverage = mixed;
        }
        if let Some(ink) = self.paint.fg {
            paint.fg = Some(mix_rgb(palette.own, palette.ink(ink), mixed));
        }
        if let Some(ink) = self.paint.bg {
            paint.bg = Some(mix_rgb(palette.surface, palette.ink(ink), mixed));
        }
        if let Some((threshold, attrs)) = self.paint.attrs_above {
            if amount >= threshold {
                paint.attrs = attrs;
            }
        }
        paint
    }

    /// How long one loop takes at the rate these inputs imply.
    ///
    /// Never zero and never unbounded: a drive that resolves to nothing would
    /// otherwise either freeze the loop or spin it.
    pub(crate) fn effective_period(&self, inputs: DriveInputs) -> Duration {
        let rate = self.rate_drive.value(inputs).clamp(0.05, 20.0);
        self.period.div_f32(rate)
    }
}

/// Names every built-in behaviour answers to.
///
/// Public constants rather than bare strings at call sites so a rename is a
/// compile error rather than a silently dead animation.
pub(crate) mod names {
    /// Bounded: arrives everywhere at once.
    pub(crate) const FADE: &str = "fade";
    /// Bounded: arrives one cell at a time, left to right.
    pub(crate) const TYPEWRITER: &str = "typewriter";
    /// Bounded: a soft edge sweeps left to right.
    pub(crate) const WIPE: &str = "wipe";
    /// Bounded: arrives top row first.
    pub(crate) const DROP_IN: &str = "drop-in";
    /// Bounded: cells arrive in a stable scatter.
    pub(crate) const DISSOLVE: &str = "dissolve";
    /// Bounded: closes inward from the edges toward the centre.
    pub(crate) const COLLAPSE: &str = "collapse";
    /// Bounded: nothing at all, at a frame spacing fine enough to move on.
    ///
    /// Every cell resolves settled, so an element playing this looks exactly as
    /// it does at rest. It exists because a *phase* is worth having on its own:
    /// an element's mount and dismount are the only clock that says "this is
    /// arriving" and "this is leaving", and a caller can carry something other
    /// than a cell effect on it — the sidebar's row motion moves a card's
    /// placement on exactly this phase. Without it, asking for motion with no
    /// cell emphasis would mean no bounded phase at all, and nothing to move
    /// through.
    pub(crate) const STILL: &str = "still";
    /// Looping: the whole element breathes toward its surface and back.
    pub(crate) const PULSE: &str = "pulse";
    /// Looping: a bright band travels across the element.
    pub(crate) const SHIMMER: &str = "shimmer";
    /// Looping: a phase-shifted ripple along the element.
    pub(crate) const WAVE: &str = "wave";
    /// Looping, live: brightness and rate follow the element's work volume.
    pub(crate) const ACTIVITY: &str = "activity";
    /// Looping, live: a band whose speed follows the element's work volume.
    pub(crate) const ACTIVITY_SHIMMER: &str = "activity-shimmer";
    /// Bounded: a crackling charge running a connector, coloured by whatever
    /// [`super::Ink::Signal`] is bound to.
    pub(crate) const RELATION_CHARGE: &str = "relation-charge";
    /// Bounded: the same travel with no discharge — for a signal that means
    /// something stopped rather than something happening.
    pub(crate) const RELATION_DRIFT: &str = "relation-drift";
    /// Looping: a tray badge with nothing behind it, on the back burner.
    ///
    /// A slow, shallow breath and no travel at all. The absence of movement is
    /// the reading — a resting badge is *present*, not demanding.
    pub(crate) const BADGE_REST: &str = "badge-rest";
    /// Looping: a tray badge that is lit, snapping on the stated motion curve.
    pub(crate) const BADGE_CHARGE: &str = "badge-charge";
    /// Looping: a tray badge that has escalated — the same snap, faster and
    /// deeper, so the difference between lit and waiting is a rhythm rather
    /// than a hue.
    pub(crate) const BADGE_ALERT: &str = "badge-alert";
    /// Looping: a card on the back burner, breathing.
    ///
    /// Slow and shallow, and the swing goes *down* from the card's own light
    /// rather than up — see [`super::super::ui::sidebar::image_card`], where
    /// the envelope is read. A card with nothing behind it should read as set
    /// back into the panel, and a breath that brightened would be the card
    /// asking to be looked at.
    pub(crate) const CARD_REST: &str = "card-rest";
    /// Looping: a card with work behind it, breathing on the stated snap.
    ///
    /// The same ladder the tray badges use, and for the same reason: rest and
    /// work are told apart by *rhythm* before they are told apart by hue, so
    /// this one snaps where [`CARD_REST`] drifts, and takes its tempo from the
    /// pane's own work volume.
    pub(crate) const CARD_LIVE: &str = "card-live";
    /// Looping: a card with a serious problem on it, breathing escalated.
    ///
    /// The third rung of the card ladder, and the exact counterpart of
    /// [`BADGE_ALERT`]: the same snap as [`CARD_LIVE`], faster and deeper, so
    /// that a card in trouble is told apart from a card merely working by
    /// *rhythm*. That is what makes the severity channel survive a reader who
    /// cannot separate two hues — the light says how bad it is and the tempo
    /// says it again.
    ///
    /// It escalates over rest and over live alike. A card that has gone quiet
    /// with a serious problem on it is not resting.
    pub(crate) const CARD_ALERT: &str = "card-alert";
    /// Bounded: a card's state change washing left to right across it.
    ///
    /// A [`super::Field::Linear`] front rather than a band, which is the whole
    /// difference between this and [`SHIMMER`]. A band peaks as it passes a
    /// cell and leaves it as it was; a front leaves everything behind it at
    /// full amount, so when this has crossed, the whole card is in the state it
    /// changed into. The card is different afterwards, and it looks it.
    pub(crate) const CARD_WASH: &str = "card-wash";
}

/// How long one rest breath takes.
///
/// Deliberately far slower than either live badge. Rest is told apart from lit
/// by tempo before it is told apart by anything else, and a resting slot that
/// breathed at a lit slot's speed would just look like a dimmer version of it.
const BADGE_REST_PERIOD: Duration = Duration::from_millis(4_200);

/// How long one snap-and-settle takes on a lit badge.
const BADGE_CHARGE_PERIOD: Duration = Duration::from_millis(1_900);

/// How long one snap-and-settle takes on an escalated badge.
///
/// Under half the lit period, which is the gap at which two rhythms read as
/// two rhythms rather than as one that drifted.
const BADGE_ALERT_PERIOD: Duration = Duration::from_millis(760);

/// Frame spacing for a badge that is moving.
///
/// Every frame here re-rasterises the whole eight-badge layer, so this is the
/// tray's real cost dial and it is set from measurement rather than from taste:
/// at 50 ms the snap resolves as motion, and the layer's own raster stays a
/// small fraction of one frame's budget. See the module tests in
/// [`crate::ui::sidebar::tray_art`] for what one badge costs.
const BADGE_FRAME_INTERVAL: Duration = SMOOTH_FRAME_INTERVAL;

/// How deeply a resting badge's breath swings.
///
/// Shallow on purpose. This is the "dimmed and slightly recessed" reading, and
/// a rest that swung as far as a lit badge would be competing with it.
const BADGE_REST_DEPTH: f32 = 0.22;

/// How often a travelling charge needs a frame.
///
/// The single source of truth for the connector's resolution: the behaviour
/// declares it and [`crate::app::relation_signal`] sizes its own sub-cell steps
/// from it, so the clock that moves a charge and the clock that draws it cannot
/// disagree. Above the app's 16 ms render floor, and comfortably below the
/// ~40 ms at which a mark travelling three cells starts to read as stepping.
pub(crate) const CHARGE_FRAME_INTERVAL: Duration = Duration::from_millis(25);

/// The left-anchored eighth-block ramp: a charge entering a cell from the left.
///
/// Emptiest first, and deliberately *not* the vertical ramp
/// [`super::cell::CellPaint::coverage_block`] uses. Coverage asks "how much of
/// this cell is filled"; a travelling charge asks "how far into this cell has
/// it come", and on a horizontal connector only the horizontal ramp answers
/// that. A cell behind the peak walks the same ramp back down, which is what
/// makes the charge read as leaving to the right rather than as fading in place.
const CHARGE_BLOCKS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// Marks a discharge can take on a connector cell.
///
/// Crossings, a diagonal cross, and two bare forks — all from the box-drawing
/// family the connector itself is drawn in, so an arc reads as happening to the
/// line rather than as a foreign glyph parked on it. The forks are the ones
/// that sell it: a charge that only ever thickened the line would read as a
/// highlight, and a discharge is exactly the thing that breaks its own path.
const CHARGE_ARCS: [char; 8] = ['╪', '╫', '╬', '┼', '╳', '╱', '╲', '┿'];

/// How long one breath takes on a card with nothing behind it.
///
/// Slower than any other loop in the catalogue, deliberately. Rest is read as a
/// tempo first, and a whole tree of cards breathing at a badge's rate would be
/// a panel full of movement saying nothing — the visual-target spec's *"back
/// burner"* is the absence of demand, not a dimmer setting.
const CARD_REST_PERIOD: Duration = Duration::from_millis(5_200);

/// How long one snap-and-settle takes on a card with work behind it.
const CARD_LIVE_PERIOD: Duration = Duration::from_millis(2_400);

/// How far a resting card's breath swings.
///
/// Shallow, and the consumer subtracts it: at the trough a resting card sits
/// about a fifth below its own light with its bloom pulled further, which is
/// the *recessed* half of "dimmed or recessed slightly". A deeper swing at this
/// period reads as a fade rather than as breathing.
const CARD_REST_DEPTH: f32 = 0.20;

/// How far a live card's breath swings.
const CARD_LIVE_DEPTH: f32 = 0.55;

/// How long one snap-and-settle takes on a card with a serious problem on it.
///
/// Under half the live period, which is the same gap [`BADGE_ALERT_PERIOD`]
/// takes against [`BADGE_CHARGE_PERIOD`] and for the same measured reason: two
/// rhythms read as two rhythms rather than as one that drifted only once they
/// are better than twice apart.
///
/// Measured against the *fastest* a live card can go, not against its stated
/// period. A working card drives its own breath to 1.7× through
/// [`Drive::Activity`], so live spans 2,400 ms down to 1,412 ms — and an alert
/// at half the stated period would sit inside that span and read as a card
/// merely working hard. Half of 1,412 is where the escalation is unambiguous,
/// which lands within a breath of [`BADGE_ALERT_PERIOD`]'s own 760 ms.
const CARD_ALERT_PERIOD: Duration = Duration::from_millis(680);

/// How far an escalated card's breath swings.
///
/// Deeper than live, and the deepest in the catalogue. The consumer subtracts
/// the swing, so this is a card that settles further back into the panel and
/// comes further forward again — the same motion at greater amplitude, not a
/// different one. Short of a full swing because the card has to stay legible at
/// the trough; a card whose text disappeared periodically would be a card you
/// cannot read the problem off.
const CARD_ALERT_DEPTH: f32 = 0.80;

/// Frame spacing for a card that is moving.
///
/// Every frame at this tier re-rasterises the cards whose quantised envelope
/// moved, so it is the card path's real cost dial. The smooth tier rather than
/// the charge tier: a card's breath and its wash are both changes of *light*
/// over a whole shape, and neither has an edge fine enough for 25 ms to resolve
/// something 50 ms does not. See [`CARD_BREATH_STEPS`] in
/// [`crate::ui::sidebar::image_card`], which is the other half of that dial.
///
/// [`CARD_BREATH_STEPS`]: crate::ui::sidebar::image_card
const CARD_FRAME_INTERVAL: Duration = SMOOTH_FRAME_INTERVAL;

/// How long a state wash takes to cross a card.
///
/// Long enough that the snap is a movement across the card rather than a
/// flash, short enough that the card has arrived in its new state before a
/// reader's eye has finished travelling to it.
pub(crate) const CARD_WASH_PERIOD: Duration = Duration::from_millis(520);

/// How much of the card the wash's leading edge takes to go from nothing to
/// full, as a fraction of its width.
///
/// Soft rather than a hard step: the wash is light arriving, and light does not
/// have a straight edge. At a fifth of the card the front is visibly a front
/// and still crosses in one read.
const CARD_WASH_SOFTNESS: f32 = 0.22;

/// Blend fraction toward the surface at the dimmest point of a pulse.
///
/// Deliberately partial: the element stays readable at its trough. This is the
/// value the sidebar's own hand-rolled pulse used before the engine existed,
/// kept exactly so the same config keeps producing the same ramp.
const PULSE_DEPTH: f32 = 0.6;

/// The named behaviours Herdr ships with.
///
/// Lookup is by name so a consumer — config, the API, another subsystem — can
/// ask for one without depending on this module's types, and registration is
/// open so a subsystem can add its own without editing this table.
#[derive(Debug, Clone)]
pub(crate) struct Catalogue {
    behaviours: HashMap<String, Behaviour>,
}

impl Default for Catalogue {
    fn default() -> Self {
        Self::built_in()
    }
}

impl Catalogue {
    pub(crate) fn built_in() -> Self {
        let mut catalogue = Self {
            behaviours: HashMap::new(),
        };
        for (name, behaviour) in built_in_behaviours() {
            catalogue.register(name, behaviour);
        }
        catalogue
    }

    /// Add or replace a named behaviour.
    ///
    /// Returns the behaviour that was displaced, if any, so a caller
    /// deliberately overriding a built-in can say so and a caller that did not
    /// mean to can notice.
    pub(crate) fn register(
        &mut self,
        name: impl Into<String>,
        behaviour: Behaviour,
    ) -> Option<Behaviour> {
        self.behaviours.insert(name.into(), behaviour)
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Behaviour> {
        self.behaviours.get(name)
    }

    /// Every registered name, sorted, for diagnostics and for the API.
    pub(crate) fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.behaviours.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

/// How far ahead of its peak a connector charge rises, in cells.
///
/// Exactly one, and that is not a taste setting: a one-cell rise is what makes
/// a single cell's amount equal to how far into that cell the charge has come,
/// which is the whole mechanism [`CHARGE_BLOCKS`] then resolves into eighths.
/// Widen it and two cells go fractional at once, and the ramp stops meaning a
/// position.
const CHARGE_HEAD_CELLS: f32 = 1.0;

/// Amount at which a charge stops being a smooth mark and starts discharging.
///
/// High enough that only the cell the peak is actually on arcs, so the crackle
/// reads as the charge's core rather than as the whole connector shaking.
const CHARGE_ARC_ABOVE: f32 = 0.72;

fn built_in_behaviours() -> [(&'static str, Behaviour); 21] {
    /// Every built-in starts from this and overrides what it means to change,
    /// so a new entry inherits the cheap frame interval and the fixed drives
    /// rather than having to remember them.
    const BASE: Behaviour = Behaviour {
        field: Field::Uniform,
        shape: Shape::Front { softness: 0.25 },
        curve: Curve::Linear,
        paint: Paint::reveal(),
        period: DEFAULT_PERIOD,
        frame_interval: DEFAULT_FRAME_INTERVAL,
        depth_drive: Drive::Fixed(1.0),
        rate_drive: Drive::Fixed(1.0),
    };
    const HORIZONTAL: Field = Field::Linear {
        axis: Axis::Horizontal,
        reverse: false,
    };

    [
        (
            names::FADE,
            Behaviour {
                curve: Curve::EaseOut,
                ..BASE
            },
        ),
        (
            names::TYPEWRITER,
            Behaviour {
                field: HORIZONTAL,
                // A hard edge: one cell is typed, the next is not. Softening it
                // would be a wipe, which is the separate entry below.
                shape: Shape::Front { softness: 0.0 },
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::WIPE,
            Behaviour {
                field: HORIZONTAL,
                shape: Shape::Front { softness: 0.3 },
                curve: Curve::EaseInOut,
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::DROP_IN,
            Behaviour {
                field: Field::Linear {
                    axis: Axis::Vertical,
                    reverse: false,
                },
                shape: Shape::Front { softness: 0.45 },
                curve: Curve::EaseOut,
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::DISSOLVE,
            Behaviour {
                field: Field::Scatter { seed: 0x5EED },
                shape: Shape::Front { softness: 0.2 },
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::COLLAPSE,
            Behaviour {
                field: Field::Radial { inward: true },
                shape: Shape::Front { softness: 0.2 },
                curve: Curve::EaseInOut,
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::STILL,
            Behaviour {
                paint: Paint::inert(),
                // It draws nothing, so this is not about what a cell shows: it
                // is the spacing at which whatever *else* rides this phase gets
                // a frame. The sidebar's row motion rides it, and motion is the
                // one thing a coarse step reads as judder rather than as calm.
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::PULSE,
            Behaviour {
                curve: Curve::Triangle,
                paint: Paint::tint(Ink::Surface, PULSE_DEPTH),
                ..BASE
            },
        ),
        (
            names::SHIMMER,
            Behaviour {
                field: HORIZONTAL,
                shape: Shape::Band { width: 0.45 },
                paint: Paint::tint(Ink::Accent, 0.85),
                period: Duration::from_millis(1_400),
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::WAVE,
            Behaviour {
                field: HORIZONTAL,
                shape: Shape::Phase { spread: 1.0 },
                curve: Curve::Sine,
                paint: Paint::tint(Ink::Surface, 0.5),
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::ACTIVITY,
            Behaviour {
                curve: Curve::Sine,
                paint: Paint::tint(Ink::Accent, 1.0),
                // A quiet element still shows a trace of its own colour, so the
                // absence of work reads as calm rather than as a broken row.
                depth_drive: Drive::Activity {
                    at_rest: 0.08,
                    at_full: 0.75,
                },
                // And a busy one breathes faster. Rate is where work volume is
                // most legible at a glance: brightness alone reads as a theme
                // change, brightness plus tempo reads as effort.
                rate_drive: Drive::Activity {
                    at_rest: 0.5,
                    at_full: 3.0,
                },
                frame_interval: SMOOTH_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::ACTIVITY_SHIMMER,
            Behaviour {
                field: HORIZONTAL,
                shape: Shape::Band { width: 0.5 },
                paint: Paint::tint(Ink::Accent, 0.9),
                period: Duration::from_millis(1_600),
                frame_interval: SMOOTH_FRAME_INTERVAL,
                depth_drive: Drive::Activity {
                    at_rest: 0.15,
                    at_full: 1.0,
                },
                rate_drive: Drive::Activity {
                    at_rest: 0.4,
                    at_full: 3.5,
                },
                ..BASE
            },
        ),
        (
            names::RELATION_CHARGE,
            Behaviour {
                field: HORIZONTAL,
                // A long tail behind a one-cell rise: the charge reads as
                // having come from the trunk rather than as having appeared.
                shape: Shape::Comet {
                    head_cells: CHARGE_HEAD_CELLS,
                    tail_cells: 2.25,
                },
                paint: Paint {
                    attrs_above: Some((CHARGE_ARC_ABOVE, AttrPatch::bold())),
                    crackle: Some(Crackle {
                        arcs: &CHARGE_ARCS,
                        above: CHARGE_ARC_ABOVE,
                        // Roughly every other frame of an 800 ms travel: fast
                        // enough to crackle, slow enough that a shape is held
                        // long enough to be seen as a shape.
                        flicker: 0.055,
                        jitter: 0.45,
                        seed: 0xA12C,
                    }),
                    ..Paint::charge(Ink::Signal, 1.0, 0.12)
                },
                frame_interval: CHARGE_FRAME_INTERVAL,
                ..BASE
            },
        ),
        (
            names::RELATION_DRIFT,
            Behaviour {
                field: HORIZONTAL,
                // The same route with a longer tail and no discharge. A signal
                // that means "this branch went quiet" must not be the loudest
                // thing on the panel, so the vocabulary separates it by motion
                // as well as by colour.
                shape: Shape::Comet {
                    head_cells: CHARGE_HEAD_CELLS,
                    tail_cells: 3.3,
                },
                curve: Curve::EaseInOut,
                paint: Paint::charge(Ink::Signal, 0.75, 0.2),
                frame_interval: CHARGE_FRAME_INTERVAL,
                ..BASE
            },
        ),
        // The three tray badges. All uniform, because a badge is one object
        // rather than a span of cells: the amount is the whole mark's, and the
        // pixel path in `tray_art` reads it as an envelope through
        // `Behaviour::strength` rather than as a cell paint. What separates the
        // three is *rhythm* — period, curve and depth — which is the one axis
        // that still reads for someone who cannot tell the hues apart.
        (
            names::BADGE_REST,
            Behaviour {
                curve: Curve::Sine,
                paint: Paint::tint(Ink::Surface, BADGE_REST_DEPTH),
                period: BADGE_REST_PERIOD,
                // The slow tier, not the smooth one. A four-second breath has
                // nothing a 50 ms step would show that a 100 ms step does not,
                // and eight resting badges are the tray's common case.
                ..BASE
            },
        ),
        (
            names::BADGE_CHARGE,
            Behaviour {
                curve: Curve::SnapPendulum,
                paint: Paint::tint(Ink::Accent, 1.0),
                period: BADGE_CHARGE_PERIOD,
                frame_interval: BADGE_FRAME_INTERVAL,
                // A working fleet snaps its badges faster. The same reasoning
                // the `activity` entry gives: tempo is where effort is legible.
                rate_drive: Drive::Activity {
                    at_rest: 1.0,
                    at_full: 1.6,
                },
                ..BASE
            },
        ),
        (
            names::BADGE_ALERT,
            Behaviour {
                curve: Curve::SnapPendulum,
                paint: Paint::tint(Ink::Accent, 1.0),
                period: BADGE_ALERT_PERIOD,
                frame_interval: BADGE_FRAME_INTERVAL,
                ..BASE
            },
        ),
        // The two card breaths and the wash between them. The breaths are
        // uniform for the same reason the badges are — a card is one object,
        // and the pixel path in `image_card` reads them as an envelope through
        // `Behaviour::strength` rather than as a cell paint. The wash is the one
        // that is not: it is a field sweep, and it is the only thing here whose
        // amount has to differ column by column.
        (
            names::CARD_REST,
            Behaviour {
                curve: Curve::Sine,
                paint: Paint::tint(Ink::Surface, CARD_REST_DEPTH),
                period: CARD_REST_PERIOD,
                // The slow tier. A five-second breath has nothing at 50 ms that
                // it does not have at 100, and a resting card is the tree's
                // common case — every card in a quiet fleet is one.
                ..BASE
            },
        ),
        (
            names::CARD_LIVE,
            Behaviour {
                curve: Curve::SnapPendulum,
                paint: Paint::tint(Ink::Accent, CARD_LIVE_DEPTH),
                period: CARD_LIVE_PERIOD,
                frame_interval: CARD_FRAME_INTERVAL,
                // A harder-working pane breathes faster, the same way the
                // `activity` entry and a lit badge do: tempo is where effort is
                // legible at a glance, and brightness alone reads as a theme.
                rate_drive: Drive::Activity {
                    at_rest: 1.0,
                    at_full: 1.7,
                },
                ..BASE
            },
        ),
        (
            names::CARD_ALERT,
            Behaviour {
                curve: Curve::SnapPendulum,
                paint: Paint::tint(Ink::Signal, CARD_ALERT_DEPTH),
                period: CARD_ALERT_PERIOD,
                frame_interval: CARD_FRAME_INTERVAL,
                // No activity drive, and that is deliberate. The live breath's
                // tempo says how hard the pane is working; this one's says how
                // much trouble it is in, and a card in trouble that had gone
                // quiet would slow down exactly when it should not.
                ..BASE
            },
        ),
        (
            names::CARD_WASH,
            Behaviour {
                field: HORIZONTAL,
                shape: Shape::Front {
                    softness: CARD_WASH_SOFTNESS,
                },
                // The arrival, not the loop: this plays once, on a state
                // change, and has to end with the card tainted rather than back
                // where it started.
                curve: Curve::SnapArrival,
                paint: Paint::tint(Ink::Accent, 1.0),
                period: CARD_WASH_PERIOD,
                frame_interval: CARD_FRAME_INTERVAL,
                ..BASE
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: InkPalette = InkPalette {
        surface: (0, 0, 0),
        own: (200, 200, 200),
        accent: (0, 0, 255),
        signal: (0, 0, 255),
    };

    fn get(name: &str) -> Behaviour {
        *Catalogue::built_in()
            .get(name)
            .unwrap_or_else(|| panic!("{name} is a built-in"))
    }

    fn coverage(behaviour: &Behaviour, extent: CellExtent, progress: f32) -> Vec<f32> {
        (0..extent.cols)
            .map(|col| {
                behaviour
                    .cell(
                        CellPos::col(col),
                        extent,
                        progress,
                        DriveInputs::default(),
                        PALETTE,
                    )
                    .coverage
            })
            .collect()
    }

    #[test]
    fn every_built_in_starts_from_nothing_and_ends_settled() {
        let catalogue = Catalogue::built_in();
        for name in catalogue.names() {
            let behaviour = catalogue.get(name).expect("listed");
            let extent = CellExtent::new(8, 3);
            let at_rest = behaviour.cell(
                CellPos::new(0, 0),
                extent,
                0.0,
                DriveInputs::default(),
                PALETTE,
            );
            // At the start of its span a behaviour has done nothing yet: an
            // emphasis draws the element exactly as it already was, and a
            // reveal has not revealed anything. Either way, arming one can
            // never make an element jump.
            let untouched = at_rest.is_settled() || at_rest.fg == Some(PALETTE.own);
            assert!(
                untouched || at_rest.coverage == 0.0,
                "{name} does not start from rest: {at_rest:?}"
            );

            // And every cell it ever produces stays inside the published range.
            for step in 0..=10 {
                for col in 0..extent.cols {
                    for row in 0..extent.rows {
                        let paint = behaviour.cell(
                            CellPos::new(col, row),
                            extent,
                            step as f32 / 10.0,
                            DriveInputs { activity: 0.5 },
                            PALETTE,
                        );
                        assert!(
                            (0.0..=1.0).contains(&paint.coverage),
                            "{name} left the coverage range: {paint:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_bounded_reveal_finishes_covering_every_cell() {
        for name in [
            names::FADE,
            names::TYPEWRITER,
            names::WIPE,
            names::DROP_IN,
            names::DISSOLVE,
            names::COLLAPSE,
        ] {
            let behaviour = get(name);
            let extent = CellExtent::new(12, 4);
            for col in 0..extent.cols {
                for row in 0..extent.rows {
                    let paint = behaviour.cell(
                        CellPos::new(col, row),
                        extent,
                        1.0,
                        DriveInputs::default(),
                        PALETTE,
                    );
                    assert_eq!(
                        paint.coverage, 1.0,
                        "{name} left cell ({col},{row}) behind at full progress"
                    );
                }
            }
        }
    }

    #[test]
    fn a_typewriter_is_a_hard_edge_and_a_wipe_is_a_soft_one() {
        let extent = CellExtent::row(8);
        let typed = coverage(&get(names::TYPEWRITER), extent, 0.5);
        // Every cell is either typed or not; nothing is half-typed.
        assert!(
            typed.iter().all(|value| *value == 0.0 || *value == 1.0),
            "a typewriter must not half-type a cell: {typed:?}"
        );
        assert!(typed[0] == 1.0 && typed[7] == 0.0, "{typed:?}");

        let wiped = coverage(&get(names::WIPE), extent, 0.5);
        assert!(
            wiped.iter().any(|value| *value > 0.0 && *value < 1.0),
            "a wipe must have a soft edge: {wiped:?}"
        );
        // Both still travel the same direction.
        assert!(wiped[0] >= wiped[7], "{wiped:?}");
    }

    #[test]
    fn a_drop_in_arrives_by_row_and_a_wipe_by_column() {
        let extent = CellExtent::new(6, 4);
        let drop = get(names::DROP_IN);
        let top = drop
            .cell(
                CellPos::new(3, 0),
                extent,
                0.35,
                DriveInputs::default(),
                PALETTE,
            )
            .coverage;
        let bottom = drop
            .cell(
                CellPos::new(3, 3),
                extent,
                0.35,
                DriveInputs::default(),
                PALETTE,
            )
            .coverage;
        assert!(top > bottom, "top row should land first: {top} vs {bottom}");
        // And columns within a row are indistinguishable, which is what makes
        // it a drop rather than a diagonal.
        assert_eq!(
            drop.cell(
                CellPos::new(0, 1),
                extent,
                0.35,
                DriveInputs::default(),
                PALETTE
            )
            .coverage,
            drop.cell(
                CellPos::new(5, 1),
                extent,
                0.35,
                DriveInputs::default(),
                PALETTE
            )
            .coverage,
        );
    }

    #[test]
    fn a_dissolve_is_scattered_but_never_reshuffles() {
        let behaviour = get(names::DISSOLVE);
        let extent = CellExtent::new(16, 2);
        let first = coverage(&behaviour, extent, 0.5);
        let again = coverage(&behaviour, extent, 0.5);
        assert_eq!(first, again, "a dissolve must be stable per cell");
        assert!(
            first.windows(2).any(|pair| pair[0] < pair[1]),
            "a dissolve must not arrive in reading order: {first:?}"
        );
        // Coverage only ever grows with progress, so a cell never un-dissolves.
        let later = coverage(&behaviour, extent, 0.7);
        assert!(first.iter().zip(&later).all(|(early, late)| late >= early));
    }

    #[test]
    fn a_collapse_closes_from_the_edges_inward() {
        let behaviour = get(names::COLLAPSE);
        let extent = CellExtent::new(9, 5);
        let edge = behaviour
            .cell(
                CellPos::new(0, 0),
                extent,
                0.4,
                DriveInputs::default(),
                PALETTE,
            )
            .coverage;
        let centre = behaviour
            .cell(
                CellPos::new(4, 2),
                extent,
                0.4,
                DriveInputs::default(),
                PALETTE,
            )
            .coverage;
        assert!(
            edge > centre,
            "the edge should close before the centre: {edge} vs {centre}"
        );
    }

    #[test]
    fn a_shimmer_band_passes_a_cell_rather_than_staying_on_it() {
        let behaviour = get(names::SHIMMER);
        let extent = CellExtent::row(10);
        let cell = CellPos::col(2);
        let track: Vec<f32> = (0..=10)
            .map(|step| {
                let paint = behaviour.cell(
                    cell,
                    extent,
                    step as f32 / 10.0,
                    DriveInputs::default(),
                    PALETTE,
                );
                // The band's strength reads as how far the foreground moved.
                let fg = paint.fg.expect("a shimmer tints");
                (fg.2 as f32) / 255.0
            })
            .collect();
        let peak = track.iter().copied().fold(f32::MIN, f32::max);
        assert!(peak > 0.5, "the band should reach this cell: {track:?}");
        assert!(
            *track.last().expect("track") < peak,
            "the band should leave again: {track:?}"
        );
    }

    #[test]
    fn a_wave_is_the_same_curve_offset_per_cell() {
        let behaviour = get(names::WAVE);
        let extent = CellExtent::row(9);
        let first = behaviour
            .cell(
                CellPos::col(0),
                extent,
                0.0,
                DriveInputs::default(),
                PALETTE,
            )
            .fg;
        // `spread: 1.0` means the last cell is exactly one whole cycle behind
        // the first, so they agree — that is what makes the ripple continuous
        // rather than showing a seam.
        let last = behaviour
            .cell(
                CellPos::col(8),
                extent,
                0.0,
                DriveInputs::default(),
                PALETTE,
            )
            .fg;
        assert_eq!(first, last);
        // And a cell part-way along is genuinely somewhere else in the cycle.
        let middle = behaviour
            .cell(
                CellPos::col(4),
                extent,
                0.0,
                DriveInputs::default(),
                PALETTE,
            )
            .fg;
        assert_ne!(first, middle);
    }

    #[test]
    fn the_pulse_ramp_matches_the_hand_rolled_one_it_replaces() {
        // The sidebar drew this pulse itself before the engine existed: a
        // triangle over sixteen 100ms frames, ramping the foreground toward the
        // panel background by up to 0.6 and back. Same config, same ramp.
        const HALF_CYCLE_FRAMES: u32 = 8;
        fn legacy_fade(tick: u32) -> f32 {
            let period = HALF_CYCLE_FRAMES * 2;
            let phase = tick % period;
            let distance_from_peak = phase.min(period - phase);
            PULSE_DEPTH * distance_from_peak as f32 / HALF_CYCLE_FRAMES as f32
        }

        let behaviour = get(names::PULSE);
        assert_eq!(
            behaviour.period,
            crate::app::ANIMATION_INTERVAL * HALF_CYCLE_FRAMES * 2,
            "the engine's period must be the sixteen frames the sidebar used"
        );
        for tick in 0..(HALF_CYCLE_FRAMES * 2) {
            let progress = tick as f32 / (HALF_CYCLE_FRAMES * 2) as f32;
            let paint = behaviour.cell(
                CellPos::col(0),
                CellExtent::row(4),
                progress,
                DriveInputs::default(),
                PALETTE,
            );
            let expected = mix_rgb(PALETTE.own, PALETTE.surface, legacy_fade(tick));
            assert_eq!(paint.fg, Some(expected), "tick {tick} drifted");
        }
    }

    #[test]
    fn a_pulse_reaches_every_cell_of_a_span_equally() {
        let behaviour = get(names::PULSE);
        assert!(behaviour.is_uniform());
        let extent = CellExtent::row(6);
        let first = behaviour.cell(
            CellPos::col(0),
            extent,
            0.5,
            DriveInputs::default(),
            PALETTE,
        );
        for col in 1..extent.cols {
            assert_eq!(
                behaviour.cell(
                    CellPos::col(col),
                    extent,
                    0.5,
                    DriveInputs::default(),
                    PALETTE
                ),
                first,
                "a uniform behaviour must let a caller resolve one span once"
            );
        }
    }

    #[test]
    fn work_volume_moves_both_the_brightness_and_the_tempo() {
        let behaviour = get(names::ACTIVITY);
        assert!(behaviour.is_metric_reactive());

        let quiet = DriveInputs { activity: 0.0 };
        let busy = DriveInputs { activity: 1.0 };
        let extent = CellExtent::row(4);
        let at = |inputs| {
            behaviour
                .cell(CellPos::col(0), extent, 0.5, inputs, PALETTE)
                .fg
                .expect("a tint")
        };
        // Busier means further toward the accent, which here is the blue axis.
        assert!(
            at(busy).2 > at(quiet).2,
            "work volume must change the brightness: {:?} vs {:?}",
            at(quiet),
            at(busy)
        );
        // A quiet element is still visible, not switched off.
        assert_ne!(at(quiet), PALETTE.own);
        // And busier means faster.
        assert!(behaviour.effective_period(busy) < behaviour.effective_period(quiet));
    }

    #[test]
    fn a_drive_can_never_freeze_or_spin_the_loop() {
        let mut behaviour = get(names::ACTIVITY);
        let natural = behaviour.period;

        // A drive that resolves to nothing must not stop the loop dead.
        behaviour.rate_drive = Drive::Fixed(0.0);
        let slowest = behaviour.effective_period(DriveInputs::default());
        assert!(slowest > natural, "a zero drive should slow, not freeze");
        assert!(slowest <= natural * 21, "and not stop: {slowest:?}");

        // And one that resolves absurdly high must not turn into a spin.
        behaviour.rate_drive = Drive::Fixed(1_000.0);
        let fastest = behaviour.effective_period(DriveInputs::default());
        assert!(fastest < natural);
        assert!(
            fastest >= natural / 21,
            "an absurd drive must stay bounded: {fastest:?}"
        );
    }

    #[test]
    fn a_looping_progress_past_one_wraps_instead_of_pinning() {
        let behaviour = get(names::PULSE);
        let extent = CellExtent::row(2);
        let at = |progress| {
            behaviour
                .cell(
                    CellPos::col(0),
                    extent,
                    progress,
                    DriveInputs::default(),
                    PALETTE,
                )
                .fg
        };
        assert_eq!(at(0.25), at(3.25));
        assert_eq!(at(0.75), at(-0.25));
    }

    #[test]
    fn registration_replaces_by_name_and_reports_what_it_displaced() {
        let mut catalogue = Catalogue::built_in();
        let before = catalogue.names().len();
        let custom = Behaviour {
            curve: Curve::Linear,
            ..get(names::PULSE)
        };
        assert!(
            catalogue.register("notification-arrive", custom).is_none(),
            "a fresh name displaces nothing"
        );
        assert_eq!(catalogue.names().len(), before + 1);
        assert_eq!(catalogue.get("notification-arrive"), Some(&custom));
        // Overriding a built-in is allowed but never silent.
        assert!(catalogue.register(names::PULSE, custom).is_some());
        assert_eq!(catalogue.names().len(), before + 1);
    }

    #[test]
    fn an_unknown_name_resolves_to_nothing_rather_than_to_a_default() {
        assert_eq!(Catalogue::built_in().get("no-such-behaviour"), None);
    }

    #[test]
    fn every_glyph_a_behaviour_can_ask_for_is_one_column_wide() {
        // A wrong-width substitution is refused at draw time rather than
        // corrupting a row, so getting this wrong would not break a layout — it
        // would silently never draw. That is worse to debug, not better.
        let catalogue = Catalogue::built_in();
        for name in catalogue.names() {
            let paint = catalogue.get(name).expect("listed").paint;
            let ramp = paint.glyphs.map(|ramp| ramp.steps).unwrap_or(&[]);
            let arcs = paint.crackle.map(|crackle| crackle.arcs).unwrap_or(&[]);
            for glyph in ramp.iter().chain(arcs) {
                assert_eq!(
                    unicode_width::UnicodeWidthChar::width(*glyph),
                    Some(1),
                    "{name} can ask for {glyph:?}, which is not one column wide"
                );
            }
        }
    }

    #[test]
    fn a_comet_resolves_a_position_finer_than_the_cell_it_is_in() {
        // The whole sub-cell claim, at the resolution the sidebar actually uses:
        // one cell of a four-cell connector has to show the charge at several
        // places inside itself, not merely lit or unlit.
        let behaviour = get(names::RELATION_CHARGE);
        let extent = CellExtent::row(4);
        let cell = CellPos::col(2);
        let mut steps: Vec<char> = (0..=64)
            .filter_map(|step| {
                behaviour
                    .cell(
                        cell,
                        extent,
                        step as f32 / 64.0,
                        DriveInputs::default(),
                        PALETTE,
                    )
                    .glyph
            })
            .filter(|glyph| CHARGE_BLOCKS.contains(glyph))
            .collect();
        let drawn = steps.clone();
        steps.sort_unstable();
        steps.dedup();
        assert!(
            steps.len() >= 4,
            "one cell showed the charge at only {} positions inside it: {drawn:?}",
            steps.len()
        );
    }

    #[test]
    fn a_comet_is_dark_at_both_ends_of_its_travel() {
        // A charge that were still lit when its signal expired would pop out of
        // existence; one lit at progress zero would pop into it.
        for name in [names::RELATION_CHARGE, names::RELATION_DRIFT] {
            let behaviour = get(name);
            let extent = CellExtent::row(4);
            for progress in [0.0, 1.0] {
                for col in 0..extent.cols {
                    let at = behaviour.strength(CellPos::col(col), extent, progress);
                    assert_eq!(
                        at, 0.0,
                        "{name} still lights cell {col} at progress {progress}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_crackle_holds_a_shape_for_a_few_frames_and_then_jumps() {
        let crackle = get(names::RELATION_CHARGE)
            .paint
            .crackle
            .expect("the charge discharges");
        let cell = CellPos::col(1);
        let at = |progress: f32| crackle.arc(crackle.roll(cell, progress));

        // Inside one flicker bucket the arc is stable. A shape re-rolled every
        // frame is static, not a discharge.
        assert_eq!(at(0.5), at(0.5 + crackle.flicker * 0.4));

        // Across buckets it genuinely takes different shapes.
        let shapes: Vec<Option<char>> = (0..12)
            .map(|bucket| at(0.5 + crackle.flicker * bucket as f32))
            .collect();
        let mut distinct = shapes.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() > 2,
            "a discharge has to change shape as it burns: {shapes:?}"
        );
    }

    #[test]
    fn a_crackle_reaches_only_the_core_of_the_charge() {
        // The fringe of the comet is what carries the sub-cell position, so a
        // discharge spreading into it would trade the smooth reading away.
        let behaviour = get(names::RELATION_CHARGE);
        let extent = CellExtent::row(4);
        for step in 0..=64 {
            let progress = step as f32 / 64.0;
            for col in 0..extent.cols {
                let cell = CellPos::col(col);
                let paint = behaviour.cell(cell, extent, progress, DriveInputs::default(), PALETTE);
                if paint
                    .glyph
                    .is_some_and(|glyph| CHARGE_ARCS.contains(&glyph))
                {
                    assert!(
                        behaviour.strength(cell, extent, progress) >= CHARGE_ARC_ABOVE,
                        "cell {col} arced at progress {progress} without being the charge's core"
                    );
                }
            }
        }
    }

    #[test]
    fn the_quiet_signal_travels_without_discharging() {
        // Colour separates the categories; motion separates urgency. Something
        // going quiet must not crackle like something happening.
        let drift = get(names::RELATION_DRIFT);
        assert!(drift.paint.crackle.is_none());
        assert!(drift.paint.glyphs.is_some(), "but it still moves sub-cell");

        let extent = CellExtent::row(4);
        for step in 0..=64 {
            for col in 0..extent.cols {
                let glyph = drift
                    .cell(
                        CellPos::col(col),
                        extent,
                        step as f32 / 64.0,
                        DriveInputs::default(),
                        PALETTE,
                    )
                    .glyph;
                assert!(
                    !glyph.is_some_and(|glyph| CHARGE_ARCS.contains(&glyph)),
                    "the quiet signal drew an arc"
                );
            }
        }
    }

    #[test]
    fn the_cheap_frame_interval_is_what_the_app_already_pays() {
        // A behaviour that does not ask for smoothness must not make the loop
        // wake more often than the sidebar pulse already does.
        assert_eq!(
            get(names::PULSE).frame_interval,
            crate::app::ANIMATION_INTERVAL
        );
        assert!(SMOOTH_FRAME_INTERVAL >= Duration::from_millis(16));
    }

    /// The stated motion character, asserted as the four things it claims:
    /// starts slow and accelerates, snaps to its target, overshoots by about a
    /// tenth, and swings back through about a twentieth.
    #[test]
    fn the_snap_accelerates_overshoots_and_pendulums_back() {
        let at = |p: f32| Curve::SnapPendulum.apply(p);

        // Exponential acceleration: half way through the ramp it has covered
        // far less than half the distance. A linear or ease-out curve fails
        // this, which is the point — the target rejected both.
        let half = at(SNAP_RISE / 2.0);
        assert!(
            half < 0.25,
            "the ramp covered {half:.3} of its distance in half its time — that is not an \
             acceleration"
        );
        assert!(at(SNAP_RISE * 0.25) < at(SNAP_RISE * 0.5) - at(SNAP_RISE * 0.25));

        // It reaches its target exactly at the end of the ramp.
        assert!((at(SNAP_RISE) - 1.0).abs() < 1e-3);

        // The overshoot, and then the reverse. Sampled as extremes rather than
        // at a nominated instant, so retuning the ring's shape cannot silently
        // drop either lobe.
        let ring: Vec<f32> = (0..=200)
            .map(|i| at(SNAP_RISE + SNAP_RING * i as f32 / 200.0))
            .collect();
        let peak = ring.iter().cloned().fold(f32::MIN, f32::max);
        let trough = ring.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            (peak - 1.10).abs() < 0.01,
            "overshoot peaked at {peak:.3} rather than 1.10"
        );
        assert!(
            (trough - 0.95).abs() < 0.01,
            "the reverse swing reached {trough:.3} rather than 0.95"
        );
        // In that order: past the target first, back the other way after.
        let peak_at = ring.iter().position(|v| *v == peak).unwrap_or(usize::MAX);
        let trough_at = ring.iter().position(|v| *v == trough).unwrap_or(0);
        assert!(peak_at < trough_at, "it swung back before it overshot");
    }

    /// It has to loop, because the badges play it as an unbounded idle phase.
    /// A curve that ended anywhere but where it started would jump once per
    /// period, which reads as a glitch rather than as a rhythm.
    #[test]
    fn the_snap_returns_to_rest_so_it_can_loop() {
        assert!(Curve::SnapPendulum.apply(0.0).abs() < 1e-4);
        assert!(Curve::SnapPendulum.apply(1.0).abs() < 1e-4);

        // And nothing in between is discontinuous: no step between adjacent
        // samples may be a large fraction of the whole travel.
        let mut previous = Curve::SnapPendulum.apply(0.0);
        for i in 1..=1_000 {
            let value = Curve::SnapPendulum.apply(i as f32 / 1_000.0);
            assert!(
                (value - previous).abs() < 0.02,
                "a step of {:.4} at {i}/1000",
                value - previous
            );
            previous = value;
        }
    }

    /// The overshoot survives to a consumer that reads the envelope, and never
    /// reaches one that reads a colour.
    #[test]
    fn the_overshoot_reaches_strength_but_never_a_mix() {
        let behaviour = get(names::BADGE_CHARGE);
        let pos = CellPos::new(0, 0);
        let extent = CellExtent::new(1, 1);
        let peak = (0..=200)
            .map(|i| behaviour.strength(pos, extent, i as f32 / 200.0))
            .fold(f32::MIN, f32::max);
        assert!(peak > 1.05, "the envelope was flattened to {peak:.3}");

        // And the colour path clamps, so no cell is ever mixed *past* its ink.
        // An unclamped fraction above 1.0 would extrapolate beyond the target
        // colour, which saturates at the channel bounds rather than erroring —
        // so the check is that every channel stays on the segment between where
        // the cell started and the ink it is moving toward.
        let (own, accent) = (PALETTE.own, PALETTE.accent);
        let on_segment =
            |value: u8, from: u8, to: u8| value >= from.min(to) && value <= from.max(to);
        for i in 0..=200 {
            let paint = behaviour.cell(
                pos,
                extent,
                i as f32 / 200.0,
                DriveInputs::default(),
                PALETTE,
            );
            if let Some(fg) = paint.fg {
                assert!(
                    on_segment(fg.0, own.0, accent.0)
                        && on_segment(fg.1, own.1, accent.1)
                        && on_segment(fg.2, own.2, accent.2),
                    "the overshoot extrapolated past the ink: {fg:?} is not between \
                     {own:?} and {accent:?}"
                );
            }
        }
    }

    /// The three badge states are three rhythms, and that is what makes them
    /// readable without colour.
    #[test]
    fn the_three_badge_behaviours_are_three_tempos() {
        let rest = get(names::BADGE_REST);
        let charge = get(names::BADGE_CHARGE);
        let alert = get(names::BADGE_ALERT);

        assert!(
            rest.period > charge.period * 2,
            "rest at {:?} is not slow enough against a lit badge at {:?}",
            rest.period,
            charge.period
        );
        assert!(
            charge.period > alert.period * 2,
            "an escalation at {:?} is not urgent enough against {:?}",
            alert.period,
            charge.period
        );

        // Rest breathes; the two live states snap. Different curves, not the
        // same curve at a different speed.
        assert_eq!(rest.curve, Curve::Sine);
        assert_eq!(charge.curve, Curve::SnapPendulum);
        assert_eq!(alert.curve, Curve::SnapPendulum);

        // A resting badge is the tray's common case, so it must not ask the
        // loop for the smooth tier that a snapping one needs.
        assert!(rest.frame_interval > charge.frame_interval);
    }

    /// The two snap curves are one snap.
    ///
    /// [`Curve::SnapArrival`] is [`Curve::SnapPendulum`]'s span up to its
    /// settle, replayed over a whole `0.0..=1.0`. Written as an equality rather
    /// than as two lists of numbers so that a change to the ramp, the overshoot
    /// or the reverse cannot land on one curve and not the other — which is the
    /// only way the *stated* motion character could quietly become two
    /// characters.
    #[test]
    fn both_snap_curves_are_the_same_snap() {
        let settled = SNAP_RISE + SNAP_RING;
        for i in 0..=400 {
            let u = i as f32 / 400.0;
            let pendulum = Curve::SnapPendulum.apply(u * settled);
            let arrival = Curve::SnapArrival.apply(u);
            assert!(
                (pendulum - arrival).abs() < 1e-4,
                "the snap diverged at {u:.3}: {pendulum:.5} against {arrival:.5}"
            );
        }
    }

    /// The stated character, read off the curve rather than off the constants:
    /// slow, then a ramp into a snap, ~10% past, ~5% back, and *arrived* at the
    /// end rather than back where it started.
    #[test]
    fn the_arrival_snap_overshoots_pendulums_back_and_ends_arrived() {
        let at = |u: f32| Curve::SnapArrival.apply(u);

        assert!(at(0.0).abs() < 1e-6, "the snap did not start at rest");
        assert!(
            (at(1.0) - 1.0).abs() < 1e-4,
            "a bounded arrival has to end arrived, ended at {:.5}",
            at(1.0)
        );

        // Exponential acceleration: half way through the ramp it has covered a
        // small fraction of the distance, which is what "starts slow then ramps
        // into a snap" means as a number.
        const RISE: f32 = SNAP_RISE / (SNAP_RISE + SNAP_RING);
        assert!(
            at(RISE / 2.0) < 0.25,
            "the ramp was linear rather than accelerating: {:.3} at its midpoint",
            at(RISE / 2.0)
        );

        let samples: Vec<f32> = (0..=1_000).map(|i| at(i as f32 / 1_000.0)).collect();
        let peak = samples.iter().cloned().fold(f32::MIN, f32::max);
        assert!(
            (peak - (1.0 + SNAP_OVERSHOOT)).abs() < 0.01,
            "the overshoot is {:.1}% rather than the stated ten",
            (peak - 1.0) * 100.0
        );
        // And the reverse swing, which is only meaningful after the peak.
        let after_peak = samples
            .iter()
            .skip_while(|value| **value < peak)
            .cloned()
            .fold(f32::MAX, f32::min);
        assert!(
            (after_peak - (1.0 - SNAP_REVERSE)).abs() < 0.01,
            "the reverse swing is {:.1}% rather than the stated five",
            (1.0 - after_peak) * 100.0
        );
    }

    /// The wash's whole claim: when it has crossed, every column of the card is
    /// at full amount and stays there.
    ///
    /// The comparison is against a band, because that is the effect this could
    /// have been and must not be. A band peaks as it passes a column and leaves
    /// it exactly as it was, so it ends with the card unchanged — a highlight.
    #[test]
    fn a_finished_wash_covers_every_column_and_a_band_would_not() {
        let extent = CellExtent::row(64);
        let wash = get(names::CARD_WASH);
        let band = Behaviour {
            shape: Shape::Band { width: 0.45 },
            ..wash
        };
        for col in 0..extent.cols {
            let pos = CellPos::col(col);
            assert!(
                wash.strength(pos, extent, 1.0) >= 0.999,
                "column {col} was left behind by a wash that finished"
            );
        }
        let band_end: f32 = (0..extent.cols)
            .map(|col| band.strength(CellPos::col(col), extent, 1.0))
            .fold(f32::MIN, f32::max);
        assert!(
            band_end < 0.5,
            "the band left the card tainted, so this test is no longer telling \
             a wash from a highlight"
        );
    }

    /// And it crosses left to right: every column arrives, and the left one
    /// arrives first.
    #[test]
    fn the_wash_crosses_from_the_left() {
        let extent = CellExtent::row(64);
        let wash = get(names::CARD_WASH);
        // The step at which each column first reaches half.
        let arrival = |col: u16| {
            (0..=200)
                .find(|i| wash.strength(CellPos::col(col), extent, *i as f32 / 200.0) >= 0.5)
                .expect("every column has to arrive")
        };
        let first = arrival(0);
        let last = arrival(extent.cols - 1);
        assert!(
            first < last,
            "the front did not travel: column 0 arrived at {first} and the last at {last}"
        );

        // Once a column has arrived it stays arrived, which is the difference
        // between a front and something passing over.
        for i in 0..=200 {
            let progress = i as f32 / 200.0;
            if progress * 200.0 < first as f32 {
                continue;
            }
            assert!(
                wash.strength(CellPos::col(0), extent, progress) >= 0.5,
                "the leftmost column un-arrived at {progress:.3}"
            );
        }
    }

    /// The two card breaths are two rhythms, the same way the three badges are:
    /// rest is slower, shallower, and on a different curve entirely.
    #[test]
    fn a_resting_card_breathes_slower_and_shallower_than_a_working_one() {
        let rest = get(names::CARD_REST);
        let live = get(names::CARD_LIVE);

        assert!(
            rest.period > live.period * 2,
            "rest at {:?} is not slow enough against a working card at {:?}",
            rest.period,
            live.period
        );
        assert_eq!(rest.curve, Curve::Sine, "rest drifts, it does not snap");
        assert_eq!(live.curve, Curve::SnapPendulum);
        assert!(rest.paint.depth < live.paint.depth);

        // Both are uniform: a card is one object, so its breath is the whole
        // card's and never a sweep across it. The wash is the only card
        // behaviour with a field.
        assert!(rest.is_uniform());
        assert!(live.is_uniform());
        assert!(!get(names::CARD_WASH).is_uniform());

        // A resting card is the tree's common case, so it must not ask the loop
        // for the tier a snapping one needs.
        assert!(rest.frame_interval > live.frame_interval);

        // A working card takes its tempo from its pane's work volume; a resting
        // one has none to take.
        assert!(live.is_metric_reactive());
        assert!(!rest.is_metric_reactive());
    }
}
