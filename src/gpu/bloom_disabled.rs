//! What [`super::bloom`] is when the `gpu-raster` feature is off.
//!
//! The types come from the same `super::scene` the real module re-exports, so
//! there is nothing here to drift: only the entry points that would have touched
//! a device, each declining. A build without `wgpu` takes the CPU path through
//! the *same* call site as a build with `wgpu` and no adapter — one bloom seam
//! in the card rasteriser, and no `cfg` anywhere near it.

#[cfg(test)]
pub(crate) use super::scene::PRETEND_INSTANT_FOR_TEST;
// `TILES_COMPOSED` never moves here, but the caller still reads it — the parity
// tests key their skip off it, and they compile with the feature either way.
#[allow(unused_imports)]
pub(crate) use super::scene::{Curve, Declined, Splat, Tile, TILES_COMPOSED};

/// Never: there is no device to estimate for, so the caller never prefers one.
pub(crate) fn estimated_ms(_tiles: &[Tile]) -> Option<f64> {
    None
}

pub(crate) fn compose(_tiles: &[Tile], _curve: Curve) -> Result<Vec<Vec<u8>>, Declined> {
    Err(Declined::NotBuilt)
}

// Read only by the parity tests' skip message; there is no adapter to name in a
// build that cannot open one.
#[allow(dead_code)]
pub(crate) fn adapter_description() -> Option<String> {
    None
}

pub(crate) fn warn_once(_declined: &Declined) {}
