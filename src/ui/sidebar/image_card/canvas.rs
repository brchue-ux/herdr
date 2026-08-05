//! A straight-alpha RGBA canvas and the two shapes the card is made of.
//!
//! The card is a rounded rectangle drawn four times over — as a bloom outside
//! it, a fill inside it, a stroke on its boundary, and an inner glow just
//! within it — so every one of those is a function of the *same* signed
//! distance to that rectangle. One distance field, four reads: the stroke
//! cannot drift off the fill's edge and the bloom cannot start somewhere the
//! stroke does not end, because none of them has its own idea of where the
//! boundary is.
//!
//! Antialiasing is coverage from that distance rather than supersampling. The
//! prototype supersampled 3–4×, which costs 9–16 times the fill rate for a
//! shape whose exact coverage is already known analytically.

/// 8-bit sRGB, the space every sampled constant is quoted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// `self` moved `t` of the way toward `other`.
    pub(super) fn mix(self, other: Self, t: f32) -> Self {
        let t = t.clamp(0.0, 1.0);
        let lerp = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round();
        Self(
            lerp(self.0, other.0).clamp(0.0, 255.0) as u8,
            lerp(self.1, other.1).clamp(0.0, 255.0) as u8,
            lerp(self.2, other.2).clamp(0.0, 255.0) as u8,
        )
    }

    /// The same colour at `sat` of its saturation and `lum` of its lightness.
    ///
    /// This is the reference's own state mechanism: an inactive card holds the
    /// hue and gives up saturation and light. Going through HSL rather than
    /// scaling the channels is what keeps the hue exactly where it was.
    pub(super) fn restate(self, sat: f32, lum: f32) -> Self {
        let (h, s, l) = self.to_hsl();
        Self::from_hsl(h, (s * sat).clamp(0.0, 1.0), (l * lum).clamp(0.0, 1.0))
    }

    fn to_hsl(self) -> (f32, f32, f32) {
        let (r, g, b) = (
            f32::from(self.0) / 255.0,
            f32::from(self.1) / 255.0,
            f32::from(self.2) / 255.0,
        );
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let l = (max + min) / 2.0;
        if (max - min).abs() < f32::EPSILON {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if (max - r).abs() < f32::EPSILON {
            ((g - b) / d).rem_euclid(6.0)
        } else if (max - g).abs() < f32::EPSILON {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h * 60.0, s, l)
    }

    /// A colour named the way the reference's own family is quoted.
    ///
    /// The chip's four inks are given in HSL because that is the space the one
    /// hue family is a *statement* in: H 181–210 across every state, and only
    /// S and L moving. Spelling them as RGB would hide the invariant.
    pub(super) fn from_hsl(h: f32, s: f32, l: f32) -> Self {
        let h = h.rem_euclid(360.0);
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = l - c / 2.0;
        let (r, g, b) = match (h / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let to8 = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
        Self(to8(r), to8(g), to8(b))
    }
}

/// An RGBA image with straight (non-premultiplied) alpha.
///
/// Straight rather than premultiplied because everything drawn here composites
/// source-over onto what is already there, and the result is handed to a PNG
/// encoder that wants straight alpha anyway.
#[derive(Clone)]
pub(super) struct Canvas {
    width: u32,
    height: u32,
    /// `RGBA8`, straight alpha — the PNG encoder's own layout, so finishing the
    /// sheet is a borrow rather than a conversion pass.
    ///
    /// Eight bits per channel rather than a float accumulator: a pixel takes at
    /// most a handful of blends (the backdrop, the bloom, the fill, the inner
    /// glow, the stroke, a glyph), so the rounding cannot compound past a level
    /// or two, and a float buffer would be four times the memory for a sheet
    /// that is already the tallest image Herdr ever builds.
    px: Vec<u8>,
}

impl Canvas {
    pub(super) fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            px: vec![0; (width as usize) * (height as usize) * 4],
        }
    }

    pub(super) fn width(&self) -> u32 {
        self.width
    }

    pub(super) fn height(&self) -> u32 {
        self.height
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| ((y as usize) * (self.width as usize) + (x as usize)) * 4)
    }

    /// Composite `color` at `alpha` over the pixel, source-over.
    pub(super) fn blend(&mut self, x: u32, y: u32, color: Rgb, alpha: f32) {
        let alpha = alpha.clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let Some(i) = self.index(x, y) else {
            return;
        };
        // Opaque source over anything, and any source over an opaque
        // destination, are both a plain lerp with no divide — and between the
        // backdrop and the card fill that is most of the pixels this draws.
        if alpha >= 1.0 {
            self.px[i] = color.0;
            self.px[i + 1] = color.1;
            self.px[i + 2] = color.2;
            self.px[i + 3] = 255;
            return;
        }
        let dst_a = f32::from(self.px[i + 3]) / 255.0;
        if dst_a >= 1.0 {
            for (channel, src) in [color.0, color.1, color.2].into_iter().enumerate() {
                let dst = f32::from(self.px[i + channel]);
                self.px[i + channel] = (f32::from(src) * alpha + dst * (1.0 - alpha))
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            return;
        }
        let out_a = alpha + dst_a * (1.0 - alpha);
        if out_a <= 0.0 {
            return;
        }
        for (channel, src) in [color.0, color.1, color.2].into_iter().enumerate() {
            let src = f32::from(src);
            let dst = f32::from(self.px[i + channel]);
            self.px[i + channel] = ((src * alpha + dst * dst_a * (1.0 - alpha)) / out_a)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        self.px[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    /// Straight RGBA8, ready for a PNG encoder.
    pub(super) fn rgba8(&self) -> &[u8] {
        &self.px
    }

    /// Scale the alpha of every pixel in a rectangle, leaving colour alone.
    ///
    /// The one operation that takes an already-drawn surface *apart*. It is
    /// alpha rather than a mix toward the backdrop for the reason the sheet is
    /// transparent in the first place: the tree's own connectors and Space rows
    /// are drawn as characters underneath, so a card coming apart has to let
    /// them back through rather than paint over them in the panel's colour.
    ///
    /// Colour is deliberately untouched. Straight alpha means a pixel at
    /// `a = 0.3` is the same ink at three tenths of its presence, so a particle
    /// dissolving keeps the card's hue all the way out instead of desaturating
    /// toward grey on its way.
    pub(super) fn scale_alpha(&mut self, x0: u32, y0: u32, x1: u32, y1: u32, factor: f32) {
        let factor = factor.clamp(0.0, 1.0);
        if factor >= 1.0 {
            return;
        }
        let x1 = x1.min(self.width);
        let y1 = y1.min(self.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let Some(i) = self.index(x, y) else {
                    continue;
                };
                self.px[i + 3] = (f32::from(self.px[i + 3]) * factor)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// A rounded rectangle in pixel space, and the distance to its boundary.
#[derive(Debug, Clone, Copy)]
pub(super) struct RoundRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

impl RoundRect {
    /// Signed distance from `(px, py)` to the boundary: negative inside,
    /// positive outside, in pixels either way.
    pub(super) fn distance(&self, px: f32, py: f32) -> f32 {
        let hx = self.w / 2.0;
        let hy = self.h / 2.0;
        let r = self.r.min(hx).min(hy).max(0.0);
        let dx = (px - (self.x + hx)).abs() - (hx - r);
        let dy = (py - (self.y + hy)).abs() - (hy - r);
        let outside = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt();
        outside + dx.max(dy).min(0.0) - r
    }
}

/// Coverage of a pixel whose centre is `d` from the boundary, inside positive.
///
/// The half-pixel ramp is the analytic answer for an edge crossing a pixel and
/// is what replaces the prototype's supersampling.
pub(super) fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}
