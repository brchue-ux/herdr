//! The eight tray badges, drawn as pixels.
//!
//! ## Why this is not eight glyphs
//!
//! A crafted mark here means a **primary silhouette, at least one interior
//! detail, and a highlight** — three ink weights, never one stroke. `ask` is a
//! speech bubble carrying two option pips, so the one badge with a yes/no
//! answer says so before you click it; `checks` is three job pills on a spine,
//! one solid, one half filled, one struck through. That is four or five pieces
//! of information per mark, and one cell of a font Herdr does not control
//! cannot carry any of it: the marks in
//! [`crate::app::fleet_signals::FleetSignal::mark`] are the fallback for a host
//! with no graphics, not the design.
//!
//! ## What it draws
//!
//! Direction A — the card's own icon container promoted to a tray — with the
//! three amendments settled on top of it:
//!
//! 1. **No plate background.** The badge artwork sits directly on the tray
//!    surface; everything outside the mark and its border is transparent, so
//!    whatever the host terminal is painting shows through.
//! 2. **The lit badge keeps its double border** — an outer stroke and an inner
//!    one inset from it, which is what makes a lit slot read as a *container*
//!    rather than as a floating icon.
//! 3. **No translucent in-between layer.** Nothing is laid between the mark and
//!    the surface. It makes the icon pop.
//!
//! Idle is *engraved*, not faded: the mark is cut into the surface, dark, with
//! a hairline lift along its lower edge. A faded badge looks broken; a carved
//! one looks like a slot that is simply empty right now.
//!
//! ## How it draws
//!
//! Every mark is described in a normalised 1×1 box across three ink layers, and
//! rasterised at [`SUPERSAMPLE`]× into binary coverage which is then box-filtered
//! down. That is the whole anti-aliasing story: no crate, no font, no runtime
//! asset, and the same normalised coordinates work at any badge size the panel
//! turns out to have.
//!
//! Colour is not invented here. The stroke gradient and the canvas are the
//! numbers sampled out of the captain's own reference illustration, and the
//! alert hue is Herdr's own `Palette::peach`.

use crate::app::fleet_signals::FleetSignal;
use crate::app::signal_tray::BadgeState;

/// How much the coverage grid is oversampled before it is filtered down.
///
/// Four is where the marks stop showing stair-stepping on their diagonals — the
/// magnifier's handle in `review` and the strike through `checks` are the two
/// that show it first — and going further costs 16× the fill for no visible
/// gain at these sizes.
const SUPERSAMPLE: u32 = 4;

/// The three ink weights every mark is composed from.
///
/// The split is the whole difference between a badge and a glyph: the
/// silhouette, the interior detail and the highlight get different
/// brightnesses, so the mark still reads as one object when it is 48 pixels
/// tall and the detail has gone soft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    Heavy,
    Mid,
    Light,
}

impl Ink {
    const ALL: [Self; 3] = [Self::Heavy, Self::Mid, Self::Light];

    fn index(self) -> usize {
        match self {
            Self::Heavy => 0,
            Self::Mid => 1,
            Self::Light => 2,
        }
    }

    /// How brightly this weight prints, against the badge's own ink colour.
    fn weight(self) -> f32 {
        match self {
            Self::Heavy => 1.0,
            Self::Mid => 0.80,
            Self::Light => 0.58,
        }
    }
}

/// A straight 8-bit RGBA image, top row first, which is what the graphics
/// pipeline takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    /// Source-over composite of one straight-alpha colour onto one pixel.
    fn blend(&mut self, x: u32, y: u32, rgb: [f32; 3], alpha: f32) {
        if alpha <= 0.0 || x >= self.width || y >= self.height {
            return;
        }
        let alpha = alpha.min(1.0);
        let index = ((y as usize) * (self.width as usize) + x as usize) * 4;
        let dst_a = f32::from(self.pixels[index + 3]) / 255.0;
        let out_a = alpha + dst_a * (1.0 - alpha);
        if out_a <= 0.0 {
            return;
        }
        for (channel, source) in rgb.iter().enumerate() {
            let dst = f32::from(self.pixels[index + channel]) / 255.0;
            let src = source.clamp(0.0, 1.0);
            let out = (src * alpha + dst * dst_a * (1.0 - alpha)) / out_a;
            self.pixels[index + channel] = (out * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        self.pixels[index + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

/// A coverage map in `0.0..=1.0`, one value per output pixel.
#[derive(Debug, Clone)]
struct Coverage {
    width: u32,
    height: u32,
    values: Vec<f32>,
}

impl Coverage {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            values: vec![0.0; (width as usize) * (height as usize)],
        }
    }

    fn at(&self, x: u32, y: u32) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        self.values[(y as usize) * (self.width as usize) + x as usize]
    }

    fn set(&mut self, x: u32, y: u32, value: f32) {
        if x < self.width && y < self.height {
            let index = (y as usize) * (self.width as usize) + x as usize;
            self.values[index] = self.values[index].max(value);
        }
    }

    /// A cheap separable blur, used for the badge's bloom.
    ///
    /// Three box passes, which is the standard approximation of a Gaussian and
    /// is indistinguishable from one at the radii a 64-pixel badge uses.
    fn blurred(&self, radius: u32) -> Self {
        if radius == 0 {
            return self.clone();
        }
        let mut current = self.clone();
        for _ in 0..3 {
            current = current.box_pass(radius, true).box_pass(radius, false);
        }
        current
    }

    fn box_pass(&self, radius: u32, horizontal: bool) -> Self {
        let mut out = Self::new(self.width, self.height);
        let radius = radius as i64;
        for y in 0..self.height {
            for x in 0..self.width {
                let mut sum = 0.0;
                let mut count = 0.0;
                for offset in -radius..=radius {
                    let (sx, sy) = if horizontal {
                        (x as i64 + offset, y as i64)
                    } else {
                        (x as i64, y as i64 + offset)
                    };
                    if sx < 0 || sy < 0 || sx >= self.width as i64 || sy >= self.height as i64 {
                        continue;
                    }
                    sum += self.at(sx as u32, sy as u32);
                    count += 1.0;
                }
                if count > 0.0 {
                    let index = (y as usize) * (self.width as usize) + x as usize;
                    out.values[index] = sum / count;
                }
            }
        }
        out
    }
}

/// The high-resolution binary grid a mark is drawn into before it is filtered.
struct Mask {
    size: u32,
    bits: Vec<bool>,
}

impl Mask {
    fn new(size: u32) -> Self {
        Self {
            size,
            bits: vec![false; (size as usize) * (size as usize)],
        }
    }

    fn mark(&mut self, x: u32, y: u32) {
        if x < self.size && y < self.size {
            self.bits[(y as usize) * (self.size as usize) + x as usize] = true;
        }
    }

    /// Box-filter the oversampled grid down to `size` output pixels.
    ///
    /// This is where the anti-aliasing comes from: the fraction of subsamples
    /// that were inside the shape becomes the pixel's coverage.
    fn resolve(&self, size: u32) -> Coverage {
        let mut out = Coverage::new(size, size);
        let scale = SUPERSAMPLE;
        let per_pixel = (scale * scale) as f32;
        for y in 0..size {
            for x in 0..size {
                let mut hits = 0u32;
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = x * scale + sx;
                        let py = y * scale + sy;
                        if px < self.size
                            && py < self.size
                            && self.bits[(py as usize) * (self.size as usize) + px as usize]
                        {
                            hits += 1;
                        }
                    }
                }
                out.set(x, y, f32::from(hits as u16) / per_pixel);
            }
        }
        out
    }
}

/// Three stacked ink layers over one normalised 1×1 box.
///
/// Every coordinate a mark gives is in `0.0..=1.0`, so the same description
/// draws a 38-pixel badge and a 96-pixel one. The port of the captain's own
/// generator is deliberately literal: these are the numbers that were already
/// looked at and approved in the renders.
struct Pen {
    size: u32,
    masks: [Mask; 3],
}

impl Pen {
    fn new(size: u32) -> Self {
        let hi = size * SUPERSAMPLE;
        Self {
            size,
            masks: [Mask::new(hi), Mask::new(hi), Mask::new(hi)],
        }
    }

    fn hi(&self) -> f32 {
        (self.size * SUPERSAMPLE) as f32
    }

    fn u(&self, value: f32) -> f32 {
        value * self.hi()
    }

    /// A stroke width in device units, never thinner than one subsample.
    fn w(&self, width: f32) -> f32 {
        self.u(width).max(1.0)
    }

    /// Rasterise `inside` over a bounding box, in device coordinates.
    fn fill(
        &mut self,
        ink: Ink,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        inside: impl Fn(f32, f32) -> bool,
    ) {
        let hi = self.hi() as i64;
        let lo_x = (x0.floor() as i64).max(0);
        let lo_y = (y0.floor() as i64).max(0);
        let hi_x = (x1.ceil() as i64).min(hi - 1);
        let hi_y = (y1.ceil() as i64).min(hi - 1);
        let index = ink.index();
        for py in lo_y..=hi_y {
            for px in lo_x..=hi_x {
                let cx = px as f32 + 0.5;
                let cy = py as f32 + 0.5;
                if inside(cx, cy) {
                    self.masks[index].mark(px as u32, py as u32);
                }
            }
        }
    }

    /// A rounded rectangle, filled or stroked inward from its edge.
    fn rrect(&mut self, ink: Ink, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, width: Option<f32>) {
        let (x0, y0, x1, y1) = (self.u(x0), self.u(y0), self.u(x1), self.u(y1));
        let radius = self.u(r).min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
        let stroke = width.map(|width| self.w(width));
        let outer = move |px: f32, py: f32| rrect_contains(px, py, x0, y0, x1, y1, radius);
        match stroke {
            None => self.fill(ink, x0, y0, x1, y1, outer),
            Some(stroke) => {
                let (ix0, iy0, ix1, iy1) = (x0 + stroke, y0 + stroke, x1 - stroke, y1 - stroke);
                let inner_radius = (radius - stroke).max(0.0);
                self.fill(ink, x0, y0, x1, y1, move |px, py| {
                    outer(px, py)
                        && !(ix1 > ix0
                            && iy1 > iy0
                            && rrect_contains(px, py, ix0, iy0, ix1, iy1, inner_radius))
                });
            }
        }
    }

    fn ellipse(&mut self, ink: Ink, cx: f32, cy: f32, rx: f32, ry: f32, width: Option<f32>) {
        let (cx, cy) = (self.u(cx), self.u(cy));
        let (rx, ry) = (self.u(rx), self.u(ry));
        let stroke = width.map(|width| self.w(width)).unwrap_or(0.0);
        let (ix, iy) = ((rx - stroke).max(0.0), (ry - stroke).max(0.0));
        let filled = stroke <= 0.0;
        self.fill(ink, cx - rx, cy - ry, cx + rx, cy + ry, move |px, py| {
            if !ellipse_contains(px, py, cx, cy, rx, ry) {
                return false;
            }
            filled || !ellipse_contains(px, py, cx, cy, ix, iy)
        });
    }

    /// An arc between two angles, measured the way the generator measured them:
    /// degrees, clockwise from three o'clock, with y running downward.
    fn arc(&mut self, ink: Ink, cx: f32, cy: f32, rx: f32, ry: f32, a0: f32, a1: f32, width: f32) {
        let (cx, cy) = (self.u(cx), self.u(cy));
        let (rx, ry) = (self.u(rx), self.u(ry));
        let stroke = self.w(width);
        let (ix, iy) = ((rx - stroke).max(0.0), (ry - stroke).max(0.0));
        self.fill(ink, cx - rx, cy - ry, cx + rx, cy + ry, move |px, py| {
            if !ellipse_contains(px, py, cx, cy, rx, ry) || ellipse_contains(px, py, cx, cy, ix, iy)
            {
                return false;
            }
            let angle = (py - cy).atan2(px - cx).to_degrees();
            angle_between(angle, a0, a1)
        });
    }

    /// A polyline with round caps and joints.
    fn line(&mut self, ink: Ink, points: &[(f32, f32)], width: f32) {
        let half = self.w(width) / 2.0;
        let points: Vec<(f32, f32)> = points
            .iter()
            .map(|(x, y)| (self.u(*x), self.u(*y)))
            .collect();
        for pair in points.windows(2) {
            let (ax, ay) = pair[0];
            let (bx, by) = pair[1];
            self.fill(
                ink,
                ax.min(bx) - half,
                ay.min(by) - half,
                ax.max(bx) + half,
                ay.max(by) + half,
                move |px, py| segment_distance(px, py, ax, ay, bx, by) <= half,
            );
        }
        // Round every joint and cap by hand. The generator did the same, for
        // the same reason: there is no cap style to ask for.
        for (x, y) in points {
            self.fill(
                ink,
                x - half,
                y - half,
                x + half,
                y + half,
                move |px, py| (px - x).hypot(py - y) <= half,
            );
        }
    }

    fn poly(&mut self, ink: Ink, points: &[(f32, f32)]) {
        let points: Vec<(f32, f32)> = points
            .iter()
            .map(|(x, y)| (self.u(*x), self.u(*y)))
            .collect();
        let min_x = points.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        let max_x = points.iter().map(|p| p.0).fold(f32::MIN, f32::max);
        let min_y = points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let max_y = points.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        self.fill(ink, min_x, min_y, max_x, max_y, move |px, py| {
            polygon_contains(&points, px, py)
        });
    }

    fn resolve(&self) -> [Coverage; 3] {
        [
            self.masks[0].resolve(self.size),
            self.masks[1].resolve(self.size),
            self.masks[2].resolve(self.size),
        ]
    }
}

fn rrect_contains(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if px < x0 || px > x1 || py < y0 || py > y1 {
        return false;
    }
    let cx = px.clamp(x0 + r, x1 - r);
    let cy = py.clamp(y0 + r, y1 - r);
    (px - cx).hypot(py - cy) <= r
}

fn ellipse_contains(px: f32, py: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> bool {
    if rx <= 0.0 || ry <= 0.0 {
        return false;
    }
    let dx = (px - cx) / rx;
    let dy = (py - cy) / ry;
    dx * dx + dy * dy <= 1.0
}

/// Whether `angle` lies on the sweep from `a0` to `a1`, both in degrees.
fn angle_between(angle: f32, a0: f32, a1: f32) -> bool {
    let normalise = |value: f32| value.rem_euclid(360.0);
    let start = normalise(a0);
    let sweep = normalise(a1 - a0);
    normalise(angle - start) <= sweep
}

fn segment_distance(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let length_sq = dx * dx + dy * dy;
    if length_sq <= f32::EPSILON {
        return (px - ax).hypot(py - ay);
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / length_sq).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

fn polygon_contains(points: &[(f32, f32)], px: f32, py: f32) -> bool {
    let mut inside = false;
    let mut j = points.len().saturating_sub(1);
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

// ---------------------------------------------------------------------------
// The eight marks
//
// Ported verbatim from the generator whose output was reviewed and approved.
// The numbers are the design; changing one changes a mark that has already been
// looked at, so treat them the way you would treat a checked-in asset.
// ---------------------------------------------------------------------------

/// A question with two answers: a speech bubble carrying two option pips.
///
/// The two pips are the whole point — this is the one badge whose action is
/// answerable in place, and the mark says so before the popup opens.
fn ask(p: &mut Pen) {
    p.rrect(Ink::Heavy, 0.06, 0.12, 0.94, 0.66, 0.16, Some(0.070));
    p.poly(Ink::Heavy, &[(0.26, 0.62), (0.30, 0.93), (0.52, 0.62)]);
    p.rrect(Ink::Light, 0.13, 0.19, 0.87, 0.59, 0.11, Some(0.028));
    p.ellipse(Ink::Mid, 0.37, 0.39, 0.082, 0.082, None);
    p.ellipse(Ink::Mid, 0.63, 0.39, 0.082, 0.082, None);
    p.ellipse(Ink::Light, 0.37, 0.39, 0.145, 0.145, Some(0.024));
    p.ellipse(Ink::Light, 0.63, 0.39, 0.145, 0.145, Some(0.024));
}

/// Finished work waiting to be looked at: a lens over a written card.
fn review(p: &mut Pen) {
    p.rrect(Ink::Mid, 0.04, 0.08, 0.70, 0.80, 0.11, Some(0.055));
    p.line(Ink::Light, &[(0.15, 0.28), (0.58, 0.28)], 0.036);
    p.line(Ink::Light, &[(0.15, 0.42), (0.48, 0.42)], 0.036);
    p.line(Ink::Light, &[(0.15, 0.56), (0.38, 0.56)], 0.036);
    p.line(Ink::Heavy, &[(0.76, 0.72), (0.95, 0.94)], 0.085);
    p.ellipse(Ink::Heavy, 0.62, 0.56, 0.27, 0.27, Some(0.072));
    p.arc(Ink::Light, 0.62, 0.56, 0.17, 0.17, 150.0, 235.0, 0.034);
}

/// A published summary: a folded sheet, three rules, and a struck seal.
fn report(p: &mut Pen) {
    p.line(
        Ink::Heavy,
        &[
            (0.14, 0.05),
            (0.60, 0.05),
            (0.86, 0.29),
            (0.86, 0.82),
            (0.14, 0.82),
            (0.14, 0.05),
        ],
        0.058,
    );
    p.line(Ink::Mid, &[(0.60, 0.05), (0.60, 0.29), (0.86, 0.29)], 0.044);
    p.line(Ink::Light, &[(0.25, 0.42), (0.74, 0.42)], 0.038);
    p.line(Ink::Light, &[(0.25, 0.54), (0.68, 0.54)], 0.038);
    p.line(Ink::Light, &[(0.25, 0.66), (0.55, 0.66)], 0.038);
    p.ellipse(Ink::Heavy, 0.74, 0.80, 0.155, 0.155, None);
    p.ellipse(Ink::Light, 0.74, 0.80, 0.085, 0.085, Some(0.030));
    p.line(Ink::Mid, &[(0.66, 0.92), (0.70, 1.00)], 0.034);
    p.line(Ink::Mid, &[(0.82, 0.92), (0.78, 1.00)], 0.034);
}

/// A worker that is no longer an agent: the plug is out of the socket.
fn stopped(p: &mut Pen) {
    // The socket, still in the wall.
    p.rrect(Ink::Heavy, 0.01, 0.24, 0.30, 0.76, 0.10, Some(0.062));
    p.rrect(Ink::Mid, 0.19, 0.36, 0.26, 0.46, 0.035, None);
    p.rrect(Ink::Mid, 0.19, 0.54, 0.26, 0.64, 0.035, None);
    // The plug, pulled out, prongs still facing it.
    p.rrect(Ink::Heavy, 0.62, 0.22, 0.93, 0.78, 0.14, Some(0.062));
    p.line(Ink::Heavy, &[(0.62, 0.38), (0.46, 0.38)], 0.055);
    p.line(Ink::Heavy, &[(0.62, 0.62), (0.46, 0.62)], 0.055);
    // Its cable, trailing away off the bottom-right.
    p.line(Ink::Mid, &[(0.86, 0.78), (0.92, 0.90), (0.80, 0.99)], 0.046);
    // The break in the gap.
    p.line(Ink::Light, &[(0.36, 0.30), (0.40, 0.20)], 0.032);
    p.line(Ink::Light, &[(0.36, 0.70), (0.40, 0.80)], 0.032);
    p.line(Ink::Light, &[(0.34, 0.50), (0.25, 0.50)], 0.032);
}

/// Local commits that have not left the machine: lifting off a dock.
fn push(p: &mut Pen) {
    p.line(Ink::Heavy, &[(0.50, 0.80), (0.50, 0.32)], 0.095);
    p.poly(Ink::Heavy, &[(0.22, 0.42), (0.50, 0.08), (0.78, 0.42)]);
    p.rrect(Ink::Mid, 0.14, 0.87, 0.86, 0.97, 0.05, None);
    p.line(Ink::Light, &[(0.30, 0.74), (0.30, 0.62)], 0.040);
    p.line(Ink::Light, &[(0.70, 0.74), (0.70, 0.62)], 0.040);
    p.line(Ink::Light, &[(0.36, 0.30), (0.42, 0.24)], 0.030);
}

/// Remote commits you do not have: the heavier crescent comes inward.
fn sync(p: &mut Pen) {
    p.arc(Ink::Heavy, 0.50, 0.50, 0.34, 0.34, 195.0, 340.0, 0.085);
    p.poly(Ink::Heavy, &[(0.90, 0.34), (0.94, 0.62), (0.68, 0.52)]);
    p.arc(Ink::Mid, 0.50, 0.50, 0.34, 0.34, 15.0, 160.0, 0.058);
    p.poly(Ink::Mid, &[(0.10, 0.66), (0.06, 0.38), (0.32, 0.48)]);
    p.ellipse(Ink::Mid, 0.50, 0.50, 0.095, 0.095, None);
    p.ellipse(Ink::Light, 0.50, 0.50, 0.185, 0.185, Some(0.028));
}

/// A pull request: a branch leaves the trunk and rejoins it.
fn pr(p: &mut Pen) {
    p.line(Ink::Heavy, &[(0.24, 0.16), (0.24, 0.84)], 0.070);
    p.arc(Ink::Heavy, 0.24, 0.50, 0.48, 0.30, -78.0, 78.0, 0.062);
    p.ellipse(Ink::Heavy, 0.24, 0.14, 0.115, 0.115, None);
    p.ellipse(Ink::Heavy, 0.24, 0.86, 0.115, 0.115, None);
    p.ellipse(Ink::Mid, 0.70, 0.50, 0.100, 0.100, None);
    p.ellipse(Ink::Light, 0.70, 0.50, 0.175, 0.175, Some(0.028));
    p.poly(Ink::Mid, &[(0.31, 0.66), (0.42, 0.70), (0.34, 0.78)]);
}

/// Check runs on a spine: one passed, one running, one struck out.
fn checks(p: &mut Pen) {
    p.line(Ink::Mid, &[(0.10, 0.14), (0.10, 0.84)], 0.048);
    for (y0, y1) in [(0.05, 0.28), (0.39, 0.62), (0.73, 0.96)] {
        let y = (y0 + y1) / 2.0;
        p.line(Ink::Light, &[(0.10, y), (0.28, y)], 0.036);
    }
    p.rrect(Ink::Mid, 0.28, 0.05, 0.97, 0.28, 0.11, None); // passed: solid
    p.rrect(Ink::Heavy, 0.28, 0.39, 0.97, 0.62, 0.11, Some(0.050)); // running: half
    p.rrect(Ink::Mid, 0.28, 0.39, 0.60, 0.62, 0.11, None);
    p.rrect(Ink::Heavy, 0.28, 0.73, 0.97, 0.96, 0.11, Some(0.050)); // failed: struck
    p.line(Ink::Heavy, &[(0.36, 0.92), (0.89, 0.77)], 0.062);
}

fn draw_mark(signal: FleetSignal, pen: &mut Pen) {
    match signal {
        FleetSignal::Ask => ask(pen),
        FleetSignal::Review => review(pen),
        FleetSignal::Report => report(pen),
        FleetSignal::Stopped => stopped(pen),
        FleetSignal::Push => push(pen),
        FleetSignal::Sync => sync(pen),
        FleetSignal::Pr => pr(pen),
        FleetSignal::Checks => checks(pen),
    }
}

// ---------------------------------------------------------------------------
// Colour
// ---------------------------------------------------------------------------

/// The lit stroke gradient, top to bottom. Sampled out of the captain's own
/// reference illustration rather than invented.
const STROKE_TOP: [f32; 3] = [
    0x7F as f32 / 255.0,
    0xE2 as f32 / 255.0,
    0xE4 as f32 / 255.0,
];
const STROKE_BOTTOM: [f32; 3] = [
    0x7E as f32 / 255.0,
    0xA5 as f32 / 255.0,
    0xD1 as f32 / 255.0,
];

/// How far the double border's inner stroke is inset from the outer one, as a
/// fraction of the badge's side.
const INNER_BORDER_INSET: f32 = 0.075;

/// How strongly the inner stroke prints against the outer one.
const INNER_BORDER_ALPHA: f32 = 0.45;

/// The badge's border, as a fraction of the side.
const BORDER_STROKE: f32 = 0.030;
const BORDER_RADIUS: f32 = 0.13;

/// How much of the badge's side the mark occupies.
const MARK_SCALE: f32 = 0.60;

fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn scale(rgb: [f32; 3], factor: f32) -> [f32; 3] {
    [
        (rgb[0] * factor).clamp(0.0, 1.0),
        (rgb[1] * factor).clamp(0.0, 1.0),
        (rgb[2] * factor).clamp(0.0, 1.0),
    ]
}

/// What one badge is drawn in.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BadgePaint {
    /// The alert hue, taken from the palette rather than invented.
    pub attention: [f32; 3],
    /// The host terminal's own background, which the engraved state is cut
    /// into. Herdr paints no global background fill, so this is what an idle
    /// badge is actually sitting on.
    pub surface: [f32; 3],
}

impl BadgePaint {
    /// The ink for a state, top and bottom of the gradient.
    fn ink(&self, state: BadgeState) -> ([f32; 3], [f32; 3]) {
        match state {
            BadgeState::Attention => (scale(self.attention, 1.08), scale(self.attention, 0.82)),
            BadgeState::Active => (STROKE_TOP, STROKE_BOTTOM),
            // Cut in, not faded out. The mark prints darker than what it sits
            // on, which is what makes an empty slot look empty rather than
            // broken.
            BadgeState::Idle => (scale(self.surface, 0.45), scale(self.surface, 0.45)),
        }
    }

    /// The hairline lift along an engraved mark's lower edge, which is the only
    /// thing that makes a carve read as a carve rather than as a smudge.
    fn lift(&self) -> [f32; 3] {
        [
            (self.surface[0] + 0.12).min(1.0),
            (self.surface[1] + 0.12).min(1.0),
            (self.surface[2] + 0.12).min(1.0),
        ]
    }
}

/// Render one badge into `size`×`size` straight RGBA.
///
/// Everything outside the mark and its border is left fully transparent — that
/// is amendment one, no plate background, and it is why the tray needs no
/// backdrop band behind it.
pub(crate) fn render_badge(
    signal: FleetSignal,
    state: BadgeState,
    size: u32,
    paint: BadgePaint,
) -> Rgba {
    let mut image = Rgba::new(size, size);
    if size == 0 {
        return image;
    }
    let (ink_top, ink_bottom) = paint.ink(state);

    if state.is_live() {
        // Amendment two: the lit badge keeps its double border.
        draw_border(
            &mut image,
            size,
            BORDER_STROKE,
            0.0,
            1.0,
            ink_top,
            ink_bottom,
        );
        draw_border(
            &mut image,
            size,
            BORDER_STROKE * 0.7,
            INNER_BORDER_INSET,
            INNER_BORDER_ALPHA,
            ink_top,
            ink_bottom,
        );
    }

    // The mark itself, centred, at a fixed fraction of the badge.
    let mark_size = ((size as f32) * MARK_SCALE).round().max(1.0) as u32;
    let offset = (size.saturating_sub(mark_size)) / 2;
    let mut pen = Pen::new(mark_size);
    draw_mark(signal, &mut pen);
    let layers = pen.resolve();

    if state.is_live() {
        // The bloom, laid under the mark. Peaks at a fraction of the stroke, so
        // it reads as light coming off the mark rather than as a halo drawn
        // around it.
        let combined = combine(&layers, mark_size);
        let bloom = combined.blurred(((mark_size as f32) * 0.09).round().max(1.0) as u32);
        let strength = if matches!(state, BadgeState::Attention) {
            0.34
        } else {
            0.19
        };
        for y in 0..mark_size {
            for x in 0..mark_size {
                let alpha = bloom.at(x, y) * strength;
                let tint = lerp(ink_top, ink_bottom, y as f32 / mark_size.max(1) as f32);
                image.blend(x + offset, y + offset, tint, alpha);
            }
        }
    }

    for ink in Ink::ALL {
        let layer = &layers[ink.index()];
        for y in 0..mark_size {
            for x in 0..mark_size {
                let coverage = layer.at(x, y);
                if coverage <= 0.0 {
                    continue;
                }
                let t = y as f32 / mark_size.max(1) as f32;
                let tint = lerp(ink_top, ink_bottom, t);
                image.blend(x + offset, y + offset, tint, coverage * ink.weight());
            }
        }
    }

    if matches!(state, BadgeState::Idle) {
        draw_engraved_lift(&mut image, &layers, mark_size, offset, paint.lift());
    }
    if matches!(state, BadgeState::Attention) {
        draw_attention_pip(&mut image, size, paint.attention);
    }
    image
}

fn combine(layers: &[Coverage; 3], size: u32) -> Coverage {
    let mut out = Coverage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let value = layers
                .iter()
                .map(|layer| layer.at(x, y))
                .fold(0.0_f32, f32::max);
            out.set(x, y, value);
        }
    }
    out
}

/// The one-pixel lift along the bottom of every engraved stroke.
///
/// A carve is only legible because of the light that catches its lower lip;
/// without this the idle state is a dark smear on a dark panel.
fn draw_engraved_lift(
    image: &mut Rgba,
    layers: &[Coverage; 3],
    size: u32,
    offset: u32,
    lift: [f32; 3],
) {
    let combined = combine(layers, size);
    for y in 0..size {
        for x in 0..size {
            let here = combined.at(x, y);
            let below = if y + 1 < size {
                combined.at(x, y + 1)
            } else {
                0.0
            };
            let edge = (here - below).max(0.0);
            if edge > 0.15 {
                image.blend(x + offset, y + offset, lift, edge * 0.5);
            }
        }
    }
}

/// The filled pip cut into the top-right corner of a badge that is demanding
/// attention, with its own dark moat so it reads as a separate object rather
/// than as part of the border it overlaps.
fn draw_attention_pip(image: &mut Rgba, size: u32, colour: [f32; 3]) {
    let radius = (size as f32) * 0.115;
    let cx = size as f32 - radius * 1.15;
    let cy = radius * 1.15;
    // Just wide enough to read as a gap. A moat any wider takes a bite out of
    // the border it overlaps, and a badge with a chunk missing from its corner
    // looks damaged rather than escalated.
    let moat = radius + (size as f32) * 0.022;

    // The moat is cleared first, as a hole rather than as ink, because what it
    // has to remove is whatever the border already drew there.
    for y in 0..size {
        for x in 0..size {
            let distance = ((x as f32 + 0.5) - cx).hypot((y as f32 + 0.5) - cy);
            if distance > radius && distance <= moat {
                let index = ((y as usize) * (image.width as usize) + x as usize) * 4;
                image.pixels[index + 3] = 0;
            }
        }
    }
    for y in 0..size {
        for x in 0..size {
            let distance = ((x as f32 + 0.5) - cx).hypot((y as f32 + 0.5) - cy);
            // A one-pixel feather, so the pip's own rim is not a staircase.
            let alpha = (radius + 0.5 - distance).clamp(0.0, 1.0);
            image.blend(x, y, colour, alpha);
        }
    }
}

/// One rounded-rectangle stroke around the badge.
fn draw_border(
    image: &mut Rgba,
    size: u32,
    stroke: f32,
    inset: f32,
    alpha: f32,
    top: [f32; 3],
    bottom: [f32; 3],
) {
    let side = size as f32;
    let stroke = (stroke * side).max(1.0);
    let inset = inset * side;
    let (x0, y0) = (inset + stroke * 0.5, inset + stroke * 0.5);
    let (x1, y1) = (side - inset - stroke * 0.5, side - inset - stroke * 0.5);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let radius = (BORDER_RADIUS * side - inset).max(1.0);
    let inner_radius = (radius - stroke).max(0.0);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let outside = !rrect_contains(px, py, x0, y0, x1, y1, radius);
            let inside = rrect_contains(
                px,
                py,
                x0 + stroke,
                y0 + stroke,
                x1 - stroke,
                y1 - stroke,
                inner_radius,
            );
            if outside || inside {
                continue;
            }
            let tint = lerp(top, bottom, py / side);
            image.blend(x, y, tint, alpha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paint() -> BadgePaint {
        BadgePaint {
            attention: [250.0 / 255.0, 179.0 / 255.0, 135.0 / 255.0],
            surface: [0.05, 0.07, 0.11],
        }
    }

    fn opaque_pixels(image: &Rgba) -> usize {
        image.pixels.chunks(4).filter(|px| px[3] > 8).count()
    }

    /// Amendment one, asserted: the badge has no plate behind it, so the
    /// corners of its box stay transparent and the tray surface shows through.
    #[test]
    fn a_badge_has_no_plate_behind_it() {
        for signal in FleetSignal::ALL {
            let image = render_badge(signal, BadgeState::Active, 48, paint());
            // The extreme corner is outside the rounded border on every badge.
            let corner = &image.pixels[0..4];
            assert_eq!(corner[3], 0, "{signal:?} painted its own top-left corner");
        }
    }

    /// Every mark has to actually draw something, at every state, or a slot is
    /// silently empty in the shipped tray.
    #[test]
    fn every_mark_draws_ink_in_every_state() {
        for signal in FleetSignal::ALL {
            for state in [BadgeState::Idle, BadgeState::Active, BadgeState::Attention] {
                let image = render_badge(signal, state, 48, paint());
                assert!(
                    opaque_pixels(&image) > 40,
                    "{signal:?} in {state:?} drew almost nothing"
                );
            }
        }
    }

    /// The property that makes these badges rather than glyphs: no two of the
    /// eight may share a silhouette. This is the failure the whole exercise
    /// exists to avoid — `checks` and `report` collapsing into one shape.
    #[test]
    fn no_two_marks_share_a_silhouette() {
        let masks: Vec<Vec<bool>> = FleetSignal::ALL
            .into_iter()
            .map(|signal| {
                render_badge(signal, BadgeState::Active, 48, paint())
                    .pixels
                    .chunks(4)
                    .map(|px| px[3] > 96)
                    .collect()
            })
            .collect();

        for (i, left) in masks.iter().enumerate() {
            for (j, right) in masks.iter().enumerate().skip(i + 1) {
                let differing = left.iter().zip(right).filter(|(a, b)| a != b).count();
                assert!(
                    differing > left.len() / 20,
                    "{:?} and {:?} differ in only {differing} pixels",
                    FleetSignal::ALL[i],
                    FleetSignal::ALL[j],
                );
            }
        }
    }

    /// A lit badge is a container; an idle one is a carve in the surface. The
    /// border is what says which, so an idle badge must not draw one.
    #[test]
    fn only_a_lit_badge_draws_its_border() {
        let lit = render_badge(FleetSignal::Ask, BadgeState::Active, 48, paint());
        let idle = render_badge(FleetSignal::Ask, BadgeState::Idle, 48, paint());
        assert!(
            opaque_pixels(&lit) > opaque_pixels(&idle),
            "the lit badge did not add its border"
        );

        // The border runs around the badge's rim, where the mark never reaches:
        // the mark occupies the middle `MARK_SCALE` of the box.
        assert!(rim_ink(&lit) > 0, "a lit badge drew no border on its rim");
        assert_eq!(rim_ink(&idle), 0, "an idle badge drew a border");
    }

    /// Ink in the outer eighth of the badge, which only the border can reach.
    fn rim_ink(image: &Rgba) -> usize {
        let rim = (image.width / 8).max(1);
        let mut count = 0;
        for y in 0..image.height {
            for x in 0..image.width {
                let on_rim =
                    x < rim || y < rim || x >= image.width - rim || y >= image.height - rim;
                let index = ((y as usize) * (image.width as usize) + x as usize) * 4;
                if on_rim && image.pixels[index + 3] > 8 {
                    count += 1;
                }
            }
        }
        count
    }

    /// Attention adds a filled pip in the top-right corner with a dark moat
    /// around it, which is the one state difference that survives a reader who
    /// cannot tell the two hues apart.
    #[test]
    fn attention_cuts_a_pip_into_the_corner() {
        let corner_ink = |state| {
            let image = render_badge(FleetSignal::Ask, state, 48, paint());
            let mut count = 0;
            for y in 0..12 {
                for x in (image.width - 12)..image.width {
                    let index = ((y as usize) * (image.width as usize) + x as usize) * 4;
                    if image.pixels[index + 3] > 200 {
                        count += 1;
                    }
                }
            }
            count
        };
        assert!(
            corner_ink(BadgeState::Attention) > corner_ink(BadgeState::Active) + 40,
            "the attention pip is not visibly filling the corner"
        );
    }

    /// The marks are normalised, so the same description has to survive every
    /// badge size the panel can hand it — including the smallest one the ladder
    /// will ever choose.
    #[test]
    fn a_mark_survives_every_size_the_ladder_can_ask_for() {
        for size in [24, 32, 48, 64, 96] {
            let image = render_badge(FleetSignal::Checks, BadgeState::Active, size, paint());
            assert_eq!(image.width, size);
            assert_eq!(image.pixels.len(), (size as usize) * (size as usize) * 4);
            assert!(opaque_pixels(&image) > 10, "nothing drawn at {size}px");
        }
    }

    #[test]
    fn a_zero_sized_badge_is_empty_rather_than_a_panic() {
        let image = render_badge(FleetSignal::Ask, BadgeState::Active, 0, paint());
        assert!(image.pixels.is_empty());
    }
}
