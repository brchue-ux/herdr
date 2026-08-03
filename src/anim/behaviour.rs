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
//!   colour, attributes, and coverage only — never to a glyph, a width, or a
//!   position. Dropping every frame leaves the element identical.
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
}

impl Paint {
    const fn tint(ink: Ink, depth: f32) -> Self {
        Self {
            fg: Some(ink),
            bg: None,
            depth,
            reveal: false,
            attrs_above: None,
        }
    }

    const fn reveal() -> Self {
        Self {
            fg: None,
            bg: None,
            depth: 1.0,
            reveal: true,
            attrs_above: None,
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
        let progress = if progress > 1.0 || progress < 0.0 {
            Self::loop_progress(progress)
        } else {
            progress
        };
        let amount = self.amount(pos, extent, progress);
        let depth = (self.paint.depth * self.depth_drive.value(inputs)).clamp(0.0, 1.0);
        let mut paint = CellPaint::default();

        if self.paint.reveal {
            // Depth scales how far a reveal gets rather than how bright it is:
            // a reveal driven to half strength arrives half-way and stays, which
            // is the honest reading of "this element is only half here".
            paint.coverage = (amount * depth).clamp(0.0, 1.0);
        }
        if let Some(ink) = self.paint.fg {
            paint.fg = Some(mix_rgb(palette.own, palette.ink(ink), amount * depth));
        }
        if let Some(ink) = self.paint.bg {
            paint.bg = Some(mix_rgb(palette.surface, palette.ink(ink), amount * depth));
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
}

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

fn built_in_behaviours() -> [(&'static str, Behaviour); 11] {
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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PALETTE: InkPalette = InkPalette {
        surface: (0, 0, 0),
        own: (200, 200, 200),
        accent: (0, 0, 255),
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
    fn the_cheap_frame_interval_is_what_the_app_already_pays() {
        // A behaviour that does not ask for smoothness must not make the loop
        // wake more often than the sidebar pulse already does.
        assert_eq!(
            get(names::PULSE).frame_interval,
            crate::app::ANIMATION_INTERVAL
        );
        assert!(SMOOTH_FRAME_INTERVAL >= Duration::from_millis(16));
    }
}
