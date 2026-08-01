//! Per-cell coverage field for pane materialisation.
//!
//! SPIKE — not production. See the scout report for the shipping shape.
//!
//! The primitive here is deliberately **not** a scalar boundary row. It is a field:
//! every cell in the pane rect carries a `level` in cell units, where
//!
//! * `level <= 0.0` — the cell is dry; nothing is drawn, the pane content is hidden
//! * `0.0 < level < 1.0` — the cell is the *surface*; it is drawn as an eighth-block
//!   glyph so one cell carries eight distinct vertical positions
//! * `level >= 1.0` — the cell is submerged; the real terminal content shows through,
//!   tinted toward the water colour by `level - 1.0` (its depth below the surface)
//!
//! Because coverage is a field rather than a number, a flat rising surface, a
//! travelling wave, a stream pouring in from a point and a set of coalescing droplets
//! are all *different functions over the same field* — see [`Behaviour::level_at`].
//! Nothing below hardcodes a single effect.

use std::f32::consts::PI;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};

/// A character cell is roughly twice as tall as it is wide. Anything that needs to
/// look round (droplets, radial spread) has to divide horizontal distance by this.
const CELL_ASPECT: f32 = 0.5;

/// Eighth blocks, anchored to the **bottom** of the cell. Eight sub-cell levels.
const LOWER_EIGHTHS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// Below this the cell is treated as dry and nothing is drawn.
///
/// This is not cosmetic. Every behaviour built on a falloff (the pour's mound, the
/// droplet kernel) has a tail that is mathematically non-zero far from its source,
/// and without an epsilon the smallest glyph `▁` paints the whole pane on frame one.
const MIN_VISIBLE_COVERAGE: f32 = 0.07;

/// How a cell should be drawn given its level. Shared by the renderer and the
/// capture harness so a captured frame classifies cells exactly as the screen does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CellKind {
    /// Nothing here yet; the pane's content is hidden.
    Dry,
    /// The surface, drawn as one of eight sub-cell block glyphs.
    Surface(usize),
    /// Submerged; the pane's real content shows through, tinted by depth.
    Submerged,
}

pub(crate) fn classify(level: f32) -> CellKind {
    if level < MIN_VISIBLE_COVERAGE {
        CellKind::Dry
    } else if level < 1.0 {
        CellKind::Surface(((level * 8.0).ceil() as i32).clamp(1, 8) as usize - 1)
    } else {
        CellKind::Submerged
    }
}

// ---------------------------------------------------------------------------
// The field
// ---------------------------------------------------------------------------

/// Per-cell fill levels over a pane rect, row-major.
pub(crate) struct CoverageField {
    width: u16,
    height: u16,
    levels: Vec<f32>,
}

impl CoverageField {
    /// Evaluate `behaviour` over a `width` x `height` grid at progress `t` in `0.0..=1.0`.
    pub(crate) fn sample(width: u16, height: u16, behaviour: Behaviour, t: f32) -> Self {
        // Anything that depends only on `t` is computed once per frame, not per cell.
        let frame = FrameConstants::new(behaviour, t);
        let mut levels = Vec::with_capacity(width as usize * height as usize);
        for y in 0..height {
            for x in 0..width {
                levels.push(behaviour.level_at(x, y, width, height, t, &frame));
            }
        }
        Self {
            width,
            height,
            levels,
        }
    }

    /// Raw level in cell units. `>= 1.0` is submerged; the excess is depth.
    pub(crate) fn level(&self, x: u16, y: u16) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        self.levels[y as usize * self.width as usize + x as usize]
    }

    /// Fill fraction of the cell, `0.0..=1.0`.
    pub(crate) fn coverage(&self, x: u16, y: u16) -> f32 {
        self.level(x, y).clamp(0.0, 1.0)
    }

    /// Mean coverage over the whole field. Used by tests to assert monotone filling.
    pub(crate) fn mean_coverage(&self) -> f32 {
        if self.levels.is_empty() {
            return 0.0;
        }
        let total: f32 = self.levels.iter().map(|l| l.clamp(0.0, 1.0)).sum();
        total / self.levels.len() as f32
    }
}

// ---------------------------------------------------------------------------
// Behaviours — every one of these is a function over the same field
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Behaviour {
    /// Flat surface rising from the floor, ease-out settle.
    Fill,
    /// A stream enters at horizontal position `origin` (0.0 = left edge, 1.0 = right)
    /// and the pane fills from that point outward.
    Pour { origin: f32 },
    /// A rising fill whose surface carries a travelling wave and a decaying slosh.
    Slosh,
    /// Droplets appear, grow and merge (metaball potential) into a full pane.
    Droplets,
}

impl Behaviour {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "fill" => Some(Self::Fill),
            "pour" => Some(Self::Pour { origin: 0.12 }),
            "pour-right" => Some(Self::Pour { origin: 0.88 }),
            "slosh" | "wave" => Some(Self::Slosh),
            "droplets" | "drops" => Some(Self::Droplets),
            _ => None,
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Fill => "fill",
            Self::Pour { .. } => "pour",
            Self::Slosh => "slosh",
            Self::Droplets => "droplets",
        }
    }

    /// The whole primitive. One function, one cell, one instant.
    fn level_at(&self, x: u16, y: u16, w: u16, h: u16, t: f32, frame: &FrameConstants) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let w_f = w.max(1) as f32;
        let h_f = h.max(1) as f32;
        // Horizontal position of the cell centre, 0.0..1.0.
        let u = (x as f32 + 0.5) / w_f;
        // Height of the cell's *bottom edge* above the floor, in cell units. The
        // fractional part of `surface - floor_of_cell` is what buys sub-cell detail.
        let floor = (h - 1 - y.min(h - 1)) as f32;

        match *self {
            Self::Fill => surface_level(fill_surface(t, h_f), floor),

            Self::Slosh => surface_level(slosh_surface(u, t, h_f, frame), floor),

            Self::Pour { origin } => {
                let puddle = surface_level(pour_surface(u, h_f, origin, frame), floor);
                let stream = pour_stream(u, y, h_f, origin, frame);
                puddle.max(stream)
            }

            Self::Droplets => droplet_level(x, y, w_f, h_f, frame),
        }
    }
}

/// Everything that depends on `t` but not on the cell. Computed once per frame.
struct FrameConstants {
    /// Slosh: envelope that forces both oscillating terms to zero by the end.
    settled: f32,
    /// Pour: normalising mean of the mound profile.
    pour_mean: f32,
    /// Pour: current mound width.
    pour_spread: f32,
    /// Pour: volume delivered so far, in mean cell-rows.
    pour_volume: f32,
    /// Pour: how far the mound has relaxed toward level, 0..1.
    pour_settle: f32,
    /// Pour: progress of the falling stream, or `None` once it has stopped.
    pour_stream_phase: Option<f32>,
    /// Droplets: per-droplet radius as a fraction of the pane's larger screen edge.
    drop_radii: [f32; DROPS.len()],
    /// Droplets: the closing film level.
    drop_closeout: f32,
    /// Raw progress.
    t: f32,
}

/// The stream lands here; the puddle does not start growing before it.
const POUR_ARRIVAL: f32 = 0.17;

impl FrameConstants {
    fn new(behaviour: Behaviour, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);

        let (pour_mean, pour_spread, pour_volume, pour_settle, pour_stream_phase) = match behaviour
        {
            Behaviour::Pour { origin } => {
                // The puddle's clock starts when the stream reaches the floor.
                let tp = ((t - POUR_ARRIVAL) / (1.0 - POUR_ARRIVAL)).clamp(0.0, 1.0);
                let spread = 0.10 + 1.60 * ease_out_cubic(tp).powf(0.75);
                (
                    gaussian_mean(origin, spread),
                    spread,
                    ease_out_cubic(tp) * 1.02,
                    smoothstep(0.35, 1.0, tp),
                    (t < POUR_STREAM_END).then(|| (t / POUR_STREAM_END).clamp(0.0, 1.0)),
                )
            }
            _ => (1.0, 1.0, 0.0, 0.0, None),
        };

        let mut drop_radii = [0.0f32; DROPS.len()];
        if behaviour == Behaviour::Droplets {
            for (i, (_, _, birth, weight)) in DROPS.iter().enumerate() {
                if t < *birth {
                    continue;
                }
                let age = ((t - birth) / (1.0 - birth).max(1e-3)).clamp(0.0, 1.0);
                drop_radii[i] = (0.07 + 0.30 * ease_out_cubic(age)) * weight;
            }
        }

        Self {
            settled: 1.0 - smoothstep(0.70, 1.0, t),
            pour_mean,
            pour_spread,
            pour_volume,
            pour_settle,
            pour_stream_phase,
            drop_radii,
            drop_closeout: smoothstep(0.62, 1.0, t),
            t,
        }
    }
}

/// Convert a surface height (cell units above the floor) into this cell's level.
fn surface_level(surface: f32, cell_floor: f32) -> f32 {
    surface - cell_floor
}

// --- fill ------------------------------------------------------------------

fn fill_surface(t: f32, h: f32) -> f32 {
    // Slight overshoot then settle: water does not stop dead when it arrives.
    ease_out_cubic(t) * h * 1.02
}

// --- slosh -----------------------------------------------------------------

fn slosh_surface(u: f32, t: f32, h: f32, frame: &FrameConstants) -> f32 {
    let base = ease_out_cubic(t) * h * 1.02;

    // Both oscillating terms are forced to exactly zero by the end, so the pane can
    // never finish with a wave-shaped notch in its top row.
    let settled = frame.settled;

    // Travelling ripple: rides the surface, amplitude decays as the pane settles.
    let ripple_amp = 0.9 * h * 0.06 * decay(t, 2.2) * settled;
    let ripple = ripple_amp * ((u * 2.0 * PI * 1.7) - (t * 2.0 * PI * 2.4)).sin();

    // Slosh: the whole surface tilts left/right and rings down. This is the part a
    // scalar boundary row cannot express at all.
    let slosh_amp = h * 0.10 * decay(t, 3.0) * settled;
    let slosh = slosh_amp * (u - 0.5) * 2.0 * (t * 2.0 * PI * 1.6).sin();

    base + ripple + slosh
}

// --- pour ------------------------------------------------------------------

/// Fraction of the animation spent with the stream still visibly falling.
const POUR_STREAM_END: f32 = 0.62;

fn pour_surface(u: f32, h: f32, origin: f32, frame: &FrameConstants) -> f32 {
    let d = (u - origin) / frame.pour_spread;
    let profile = (-d * d).exp();

    // `profile / mean` conserves volume however wide the mound is; blending it toward
    // a flat `1.0` is the surface relaxing to level. Both are one expression.
    let shape = (profile / frame.pour_mean.max(1e-3)) * (1.0 - frame.pour_settle)
        + frame.pour_settle;
    let level = frame.pour_volume * h * shape;

    // Outward surge: a bulge travels away from the entry point over the surface.
    // Gated on the puddle's own clock — no water on the floor before the stream lands.
    let t = frame.t;
    let since_arrival = (t - POUR_ARRIVAL).max(0.0);
    let front = 2.2 * since_arrival;
    let dist = (u - origin).abs();
    let surge = h * 0.09
        * decay(since_arrival, 2.6)
        * smoothstep(0.0, 0.04, since_arrival)
        * (-((dist - front) * 6.0).powi(2)).exp();

    (level + surge).min(h * 1.04)
}

/// The falling stream itself: a narrow column above the rising puddle.
fn pour_stream(u: f32, y: u16, h: f32, origin: f32, frame: &FrameConstants) -> f32 {
    let Some(phase) = frame.pour_stream_phase else {
        return 0.0;
    };
    // Head accelerates downward (gravity) and reaches the floor at POUR_ARRIVAL.
    let head_rows = ease_in_quad((phase * 3.65).min(1.0)) * h;
    let row_from_top = y as f32 + 0.5;
    if row_from_top > head_rows {
        return 0.0;
    }

    // Stream narrows as it accelerates, and thins out as the pour tails off.
    let width_u = (0.028 + 0.030 * (1.0 - phase)) * (1.0 - phase * 0.35);
    let d = (u - origin) / width_u.max(1e-3);
    let body = (-d * d).exp();
    let fade = 1.0 - smoothstep(0.55, 1.0, phase);
    body * 1.6 * fade
}

fn gaussian_mean(origin: f32, spread: f32) -> f32 {
    let mut acc = 0.0;
    const N: usize = 24;
    for i in 0..N {
        let u = (i as f32 + 0.5) / N as f32;
        let d = (u - origin) / spread;
        acc += (-d * d).exp();
    }
    acc / N as f32
}

// --- droplets --------------------------------------------------------------

/// Deterministic droplet seeds: (u, v, birth, weight). Hand-placed rather than RNG so
/// captures are reproducible frame-for-frame.
const DROPS: [(f32, f32, f32, f32); 11] = [
    (0.18, 0.30, 0.00, 1.15),
    (0.72, 0.22, 0.04, 1.05),
    (0.45, 0.68, 0.08, 1.20),
    (0.88, 0.58, 0.13, 0.95),
    (0.30, 0.85, 0.17, 1.00),
    (0.60, 0.44, 0.21, 1.10),
    (0.08, 0.62, 0.26, 0.90),
    (0.52, 0.12, 0.31, 0.95),
    (0.80, 0.86, 0.36, 1.00),
    (0.22, 0.48, 0.41, 0.85),
    (0.95, 0.34, 0.46, 0.85),
];

fn droplet_level(x: u16, y: u16, w: f32, h: f32, frame: &FrameConstants) -> f32 {
    // Work in *column units* so droplets come out round: one row is two columns tall.
    let row_units = 1.0 / CELL_ASPECT;
    let px = x as f32 + 0.5;
    let py = (y as f32 + 0.5) * row_units;
    let sw = w;
    let sh = h * row_units;
    let scale = sw.max(sh);

    // Metaball potential with **compact support** — the Wyvill kernel is zero beyond
    // the droplet radius. An inverse-square kernel would leave a faint haze over the
    // entire pane, which is exactly what it did on the first attempt.
    //
    // Overlapping droplets sum, so coalescence is not a special case: it falls out of
    // the field. This is the clearest demonstration that the primitive is a field.
    let mut potential = 0.0f32;
    for (i, (cu, cv, _, _)) in DROPS.iter().enumerate() {
        let radius = frame.drop_radii[i] * scale;
        if radius <= 0.0 {
            continue;
        }
        let dx = px - cu * sw;
        let dy = py - cv * sh;
        let d2 = dx * dx + dy * dy;
        let r2 = radius * radius;
        if d2 >= r2 {
            continue;
        }
        let q = 1.0 - d2 / r2;
        potential += q * q * q;
    }

    // Isosurface at `potential == THRESHOLD`; GAIN sets how many cells the edge spans.
    const THRESHOLD: f32 = 0.30;
    const GAIN: f32 = 5.0;
    let mut level = 1.0 + (potential - THRESHOLD) * GAIN;

    // Closeout: a film rises between the droplets and finishes the pane. This is the
    // one term here that is staging rather than physics — without it the corners
    // between droplets never fill and the pane finishes ~96% covered.
    level = level.max(frame.drop_closeout * 5.0 - 1.6);

    level
}

// --- easing ----------------------------------------------------------------

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn ease_in_quad(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    if (b - a).abs() < 1e-6 {
        return if x < a { 0.0 } else { 1.0 };
    }
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Exponential ring-down used by every settling term.
fn decay(t: f32, rate: f32) -> f32 {
    (-t * rate).exp()
}

// ---------------------------------------------------------------------------
// Paint — coverage field to glyph + style
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(crate) struct WaterStyle {
    /// Leading edge / crest highlight.
    pub crest: (u8, u8, u8),
    /// Body of the water just under the surface.
    pub body: (u8, u8, u8),
    /// Background the pane sits on before any water reaches it.
    pub dry_bg: (u8, u8, u8),
    /// How many cells below the surface the wet tint takes to fade out.
    pub tint_depth: f32,
}

impl Default for WaterStyle {
    fn default() -> Self {
        Self {
            // Catppuccin sky, brightened — the crest reads as the lit edge.
            crest: (186, 240, 255),
            // Catppuccin sapphire — the body.
            body: (116, 199, 236),
            // Catppuccin base — the pane's own ground.
            dry_bg: (30, 30, 46),
            tint_depth: 3.5,
        }
    }
}

/// Overwrite `area` in `buf` according to `field`.
///
/// Runs **after** the pane's real content has been drawn, exactly like the existing
/// pane-dim pass in `panes.rs`. It never touches the runtime or the resize path.
pub(crate) fn paint(buf: &mut Buffer, area: Rect, field: &CoverageField, style: &WaterStyle) {
    for cy in 0..area.height {
        for cx in 0..area.width {
            let px = area.x.saturating_add(cx);
            let py = area.y.saturating_add(cy);
            if px >= buf.area.right() || py >= buf.area.bottom() {
                continue;
            }
            let level = field.level(cx, cy);
            let cell = &mut buf[(px, py)];

            let kind = classify(level);
            if kind == CellKind::Dry {
                // Dry: the pane is not here yet.
                cell.set_symbol(" ");
                cell.set_style(Style::default().fg(rgb(style.dry_bg)).bg(rgb(style.dry_bg)));
                continue;
            }

            if let CellKind::Surface(idx) = kind {
                // Surface cell: eight sub-cell positions inside one row.
                // The thinner the sliver, the closer to the crest highlight it reads.
                let crestness = 1.0 - (level * 0.65).clamp(0.0, 1.0);
                let fg = mix(style.body, style.crest, crestness);
                cell.set_symbol(LOWER_EIGHTHS[idx]);
                cell.set_style(Style::default().fg(rgb(fg)).bg(rgb(style.dry_bg)));
                continue;
            }

            // Submerged: the real content shows through, tinted by depth.
            let depth = level - 1.0;
            let wet = (1.0 - depth / style.tint_depth.max(0.001)).clamp(0.0, 1.0);
            if wet <= 0.005 {
                continue;
            }
            let current_bg = resolve(cell.bg).unwrap_or(style.dry_bg);
            let current_fg = resolve(cell.fg).unwrap_or((205, 214, 244));
            // Background carries most of the effect — it fills even where the pane's
            // own content is blank, so the water body is visible regardless of text.
            let new_bg = mix(current_bg, style.body, wet * 0.42);
            let new_fg = mix(current_fg, style.crest, wet * 0.55);
            cell.set_style(cell.style().fg(rgb(new_fg)).bg(rgb(new_bg)));
        }
    }
}

fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), k: f32) -> (u8, u8, u8) {
    let k = k.clamp(0.0, 1.0);
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round().clamp(0.0, 255.0) as u8;
    (f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
}

fn resolve(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Black => Some((0, 0, 0)),
        Color::White => Some((255, 255, 255)),
        Color::Gray => Some((192, 192, 192)),
        Color::DarkGray => Some((128, 128, 128)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Spike wiring: which behaviour, how long, and per-pane start times.
// ---------------------------------------------------------------------------
//
// A shipping version puts the start times in `AppState` next to the other
// server-held presentation state. A thread-local is used here only so the spike
// diff stays inside the render path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

thread_local! {
    static STARTS: RefCell<HashMap<u64, Instant>> = RefCell::new(HashMap::new());
}

/// Reads `HERDR_WATER=fill|pour|pour-right|slosh|droplets` (unset or `off` disables).
pub(crate) fn configured_behaviour() -> Option<Behaviour> {
    let raw = std::env::var("HERDR_WATER").ok()?;
    if raw.trim().is_empty() || raw.trim().eq_ignore_ascii_case("off") {
        return None;
    }
    Behaviour::parse(&raw)
}

/// Reads `HERDR_WATER_MS` (default 600 ms — the shipping figure).
pub(crate) fn configured_duration() -> Duration {
    let ms = std::env::var("HERDR_WATER_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(600);
    Duration::from_millis(ms.clamp(60, 20_000))
}

/// Progress for `pane_key`, starting the clock the first time the pane is seen.
/// Returns `None` once the animation is finished.
pub(crate) fn progress(pane_key: u64, now: Instant, duration: Duration) -> Option<f32> {
    STARTS.with(|starts| {
        let mut starts = starts.borrow_mut();
        let start = *starts.entry(pane_key).or_insert(now);
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= duration {
            return None;
        }
        Some((elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0))
    })
}

/// True while any pane is still materialising — used to arm a fast render deadline.
pub(crate) fn any_active(now: Instant, duration: Duration) -> bool {
    STARTS.with(|starts| {
        starts
            .borrow()
            .values()
            .any(|start| now.saturating_duration_since(*start) < duration)
    })
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Behaviour; 4] = [
        Behaviour::Fill,
        Behaviour::Pour { origin: 0.12 },
        Behaviour::Slosh,
        Behaviour::Droplets,
    ];

    #[test]
    fn every_behaviour_starts_empty_and_finishes_full() {
        for b in ALL {
            let start = CoverageField::sample(40, 12, b, 0.0);
            let end = CoverageField::sample(40, 12, b, 1.0);
            assert!(
                start.mean_coverage() < 0.12,
                "{} starts too full: {}",
                b.label(),
                start.mean_coverage()
            );
            assert!(
                end.mean_coverage() > 0.999,
                "{} does not finish full: {}",
                b.label(),
                end.mean_coverage()
            );
        }
    }

    #[test]
    fn coverage_is_a_field_not_a_boundary_row() {
        // A scalar boundary can only ever produce one partially covered row per
        // column, all at the same height. Slosh/pour/droplets must not.
        for b in [
            Behaviour::Slosh,
            Behaviour::Pour { origin: 0.12 },
            Behaviour::Droplets,
        ] {
            let mut distinct_surface_rows = std::collections::HashSet::new();
            let f = CoverageField::sample(60, 16, b, 0.45);
            for x in 0..60u16 {
                for y in 0..16u16 {
                    let c = f.coverage(x, y);
                    if c > 0.0 && c < 1.0 {
                        distinct_surface_rows.insert(y);
                    }
                }
            }
            assert!(
                distinct_surface_rows.len() > 1,
                "{} collapsed to a single boundary row",
                b.label()
            );
        }
    }

    #[test]
    fn sub_cell_resolution_is_eight_levels() {
        // Sweep a fill across one cell height and count distinct glyphs produced.
        let mut glyphs = std::collections::HashSet::new();
        for step in 0..400 {
            let t = step as f32 / 400.0;
            let f = CoverageField::sample(1, 8, Behaviour::Fill, t);
            let mut buf = Buffer::empty(Rect::new(0, 0, 1, 8));
            paint(&mut buf, Rect::new(0, 0, 1, 8), &f, &WaterStyle::default());
            for y in 0..8u16 {
                glyphs.insert(buf[(0, y)].symbol().to_string());
            }
        }
        for g in LOWER_EIGHTHS {
            assert!(glyphs.contains(g), "missing sub-cell glyph {g}");
        }
    }

    #[test]
    fn paint_leaves_submerged_content_intact() {
        let area = Rect::new(0, 0, 10, 6);
        let mut buf = Buffer::empty(area);
        for y in 0..6u16 {
            for x in 0..10u16 {
                buf[(x, y)].set_symbol("X");
            }
        }
        let f = CoverageField::sample(10, 6, Behaviour::Fill, 1.0);
        paint(&mut buf, area, &f, &WaterStyle::default());
        for y in 0..6u16 {
            for x in 0..10u16 {
                assert_eq!(buf[(x, y)].symbol(), "X", "content clobbered at {x},{y}");
            }
        }
    }

    // -- frame capture ------------------------------------------------------
    //
    // Dumps real frames from the real paint path. Run with:
    //   HERDR_WATER_FRAMES_OUT=/path/frames.txt \
    //     cargo test --lib water::tests::capture_frames -- --ignored --nocapture

    /// Stand-in pane content, so a captured frame shows content being revealed
    /// rather than an empty grid.
    const PANE_CONTENT: &[&str] = &[
        "$ claude",
        "",
        "╭────────────────────────────────────────────────────╮",
        "│ > wire the coverage field into render_panes         │",
        "╰────────────────────────────────────────────────────╯",
        "",
        "  ⏺ Read src/ui/panes.rs (1289 lines)",
        "  ⏺ Read src/layout.rs (612 lines)",
        "  ⏺ Update src/ui/panes.rs",
        "      + let field = CoverageField::sample(w, h, b, t);",
        "      + water::paint(frame.buffer_mut(), inner, &field);",
        "  ⏺ Bash(cargo build -j 2)",
        "    Finished dev profile in 41.20s",
        "",
    ];

    fn fill_content(buf: &mut Buffer, area: Rect) {
        for y in 0..area.height {
            let line = PANE_CONTENT
                .get(y as usize % PANE_CONTENT.len())
                .copied()
                .unwrap_or("");
            let mut x = 0u16;
            for ch in line.chars() {
                if x >= area.width {
                    break;
                }
                buf[(area.x + x, area.y + y)].set_symbol(&ch.to_string());
                x += 1;
            }
            while x < area.width {
                buf[(area.x + x, area.y + y)].set_symbol(" ");
                x += 1;
            }
        }
    }

    /// Dump the painted buffer. Dry cells print as `·` **for legibility only** — on
    /// screen they are flat background. Everything else is verbatim: block glyphs are
    /// what the renderer emitted, text is the pane's own content showing through.
    fn frame_text(w: u16, h: u16, behaviour: Behaviour, t: f32) -> String {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        fill_content(&mut buf, area);
        let field = CoverageField::sample(w, h, behaviour, t);
        paint(&mut buf, area, &field, &WaterStyle::default());
        let mut out = String::new();
        for y in 0..h {
            for x in 0..w {
                if classify(field.level(x, y)) == CellKind::Dry {
                    out.push('·');
                } else {
                    out.push_str(buf[(x, y)].symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    #[ignore = "measurement harness, not an assertion"]
    fn measure_cost() {
        // Cost of one frame = sample the field + paint it, on a full-tab pane.
        for (w, h) in [(56u16, 14u16), (120, 40), (240, 80)] {
            let area = Rect::new(0, 0, w, h);
            for b in ALL {
                let mut buf = Buffer::empty(area);
                fill_content(&mut buf, area);
                // Warm.
                for i in 0..20 {
                    let f = CoverageField::sample(w, h, b, i as f32 / 20.0);
                    paint(&mut buf, area, &f, &WaterStyle::default());
                }
                const N: u32 = 400;
                let start = Instant::now();
                for i in 0..N {
                    let f = CoverageField::sample(w, h, b, (i % 100) as f32 / 100.0);
                    paint(&mut buf, area, &f, &WaterStyle::default());
                }
                let per_frame = start.elapsed() / N;
                println!(
                    "COST {:>8} {:>3}x{:<3} ({:>5} cells)  {:>7.1} us/frame  ({:.2}% of a 16ms budget)",
                    b.label(),
                    w,
                    h,
                    w as u32 * h as u32,
                    per_frame.as_secs_f64() * 1e6,
                    per_frame.as_secs_f64() / 0.016 * 100.0,
                );
            }
        }
    }

    #[test]
    #[ignore = "capture harness, not an assertion"]
    fn capture_frames() {
        let mut out = String::new();
        let stops = [0.10f32, 0.25, 0.40, 0.55, 0.70, 0.85, 1.00];
        let sizes: [(u16, u16, &str); 3] = [
            (56, 14, "56x14 (normal split)"),
            (24, 6, "24x6 (small pane)"),
            (80, 24, "80x24 (full tab)"),
        ];
        for b in ALL {
            for (w, h, size_label) in sizes {
                for t in stops {
                    out.push_str(&format!(
                        "### {} — {} — t={:.2}\n```\n{}```\n\n",
                        b.label(),
                        size_label,
                        t,
                        frame_text(w, h, b, t)
                    ));
                }
            }
        }
        match std::env::var("HERDR_WATER_FRAMES_OUT") {
            Ok(path) if !path.trim().is_empty() => {
                std::fs::write(&path, &out).expect("write frames");
                println!("wrote {} bytes to {path}", out.len());
            }
            _ => println!("{out}"),
        }
    }

    #[test]
    fn paint_hides_content_that_is_still_dry() {
        let area = Rect::new(0, 0, 10, 6);
        let mut buf = Buffer::empty(area);
        for y in 0..6u16 {
            for x in 0..10u16 {
                buf[(x, y)].set_symbol("X");
            }
        }
        let f = CoverageField::sample(10, 6, Behaviour::Fill, 0.0);
        paint(&mut buf, area, &f, &WaterStyle::default());
        for y in 0..6u16 {
            for x in 0..10u16 {
                assert_eq!(buf[(x, y)].symbol(), " ", "content leaked at {x},{y}");
            }
        }
    }
}
