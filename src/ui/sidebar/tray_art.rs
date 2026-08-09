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

/// The four layers every mark is composed from: three that print, one that
/// takes away.
///
/// The split is the whole difference between a badge and a glyph. Three
/// brightnesses let the silhouette, the interior detail and the highlight read
/// as one object rather than as one stroke; and [`Ink::Cut`] is what lets a
/// mark be drawn as **mass rather than as line**.
///
/// ## Why a cut layer exists at all
///
/// The three printing weights all lay down the *same hue* — they differ only in
/// alpha, and they composite source-over, so a lighter weight painted on top of
/// a heavier one does not read as a detail inside it, it reads as more of the
/// same ink. That is fine for an outline drawing, where every stroke sits on
/// bare surface. It is useless for a solid one, and solid is what these marks
/// now are: a filled speech bubble has nowhere for a pip to go except *through*
/// it. `Cut` removes coverage after the three have printed, so an interior
/// detail is negative space showing the tray surface — the same thing the
/// badge's own transparent corners already are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    Heavy,
    Mid,
    Light,
    /// Removes what the three printing layers put down. Never prints.
    Cut,
}

impl Ink {
    /// The layers that print, heaviest first. Deliberately not every variant:
    /// [`Ink::Cut`] is applied after these, as a subtraction.
    const ALL: [Self; 3] = [Self::Heavy, Self::Mid, Self::Light];

    /// How many mask layers a pen carries.
    const LAYERS: usize = 4;

    fn index(self) -> usize {
        match self {
            Self::Heavy => 0,
            Self::Mid => 1,
            Self::Light => 2,
            Self::Cut => 3,
        }
    }

    /// How brightly this weight prints, against the badge's own ink colour.
    fn weight(self) -> f32 {
        match self {
            Self::Heavy => 1.0,
            Self::Mid => 0.80,
            Self::Light => 0.58,
            Self::Cut => 0.0,
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

    /// This coverage read `dy` output pixels further down than it was drawn, so
    /// a positive `dy` lifts the mark on screen.
    ///
    /// Interpolated between the two rows the sample falls between rather than
    /// rounded to one of them. That is what makes a three-pixel travel read as
    /// movement instead of as three steps, and it is why the badge needs no
    /// sub-cell placement plumbing to move smoothly: the motion happens inside
    /// the image, where a fraction of a pixel is expressible.
    fn at_shifted(&self, x: u32, y: u32, dy: f32) -> f32 {
        let source = y as f32 + dy;
        let base = source.floor();
        let frac = source - base;
        let row = |value: f32| {
            if value < 0.0 {
                0.0
            } else {
                self.at(x, value as u32)
            }
        };
        let lo = row(base);
        lo + (row(base + 1.0) - lo) * frac
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
    masks: [Mask; Ink::LAYERS],
}

impl Pen {
    fn new(size: u32) -> Self {
        let hi = size * SUPERSAMPLE;
        Self {
            size,
            masks: [Mask::new(hi), Mask::new(hi), Mask::new(hi), Mask::new(hi)],
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

    fn resolve(&self) -> [Coverage; Ink::LAYERS] {
        [
            self.masks[0].resolve(self.size),
            self.masks[1].resolve(self.size),
            self.masks[2].resolve(self.size),
            self.masks[3].resolve(self.size),
        ]
    }
}

fn rrect_contains(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if px < x0 || px > x1 || py < y0 || py > y1 {
        return false;
    }
    // The band of corner centres on each axis. It collapses to a single point
    // when the radius is exactly half the extent — a pill — and [`Pen::rrect`]
    // clamps the radius to exactly that, so the collapse is a shape the
    // catalogue actually draws rather than a pathological input.
    //
    // The upper bound is held at or above the lower one because the two are
    // computed by different arithmetic (`x0 + r` against `x1 - r`) and f32
    // rounding can order them backwards at the collapse: a badge drawn at a
    // real 11x21 px cell lands on 45.08 against 45.079998, and `f32::clamp`
    // panics on an inverted range instead of returning the point. A cell that
    // is 8x16 because nothing measured the terminal never reaches it, so this
    // only became reachable once clients started using the cell they have.
    let cx = px.clamp(x0 + r, (x1 - r).max(x0 + r));
    let cy = py.clamp(y0 + r, (y1 - r).max(y0 + r));
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
// Drawn for MASS, not for line. Every one of the eight is a solid silhouette
// with its detail cut out of it, rather than an outline with strokes inside it,
// and they are all built from the same three weights so the grid reads as one
// object rather than as eight unrelated drawings.
//
// The recipe each mark follows, and the reason it is a recipe rather than eight
// separate decisions:
//
// 1. **One dominant body**, filled, occupying roughly a third of the mark box.
//    That is what survives at 20 pixels, where a 0.03-wide stroke is a third of
//    a pixel and disappears.
// 2. **Its detail cut out of it** ([`Ink::Cut`]), never drawn on top. A hole
//    reads at every size a shape does, because it *is* the shape.
// 3. **A secondary mass in `Mid`**, and a highlight in `Light`, both sized from
//    the shared constants below rather than per mark — that is the whole of
//    "consistent in weight", and `all_eight_marks_carry_a_comparable_mass`
//    is the check that keeps it true.
//
// The numbers below are the design. `no_two_marks_share_a_silhouette` and the
// mass band are what a change to one of them has to keep satisfying.
// ---------------------------------------------------------------------------

/// The one weight any cut detail is drawn at, as a fraction of the mark box.
///
/// Stated once and used by all eight, because eight marks that each chose their
/// own detail weight is exactly how a grid stops reading as one object. Wide
/// enough to still be a visible hole at the smallest badge the ladder asks for.
const CUT: f32 = 0.075;

/// The heavier cut, for a detail carrying meaning rather than texture — the
/// strike through a failed check, the break across a pulled plug.
const CUT_BOLD: f32 = 0.105;

/// Corner rounding on a solid body, as a fraction of the mark box.
const BODY_R: f32 = 0.12;

/// A question with two answers: a solid speech bubble with two pips cut out.
///
/// The two pips are the whole point — this is the one badge whose action is
/// answerable in place, and the mark says so before the popup opens. They are
/// holes rather than dots because a dot painted on a filled bubble is the same
/// colour as the bubble.
fn ask(p: &mut Pen) {
    p.rrect(Ink::Heavy, 0.05, 0.09, 0.95, 0.68, 0.17, None);
    p.poly(Ink::Heavy, &[(0.25, 0.62), (0.28, 0.96), (0.53, 0.62)]);
    // The two answers.
    p.ellipse(Ink::Cut, 0.365, 0.385, 0.105, 0.105, None);
    p.ellipse(Ink::Cut, 0.635, 0.385, 0.105, 0.105, None);
    // The lit top edge, which is what stops a solid slab reading as flat.
    p.rrect(Ink::Light, 0.13, 0.15, 0.87, 0.21, 0.03, None);
}

/// Finished work waiting to be looked at: a solid page under a solid lens.
fn review(p: &mut Pen) {
    p.rrect(Ink::Mid, 0.02, 0.06, 0.66, 0.86, BODY_R, None);
    // The writing on it, cut rather than drawn.
    p.line(Ink::Cut, &[(0.13, 0.26), (0.55, 0.26)], CUT);
    p.line(Ink::Cut, &[(0.13, 0.44), (0.47, 0.44)], CUT);
    p.line(Ink::Cut, &[(0.13, 0.62), (0.38, 0.62)], CUT);
    // The lens: one solid disc, its glass cut back out, sitting over the page.
    p.line(Ink::Heavy, &[(0.72, 0.70), (0.96, 0.97)], 0.135);
    p.ellipse(Ink::Heavy, 0.63, 0.55, 0.315, 0.315, None);
    p.ellipse(Ink::Cut, 0.63, 0.55, 0.185, 0.185, None);
    // The catchlight on the glass.
    p.arc(Ink::Light, 0.63, 0.55, 0.255, 0.255, 165.0, 230.0, 0.075);
}

/// A published summary: a solid folded sheet with its rules cut out and a seal.
fn report(p: &mut Pen) {
    p.poly(
        Ink::Heavy,
        &[
            (0.11, 0.03),
            (0.60, 0.03),
            (0.89, 0.31),
            (0.89, 0.84),
            (0.11, 0.84),
        ],
    );
    // The fold, cut so the corner reads as turned rather than as printed on.
    p.poly(Ink::Cut, &[(0.60, 0.05), (0.87, 0.31), (0.60, 0.31)]);
    p.line(Ink::Cut, &[(0.23, 0.45), (0.75, 0.45)], CUT);
    p.line(Ink::Cut, &[(0.23, 0.60), (0.68, 0.60)], CUT);
    // The seal, overlapping the sheet's lower edge, with its centre cut.
    p.ellipse(Ink::Mid, 0.72, 0.83, 0.185, 0.185, None);
    p.ellipse(Ink::Cut, 0.72, 0.83, 0.070, 0.070, None);
    p.poly(Ink::Mid, &[(0.63, 0.94), (0.72, 0.92), (0.66, 1.00)]);
    p.poly(Ink::Mid, &[(0.81, 0.94), (0.72, 0.92), (0.78, 1.00)]);
}

/// A worker that is no longer an agent: the plug is out of the socket.
fn stopped(p: &mut Pen) {
    // The socket, still in the wall: one solid block with two slots cut in it.
    p.rrect(Ink::Heavy, 0.00, 0.20, 0.31, 0.80, BODY_R, None);
    p.rrect(Ink::Cut, 0.15, 0.33, 0.25, 0.44, 0.03, None);
    p.rrect(Ink::Cut, 0.15, 0.56, 0.25, 0.67, 0.03, None);
    // The plug, pulled out, prongs still facing it.
    p.rrect(Ink::Heavy, 0.60, 0.18, 0.96, 0.82, 0.16, None);
    p.rrect(Ink::Heavy, 0.47, 0.30, 0.62, 0.41, 0.04, None);
    p.rrect(Ink::Heavy, 0.47, 0.59, 0.62, 0.70, 0.04, None);
    // Its cable, trailing away off the bottom.
    p.line(Ink::Mid, &[(0.86, 0.82), (0.93, 0.93), (0.79, 1.00)], 0.085);
    // The break across the gap: the one detail that says *disconnected* rather
    // than *about to connect*, so it gets the heavier cut weight.
    p.line(Ink::Light, &[(0.34, 0.62), (0.44, 0.38)], CUT_BOLD);
}

/// Local commits that have not left the machine: lifting off a dock.
fn push(p: &mut Pen) {
    // Head and shaft as one solid body, so there is no seam to come apart at
    // small sizes.
    p.poly(
        Ink::Heavy,
        &[
            (0.50, 0.04),
            (0.86, 0.42),
            (0.65, 0.42),
            (0.65, 0.76),
            (0.35, 0.76),
            (0.35, 0.42),
            (0.14, 0.42),
        ],
    );
    // The notch, which is what stops a solid arrow reading as a triangle on a
    // bar.
    p.rrect(Ink::Cut, 0.44, 0.48, 0.56, 0.70, 0.03, None);
    // The dock it is leaving.
    p.rrect(Ink::Mid, 0.10, 0.86, 0.90, 0.99, 0.05, None);
}

/// Remote commits you do not have: two solid crescents turning inward.
fn sync(p: &mut Pen) {
    p.arc(Ink::Heavy, 0.50, 0.50, 0.40, 0.40, 200.0, 345.0, 0.155);
    p.poly(Ink::Heavy, &[(0.95, 0.28), (0.99, 0.64), (0.64, 0.53)]);
    p.arc(Ink::Mid, 0.50, 0.50, 0.40, 0.40, 20.0, 165.0, 0.155);
    p.poly(Ink::Mid, &[(0.05, 0.72), (0.01, 0.36), (0.36, 0.47)]);
    // The hub, with its centre cut so the middle of the mark is not a blob.
    p.ellipse(Ink::Light, 0.50, 0.50, 0.135, 0.135, None);
    p.ellipse(Ink::Cut, 0.50, 0.50, 0.055, 0.055, None);
}

/// A pull request: a branch leaves the solid trunk and rejoins it.
fn pr(p: &mut Pen) {
    p.rrect(Ink::Heavy, 0.14, 0.10, 0.32, 0.90, 0.09, None);
    p.arc(Ink::Heavy, 0.23, 0.50, 0.52, 0.33, -74.0, 74.0, 0.115);
    // The nodes, each a solid disc with its centre cut, so all three read as
    // the same kind of thing.
    for (cx, cy) in [(0.23, 0.13), (0.23, 0.87)] {
        p.ellipse(Ink::Cut, cx, cy, 0.052, 0.052, None);
    }
    p.ellipse(Ink::Mid, 0.72, 0.50, 0.185, 0.185, None);
    p.ellipse(Ink::Cut, 0.72, 0.50, 0.070, 0.070, None);
    // Direction: the branch is going somewhere.
    p.poly(Ink::Light, &[(0.34, 0.63), (0.48, 0.68), (0.36, 0.79)]);
}

/// Check runs on a spine: one passed, one part way, one struck out.
///
/// Three pills of identical mass, told apart only by what is cut out of them —
/// which is the whole reason this mark does not collapse into `report`.
fn checks(p: &mut Pen) {
    p.rrect(Ink::Mid, 0.02, 0.06, 0.16, 0.94, 0.06, None);
    let pills = [(0.04_f32, 0.28_f32), (0.39, 0.63), (0.74, 0.98)];
    for (index, (y0, y1)) in pills.into_iter().enumerate() {
        p.rrect(Ink::Heavy, 0.24, y0, 0.99, y1, 0.11, None);
        // The tie back to the spine.
        p.rrect(
            Ink::Mid,
            0.10,
            (y0 + y1) / 2.0 - 0.035,
            0.26,
            (y0 + y1) / 2.0 + 0.035,
            0.02,
            None,
        );
        match index {
            // Passed: whole.
            0 => {}
            // Part way: the unfinished half cut back out.
            1 => p.rrect(Ink::Cut, 0.60, y0 + 0.045, 0.945, y1 - 0.045, 0.06, None),
            // Failed: struck through.
            _ => p.line(
                Ink::Cut,
                &[(0.30, y1 - 0.035), (0.93, y0 + 0.035)],
                CUT_BOLD,
            ),
        }
    }
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

// ---------------------------------------------------------------------------
// Motion
//
// What the animation engine's envelope does to a badge's pixels. The *shape* of
// that envelope — the exponential ramp, the ten percent overshoot, the five
// percent back — belongs to `anim::behaviour::Curve::SnapPendulum` and is not
// re-expressed here. This module only says what the number means once it
// arrives: how far the mark travels, how far its light comes up.
// ---------------------------------------------------------------------------

/// How far a lit badge's mark travels on its snap, as a fraction of the badge.
///
/// Small, and deliberately so: eight badges each swinging a tenth of their own
/// height is a grid that shakes. What has to be legible is *that* it moved and
/// with what character, and at a 50-pixel badge this is three pixels resolved
/// to a fraction of one.
const LIT_TRAVEL: f32 = 0.055;

/// The same for an escalated badge. Further, because the state is louder.
const ALERT_TRAVEL: f32 = 0.085;

/// How much brighter a lit badge's ink runs at the top of its snap.
const LIT_GAIN: f32 = 0.30;

/// The ambient glow a resting badge carries before its breath modulates it.
///
/// This is the "back burner" reading, and it is a glow rather than a dimmer on
/// purpose — the target says *recessed*, which is a depth cue. A faint halo in
/// the surface's own lifted colour puts the mark slightly behind the panel
/// instead of merely darker than it.
const REST_GLOW: f32 = 0.09;

/// How far the resting breath swings that glow, as a fraction of itself.
const REST_GLOW_SWING: f32 = 0.6;

/// How deeply a resting badge's carve breathes.
///
/// The carve getting fractionally deeper and shallower is the whole of rest's
/// motion. Anything more would be a resting badge asking for attention, which
/// is the one thing rest must not do.
const REST_CARVE_SWING: f32 = 0.10;

/// How far the mark travels at `amount`, in pixels of a `size`-pixel badge.
///
/// Zero for [`BadgeState::Idle`] — and that zero is a *contract*, not an
/// omission. Rest is told from lit by whether the mark travels at all, which is
/// the one distinction that survives a reader who cannot separate two hues.
fn travel(state: BadgeState, size: u32, amount: f32) -> f32 {
    let fraction = match state {
        BadgeState::Idle => return 0.0,
        BadgeState::Active => LIT_TRAVEL,
        BadgeState::Attention => ALERT_TRAVEL,
    };
    amount * (size as f32) * fraction
}

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
///
/// Serializable because a client that rasterises the tray itself is handed
/// exactly this — two resolved colours — rather than the palette and host
/// theme they were resolved from. See [`super::tray::TrayScene`].
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// One mark, rasterised: the layers it prints, and the halo they throw.
///
/// Held apart from [`render_badge`] and cached, because rasterising a mark is
/// the expensive half of a badge and it does not depend on the badge's state or
/// on where it is in its animation. Without this, a moving tray would re-run
/// the supersampled vector rasteriser eight times per frame — which is the
/// difference between motion that costs a fraction of a millisecond and motion
/// that costs more than a whole frame.
struct MarkArt {
    /// Coverage per printing weight, already reduced by the cut layer, so a
    /// consumer never has to remember to subtract.
    layers: [Coverage; 3],
    /// The union of the three, for the halo and the engraved lift.
    combined: Coverage,
    /// That union, blurred.
    bloom: Coverage,
}

/// How many rasterised marks are kept.
///
/// Eight signals across the two or three badge sizes one session's panel
/// widths produce. Well above that so a divider drag does not thrash it, and
/// small enough that a pathological caller cannot grow it without bound — over
/// the cap it is cleared rather than evicted one at a time, because the sizes
/// move together when they move at all.
const MARK_CACHE_CAP: usize = 48;

thread_local! {
    static MARK_CACHE: std::cell::RefCell<
        std::collections::HashMap<(FleetSignal, u32), std::rc::Rc<MarkArt>>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

impl MarkArt {
    fn rasterise(signal: FleetSignal, size: u32) -> Self {
        let mut pen = Pen::new(size);
        draw_mark(signal, &mut pen);
        let resolved = pen.resolve();
        let cut = &resolved[Ink::Cut.index()];

        // The cut is folded in here, once, rather than at every read: a mark's
        // negative space is a property of the mark, not of how it is being
        // drawn this frame.
        let reduce = |layer: &Coverage| {
            let mut out = Coverage::new(size, size);
            for y in 0..size {
                for x in 0..size {
                    out.set(x, y, (layer.at(x, y) - cut.at(x, y)).max(0.0));
                }
            }
            out
        };
        let layers = [
            reduce(&resolved[0]),
            reduce(&resolved[1]),
            reduce(&resolved[2]),
        ];

        let mut combined = Coverage::new(size, size);
        for y in 0..size {
            for x in 0..size {
                let value = layers
                    .iter()
                    .map(|layer| layer.at(x, y))
                    .fold(0.0_f32, f32::max);
                combined.set(x, y, value);
            }
        }
        let bloom = combined.blurred(((size as f32) * 0.09).round().max(1.0) as u32);
        Self {
            layers,
            combined,
            bloom,
        }
    }
}

/// This mark at this size, rasterising it only the first time it is asked for.
fn mark_art(signal: FleetSignal, size: u32) -> std::rc::Rc<MarkArt> {
    MARK_CACHE.with(|cache| {
        // A poisoned or already-borrowed cache is a reason to do the work
        // again, never a reason to fail: the badge is a decoration and a
        // decoration must not be able to break the thing it decorates.
        let Ok(mut cache) = cache.try_borrow_mut() else {
            return std::rc::Rc::new(MarkArt::rasterise(signal, size));
        };
        if let Some(found) = cache.get(&(signal, size)) {
            return std::rc::Rc::clone(found);
        }
        if cache.len() >= MARK_CACHE_CAP {
            cache.clear();
        }
        let art = std::rc::Rc::new(MarkArt::rasterise(signal, size));
        cache.insert((signal, size), std::rc::Rc::clone(&art));
        art
    })
}

/// Render one badge into `size`×`size` straight RGBA.
///
/// `amount` is the animation engine's envelope for this badge, read through
/// [`crate::anim::behaviour::Behaviour::strength`]: `0.0` at rest, `1.0` at the
/// snap's target, and up to about `1.10` through its overshoot. A caller with
/// no engine — a test, a still tray — passes `0.0` and gets exactly the settled
/// badge, which is what makes the animation an addition rather than a rewrite.
///
/// Everything outside the mark and its border is left fully transparent — that
/// is amendment one, no plate background, and it is why the tray needs no
/// backdrop band behind it.
pub(crate) fn render_badge(
    signal: FleetSignal,
    state: BadgeState,
    size: u32,
    paint: BadgePaint,
    amount: f32,
) -> Rgba {
    let mut image = Rgba::new(size, size);
    if size == 0 {
        return image;
    }
    let amount = if amount.is_finite() {
        amount.clamp(0.0, 2.0)
    } else {
        0.0
    };
    let (ink_top, ink_bottom) = paint.ink(state);
    // The snap brightens as it rises. Rest does not: its light is carried by
    // the halo below, not by the ink, because a carve that got brighter would
    // stop reading as a carve.
    let gain = if state.is_live() {
        1.0 + amount * LIT_GAIN
    } else {
        1.0
    };
    let (ink_top, ink_bottom) = (scale(ink_top, gain), scale(ink_bottom, gain));

    if state.is_live() {
        // Amendment two: the lit badge keeps its double border. The border does
        // not travel — it is the container, and the mark snaps *inside* it.
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
    let art = mark_art(signal, mark_size);
    let lift = travel(state, size, amount);
    let span = mark_size.max(1) as f32;

    // The halo, laid under the mark and travelling with it. On a lit badge it
    // is light coming off the mark; on a resting one it is the recession — the
    // faint, breathing depth cue that reads as "on the back burner" instead of
    // as "switched off".
    let (halo, halo_tint) = if state.is_live() {
        let peak = if matches!(state, BadgeState::Attention) {
            0.34
        } else {
            0.19
        };
        (peak * (0.55 + 0.65 * amount), None)
    } else {
        (
            REST_GLOW * (1.0 - REST_GLOW_SWING + REST_GLOW_SWING * amount),
            Some(paint.lift()),
        )
    };
    if halo > 0.0 {
        for y in 0..mark_size {
            for x in 0..mark_size {
                let alpha = art.bloom.at_shifted(x, y, lift) * halo;
                let tint = halo_tint.unwrap_or_else(|| lerp(ink_top, ink_bottom, y as f32 / span));
                image.blend(x + offset, y + offset, tint, alpha);
            }
        }
    }

    // A resting carve deepens and shallows with the same breath, which is the
    // only thing rest does that a still badge does not.
    let carve = if state.is_live() {
        1.0
    } else {
        1.0 - REST_CARVE_SWING * (1.0 - amount)
    };

    for ink in Ink::ALL {
        let layer = &art.layers[ink.index()];
        for y in 0..mark_size {
            for x in 0..mark_size {
                let coverage = layer.at_shifted(x, y, lift);
                if coverage <= 0.0 {
                    continue;
                }
                let tint = lerp(ink_top, ink_bottom, y as f32 / span);
                image.blend(
                    x + offset,
                    y + offset,
                    tint,
                    coverage * ink.weight() * carve,
                );
            }
        }
    }

    if matches!(state, BadgeState::Idle) {
        draw_engraved_lift(&mut image, &art.combined, mark_size, offset, paint.lift());
    }
    if matches!(state, BadgeState::Attention) {
        draw_attention_pip(&mut image, size, paint.attention);
    }
    image
}

/// The one-pixel lift along the bottom of every engraved stroke.
///
/// A carve is only legible because of the light that catches its lower lip;
/// without this the idle state is a dark smear on a dark panel.
fn draw_engraved_lift(
    image: &mut Rgba,
    combined: &Coverage,
    size: u32,
    offset: u32,
    lift: [f32; 3],
) {
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

    /// A badge at the bottom of its animation — the settled artwork.
    ///
    /// Everything that is not *about* the motion asserts against this, which is
    /// also the proof that the animation is an addition: a caller with no
    /// engine gets exactly the badge that shipped before it existed.
    fn settled(signal: FleetSignal, state: BadgeState, size: u32) -> Rgba {
        render_badge(signal, state, size, paint(), 0.0)
    }

    fn opaque_pixels(image: &Rgba) -> usize {
        image.pixels.chunks(4).filter(|px| px[3] > 8).count()
    }

    /// Where the mark's ink sits vertically, in output pixels.
    ///
    /// The alpha-weighted centroid rather than a bounding box, because the
    /// motion this measures is fractions of a pixel: a box edge quantises to
    /// whole pixels and would report a smooth travel as three steps.
    ///
    /// Measured over the mark's own box, and with the escalation pip's corner
    /// left out. Both the border and the pip are anchored by design — the mark
    /// snaps *inside* its container — so a window that included them would
    /// average a real travel against two things that never move and report the
    /// louder state as the quieter one.
    fn ink_centroid_y(image: &Rgba) -> f32 {
        let side = image.width as f32;
        let mark = (side * MARK_SCALE).round() as u32;
        let lo = (image.width.saturating_sub(mark)) / 2;
        let hi = (lo + mark).min(image.width);
        // Rows are taken wider than the mark's own box so that ink lifted out
        // of it is still counted. A window clipped to the box would lose the
        // very pixels the travel moved and report the longer travel as the
        // shorter one.
        let rim = image.height / 8;
        let (top, bottom) = (rim, image.height.saturating_sub(rim));
        // Everything the pip and its moat can reach, from `draw_attention_pip`.
        let pip_reach = side * 0.115 * 2.4;
        let mut weight = 0.0_f32;
        let mut total = 0.0_f32;
        for y in top..bottom {
            for x in lo..hi {
                if (side - x as f32) < pip_reach && (y as f32) < pip_reach {
                    continue;
                }
                let index = ((y as usize) * (image.width as usize) + x as usize) * 4;
                let alpha = f32::from(image.pixels[index + 3]) / 255.0;
                weight += alpha * y as f32;
                total += alpha;
            }
        }
        if total <= 0.0 {
            return 0.0;
        }
        weight / total
    }

    /// Amendment one, asserted: the badge has no plate behind it, so the
    /// corners of its box stay transparent and the tray surface shows through.
    #[test]
    fn a_badge_has_no_plate_behind_it() {
        for signal in FleetSignal::ALL {
            let image = settled(signal, BadgeState::Active, 48);
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
                let image = settled(signal, state, 48);
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
                settled(signal, BadgeState::Active, 48)
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
        let lit = settled(FleetSignal::Ask, BadgeState::Active, 48);
        let idle = settled(FleetSignal::Ask, BadgeState::Idle, 48);
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
            let image = settled(FleetSignal::Ask, state, 48);
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
            let image = settled(FleetSignal::Checks, BadgeState::Active, size);
            assert_eq!(image.width, size);
            assert_eq!(image.pixels.len(), (size as usize) * (size as usize) * 4);
            assert!(opaque_pixels(&image) > 10, "nothing drawn at {size}px");
        }
    }

    #[test]
    fn a_zero_sized_badge_is_empty_rather_than_a_panic() {
        let image = settled(FleetSignal::Ask, BadgeState::Active, 0);
        assert!(image.pixels.is_empty());
    }

    /// Ink inside the mark's own box, as a fraction of that box.
    ///
    /// Measured over the middle [`MARK_SCALE`] of the badge so the border,
    /// which every lit badge draws identically, cannot flatter a thin mark into
    /// looking as heavy as a solid one.
    fn mark_mass(signal: FleetSignal) -> f32 {
        let size = 64u32;
        let image = settled(signal, BadgeState::Idle, size);
        let mark = ((size as f32) * MARK_SCALE).round() as u32;
        let offset = (size - mark) / 2;
        let mut inked = 0.0_f32;
        for y in offset..offset + mark {
            for x in offset..offset + mark {
                let index = ((y as usize) * (image.width as usize) + x as usize) * 4;
                if image.pixels[index + 3] > 96 {
                    inked += 1.0;
                }
            }
        }
        inked / (mark * mark) as f32
    }

    /// Drawn for mass, not for line — and *consistently* so.
    ///
    /// This is the acceptance criterion made checkable. An outline drawing
    /// covers a few percent of its box; a solid one covers a third of it. The
    /// band is what stops the grid drifting back to eight unrelated weights one
    /// well-meaning tweak at a time: no mark may be thinner than a fifth of its
    /// box, and none may be more than twice as heavy as the lightest.
    #[test]
    fn all_eight_marks_carry_a_comparable_mass() {
        let masses: Vec<(FleetSignal, f32)> = FleetSignal::ALL
            .into_iter()
            .map(|signal| (signal, mark_mass(signal)))
            .collect();

        for (signal, mass) in &masses {
            assert!(
                *mass > 0.20,
                "{signal:?} covers only {:.1}% of its box — that is a line drawing, not a mass",
                mass * 100.0
            );
        }
        let lightest = masses.iter().map(|(_, m)| *m).fold(f32::MAX, f32::min);
        let heaviest = masses.iter().map(|(_, m)| *m).fold(0.0_f32, f32::max);
        assert!(
            heaviest <= lightest * 2.0,
            "the grid is not one object: heaviest {heaviest:.3} against lightest {lightest:.3} in {masses:?}"
        );
    }

    /// Drawing for mass only works if the detail is cut *out* of the mass.
    ///
    /// A mark whose cut layer did nothing would still pass the mass band — it
    /// would just be a solid blob. This asserts the holes are really holes:
    /// removing the cut has to leave a measurably heavier mark.
    #[test]
    fn a_cut_takes_ink_away_rather_than_adding_it() {
        for signal in [
            FleetSignal::Ask,
            FleetSignal::Review,
            FleetSignal::Report,
            FleetSignal::Checks,
        ] {
            let art = MarkArt::rasterise(signal, 48);
            let mut pen = Pen::new(48);
            draw_mark(signal, &mut pen);
            let raw = pen.resolve();

            let sum = |layer: &Coverage| -> f32 { layer.values.iter().sum() };
            let cut = sum(&raw[Ink::Cut.index()]);
            assert!(cut > 1.0, "{signal:?} cut nothing out of itself");

            let before: f32 = (0..3).map(|i| sum(&raw[i])).sum();
            let after: f32 = art.layers.iter().map(sum).sum();
            assert!(
                after < before,
                "{signal:?} kept all {before:.1} of its ink after a cut of {cut:.1}"
            );
        }
    }

    /// The state ladder in pixels: a lit badge's mark travels, a resting one
    /// never does.
    ///
    /// This is the behavioural half of "distinguishable by behaviour, not
    /// colour alone" as it lands on the image. The other half — that the three
    /// states play three different catalogue behaviours at three different
    /// tempos — is asserted in `crate::app::signal_tray`.
    #[test]
    fn only_a_lit_badge_travels() {
        let at = |state, amount| {
            ink_centroid_y(&render_badge(FleetSignal::Ask, state, 64, paint(), amount))
        };

        // Rest breathes, but it does not move. A resting tray that drifted
        // would be eight things asking for attention at once.
        let rest_low = at(BadgeState::Idle, 0.0);
        let rest_high = at(BadgeState::Idle, 1.0);
        assert!(
            (rest_high - rest_low).abs() < 0.05,
            "a resting badge travelled {:.3} px",
            rest_high - rest_low
        );

        for state in [BadgeState::Active, BadgeState::Attention] {
            let settled = at(state, 0.0);
            let snapped = at(state, 1.0);
            assert!(
                settled - snapped > 0.5,
                "{state:?} moved only {:.3} px between rest and its snap",
                settled - snapped
            );
        }

        // And the two live states do not travel the same distance, so an
        // escalation is legible from the motion alone.
        let active = at(BadgeState::Active, 0.0) - at(BadgeState::Active, 1.0);
        let alert = at(BadgeState::Attention, 0.0) - at(BadgeState::Attention, 1.0);
        assert!(
            alert > active,
            "attention travelled {alert:.3} px against active's {active:.3} px"
        );
    }

    /// How far each state travels, asserted on the contract rather than on the
    /// image.
    ///
    /// Separate from `only_a_lit_badge_travels` on purpose. A badge's *measured*
    /// centroid also moves with its halo, which grows with the state, so the
    /// image damps the ratio between two travels even though both are exactly
    /// what was asked for. The image is the right place to prove the mark moved
    /// at all; this is the right place to prove by how much.
    #[test]
    fn escalating_lengthens_the_travel_and_rest_has_none() {
        let size = 64;
        assert_eq!(travel(BadgeState::Idle, size, 1.0), 0.0);
        let active = travel(BadgeState::Active, size, 1.0);
        let alert = travel(BadgeState::Attention, size, 1.0);
        assert!(
            active > 0.0 && alert > active * 1.4,
            "{active} then {alert}"
        );

        // And the travel is a straight scaling of the envelope, so the whole of
        // the motion's character — the ramp, the overshoot, the swing back —
        // arrives from the curve rather than being reshaped here.
        for amount in [0.25_f32, 0.5, 1.0, 1.1] {
            let scaled = travel(BadgeState::Active, size, amount);
            assert!(
                (scaled - active * amount).abs() < 1e-3,
                "the travel bent the envelope at {amount}: {scaled} against {}",
                active * amount
            );
        }
    }

    /// The overshoot has to reach the pixels, or the curve is decoration in a
    /// unit test and nothing on screen.
    ///
    /// At the top of the snap the engine hands over an envelope above `1.0`,
    /// and the mark has to be *further* than its target — that is what a
    /// pendulum overshooting looks like from the outside.
    #[test]
    fn the_overshoot_carries_the_mark_past_its_target() {
        let at = |amount| {
            ink_centroid_y(&render_badge(
                FleetSignal::Push,
                BadgeState::Active,
                64,
                paint(),
                amount,
            ))
        };
        let target = at(1.0);
        let overshot = at(1.10);
        let reversed = at(0.95);
        assert!(
            overshot < target,
            "the overshoot did not carry past the target: {overshot:.3} against {target:.3}"
        );
        assert!(
            reversed > target,
            "the reverse swing did not fall back: {reversed:.3} against {target:.3}"
        );
    }

    /// A resting badge is *present*, not switched off.
    ///
    /// The back-burner reading is a glow that breathes, so rest has to put
    /// visibly more light on the badge at the top of its breath than at the
    /// bottom — without ever reaching what a lit badge does.
    #[test]
    fn rest_glows_without_demanding() {
        let light = |state, amount| -> f32 {
            render_badge(FleetSignal::Review, state, 64, paint(), amount)
                .pixels
                .chunks(4)
                .map(|px| f32::from(px[3]))
                .sum::<f32>()
        };
        let low = light(BadgeState::Idle, 0.0);
        let high = light(BadgeState::Idle, 1.0);
        assert!(
            high > low * 1.01,
            "a resting badge did not breathe: {low:.0} to {high:.0}"
        );
        assert!(
            high < light(BadgeState::Active, 0.0),
            "a resting badge is putting out more light than a lit one at rest"
        );
    }

    /// The cache must not change what is drawn — only how long it takes.
    #[test]
    fn a_cached_mark_draws_the_same_badge_as_a_cold_one() {
        MARK_CACHE.with(|cache| {
            if let Ok(mut cache) = cache.try_borrow_mut() {
                cache.clear();
            }
        });
        let cold = settled(FleetSignal::Sync, BadgeState::Active, 48);
        let warm = settled(FleetSignal::Sync, BadgeState::Active, 48);
        assert_eq!(cold, warm);
    }

    /// A pill — a rounded rect whose radius is exactly half its extent — is a
    /// shape the catalogue draws, not a pathological input.
    ///
    /// `Pen::rrect` clamps the radius to half the shorter side, so the two
    /// corner-centre bounds meet; computed as `x0 + r` and `x1 - r` they can
    /// meet a rounding error apart and in the wrong order. That used to reach
    /// `f32::clamp` as an inverted range and abort the process. It is only
    /// reachable at some badge sizes, and the badge size follows the host
    /// terminal's cell — so a client pinned to an assumed 8x16 never found it
    /// and one using the cell it actually has does.
    #[test]
    fn a_pill_shaped_badge_mark_does_not_panic_at_any_size() {
        for size in 8u32..=96 {
            for signal in FleetSignal::ALL {
                for state in [BadgeState::Idle, BadgeState::Active, BadgeState::Attention] {
                    let _ = render_badge(signal, state, size, paint(), 0.5);
                }
            }
        }
    }

    /// The degenerate band itself, straight at the predicate.
    ///
    /// A pill's two corner-centre bounds are the same point, reached by
    /// different arithmetic. The scan is over the device coordinates
    /// `Pen::rrect` derives at real badge sizes -- `pr`'s own 0.14/0.32
    /// horizontal span against its 0.09 radius -- so a case it finds is a badge
    /// that can be drawn, not one invented to break the arithmetic.
    #[test]
    fn rrect_contains_survives_a_collapsed_corner_band() {
        // The exactly-equal case, which was always fine.
        assert!(rrect_contains(2.0, 5.0, 0.0, 0.0, 4.0, 20.0, 2.0));

        let mut inverted = 0;
        for size in 1..4_000u32 {
            let hi = size as f32;
            let (x0, x1) = (0.14 * hi, 0.32 * hi);
            let r = (0.09 * hi).min((x1 - x0) / 2.0);
            if x0 + r > x1 - r {
                inverted += 1;
                // Answers rather than aborting, and answers correctly: the
                // band's own point is inside the shape that spans it.
                assert!(rrect_contains(x0 + r, (x0 + x1) / 2.0, x0, x0, x1, x1, r));
            }
        }
        assert!(
            inverted > 0,
            "no f32-inverted corner band in the scanned range; \
             this test is no longer testing anything"
        );
    }
}
