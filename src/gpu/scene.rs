//! The device-independent half of [`super::bloom`]: what a bloom *is*, with no
//! `wgpu` in sight.
//!
//! Split out so it compiles whether or not the `gpu-raster` feature is on. The
//! caller builds these types either way — the cost model reads them to decide
//! whether a GPU is worth using at all — and having one definition rather than a
//! real one and a stub is what stops the two builds drifting apart.

/// One card's bloom contribution: a rounded rect, the curve its halo falls off
/// along, and the ink of each pixel column it crosses.
///
/// Everything here is already resolved by the caller. In particular `columns`
/// holds 8-bit sRGB channel values as floats, because that is what the CPU path
/// resolves them to before it blends — quantising on the GPU instead would be a
/// second, differently-rounded answer to the same question.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Splat {
    /// `x, y, w, h` of the rounded rect, in the tile's own pixels.
    pub(crate) rect: [f32; 4],
    /// Corner radius, before the half-extent clamp the distance function applies.
    pub(crate) radius: f32,
    pub(crate) near_sigma: f32,
    pub(crate) far_sigma: f32,
    /// Last valid index into the distance profile. A pixel further out than
    /// this is not lit at all, matching the CPU's `profile.get(..)` returning
    /// `None`.
    pub(crate) max_step: u32,
    /// `x0, y0, x1, y1` — the clamped box this splat may write in, half-open.
    pub(crate) bounds: [u32; 4],
    /// `(r, g, b, strength)` for each column in `x0..x1`, in that order.
    pub(crate) columns: Vec<[f32; 4]>,
}

/// One image to be bloomed, and the splats that light it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Tile {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) splats: Vec<Splat>,
}

impl Tile {
    pub(crate) fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Pixels the CPU scatter path would actually touch for this tile: each
    /// splat's own box, walked once, plus the composite pass over the whole
    /// image. The GPU instead visits every pixel for every splat, which is why
    /// the two cost models are not the same number.
    pub(crate) fn cpu_pixels(&self) -> u64 {
        self.pixels()
            + self
                .splats
                .iter()
                .map(|splat| {
                    let [x0, y0, x1, y1] = splat.bounds;
                    u64::from(x1.saturating_sub(x0)) * u64::from(y1.saturating_sub(y0))
                })
                .sum::<u64>()
    }
}

/// The shape of the falloff curve, passed in rather than written twice.
///
/// These are `measured::BLOOM_PEAK` and its siblings. The shader has no copy of
/// any of them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Curve {
    pub(crate) steps_per_px: f32,
    pub(crate) peak: f32,
    pub(crate) near_weight: f32,
    pub(crate) far_weight: f32,
    pub(crate) paint_floor: f32,
}

/// Why a batch did not run on the GPU. Every variant means the same thing to
/// the caller — draw it on the CPU — and exists only so the log line says which.
// Which of these can be constructed depends on the `gpu-raster` feature: with
// it off there is no device to fail, and `NotBuilt` is the only answer.
#[cfg_attr(not(feature = "gpu-raster"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Declined {
    /// No adapter, or no device from the adapter. Already logged once, at
    /// acquisition.
    NoDevice,
    /// The batch needs a buffer larger than this device will bind.
    TooLarge,
    /// The device produced an error mid-pass.
    Failed(String),
    /// This binary was built without the `gpu-raster` feature. Only
    /// `super::bloom_disabled` ever produces it, so a build *with* the feature
    /// carries a variant it cannot construct — cheaper than a second enum, and
    /// it keeps one `Display` for the one log line.
    #[cfg_attr(feature = "gpu-raster", allow(dead_code))]
    NotBuilt,
}

impl std::fmt::Display for Declined {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Declined::NoDevice => f.write_str("no GPU device"),
            Declined::TooLarge => f.write_str("batch larger than the device will bind"),
            Declined::Failed(why) => write!(f, "GPU pass failed: {why}"),
            Declined::NotBuilt => f.write_str("built without the gpu-raster feature"),
        }
    }
}

/// How many tiles this process has actually composed on the GPU.
///
/// The positive control for every test of this path. A GPU test that cannot
/// tell "identical because both ran and agreed" from "identical because the GPU
/// never ran" is not testing anything, and the pass declines for several
/// different reasons without raising an error.
// Only ever incremented by the real pass, and read by its tests — which still
// compile with the feature off, where they skip on the zero this stays at.
#[cfg_attr(not(feature = "gpu-raster"), allow(dead_code))]
pub(crate) static TILES_COMPOSED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Make `bloom::estimated_ms` answer "free" for a test comparing the two
/// backends on a batch no real cost model would send to a GPU. Still `None`
/// without a device, so a machine with no adapter still skips rather than lies.
#[cfg(test)]
pub(crate) static PRETEND_INSTANT_FOR_TEST: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;

    /// The cost model counts what each backend actually walks, which is not the
    /// same set of pixels.
    #[test]
    fn the_two_cost_models_count_different_pixels() {
        let tile = Tile {
            width: 100,
            height: 100,
            splats: vec![
                Splat {
                    rect: [10.0, 10.0, 80.0, 80.0],
                    radius: 6.0,
                    near_sigma: 2.0,
                    far_sigma: 3.0,
                    max_step: 60,
                    // A box covering a quarter of the image.
                    bounds: [0, 0, 50, 50],
                    columns: vec![[0.0, 0.0, 0.0, 1.0]; 50],
                };
                2
            ],
        };
        assert_eq!(tile.pixels(), 10_000, "the GPU visits every pixel");
        assert_eq!(
            tile.cpu_pixels(),
            10_000 + 2 * 2_500,
            "the CPU walks each splat's own box, then composites the image once"
        );
    }
}
