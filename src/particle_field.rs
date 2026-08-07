//! Pure particle-field frame generator for the "Rung 2" background fidelity level chosen in
//! `data/decisions/2026-08-06-particle-fidelity-rung.md`: 13k particles, depth of field, bloom,
//! flat dark background. `(phase, w, h, cfg) -> RGBA8`, no I/O, no terminal — matches the
//! project's "state is separated from runtime / render is pure" principle, so it is testable
//! without PTYs or async.
//!
//! Ported from the feasibility scout's `field2.rs` prototype (kept as reference material at
//! `data/herdr-particle-feasibility/field2.rs` in the firstmate home, not part of this repo) and
//! trimmed to the levers Rung 2 actually uses. The prototype's nebula/glow wash is not
//! reproduced here: the captain's decision explicitly picked the flat-background rung, and the
//! glow was measured to cost 3.5x the entire particle field on its own.
//!
//! Wired into the sidebar's ambient wash behind `[experimental] sidebar_particle_field` (see
//! `src/ui/sidebar/particle_background.rs`), which calls [`loop_frames`] once per sidebar size
//! and hands the sequence to Kitty's native animation-frame transport
//! (`src/kitty_graphics.rs`) so the terminal plays it back without further per-frame uploads.
//!
//! Measured on this module (`cargo test --release --bin herdr particle_field::tests::profile_cost_curve
//! -- --ignored --nocapture`), Rung 2 (13k particles, dof, bloom) at its shipped 480x312 wire size:
//! generation is 7.1ms (well under the report's 19ms estimate — this generator renders directly
//! at the target resolution instead of full-res-then-downscale). Wire size does not confirm the
//! report's 32.8 KB: that number assumed a true ~32-entry colour palette (indexed PNG), which
//! `PaneGraphicsFormat` (`src/api/schema/panes.rs`) cannot carry today — only `Png | Rgb | Rgba`.
//! The lever this module can actually apply, 5-bit-per-channel quantization via
//! [`quantize_channels`], measures 112.2 KB (1122 KB/s @ 10fps) instead — real, but ~3.4x the
//! report's figure. Rung 2 stays affordable either way (an order of magnitude under the
//! full-resolution-with-glow ceiling), but sizing decisions on top of this generator should use
//! 1122 KB/s, not 328 KB/s, until indexed-PNG output exists.

use std::f32::consts::PI;

const MAX_SPLAT_RADIUS: i32 = 7;

/// Background floor colour measured from the reference field (deep plum, not black).
const BG_FLOOR: (f32, f32, f32) = (20.0 / 255.0, 19.0 / 255.0, 32.0 / 255.0);

/// Fidelity configuration for one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cfg {
    pub particles: usize,
    pub dof: bool,
    pub bloom: bool,
}

impl Cfg {
    /// The captain's chosen fidelity rung: 13k particles, depth of field, bloom.
    pub const fn rung2() -> Self {
        Cfg {
            particles: 13_000,
            dof: true,
            bloom: true,
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }

    fn f(&mut self) -> f32 {
        self.next() as f32 / u32::MAX as f32
    }
}

/// A generated particle model plus the scratch buffers needed to splat and tone-map it. Create
/// once per (width, height, particle count) and call [`Field::frame`] per animation tick.
pub struct Field {
    w: usize,
    h: usize,
    acc: Vec<f32>,
    /// kernels[r] = normalised splat kernel of radius r, size (2r+1)^2. Energy-conserving: a
    /// larger radius spreads the same total light, so out-of-focus particles dim automatically.
    kernels: Vec<Vec<f32>>,
    px: Vec<f32>,
    py: Vec<f32>,
    pz: Vec<f32>,
    pb: Vec<f32>,
    pt: Vec<f32>,
}

impl Field {
    pub fn new(w: usize, h: usize, particles: usize) -> Self {
        let kernels = build_kernels();

        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        // `.max(1)` only guards the division below from a zero denominator; the loop bound stays
        // `particles` so `particles == 0` genuinely produces an empty field.
        let denom = particles.max(1) as f32;
        let (mut px, mut py, mut pz, mut pb, mut pt) = (
            Vec::with_capacity(particles),
            Vec::with_capacity(particles),
            Vec::with_capacity(particles),
            Vec::with_capacity(particles),
            Vec::with_capacity(particles),
        );
        for i in 0..particles {
            let t = i as f32 / denom;
            let kind = i % 8;
            let turns = 3.2;
            let u = t * turns * 2.0 * PI;
            let (cx, cy, cz, bright, temp);
            if kind < 3 {
                // strand A: dense, bright, tight to the curve
                let j = (rng.f() - 0.5) * 0.055;
                cx = u.cos() + j;
                cz = u.sin() + (rng.f() - 0.5) * 0.055;
                cy = t * 2.0 - 1.0;
                bright = 0.45 + rng.f() * rng.f() * 1.8;
                temp = 0.55 + rng.f() * 0.45;
            } else if kind < 6 {
                // strand B: 180 degrees out of phase
                let a = u + PI;
                cx = a.cos() + (rng.f() - 0.5) * 0.055;
                cz = a.sin() + (rng.f() - 0.5) * 0.055;
                cy = t * 2.0 - 1.0;
                bright = 0.45 + rng.f() * rng.f() * 1.8;
                temp = 0.55 + rng.f() * 0.45;
            } else if kind == 6 {
                // rungs bridging the two strands
                let s = rng.f();
                let ax = u.cos();
                let az = u.sin();
                cx = ax * (1.0 - 2.0 * s);
                cz = az * (1.0 - 2.0 * s);
                cy = t * 2.0 - 1.0 + (rng.f() - 0.5) * 0.01;
                bright = 0.30 + rng.f() * rng.f() * 1.1;
                temp = 0.35 + rng.f() * 0.4;
            } else {
                // ambient dust drifting around the field
                let r = 1.25 + rng.f() * 1.5;
                let a = rng.f() * 2.0 * PI;
                cx = a.cos() * r;
                cz = a.sin() * r;
                cy = (rng.f() * 2.0 - 1.0) * 1.25;
                bright = 0.12 + rng.f() * rng.f() * 0.85;
                temp = 0.2 + rng.f() * 0.5;
            }
            px.push(cx);
            py.push(cy);
            pz.push(cz);
            pb.push(bright);
            pt.push(temp);
        }

        Field {
            w,
            h,
            acc: vec![0.0; w * h * 3],
            kernels,
            px,
            py,
            pz,
            pb,
            pt,
        }
    }

    #[inline]
    fn splat(&mut self, cx: f32, cy: f32, r: i32, cr: f32, cg: f32, cb: f32) {
        let ix = cx.floor() as i32;
        let iy = cy.floor() as i32;
        let (w, h) = (self.w as i32, self.h as i32);
        if ix < r || iy < r || ix >= w - r || iy >= h - r {
            return;
        }
        let k = 2 * r + 1;
        for dy in -r..=r {
            let row = ((iy + dy) as usize) * self.w;
            let ko = ((dy + r) * k) as usize;
            for dx in -r..=r {
                let kv = self.kernels[r as usize][ko + (dx + r) as usize];
                let o = (row + (ix + dx) as usize) * 3;
                self.acc[o] += cr * kv;
                self.acc[o + 1] += cg * kv;
                self.acc[o + 2] += cb * kv;
            }
        }
    }

    fn bloom(&mut self, div: usize) {
        let d = div;
        let (bw, bh) = (self.w / d, self.h / d);
        if bw < 8 || bh < 8 {
            return;
        }
        let mut lo = vec![0.0f32; bw * bh * 3];
        for y in 0..bh {
            for x in 0..bw {
                let o = (y * bw + x) * 3;
                let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
                for sy in 0..d {
                    for sx in 0..d {
                        let so = ((y * d + sy) * self.w + (x * d + sx)) * 3;
                        r += self.acc[so];
                        g += self.acc[so + 1];
                        b += self.acc[so + 2];
                    }
                }
                let inv = 1.0 / (d * d) as f32;
                // Only bright cores bloom (threshold), like a real glare filter.
                let l = (r + g + b) * inv / 3.0;
                let kf = ((l - 0.16).max(0.0) * 2.2).min(3.0);
                lo[o] = r * inv * kf;
                lo[o + 1] = g * inv * kf;
                lo[o + 2] = b * inv * kf;
            }
        }
        let mut buf = vec![0.0f32; bw * bh * 3];
        let rad = 5i32;
        for _ in 0..3 {
            for y in 0..bh {
                for x in 0..bw {
                    let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0.0);
                    for dx in -rad..=rad {
                        let xx = x as i32 + dx;
                        if xx < 0 || xx >= bw as i32 {
                            continue;
                        }
                        let o = (y * bw + xx as usize) * 3;
                        r += lo[o];
                        g += lo[o + 1];
                        b += lo[o + 2];
                        n += 1.0;
                    }
                    let o = (y * bw + x) * 3;
                    buf[o] = r / n;
                    buf[o + 1] = g / n;
                    buf[o + 2] = b / n;
                }
            }
            for y in 0..bh {
                for x in 0..bw {
                    let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0.0);
                    for dy in -rad..=rad {
                        let yy = y as i32 + dy;
                        if yy < 0 || yy >= bh as i32 {
                            continue;
                        }
                        let o = (yy as usize * bw + x) * 3;
                        r += buf[o];
                        g += buf[o + 1];
                        b += buf[o + 2];
                        n += 1.0;
                    }
                    let o = (y * bw + x) * 3;
                    lo[o] = r / n;
                    lo[o + 1] = g / n;
                    lo[o + 2] = b / n;
                }
            }
        }
        for y in 0..self.h {
            let sy = (y / d).min(bh - 1);
            for x in 0..self.w {
                let sx = (x / d).min(bw - 1);
                let so = (sy * bw + sx) * 3;
                let o = (y * self.w + x) * 3;
                self.acc[o] += lo[so] * 0.55;
                self.acc[o + 1] += lo[so + 1] * 0.55;
                self.acc[o + 2] += lo[so + 2] * 0.55;
            }
        }
    }

    /// Render one frame at the given rotation phase. `out` is resized to `w * h * 4` RGBA8.
    pub fn frame(&mut self, cfg: &Cfg, phase: f32, out: &mut Vec<u8>) {
        for v in self.acc.iter_mut() {
            *v = 0.0;
        }

        let (w, h) = (self.w as f32, self.h as f32);
        let scale = h * 0.30;
        let (ox, oy) = (w * 0.52, h * 0.50);
        let (s, c) = phase.sin_cos();
        // Tilt the field so it sweeps diagonally across frame.
        let tilt = 0.42f32;
        let (ts, tc) = tilt.sin_cos();
        let focal = 0.35f32; // focal plane in rotated z

        let n = self.px.len();
        for i in 0..n {
            let (x0, y0, z0) = (self.px[i], self.py[i], self.pz[i]);
            let xr = x0 * c - z0 * s;
            let zr = x0 * s + z0 * c;
            let sxp = xr * tc - y0 * ts;
            let syp = xr * ts + y0 * tc;

            let persp = 1.0 / (2.75 - zr * 0.62);
            let sx = ox + sxp * scale * persp * 2.35;
            let sy = oy + syp * scale * persp * 1.9;

            let depth = (zr + 1.0) * 0.5; // 0 = far, 1 = near
            let r = if cfg.dof {
                let blur = (zr - focal).abs();
                (1.0 + blur * 3.4)
                    .round()
                    .clamp(1.0, MAX_SPLAT_RADIUS as f32) as i32
            } else {
                2
            };
            let b = self.pb[i] * (0.22 + 0.95 * depth) * persp * 14.0;
            let t = self.pt[i] * (0.45 + 0.55 * depth);
            let cr = b * (0.72 + 0.28 * t);
            let cg = b * (0.30 + 0.52 * t);
            let cb = b * (0.11 + 0.36 * t);
            self.splat(sx, sy, r, cr, cg, cb);
        }

        if cfg.bloom {
            self.bloom(4);
        }

        out.clear();
        out.resize(self.w * self.h * 4, 255);
        let (fr, fg, fb) = BG_FLOOR;
        for i in 0..self.w * self.h {
            let o = i * 3;
            let tm = |v: f32, floor: f32| -> u8 {
                let t = v / (1.0 + v * 0.92); // Reinhard, gentle shoulder
                let q = ((t.powf(0.90) + floor) * 255.0) as i32;
                q.clamp(0, 255) as u8
            };
            let d = i * 4;
            out[d] = tm(self.acc[o], fr);
            out[d + 1] = tm(self.acc[o + 1], fg);
            out[d + 2] = tm(self.acc[o + 2], fb);
        }
    }
}

fn build_kernels() -> Vec<Vec<f32>> {
    let mut kernels = Vec::with_capacity(MAX_SPLAT_RADIUS as usize + 1);
    for r in 0..=MAX_SPLAT_RADIUS {
        let k = 2 * r + 1;
        let mut v = vec![0.0f32; (k * k) as usize];
        // Tiny r -> tight gaussian (sharp speck). Large r -> flat-ish disc (bokeh).
        let sigma = if r <= 1 { 0.62 } else { r as f32 * 0.55 };
        let mut sum = 0.0;
        for dy in -r..=r {
            for dx in -r..=r {
                let d2 = (dx * dx + dy * dy) as f32;
                let g = if r >= 4 {
                    // Out-of-focus: soft-edged disc, energy on the rim like real bokeh.
                    let d = d2.sqrt() / r as f32;
                    if d > 1.0 {
                        0.0
                    } else {
                        (1.0 - d * d * d * d).max(0.0) * (0.65 + 0.35 * d)
                    }
                } else {
                    (-d2 / (2.0 * sigma * sigma)).exp()
                };
                v[((dy + r) * k + (dx + r)) as usize] = g;
                sum += g;
            }
        }
        // Normalise so total energy is constant across radii.
        if sum > 0.0 {
            for x in v.iter_mut() {
                *x /= sum;
            }
        }
        kernels.push(v);
    }
    kernels
}

/// Reduce each RGB channel (not alpha) to `bits` bits of precision in place. This is the
/// per-channel colour-depth lever measured in the feasibility report; herdr's `PaneGraphicsFormat`
/// does not carry indexed/palette PNG today (see `src/ghostty/mod.rs`, which already rejects
/// `png::ColorType::Indexed` on decode), so this approximates the wire-size win of a smaller
/// palette without requiring a new encode path.
pub fn quantize_channels(rgba: &mut [u8], bits: u8) {
    debug_assert!((1..=8).contains(&bits));
    let mask = (0xFFu16 << (8 - bits)) as u8;
    for px in rgba.chunks_exact_mut(4) {
        px[0] &= mask;
        px[1] &= mask;
        px[2] &= mask;
    }
}

/// Renders one full seamless loop as `frame_count` evenly-spaced phase samples over a full
/// rotation: `phase.sin_cos()` (in [`Field::frame`]) returns to its starting value every 2π, so
/// the step from the last frame back to frame 0 is the same size as every other step.
///
/// Generation only, same as [`Field::frame`] — the caller owns transmitting the sequence and
/// arming playback.
pub fn loop_frames(w: usize, h: usize, cfg: &Cfg, frame_count: usize) -> Vec<Vec<u8>> {
    let frame_count = frame_count.max(1);
    let mut field = Field::new(w, h, cfg.particles);
    (0..frame_count)
        .map(|i| {
            let phase = (i as f32 / frame_count as f32) * 2.0 * PI;
            let mut out = Vec::new();
            field.frame(cfg, phase, &mut out);
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_has_correct_rgba_length() {
        let cfg = Cfg::rung2();
        let mut field = Field::new(480, 312, cfg.particles);
        let mut out = Vec::new();
        field.frame(&cfg, 0.0, &mut out);
        assert_eq!(out.len(), 480 * 312 * 4);
    }

    #[test]
    fn frame_alpha_is_opaque() {
        let cfg = Cfg::rung2();
        let mut field = Field::new(64, 64, 200);
        let mut out = Vec::new();
        field.frame(&cfg, 0.0, &mut out);
        assert!(out.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn frame_is_deterministic_for_same_phase() {
        let cfg = Cfg::rung2();
        let mut field = Field::new(200, 150, 500);
        let mut a = Vec::new();
        let mut b = Vec::new();
        field.frame(&cfg, 0.7, &mut a);
        field.frame(&cfg, 0.7, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn frame_changes_across_phase() {
        let cfg = Cfg::rung2();
        let mut field = Field::new(200, 150, 500);
        let mut a = Vec::new();
        let mut b = Vec::new();
        field.frame(&cfg, 0.0, &mut a);
        field.frame(&cfg, 1.5, &mut b);
        assert_ne!(
            a, b,
            "rotating the phase should move at least one particle's splat"
        );
    }

    #[test]
    fn frame_does_not_panic_across_sizes() {
        let cfg = Cfg::rung2();
        // Sidebar width, Rung 2's wire size, and a size too small for the bloom downsample to
        // apply at all (bloom() bails out under 8px in its low-res buffer).
        for (w, h) in [(312, 625), (480, 312), (16, 16), (1, 1)] {
            let mut field = Field::new(w, h, 300);
            let mut out = Vec::new();
            field.frame(&cfg, 0.42, &mut out);
            assert_eq!(out.len(), w * h * 4);
        }
    }

    #[test]
    fn zero_particles_is_a_flat_background() {
        let cfg = Cfg {
            particles: 0,
            dof: true,
            bloom: true,
        };
        let mut field = Field::new(32, 32, cfg.particles);
        let mut out = Vec::new();
        field.frame(&cfg, 0.0, &mut out);
        let expected = [
            (BG_FLOOR.0 * 255.0) as u8,
            (BG_FLOOR.1 * 255.0) as u8,
            (BG_FLOOR.2 * 255.0) as u8,
        ];
        for px in out.chunks_exact(4) {
            assert_eq!([px[0], px[1], px[2]], expected);
        }
    }

    #[test]
    fn quantize_channels_masks_low_bits_and_preserves_alpha() {
        let mut rgba = vec![0b1111_1111, 0b0000_0111, 0b1010_1010, 42];
        quantize_channels(&mut rgba, 5);
        assert_eq!(rgba, vec![0b1111_1000, 0b0000_0000, 0b1010_1000, 42]);
    }

    #[test]
    fn loop_frames_returns_requested_count_at_correct_size() {
        let cfg = Cfg::rung2();
        let frames = loop_frames(64, 48, &cfg, 6);
        assert_eq!(frames.len(), 6);
        for frame in &frames {
            assert_eq!(frame.len(), 64 * 48 * 4);
        }
    }

    #[test]
    fn loop_frames_samples_a_full_rotation() {
        // Frame 0 is phase 0.0; with 4 frames over a full 2π rotation, frame 2 sits at π,
        // matching a direct two-phase call to `Field::frame` on a fresh field with the same seed.
        let cfg = Cfg::rung2();
        let frames = loop_frames(64, 48, &cfg, 4);

        let mut field = Field::new(64, 48, cfg.particles);
        let mut expected_first = Vec::new();
        field.frame(&cfg, 0.0, &mut expected_first);
        let mut expected_third = Vec::new();
        field.frame(&cfg, PI, &mut expected_third);

        assert_eq!(frames[0], expected_first);
        assert_eq!(frames[2], expected_third);
    }

    #[test]
    fn loop_frames_rejects_zero_by_generating_one() {
        let cfg = Cfg::rung2();
        assert_eq!(loop_frames(16, 16, &cfg, 0).len(), 1);
    }

    /// Cost-vs-density profile against real fork code (this crate's actual `png = "0.17"` dep,
    /// built with `cargo test --release`), not the standalone feasibility-scout harness.
    ///
    /// Run explicitly: `cargo test --release particle_field::tests::profile_cost_curve -- \
    /// --ignored --nocapture`
    #[test]
    #[ignore = "profiling: prints a cost table, run explicitly with --release --ignored --nocapture"]
    fn profile_cost_curve() {
        use std::time::Instant;

        fn median(v: &mut [f64]) -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v[v.len() / 2]
        }

        fn encode_png(w: u32, h: u32, rgba: &[u8]) -> usize {
            let mut buf = Vec::new();
            {
                let mut enc = png::Encoder::new(&mut buf, w, h);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                enc.set_compression(png::Compression::Fast);
                let mut writer = enc.write_header().expect("png header");
                writer.write_image_data(rgba).expect("png data");
            }
            buf.len()
        }

        println!("\n# particle_field cost curve (real fork code, cargo test --release, png 0.17)");
        println!(
            "{:<28} {:>7} {:>9} {:>11} {:>11}",
            "case", "gen ms", "png KB", "5-bit KB", "KB/s@10fps"
        );

        // A. generation cost vs particle count, at Rung 2's shipped wire resolution.
        let particle_counts = [600usize, 2_500, 8_000, 13_000, 30_000];
        for &n in &particle_counts {
            let cfg = Cfg {
                particles: n,
                dof: true,
                bloom: true,
            };
            let (w, h) = (480usize, 312usize);
            let mut field = Field::new(w, h, n);
            let mut rgba = Vec::new();
            let mut gens = Vec::new();
            for k in 0..7 {
                let t = Instant::now();
                field.frame(&cfg, k as f32 * 0.09, &mut rgba);
                gens.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let gen_ms = median(&mut gens);
            let png_kb = encode_png(w as u32, h as u32, &rgba) as f64 / 1024.0;
            let mut quantized = rgba.clone();
            quantize_channels(&mut quantized, 5);
            let quant_kb = encode_png(w as u32, h as u32, &quantized) as f64 / 1024.0;
            println!(
                "{:<28} {:>7.1} {:>9.1} {:>11.1} {:>11.0}",
                format!("{n} particles, 480x312"),
                gen_ms,
                png_kb,
                quant_kb,
                quant_kb * 10.0
            );
        }

        // B. resolution/density: same particle count, sidebar vs Rung 2 vs full window.
        let cfg = Cfg::rung2();
        for &(name, w, h) in &[
            ("sidebar 312x625", 312usize, 625usize),
            ("rung2 480x312", 480, 312),
            ("full 960x625", 960, 625),
        ] {
            let mut field = Field::new(w, h, cfg.particles);
            let mut rgba = Vec::new();
            let mut gens = Vec::new();
            for k in 0..7 {
                let t = Instant::now();
                field.frame(&cfg, k as f32 * 0.09, &mut rgba);
                gens.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let gen_ms = median(&mut gens);
            let png_kb = encode_png(w as u32, h as u32, &rgba) as f64 / 1024.0;
            let mut quantized = rgba.clone();
            quantize_channels(&mut quantized, 5);
            let quant_kb = encode_png(w as u32, h as u32, &quantized) as f64 / 1024.0;
            println!(
                "{:<28} {:>7.1} {:>9.1} {:>11.1} {:>11.0}",
                format!("13k particles, {name}"),
                gen_ms,
                png_kb,
                quant_kb,
                quant_kb * 10.0
            );
        }

        // C. lever ablation at Rung 2's shipped size: dof/bloom on/off.
        for &(name, dof, bloom) in &[
            ("13k flat (no dof/bloom)", false, false),
            ("13k + dof", true, false),
            ("13k + dof + bloom (Rung 2)", true, true),
        ] {
            let cfg = Cfg {
                particles: 13_000,
                dof,
                bloom,
            };
            let (w, h) = (480usize, 312usize);
            let mut field = Field::new(w, h, cfg.particles);
            let mut rgba = Vec::new();
            let mut gens = Vec::new();
            for k in 0..7 {
                let t = Instant::now();
                field.frame(&cfg, k as f32 * 0.09, &mut rgba);
                gens.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let gen_ms = median(&mut gens);
            let png_kb = encode_png(w as u32, h as u32, &rgba) as f64 / 1024.0;
            let mut quantized = rgba.clone();
            quantize_channels(&mut quantized, 5);
            let quant_kb = encode_png(w as u32, h as u32, &quantized) as f64 / 1024.0;
            println!(
                "{name:<28} {gen_ms:>7.1} {png_kb:>9.1} {quant_kb:>11.1} {:>11.0}",
                quant_kb * 10.0
            );
        }
    }
}
