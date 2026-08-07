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
//!
//! Every per-pixel and per-particle pass in [`Field::frame`] (zero, splat, bloom, tonemap) runs
//! across up to [`FIELD_MAX_THREADS`] row bands via `std::thread::scope`, following the same
//! fleet-friendly cap `src/ui/sidebar/image_card.rs` already uses for card rasterisation.
//! Measured on this module at the captain's confirmed 1440p target (`cargo test --release --bin
//! herdr particle_field::tests::bench_1440p -- --ignored --nocapture`, 12-core box, Rung 2):
//! **94.2 ms/frame single-threaded to 28.6 ms/frame threaded — 3.3x, 10.6 fps to 35.0 fps**,
//! consistent with the feasibility report's own ~32 fps CPU-thread ceiling at 1440p
//! (`data/herdr-terminal-field-and-gpu/report.md`, `cpubench.rs`). Frame bytes are identical
//! regardless of thread count — see `frame_is_identical_across_thread_counts` and its siblings.

use std::f32::consts::PI;

const MAX_SPLAT_RADIUS: i32 = 7;

/// Background floor colour measured from the reference field (deep plum, not black).
const BG_FLOOR: (f32, f32, f32) = (20.0 / 255.0, 19.0 / 255.0, 32.0 / 255.0);

/// Cap on how many row bands a single frame's phases will split across.
///
/// Mirrors [`crate::ui::sidebar::image_card`]'s `CARD_RASTER_MAX_THREADS`: this process hosts a
/// fleet of agent panes, so a frame's generation must not be free to take the whole machine even
/// when it is the bottleneck. Six threads already captured most of the measured speedup on a
/// 12-core box in the feasibility report (`cpubench.rs`: six threads beat twelve at both 4K and
/// 1440p because per-thread contention past six ate the rest of the win).
const FIELD_MAX_THREADS: usize = 6;

/// How many row bands to split a `rows`-tall phase across.
///
/// Bounded three ways, tightest wins: never more bands than rows, never more than
/// [`FIELD_MAX_THREADS`], and never more than half the machine's parallelism — the other half is
/// the fleet this process is hosting. A machine reporting fewer than four ways of parallelism, or
/// a phase with fewer than two rows, runs on the calling thread with no scope at all.
fn field_threads(rows: usize) -> usize {
    #[cfg(test)]
    {
        let forced = FIELD_THREADS_FOR_TEST.load(std::sync::atomic::Ordering::Relaxed);
        if forced > 0 {
            return rows.min(forced).max(1);
        }
    }
    if rows < 2 {
        return 1;
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    rows.min(FIELD_MAX_THREADS).min((cores / 2).max(1))
}

/// Pin the thread count for a test that needs to compare two of them. Zero means "use the real
/// bound".
#[cfg(test)]
static FIELD_THREADS_FOR_TEST: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

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
    /// Per-particle screen-space transform, recomputed once per frame in [`Field::transform`]
    /// and then read by every splat band — so the trig and perspective divide happen once
    /// instead of once per band.
    sx: Vec<f32>,
    sy: Vec<f32>,
    sr: Vec<i32>,
    scr: Vec<f32>,
    scg: Vec<f32>,
    scb: Vec<f32>,
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

        let n = px.len();
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
            sx: vec![0.0; n],
            sy: vec![0.0; n],
            sr: vec![0; n],
            scr: vec![0.0; n],
            scg: vec![0.0; n],
            scb: vec![0.0; n],
        }
    }

    /// Zero the accumulator across up to [`FIELD_MAX_THREADS`] row bands.
    fn zero_acc(&mut self) {
        let w = self.w;
        let threads = field_threads(self.h);
        if threads <= 1 {
            self.acc.iter_mut().for_each(|v| *v = 0.0);
            return;
        }
        let band_rows = self.h.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            for band in self.acc.chunks_mut(band_rows * w * 3) {
                scope.spawn(move || band.iter_mut().for_each(|v| *v = 0.0));
            }
        });
    }

    /// Compute each particle's screen-space position, splat radius and colour for this frame's
    /// `phase`, once, ahead of splatting. Cheap relative to the splat and per-pixel passes at
    /// Rung 2's particle count, so it stays on the calling thread.
    fn transform(&mut self, cfg: &Cfg, phase: f32) {
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
            self.sx[i] = sx;
            self.sy[i] = sy;
            self.sr[i] = r;
            self.scr[i] = b * (0.72 + 0.28 * t);
            self.scg[i] = b * (0.30 + 0.52 * t);
            self.scb[i] = b * (0.11 + 0.36 * t);
        }
    }

    /// Splat every particle, transformed by [`Field::transform`], into [`Self::acc`] across up
    /// to [`FIELD_MAX_THREADS`] disjoint row bands.
    ///
    /// **Determinism.** Each band owns a disjoint slice of rows and walks the full particle list
    /// in the same index order a single thread would, skipping only the rows outside its own
    /// slice — so the accumulated value at any pixel is the same sum in the same order no matter
    /// how many threads did the work, including one.
    fn splat_parallel(&mut self) {
        let w = self.w;
        let h = self.h;
        let full_h = h as i32;
        let threads = field_threads(h);

        let Field {
            acc,
            kernels,
            sx,
            sy,
            sr,
            scr,
            scg,
            scb,
            ..
        } = self;
        let kernels: &[Vec<f32>] = kernels;
        let sx: &[f32] = sx;
        let sy: &[f32] = sy;
        let sr: &[i32] = sr;
        let scr: &[f32] = scr;
        let scg: &[f32] = scg;
        let scb: &[f32] = scb;

        if threads <= 1 {
            splat_band(acc, w, 0, h, full_h, kernels, sx, sy, sr, scr, scg, scb);
            return;
        }

        let band_rows = h.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            let mut row0 = 0usize;
            for band in acc.chunks_mut(band_rows * w * 3) {
                let rows = band.len() / (w * 3);
                let this_row0 = row0;
                scope.spawn(move || {
                    splat_band(
                        band, w, this_row0, rows, full_h, kernels, sx, sy, sr, scr, scg, scb,
                    );
                });
                row0 += rows;
            }
        });
    }

    fn downsample_bloom(&self, d: usize, bw: usize, bh: usize, lo: &mut [f32]) {
        let acc = &self.acc;
        let w = self.w;
        let threads = field_threads(bh);
        if threads <= 1 {
            downsample_band(acc, w, d, bw, 0, bh, lo);
            return;
        }
        let band_rows = bh.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            let mut y0 = 0usize;
            for band in lo.chunks_mut(band_rows * bw * 3) {
                let rows = band.len() / (bw * 3);
                let this_y0 = y0;
                scope.spawn(move || downsample_band(acc, w, d, bw, this_y0, rows, band));
                y0 += rows;
            }
        });
    }

    fn upsample_add(&mut self, d: usize, bw: usize, bh: usize, lo: &[f32]) {
        let w = self.w;
        let h = self.h;
        let threads = field_threads(h);
        if threads <= 1 {
            upsample_band(&mut self.acc, w, d, bw, bh, 0, h, lo);
            return;
        }
        let band_rows = h.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            let mut y0 = 0usize;
            for band in self.acc.chunks_mut(band_rows * w * 3) {
                let rows = band.len() / (w * 3);
                let this_y0 = y0;
                scope.spawn(move || upsample_band(band, w, d, bw, bh, this_y0, rows, lo));
                y0 += rows;
            }
        });
    }

    fn bloom(&mut self, div: usize) {
        let d = div;
        let (bw, bh) = (self.w / d, self.h / d);
        if bw < 8 || bh < 8 {
            return;
        }
        let mut lo = vec![0.0f32; bw * bh * 3];
        self.downsample_bloom(d, bw, bh, &mut lo);
        let mut buf = vec![0.0f32; bw * bh * 3];
        let rad = 5i32;
        for _ in 0..3 {
            blur_pass(&lo, &mut buf, bw, bh, rad, Axis::Horizontal);
            blur_pass(&buf, &mut lo, bw, bh, rad, Axis::Vertical);
        }
        self.upsample_add(d, bw, bh, &lo);
    }

    /// Render one frame at the given rotation phase. `out` is resized to `w * h * 4` RGBA8.
    ///
    /// Each phase below (zero, transform, splat, bloom, tonemap) runs to completion before the
    /// next starts, so within a frame there is only ever one phase's threads touching `self` at
    /// a time — sequenced parallelism, not concurrent access to shared state.
    pub fn frame(&mut self, cfg: &Cfg, phase: f32, out: &mut Vec<u8>) {
        self.zero_acc();
        self.transform(cfg, phase);
        self.splat_parallel();

        if cfg.bloom {
            self.bloom(4);
        }

        self.tonemap(out);
    }

    fn tonemap(&self, out: &mut Vec<u8>) {
        out.clear();
        out.resize(self.w * self.h * 4, 255);
        let acc = &self.acc;
        let w = self.w;
        let threads = field_threads(self.h);
        if threads <= 1 {
            tonemap_band(acc, w, 0, self.h, out);
            return;
        }
        let band_rows = self.h.div_ceil(threads).max(1);
        std::thread::scope(|scope| {
            let mut y0 = 0usize;
            for band in out.chunks_mut(band_rows * w * 4) {
                let rows = band.len() / (w * 4);
                let this_y0 = y0;
                scope.spawn(move || tonemap_band(acc, w, this_y0, rows, band));
                y0 += rows;
            }
        });
    }
}

/// The rows-of-`acc` this band owns: `[row0, row0 + rows)` out of `full_h` total rows. Splats a
/// particle's kernel only where it overlaps this band, so a particle whose kernel spans a band
/// boundary is split correctly across the two bands that own its rows.
#[inline]
#[allow(clippy::too_many_arguments)]
fn splat_band(
    acc: &mut [f32],
    w: usize,
    row0: usize,
    rows: usize,
    full_h: i32,
    kernels: &[Vec<f32>],
    sx: &[f32],
    sy: &[f32],
    sr: &[i32],
    scr: &[f32],
    scg: &[f32],
    scb: &[f32],
) {
    let full_w = w as i32;
    let row0_i = row0 as i32;
    let rows_i = rows as i32;
    for i in 0..sx.len() {
        let r = sr[i];
        let ix = sx[i].floor() as i32;
        let iy = sy[i].floor() as i32;
        if ix < r || iy < r || ix >= full_w - r || iy >= full_h - r {
            continue;
        }
        let lo_row = (iy - r).max(row0_i);
        let hi_row = (iy + r).min(row0_i + rows_i - 1);
        if lo_row > hi_row {
            continue;
        }
        let (cr, cg, cb) = (scr[i], scg[i], scb[i]);
        let k = 2 * r + 1;
        for global_row in lo_row..=hi_row {
            let dy = global_row - iy;
            let row = (global_row - row0_i) as usize * w;
            let ko = ((dy + r) * k) as usize;
            for dx in -r..=r {
                let kv = kernels[r as usize][ko + (dx + r) as usize];
                let o = (row + (ix + dx) as usize) * 3;
                acc[o] += cr * kv;
                acc[o + 1] += cg * kv;
                acc[o + 2] += cb * kv;
            }
        }
    }
}

/// Downsample rows `[y0, y0 + rows)` of the low-res bloom buffer from the full-res accumulator.
fn downsample_band(
    acc: &[f32],
    w: usize,
    d: usize,
    bw: usize,
    y0: usize,
    rows: usize,
    lo: &mut [f32],
) {
    for y_local in 0..rows {
        let y = y0 + y_local;
        for x in 0..bw {
            let o = (y_local * bw + x) * 3;
            let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
            for sy in 0..d {
                for sx in 0..d {
                    let so = ((y * d + sy) * w + (x * d + sx)) * 3;
                    r += acc[so];
                    g += acc[so + 1];
                    b += acc[so + 2];
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
}

/// Which way [`blur_pass`] walks its box filter. The bloom low-res buffer is small relative to
/// the full-res passes, but at 1440p it is still 640×360 — three iterations of both a horizontal
/// and a vertical pass over that buffer measured as the single largest unthreaded cost in this
/// module before this axis was added, so it gets the same row-band treatment as everything else.
#[derive(Clone, Copy)]
enum Axis {
    /// Output row `y` reads only row `y` of `src` (varying taps in `x`), so bands can never read
    /// outside their own output rows.
    Horizontal,
    /// Output row `y` reads rows `[y - rad, y + rad]` of `src`, so a band's threads read outside
    /// their own output rows — safe because `src` is a distinct, fully-populated buffer from
    /// `dst` and is only ever read here, never written.
    Vertical,
}

/// Box-blur `src` into `dst` (same shape, `bw * bh * 3`) along `axis`, across up to
/// [`FIELD_MAX_THREADS`] disjoint row bands of `dst`.
fn blur_pass(src: &[f32], dst: &mut [f32], bw: usize, bh: usize, rad: i32, axis: Axis) {
    let threads = field_threads(bh);
    if threads <= 1 {
        blur_band(src, dst, bw, bh, rad, axis, 0, bh);
        return;
    }
    let band_rows = bh.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let mut y0 = 0usize;
        for band in dst.chunks_mut(band_rows * bw * 3) {
            let rows = band.len() / (bw * 3);
            let this_y0 = y0;
            scope.spawn(move || blur_band(src, band, bw, bh, rad, axis, this_y0, rows));
            y0 += rows;
        }
    });
}

/// Box-blur rows `[y0, y0 + rows)` of `dst` from `src`, along `axis`. `src` spans the full `bh`
/// rows regardless of which band this is — only `dst` is restricted to the band's own slice.
fn blur_band(
    src: &[f32],
    dst: &mut [f32],
    bw: usize,
    bh: usize,
    rad: i32,
    axis: Axis,
    y0: usize,
    rows: usize,
) {
    for y_local in 0..rows {
        let y = y0 + y_local;
        for x in 0..bw {
            let (mut r, mut g, mut b, mut n) = (0.0, 0.0, 0.0, 0.0);
            match axis {
                Axis::Horizontal => {
                    for dx in -rad..=rad {
                        let xx = x as i32 + dx;
                        if xx < 0 || xx >= bw as i32 {
                            continue;
                        }
                        let o = (y * bw + xx as usize) * 3;
                        r += src[o];
                        g += src[o + 1];
                        b += src[o + 2];
                        n += 1.0;
                    }
                }
                Axis::Vertical => {
                    for dy in -rad..=rad {
                        let yy = y as i32 + dy;
                        if yy < 0 || yy >= bh as i32 {
                            continue;
                        }
                        let o = (yy as usize * bw + x) * 3;
                        r += src[o];
                        g += src[o + 1];
                        b += src[o + 2];
                        n += 1.0;
                    }
                }
            }
            let o = (y_local * bw + x) * 3;
            dst[o] = r / n;
            dst[o + 1] = g / n;
            dst[o + 2] = b / n;
        }
    }
}

/// Add the blurred low-res bloom buffer back into rows `[y0, y0 + rows)` of the full-res
/// accumulator.
#[allow(clippy::too_many_arguments)]
fn upsample_band(
    acc: &mut [f32],
    w: usize,
    d: usize,
    bw: usize,
    bh: usize,
    y0: usize,
    rows: usize,
    lo: &[f32],
) {
    for y_local in 0..rows {
        let y = y0 + y_local;
        let sy = (y / d).min(bh - 1);
        for x in 0..w {
            let sx = (x / d).min(bw - 1);
            let so = (sy * bw + sx) * 3;
            let o = (y_local * w + x) * 3;
            acc[o] += lo[so] * 0.55;
            acc[o + 1] += lo[so + 1] * 0.55;
            acc[o + 2] += lo[so + 2] * 0.55;
        }
    }
}

/// Tonemap rows `[y0, y0 + rows)` of the accumulator into RGBA8 `out`, which owns only those
/// rows.
fn tonemap_band(acc: &[f32], w: usize, y0: usize, rows: usize, out: &mut [u8]) {
    let (fr, fg, fb) = BG_FLOOR;
    let tm = |v: f32, floor: f32| -> u8 {
        let t = v / (1.0 + v * 0.92); // Reinhard, gentle shoulder
        let q = ((t.powf(0.90) + floor) * 255.0) as i32;
        q.clamp(0, 255) as u8
    };
    for y_local in 0..rows {
        let y = y0 + y_local;
        for x in 0..w {
            let o = (y * w + x) * 3;
            let d = (y_local * w + x) * 4;
            out[d] = tm(acc[o], fr);
            out[d + 1] = tm(acc[o + 1], fg);
            out[d + 2] = tm(acc[o + 2], fb);
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

    /// The thread bound, pinned for one test and released however that test ends (including a
    /// panicking `assert_eq!`, via `Drop`). Serialised against the other tests that pin it, so
    /// this holds under a parallel harness as well as the single-threaded one the suite is run
    /// with. Mirrors [`crate::ui::sidebar::image_card::tests::ThreadPin`].
    struct ThreadPin(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    static PIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl ThreadPin {
        fn at(threads: usize) -> Self {
            let guard = PIN_LOCK.lock().unwrap_or_else(|held| held.into_inner());
            FIELD_THREADS_FOR_TEST.store(threads, std::sync::atomic::Ordering::Relaxed);
            Self(guard)
        }
    }

    impl Drop for ThreadPin {
        fn drop(&mut self) {
            FIELD_THREADS_FOR_TEST.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The core acceptance test for this change: a frame's bytes must not depend on how many
    /// threads generated it, at a size and particle count that forces splat kernels (radius up
    /// to [`MAX_SPLAT_RADIUS`]) to straddle band boundaries at every thread count above one.
    #[test]
    fn frame_is_identical_across_thread_counts() {
        let cfg = Cfg::rung2();
        let (w, h) = (200, 150);
        let phase = 1.1;

        let baseline = {
            let _pin = ThreadPin::at(1);
            let mut field = Field::new(w, h, cfg.particles);
            let mut out = Vec::new();
            field.frame(&cfg, phase, &mut out);
            out
        };

        for threads in [2, 3, 6, 12] {
            let out = {
                let _pin = ThreadPin::at(threads);
                let mut field = Field::new(w, h, cfg.particles);
                let mut out = Vec::new();
                field.frame(&cfg, phase, &mut out);
                out
            };
            assert_eq!(
                out, baseline,
                "frame bytes differed at {threads} threads vs. single-threaded"
            );
        }
    }

    /// Same determinism check with bloom off and dof off, so the plain `r = 2` splat path (no
    /// per-particle radius variation) and the bloom-skipping frame path are covered too.
    #[test]
    fn frame_is_identical_across_thread_counts_without_bloom_or_dof() {
        let cfg = Cfg {
            particles: 4_000,
            dof: false,
            bloom: false,
        };
        let (w, h) = (200, 150);

        let baseline = {
            let _pin = ThreadPin::at(1);
            let mut field = Field::new(w, h, cfg.particles);
            let mut out = Vec::new();
            field.frame(&cfg, 0.4, &mut out);
            out
        };

        for threads in [2, 6, 12] {
            let out = {
                let _pin = ThreadPin::at(threads);
                let mut field = Field::new(w, h, cfg.particles);
                let mut out = Vec::new();
                field.frame(&cfg, 0.4, &mut out);
                out
            };
            assert_eq!(out, baseline, "mismatch at {threads} threads");
        }
    }

    /// Repeated frames from one [`Field`] must also be stable under threading — not just the
    /// first frame, since `acc` is reused across calls and a leftover-state bug would only show
    /// up on frame two onward.
    #[test]
    fn repeated_frames_are_identical_across_thread_counts() {
        let cfg = Cfg::rung2();
        let (w, h) = (160, 130);
        let phases = [0.0f32, 0.6, 1.9, 3.4, 5.0];

        let baseline = {
            let _pin = ThreadPin::at(1);
            let mut field = Field::new(w, h, cfg.particles);
            phases
                .iter()
                .map(|&phase| {
                    let mut out = Vec::new();
                    field.frame(&cfg, phase, &mut out);
                    out
                })
                .collect::<Vec<_>>()
        };

        let threaded = {
            let _pin = ThreadPin::at(6);
            let mut field = Field::new(w, h, cfg.particles);
            phases
                .iter()
                .map(|&phase| {
                    let mut out = Vec::new();
                    field.frame(&cfg, phase, &mut out);
                    out
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(threaded, baseline);
    }

    /// Several [`Field`]s, each with their own internally-threaded generation, running at the
    /// same time from independent OS threads must not corrupt each other's output or panic. This
    /// is the concurrent-generation correctness case: no torn frames, no shared mutable state
    /// leaking between fields generating in parallel (e.g. the sidebar's field and a future
    /// whole-screen field running side by side).
    #[test]
    fn concurrent_fields_do_not_corrupt_each_other() {
        let cfg = Cfg::rung2();
        let (w, h) = (180, 140);

        // What each phase should produce, computed serially, once, up front.
        let phases: Vec<f32> = (0..8).map(|i| i as f32 * 0.37).collect();
        let expected: Vec<Vec<u8>> = phases
            .iter()
            .map(|&phase| {
                let mut field = Field::new(w, h, cfg.particles);
                let mut out = Vec::new();
                field.frame(&cfg, phase, &mut out);
                out
            })
            .collect();

        std::thread::scope(|scope| {
            let handles: Vec<_> = phases
                .iter()
                .zip(expected.iter())
                .map(|(&phase, expected)| {
                    let cfg = &cfg;
                    scope.spawn(move || {
                        let mut field = Field::new(w, h, cfg.particles);
                        let mut out = Vec::new();
                        field.frame(cfg, phase, &mut out);
                        assert_eq!(
                            &out, expected,
                            "field generated concurrently with others produced different bytes \
                             than the same field generated in isolation"
                        );
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("generation thread panicked");
            }
        });
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

    /// Before/after cost of this change at the captain's confirmed 1440p target, single core vs.
    /// the real thread bound on this box. Multithreading is internal to [`Field::frame`]; this
    /// pins [`field_threads`] to one to get the "before" number from the same code path rather
    /// than from a separate unthreaded copy.
    ///
    /// Run explicitly: `cargo test --release particle_field::tests::bench_1440p -- \
    /// --ignored --nocapture`
    #[test]
    #[ignore = "benchmark: prints ms/frame, run explicitly with --release --ignored --nocapture"]
    fn bench_1440p() {
        use std::time::Instant;

        fn median(v: &mut [f64]) -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            v[v.len() / 2]
        }

        let cfg = Cfg::rung2();
        let (w, h) = (2560usize, 1440usize);
        let cores = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);

        println!("\n# particle_field 1440p bench ({cores} logical cores on this box)");
        for (label, pin) in [
            ("single core (before)", Some(1)),
            ("real thread bound (after)", None),
        ] {
            let _guard = pin.map(ThreadPin::at);
            let mut field = Field::new(w, h, cfg.particles);
            let mut out = Vec::new();
            // Warm up (first frame pays one-time allocator/cache costs).
            field.frame(&cfg, 0.0, &mut out);
            let mut samples = Vec::new();
            for k in 0..9 {
                let t = Instant::now();
                field.frame(&cfg, k as f32 * 0.31, &mut out);
                samples.push(t.elapsed().as_secs_f64() * 1000.0);
            }
            let ms = median(&mut samples);
            println!("{label:<28} {ms:>8.2} ms/frame  ({:>6.1} fps)", 1000.0 / ms);
        }
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
