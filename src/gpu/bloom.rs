//! The card bloom splat/blend pass, on a compute shader.
//!
//! This module knows nothing about cards, fonts or the sidebar. It takes a
//! frame's worth of [`Tile`]s — an image and the [`Splat`]s that light it — and
//! returns each one's straight-alpha RGBA8 bytes. The caller
//! (`ui::sidebar::image_card`) owns deciding what a splat *is*; the contract
//! between them is that a tile starts fully transparent and comes back holding
//! only the bloom, ready for the card itself to be drawn on top.
//!
//! # One dispatch, one readback
//!
//! A card's own image is small — around 400x110 pixels — and the fixed cost of
//! reaching a GPU and getting bytes back is around a third of a millisecond,
//! which is most of what drawing one card's bloom costs on the CPU in the first
//! place. So a *frame* is the unit, not a card: every tile lands in one pixel
//! buffer, gets one dispatch off a dynamically-offset uniform, and comes home in
//! one map. That is the difference between the GPU being worth using here and
//! not.

use std::sync::atomic::{AtomicBool, Ordering};

use tracing::warn;

use super::device;
#[cfg(test)]
pub(crate) use super::scene::PRETEND_INSTANT_FOR_TEST;
pub(crate) use super::scene::{Curve, Declined, Splat, Tile, TILES_COMPOSED};

/// Bytes one `Splat` occupies in the storage buffer: four `vec4`s.
const SPLAT_STRIDE: usize = 64;
/// Bytes one `Tile`'s uniform record occupies before alignment padding.
const TILE_RECORD: u64 = 64;

/// What one round trip to *this* device costs, measured on this device.
///
/// # Why this is measured and not a constant
///
/// The whole question of whether the GPU is worth using here is a race between
/// a few milliseconds of CPU pixel work and the fixed cost of submitting a
/// dispatch and waiting for the fence — and that fixed cost is a property of the
/// machine, not of Herdr. It was measured at about **1.9 ms** on an Intel UHD
/// 770 under Mesa, which is more than the entire CPU bloom for a twelve-card
/// frame; a discrete card with a mature driver is normally an order of magnitude
/// under that. One hard-coded threshold cannot be right for both, and getting it
/// wrong in the optimistic direction is a silent regression on somebody's
/// machine.
///
/// So the pass times itself once, on two batch sizes, and derives a straight
/// line: a fixed cost and a per-megapixel cost. [`estimated_ms`] reads that line,
/// and the caller compares it against what the CPU would have cost.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Calibration {
    /// The cost of a round trip that computes almost nothing.
    pub(crate) fixed_ms: f64,
    /// What each further megapixel of batch adds, dispatch and readback together.
    pub(crate) ms_per_megapixel: f64,
}

impl Calibration {
    fn estimate(&self, pixels: u64) -> f64 {
        self.fixed_ms + self.ms_per_megapixel * (pixels as f64 / 1_000_000.0)
    }
}

/// The two batch sizes the line is fitted through. The small one is essentially
/// pure latency; the large one is about a twelve-card frame, which is the batch
/// this path exists for.
const CALIBRATION_SMALL: (u32, u32, usize) = (64, 64, 1);
const CALIBRATION_LARGE: (u32, u32, usize) = (400, 110, 12);
/// Runs per point. The minimum is taken: this is a floor measurement, and a
/// sample that caught the machine doing something else is not evidence about
/// the device.
const CALIBRATION_RUNS: usize = 3;

/// This device's measured cost line, or `None` when there is no device.
///
/// Measured once, lazily, on the first frame that asks — about ten milliseconds,
/// paid on a frame that was going to rasterise a whole tree anyway, and never
/// again.
pub(crate) fn calibration() -> Option<Calibration> {
    static CALIBRATION: std::sync::OnceLock<Option<Calibration>> = std::sync::OnceLock::new();
    *CALIBRATION.get_or_init(measure)
}

fn measure() -> Option<Calibration> {
    let curve = Curve {
        steps_per_px: 8.0,
        peak: 0.38,
        near_weight: 1.0,
        far_weight: 0.0,
        paint_floor: 0.002,
    };
    let point = |(width, height, count): (u32, u32, usize)| -> Option<(f64, u64)> {
        let tiles: Vec<Tile> = (0..count)
            .map(|_| Tile {
                width,
                height,
                splats: vec![Splat {
                    rect: [
                        4.0,
                        4.0,
                        f32::from(width as u16) - 8.0,
                        f32::from(height as u16) - 8.0,
                    ],
                    radius: 6.0,
                    near_sigma: 2.0,
                    far_sigma: 3.0,
                    max_step: 60,
                    bounds: [0, 0, width, height],
                    columns: vec![[40.0, 200.0, 190.0, 0.7]; width as usize],
                }],
            })
            .collect();
        let pixels: u64 = tiles.iter().map(Tile::pixels).sum();
        let mut best = f64::INFINITY;
        for _ in 0..CALIBRATION_RUNS {
            let at = std::time::Instant::now();
            run_pass(&tiles, curve).ok()?;
            best = best.min(at.elapsed().as_secs_f64() * 1000.0);
        }
        Some((best, pixels))
    };

    let (small_ms, small_px) = point(CALIBRATION_SMALL)?;
    let (large_ms, large_px) = point(CALIBRATION_LARGE)?;
    let span = (large_px.saturating_sub(small_px)) as f64 / 1_000_000.0;
    // A device fast enough that the large batch measured no slower than the
    // small one has a per-pixel cost below this instrument's resolution; zero is
    // the honest reading, and `fixed_ms` still holds the round trip.
    let ms_per_megapixel = if span > 0.0 {
        ((large_ms - small_ms) / span).max(0.0)
    } else {
        0.0
    };
    let calibration = Calibration {
        fixed_ms: small_ms,
        ms_per_megapixel,
    };
    tracing::info!(
        fixed_ms = calibration.fixed_ms,
        ms_per_megapixel = calibration.ms_per_megapixel,
        adapter = adapter_description().as_deref().unwrap_or("none"),
        "measured this device's card-bloom round trip"
    );
    Some(calibration)
}

/// What this batch would cost on the GPU, from [`calibration`]. `None` when
/// there is no device to run it on.
pub(crate) fn estimated_ms(tiles: &[Tile]) -> Option<f64> {
    #[cfg(test)]
    if PRETEND_INSTANT_FOR_TEST.load(Ordering::Relaxed) {
        return device::context().map(|_| 0.0);
    }
    let pixels: u64 = tiles.iter().map(Tile::pixels).sum();
    Some(calibration()?.estimate(pixels))
}

/// Bloom every tile in one pass, returning each one's straight-alpha RGBA8.
///
/// Tiles come back in the order they went in, one `Vec<u8>` of `width * height *
/// 4` bytes each. A tile with no splats comes back fully transparent, which is
/// what it started as.
pub(crate) fn compose(tiles: &[Tile], curve: Curve) -> Result<Vec<Vec<u8>>, Declined> {
    if tiles.is_empty() {
        return Ok(Vec::new());
    }
    run_pass(tiles, curve)
}

/// [`compose`] with no emptiness check: the calibration's own entry point, so
/// measuring the device does not recurse through the decision the measurement
/// is for.
fn run_pass(tiles: &[Tile], curve: Curve) -> Result<Vec<Vec<u8>>, Declined> {
    let context = device::context().ok_or(Declined::NoDevice)?;

    // ---- lay the batch out -------------------------------------------------
    let stride = u64::from(context.uniform_alignment).max(TILE_RECORD);
    let mut tile_uniforms = vec![0u8; (stride as usize) * tiles.len()];
    let mut splat_bytes: Vec<u8> = Vec::with_capacity(
        tiles.iter().map(|tile| tile.splats.len()).sum::<usize>() * SPLAT_STRIDE,
    );
    let mut columns: Vec<u8> = Vec::new();
    let mut pixel_offset: u64 = 0;

    for (index, tile) in tiles.iter().enumerate() {
        let splat_offset = splat_bytes.len() / SPLAT_STRIDE;
        for splat in &tile.splats {
            let column_offset = columns.len() / 16;
            for column in &splat.columns {
                for channel in column {
                    columns.extend_from_slice(&channel.to_le_bytes());
                }
            }
            push_f32(&mut splat_bytes, &splat.rect);
            push_f32(
                &mut splat_bytes,
                &[splat.radius, splat.near_sigma, splat.far_sigma, 0.0],
            );
            push_u32(&mut splat_bytes, &splat.bounds);
            push_u32(
                &mut splat_bytes,
                &[splat.max_step, to_u32(column_offset)?, 0, 0],
            );
        }

        let mut record = Vec::with_capacity(TILE_RECORD as usize);
        push_u32(
            &mut record,
            &[
                tile.width,
                tile.height,
                to_u32(splat_offset)?,
                to_u32(tile.splats.len())?,
            ],
        );
        push_u32(&mut record, &[to_u32_64(pixel_offset)?, 0, 0, 0]);
        push_f32(
            &mut record,
            &[
                curve.steps_per_px,
                curve.peak,
                curve.near_weight,
                curve.far_weight,
            ],
        );
        push_f32(&mut record, &[curve.paint_floor, 0.0, 0.0, 0.0]);
        let at = index * stride as usize;
        tile_uniforms[at..at + record.len()].copy_from_slice(&record);

        pixel_offset += tile.pixels();
    }

    let pixel_bytes = pixel_offset
        .checked_mul(4)
        .ok_or(Declined::TooLarge)?
        .max(4);
    // Every binding has to fit the device's limit, and the pixel buffer is the
    // one that grows with the fleet. Declining here rather than letting wgpu's
    // validation fire keeps an oversized frame a CPU frame instead of a lost one.
    let largest = pixel_bytes
        .max(splat_bytes.len() as u64)
        .max(columns.len() as u64);
    if largest > context.max_storage_binding {
        return Err(Declined::TooLarge);
    }

    // ---- run it ------------------------------------------------------------
    let device = &context.device;
    let pipeline = pipeline(context)?;
    // Everything from here to the readback is inside an error scope. Without
    // one, a validation error is reported out of band and the pass still
    // "succeeds" — handing back the zeroed buffer, which is a card with no glow
    // rather than a card drawn on the CPU. A blank surface that reports success
    // is the one failure mode this fallback must not have.
    device.push_error_scope(wgpu::ErrorFilter::Validation);

    let tile_buffer = upload(
        device,
        "herdr bloom tiles",
        &tile_uniforms,
        wgpu::BufferUsages::UNIFORM,
    );
    // An empty storage binding is not bindable, and a batch can legitimately
    // hold a tile with no splats, so both of these carry a zeroed placeholder
    // rather than nothing.
    let splat_buffer = upload(
        device,
        "herdr bloom splats",
        &pad_to_one(splat_bytes, SPLAT_STRIDE),
        wgpu::BufferUsages::STORAGE,
    );
    let column_buffer = upload(
        device,
        "herdr bloom columns",
        &pad_to_one(columns, 16),
        wgpu::BufferUsages::STORAGE,
    );
    // Not written: wgpu zero-initialises buffer memory, and a tile starts
    // transparent. Uploading a megabyte of zeroes to say so would be most of
    // this pass's transfer cost.
    let pixel_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("herdr bloom pixels"),
        size: pixel_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("herdr bloom readback"),
        size: pixel_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("herdr bloom"),
        layout: &pipeline.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &tile_buffer,
                    offset: 0,
                    size: std::num::NonZeroU64::new(TILE_RECORD),
                }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: splat_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: column_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: pixel_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("herdr bloom"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("herdr bloom"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline.pipeline);
        for (index, tile) in tiles.iter().enumerate() {
            if tile.width == 0 || tile.height == 0 {
                continue;
            }
            pass.set_bind_group(0, &bind_group, &[to_u32_64(index as u64 * stride)?]);
            pass.dispatch_workgroups(tile.width.div_ceil(8), tile.height.div_ceil(8), 1);
        }
    }
    encoder.copy_buffer_to_buffer(&pixel_buffer, 0, &staging, 0, pixel_bytes);
    context.queue.submit([encoder.finish()]);

    if let Some(error) = device::block_on(device.pop_error_scope()) {
        return Err(Declined::Failed(error.to_string()));
    }

    let (tx, rx) = std::sync::mpsc::channel();
    staging
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| Declined::Failed(format!("{error:?}")))?;
    rx.recv()
        .map_err(|_| Declined::Failed("readback never completed".into()))?
        .map_err(|error| Declined::Failed(format!("{error:?}")))?;

    let mapped = staging.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity(tiles.len());
    let mut at = 0usize;
    for tile in tiles {
        let len = (tile.pixels() * 4) as usize;
        out.push(mapped[at..at + len].to_vec());
        at += len;
    }
    drop(mapped);
    staging.unmap();
    TILES_COMPOSED.fetch_add(tiles.len() as u64, Ordering::Relaxed);
    Ok(out)
}

/// The adapter this process is using, for reporting. `None` before the first
/// pass, or on a machine with no usable device.
pub(crate) fn adapter_description() -> Option<String> {
    let context = device::context()?;
    let info = &context.adapter;
    Some(format!(
        "{} ({:?}, {:?}, {} {})",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    ))
}

// ---------------------------------------------------------------------------

struct Pipeline {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

/// Built once. Compiling the shader is the one part of this that costs real
/// milliseconds, and it does not depend on the batch.
fn pipeline(context: &'static device::Context) -> Result<&'static Pipeline, Declined> {
    static PIPELINE: std::sync::OnceLock<Option<Pipeline>> = std::sync::OnceLock::new();
    PIPELINE
        .get_or_init(|| build_pipeline(context))
        .as_ref()
        .ok_or_else(|| Declined::Failed("the bloom shader would not build".into()))
}

fn build_pipeline(context: &'static device::Context) -> Option<Pipeline> {
    let device = &context.device;
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let built = {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("herdr bloom"),
            source: wgpu::ShaderSource::Wgsl(include_str!("bloom.wgsl").into()),
        });
        let storage = |read_only: bool| wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let entry = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("herdr bloom"),
            entries: &[
                // The per-tile record, re-bound at a dynamic offset once per
                // tile so the whole frame is one pass.
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: std::num::NonZeroU64::new(TILE_RECORD),
                    },
                ),
                entry(1, storage(true)),
                entry(2, storage(true)),
                entry(3, storage(false)),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("herdr bloom"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("herdr bloom"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Pipeline { pipeline, layout }
    };
    // A WGSL error is a programming error in this repository, not a property of
    // the machine — but it must still cost a CPU frame rather than a blank card,
    // and it must say so out loud.
    match device::block_on(device.pop_error_scope()) {
        Some(error) => {
            warn!(%error, "the card bloom shader would not build");
            None
        }
        None => Some(built),
    }
}

fn upload(
    device: &wgpu::Device,
    label: &str,
    bytes: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    use wgpu::util::DeviceExt;
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some(label),
        contents: bytes,
        usage,
    })
}

fn push_f32(out: &mut Vec<u8>, values: &[f32; 4]) {
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn push_u32(out: &mut Vec<u8>, values: &[u32; 4]) {
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
}

fn pad_to_one(mut bytes: Vec<u8>, stride: usize) -> Vec<u8> {
    if bytes.is_empty() {
        bytes.resize(stride, 0);
    }
    bytes
}

fn to_u32(value: usize) -> Result<u32, Declined> {
    u32::try_from(value).map_err(|_| Declined::TooLarge)
}

fn to_u32_64(value: u64) -> Result<u32, Declined> {
    u32::try_from(value).map_err(|_| Declined::TooLarge)
}

/// Say once, per process, that the GPU path stood down — and never again.
///
/// This sits under the render loop, so a machine that declines every frame must
/// not write a log line every frame.
pub(crate) fn warn_once(declined: &Declined) {
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    // Named, because "the GPU pass failed" is not actionable and "this adapter,
    // this driver, this failure" is — and because the one thing this module must
    // never do is leave someone unsure whether a GPU was involved at all.
    warn!(
        %declined,
        adapter = adapter_description().as_deref().unwrap_or("none"),
        "GPU card bloom stood down; drawing on the CPU instead"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device measures itself, and the line it fits is usable: a positive
    /// fixed cost, and a batch that is never estimated cheaper than an empty one.
    #[test]
    fn the_device_measures_its_own_round_trip() {
        let _gate = crate::gpu::ForceEnabled::new(true);
        let Some(calibration) = calibration() else {
            println!("SKIP: no GPU adapter on this machine");
            return;
        };
        assert!(
            calibration.fixed_ms > 0.0,
            "a round trip that measured as free is an instrument fault, not a device: {calibration:?}"
        );
        assert!(calibration.ms_per_megapixel >= 0.0);
        let empty: Vec<Tile> = Vec::new();
        let big = vec![
            Tile {
                width: 400,
                height: 110,
                splats: Vec::new(),
            };
            12
        ];
        let (Some(small_ms), Some(big_ms)) = (estimated_ms(&empty), estimated_ms(&big)) else {
            panic!("a calibrated device gave no estimate");
        };
        assert!(
            big_ms >= small_ms,
            "a bigger batch was estimated cheaper than nothing at all"
        );
        println!("{calibration:?} -> 12 cards estimated at {big_ms:.2}ms");
    }

    /// An empty batch is not an error and is not a dispatch.
    #[test]
    fn an_empty_batch_composes_to_nothing() {
        assert_eq!(compose(&[], curve()), Ok(Vec::new()));
    }

    /// This target has a backend compiled in.
    ///
    /// Backends are chosen per target in `Cargo.toml`, and a target left without
    /// one does not merely lose the GPU: `wgpu::Instance::new` *panics*. That is
    /// how the first attempt at this shipped — `vulkan` and `gles` under
    /// `cfg(unix)`, and macOS is Unix and implements neither. `device::acquire`
    /// now checks before it asks, so the consequence is a CPU fallback rather
    /// than a crash; this makes it a build failure instead of a silent one.
    #[test]
    fn this_target_has_a_wgpu_backend() {
        assert!(
            !wgpu::Instance::enabled_backend_features().is_empty(),
            "no wgpu backend is compiled in for {} — every machine on this \
             target silently falls back to the CPU",
            std::env::consts::OS
        );
    }

    /// The shader compiles on this machine's driver.
    ///
    /// Its own test because the failure it guards is invisible from anywhere
    /// else: a WGSL error is reported out of band, and before the error scope in
    /// [`compose`] existed it produced *blank cards that reported success*. The
    /// bug that motivated this was a struct field named `meta`, which is a
    /// reserved word — nothing about it is catchable at Rust compile time, and
    /// the parity tests could only say "these differ".
    #[test]
    fn the_shader_builds_on_this_machine() {
        let Some(context) = device::context() else {
            println!("SKIP: no GPU adapter on this machine");
            return;
        };
        assert!(
            pipeline(context).is_ok(),
            "the bloom shader would not build on {}",
            adapter_description().unwrap_or_default()
        );
    }

    /// A tile whose bloom lights nothing comes back transparent rather than
    /// undefined — the contract `Canvas::from_rgba8` is handed straight into.
    #[test]
    fn a_tile_with_no_splats_comes_back_transparent() {
        let _gate = crate::gpu::ForceEnabled::new(true).ignoring_the_cost();
        let tile = Tile {
            width: 16,
            height: 16,
            splats: Vec::new(),
        };
        match compose(std::slice::from_ref(&tile), curve()) {
            Ok(images) => {
                assert_eq!(images.len(), 1);
                assert_eq!(images[0], vec![0u8; 16 * 16 * 4]);
            }
            Err(Declined::NoDevice) => println!("SKIP: no GPU adapter on this machine"),
            Err(other) => panic!("unexpected decline: {other}"),
        }
    }

    fn curve() -> Curve {
        Curve {
            steps_per_px: 8.0,
            peak: 0.38,
            near_weight: 1.0,
            far_weight: 0.0,
            paint_floor: 0.002,
        }
    }
}
