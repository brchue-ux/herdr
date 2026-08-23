//! `herdr bench upload-churn` — does Herdr's idle upload cadence make a
//! graphics allocator grow?
//!
//! # The question, and why it is a benchmark and not a test
//!
//! The Windows render-stall investigation (`herdr-windows-render-stall-20260822`
//! §4.1/§4.2) measured what an *idle* fleet costs the terminal drawing it:
//! **13.9 whole-surface image uploads per second**, one 410x168 signal tray at
//! 2.83/s and six 390x126 sidebar cards at 11.07/s between them. Rio's wgpu
//! renderer has no texture-reuse path — every one of those uploads calls
//! `Device::create_texture` and drops the previous texture — so on DX12 the
//! churn lands on `wgpu-hal`'s suballocator, and when a request can no longer be
//! placed in an existing heap the allocator calls `ID3D12Device::CreateHeap`,
//! synchronously, on the thread that is drawing the window.
//!
//! That is a hypothesis about *this* machine's allocator, so it cannot be an
//! assertion in the test suite: what the free list does under a quarter of an
//! hour of steady mixed-size churn is a property of a driver, a heap policy and
//! a clock, and a threshold here would mean a different thing on every runner.
//! It is the same bargain [`super`] already makes for `bench cards`.
//!
//! # What it actually does
//!
//! It replays the measured cadence and nothing else. Seven surfaces, each with
//! its own period; on every tick the surface builds a fresh texture, uploads its
//! bytes, takes a view, and replaces what it was holding — which drops the
//! previous texture, exactly as
//! `sugarloaf/src/renderer/mod.rs`'s `ImageTexture::Wgpu` does. No render pass,
//! no swapchain, no compute: the claim under test is about allocation, and
//! anything else in the loop would only add noise the reader cannot subtract.
//!
//! Every few seconds it asks the device for
//! [`wgpu::Device::generate_allocator_report`] and records the four numbers that
//! answer the question — how many live allocations, how many memory blocks, how
//! many bytes are allocated and how many are *reserved* (the blocks' total size,
//! including everything the free list is holding but not using).
//!
//! # Reading the series, and the control that keeps it honest
//!
//! - **`allocated` is the harness's own control.** Seven surfaces hold one
//!   texture each, so live allocations and allocated bytes must sit flat at
//!   seven textures' worth for the whole run. If *that* climbs, this probe is
//!   leaking and no other column means anything.
//! - **`reserved` and `blocks` are the result.** Flat across the run with
//!   `allocated` flat means the allocator settled into its heaps and never asked
//!   the driver for another — which kills the hypothesis outright. Climbing
//!   means the mechanism is real above the driver, and only what `CreateHeap`
//!   *costs* on a given card is still open.
//! - **`create` and `upload` are what a stall would look like.** Each upload's
//!   `create_texture` is timed on its own, because that is the call `CreateHeap`
//!   hides inside; a run whose worst `create` is a millisecond did not stall, and
//!   one whose worst is hundreds has reproduced the symptom.
//!
//! A software adapter reproduces every allocator decision faithfully — the whole
//! path from `create_texture` down to `CreateHeap` is backend Rust — but not
//! what the driver charges for the heap. So a **flat** series from a WARP runner
//! is a real answer and a **slow** one is not.
//!
//! # Why it builds its own device
//!
//! [`crate::gpu::device`]'s process-wide device asks for
//! `MemoryHints::MemoryUsage`, which gives `wgpu-hal`'s DX12 allocator **8 MiB**
//! memory blocks. `MemoryHints::default()` — what a renderer that never thought
//! about it gets, Rio included — is `Performance`, which gives **256 MiB**
//! blocks. Since the block size is the whole mechanism, the probe stands up its
//! own device with the hint under test (`--memory-hint`, defaulting to the one
//! Rio has) rather than borrowing a device configured for something else.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(super) const USAGE: &str =
    "usage: herdr bench upload-churn [--seconds N] [--sample-seconds N] [--rate PER_SEC]\n\
     \x20                               [--cards N] [--card-size WxH] [--tray-size WxH]\n\
     \x20                               [--memory-hint performance|memory-usage]\n\
     \x20                               [--gpu-backend auto|dx12|vulkan|gl|metal] [--out PATH]";

/// The measured idle cadence, from `herdr-windows-render-stall-20260822` §4.1:
/// 1,948 uploads over 140 s of a completely idle fleet.
const MEASURED_RATE: f64 = 13.9;
/// The signal tray's share of it: 396 of those uploads, at 410x168.
const MEASURED_TRAY_RATE: f64 = 2.83;
/// The six sidebar cards' share: 1,552 uploads, at 390x126.
const MEASURED_CARD_RATE: f64 = 11.07;
const DEFAULT_CARDS: usize = 6;
const DEFAULT_TRAY: Size = Size {
    width: 410,
    height: 168,
};
const DEFAULT_CARD: Size = Size {
    width: 390,
    height: 126,
};
/// Fifteen minutes. Long enough that a free list which is going to fragment has
/// put ~12,500 textures through the allocator, short enough to sit inside a CI
/// job next to a release build.
const DEFAULT_SECONDS: u64 = 900;
const DEFAULT_SAMPLE_SECONDS: u64 = 5;
/// Samples inside this much of the start are the allocator warming up, not the
/// steady state the verdict is about.
const SETTLE: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Size {
    width: u32,
    height: u32,
}

impl Size {
    fn bytes(self) -> usize {
        self.width as usize * self.height as usize * 4
    }
}

impl std::fmt::Display for Size {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Which memory-block size policy `wgpu-hal` should be asked for. The two names
/// are wgpu's, not ours; the block sizes are what its DX12 backend maps them to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MemoryHint {
    /// `MemoryHints::Performance`, wgpu's default and so Rio's: 256 MiB blocks.
    Performance,
    /// `MemoryHints::MemoryUsage`, what Herdr's own compute device asks for:
    /// 8 MiB blocks.
    MemoryUsage,
}

impl MemoryHint {
    fn label(self) -> &'static str {
        match self {
            MemoryHint::Performance => "performance",
            MemoryHint::MemoryUsage => "memory-usage",
        }
    }

    /// What `wgpu-hal`'s DX12 backend turns this into
    /// (`dx12/suballocation.rs::Allocator::new`), stated so a reader does not
    /// have to go and look it up to size the result.
    fn device_blocks(self) -> &'static str {
        match self {
            MemoryHint::Performance => "256 MiB device blocks / 64 MiB host",
            MemoryHint::MemoryUsage => "8 MiB device blocks / 4 MiB host",
        }
    }

    fn hints(self) -> wgpu::MemoryHints {
        match self {
            MemoryHint::Performance => wgpu::MemoryHints::Performance,
            MemoryHint::MemoryUsage => wgpu::MemoryHints::MemoryUsage,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BackendChoice {
    Auto,
    Dx12,
    Vulkan,
    Gl,
    Metal,
}

impl BackendChoice {
    fn label(self) -> &'static str {
        match self {
            BackendChoice::Auto => "auto",
            BackendChoice::Dx12 => "dx12",
            BackendChoice::Vulkan => "vulkan",
            BackendChoice::Gl => "gl",
            BackendChoice::Metal => "metal",
        }
    }

    fn backends(self) -> wgpu::Backends {
        match self {
            BackendChoice::Auto => wgpu::Backends::all(),
            BackendChoice::Dx12 => wgpu::Backends::DX12,
            BackendChoice::Vulkan => wgpu::Backends::VULKAN,
            BackendChoice::Gl => wgpu::Backends::GL,
            BackendChoice::Metal => wgpu::Backends::METAL,
        }
    }
}

pub(super) struct Options {
    seconds: u64,
    sample_seconds: u64,
    rate: f64,
    cards: usize,
    card: Size,
    tray: Size,
    hint: MemoryHint,
    backend: BackendChoice,
    out: Option<PathBuf>,
}

pub(super) fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        seconds: DEFAULT_SECONDS,
        sample_seconds: DEFAULT_SAMPLE_SECONDS,
        rate: MEASURED_RATE,
        cards: DEFAULT_CARDS,
        card: DEFAULT_CARD,
        tray: DEFAULT_TRAY,
        hint: MemoryHint::Performance,
        backend: BackendChoice::Auto,
        out: None,
    };

    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--seconds" => options.seconds = number(value(args, &mut index)?, flag)?,
            "--sample-seconds" => options.sample_seconds = number(value(args, &mut index)?, flag)?,
            "--rate" => options.rate = number(value(args, &mut index)?, flag)?,
            "--cards" => options.cards = number(value(args, &mut index)?, flag)?,
            "--card-size" => options.card = size(value(args, &mut index)?, flag)?,
            "--tray-size" => options.tray = size(value(args, &mut index)?, flag)?,
            "--memory-hint" => {
                options.hint = match value(args, &mut index)? {
                    "performance" => MemoryHint::Performance,
                    "memory-usage" => MemoryHint::MemoryUsage,
                    other => {
                        return Err(format!(
                            "--memory-hint must be performance or memory-usage, not {other}"
                        ))
                    }
                }
            }
            "--gpu-backend" => {
                options.backend = match value(args, &mut index)? {
                    "auto" => BackendChoice::Auto,
                    "dx12" => BackendChoice::Dx12,
                    "vulkan" => BackendChoice::Vulkan,
                    "gl" => BackendChoice::Gl,
                    "metal" => BackendChoice::Metal,
                    other => {
                        return Err(format!(
                            "--gpu-backend must be auto, dx12, vulkan, gl or metal, not {other}"
                        ))
                    }
                }
            }
            "--out" => options.out = Some(PathBuf::from(value(args, &mut index)?)),
            other => return Err(format!("unknown option {other}")),
        }
        index += 1;
    }

    if options.seconds == 0 {
        return Err("--seconds must be at least 1".into());
    }
    if options.sample_seconds == 0 {
        return Err("--sample-seconds must be at least 1".into());
    }
    // NaN included: `--rate nan` parses as an f64 and would then divide the
    // schedule into intervals nothing can wait for.
    if !options.rate.is_finite() || options.rate <= 0.0 {
        return Err("--rate must be a finite number greater than zero".into());
    }
    if options.card.width == 0
        || options.card.height == 0
        || options.tray.width == 0
        || options.tray.height == 0
    {
        return Err("a surface size must be WxH with both above zero".into());
    }
    Ok(options)
}

/// The value after the flag at `index`, advancing `index` onto it.
fn value<'a>(args: &'a [String], index: &mut usize) -> Result<&'a str, String> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("{} needs a value", args[*index - 1]))
}

fn number<T: std::str::FromStr>(raw: &str, flag: &str) -> Result<T, String> {
    raw.parse()
        .map_err(|_| format!("{flag} needs a non-negative number, not '{raw}'"))
}

fn size(raw: &str, flag: &str) -> Result<Size, String> {
    let (width, height) = raw
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{flag} needs a WxH size, not '{raw}'"))?;
    Ok(Size {
        width: number(width, flag)?,
        height: number(height, flag)?,
    })
}

/// One surface that redraws on its own clock, holding exactly one texture at a
/// time — the shape of a sugarloaf image entry keyed by a stable Herdr image id.
struct Slot {
    label: String,
    size: Size,
    interval: Duration,
    next_at: Instant,
    pixels: Vec<u8>,
    held: Option<Held>,
}

struct Held {
    /// Held, not read, exactly as sugarloaf holds it: the texture stays alive
    /// because the view does not own it, and dropping this is what frees the
    /// allocation.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    #[allow(dead_code)]
    view: wgpu::TextureView,
}

/// The seven clocks, staggered so same-period surfaces do not all fire on the
/// same instant — six cards arriving together would be one 1.15 MB burst every
/// 540 ms rather than the steady trickle that was measured.
fn schedule(options: &Options, start: Instant) -> Vec<Slot> {
    let scale = options.rate / MEASURED_RATE;
    let mut slots = Vec::with_capacity(options.cards + 1);

    let tray_rate = MEASURED_TRAY_RATE * scale;
    if tray_rate > 0.0 {
        slots.push(Slot {
            label: "signal tray".into(),
            size: options.tray,
            interval: Duration::from_secs_f64(1.0 / tray_rate),
            next_at: start,
            pixels: vec![0u8; options.tray.bytes()],
            held: None,
        });
    }

    if options.cards > 0 {
        let each = MEASURED_CARD_RATE * scale / options.cards as f64;
        let interval = Duration::from_secs_f64(1.0 / each);
        for card in 0..options.cards {
            slots.push(Slot {
                label: format!("card {card}"),
                size: options.card,
                interval,
                next_at: start + interval.mul_f64(card as f64 / options.cards as f64),
                pixels: vec![0u8; options.card.bytes()],
                held: None,
            });
        }
    }
    slots
}

/// What the allocator was holding at one instant.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Allocator {
    allocations: usize,
    blocks: usize,
    allocated_bytes: u64,
    reserved_bytes: u64,
    largest_block_bytes: u64,
}

/// One row of the series.
#[derive(Clone, Copy, Debug)]
struct Sample {
    at: Duration,
    uploads: u64,
    live: usize,
    /// `None` on a backend that does not report one — see [`verdict`].
    allocator: Option<Allocator>,
    slowest_create: Duration,
    slowest_upload: Duration,
}

/// What the series says about the hypothesis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// The backend produced no allocator report, so nothing was measured.
    Unavailable,
    /// Not enough samples past [`SETTLE`] to compare a start with an end.
    TooShort,
    /// This probe's own live allocations grew, so the series is about the probe
    /// and not about the allocator.
    HarnessLeaked { from: usize, to: usize },
    /// Reserved bytes and block count held still: the allocator settled.
    Flat { reserved: u64, blocks: usize },
    /// The allocator kept asking for memory under steady churn.
    Grew {
        reserved_from: u64,
        reserved_to: u64,
        blocks_from: usize,
        blocks_to: usize,
    },
}

/// Compare the first settled sample with the last one.
///
/// Split out from the run loop so the classification is testable without a GPU:
/// it is the one piece of judgement in this file, and it is the piece a reader
/// is most entitled to check.
fn verdict(samples: &[Sample]) -> Verdict {
    let settled: Vec<&Sample> = samples.iter().filter(|s| s.at >= SETTLE).collect();
    let (Some(first), Some(last)) = (settled.first(), settled.last()) else {
        return Verdict::TooShort;
    };
    if std::ptr::eq(*first, *last) {
        return Verdict::TooShort;
    }
    let (Some(from), Some(to)) = (first.allocator, last.allocator) else {
        return Verdict::Unavailable;
    };
    if last.live > first.live {
        return Verdict::HarnessLeaked {
            from: first.live,
            to: last.live,
        };
    }
    if to.reserved_bytes <= from.reserved_bytes && to.blocks <= from.blocks {
        return Verdict::Flat {
            reserved: to.reserved_bytes,
            blocks: to.blocks,
        };
    }
    Verdict::Grew {
        reserved_from: from.reserved_bytes,
        reserved_to: to.reserved_bytes,
        blocks_from: from.blocks,
        blocks_to: to.blocks,
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::AdapterInfo,
}

/// A device configured the way the renderer under investigation configures its
/// own — see this module's header for why it is not [`crate::gpu::device`]'s.
fn acquire(options: &Options) -> Result<Gpu, String> {
    // `wgpu::Instance::new` panics rather than erroring on a target with no
    // backend compiled in, so the set is checked before an instance is asked
    // for — the same guard `crate::gpu::device` takes.
    if wgpu::Instance::enabled_backend_features().is_empty() {
        return Err(format!(
            "no wgpu backend is compiled in for {}",
            std::env::consts::OS
        ));
    }
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: options.backend.backends(),
        ..Default::default()
    });
    let adapter = crate::gpu::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .map_err(|error| {
        format!(
            "no adapter for --gpu-backend {}: {error}",
            options.backend.label()
        )
    })?;
    let info = adapter.get_info();
    let limits = adapter.limits();
    let (device, queue) = crate::gpu::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("herdr upload-churn probe"),
        required_features: wgpu::Features::empty(),
        required_limits: limits,
        memory_hints: options.hint.hints(),
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .map_err(|error| format!("adapter {} refused a device: {error}", info.name))?;

    device.on_uncaptured_error(std::sync::Arc::new(|error| {
        eprintln!("wgpu reported an uncaptured error during the churn probe: {error}");
    }));

    Ok(Gpu {
        device,
        queue,
        adapter: info,
    })
}

/// One surface's redraw: build, upload, view, replace. The previous texture dies
/// on the assignment, which is the whole point.
///
/// Returns how long `create_texture` took on its own and how long the whole
/// upload took, in that order.
fn upload(gpu: &Gpu, slot: &mut Slot, tick: u64) -> (Duration, Duration) {
    // Vary the content so nothing anywhere can decide two uploads are the same
    // image. It changes no allocation; it removes a doubt.
    if let Some(first) = slot.pixels.first_mut() {
        *first = tick as u8;
    }

    let started = Instant::now();
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&slot.label),
        size: wgpu::Extent3d {
            width: slot.size.width,
            height: slot.size.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let created = started.elapsed();

    gpu.queue.write_texture(
        texture.as_image_copy(),
        &slot.pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(slot.size.width * 4),
            rows_per_image: Some(slot.size.height),
        },
        wgpu::Extent3d {
            width: slot.size.width,
            height: slot.size.height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // Replacing the entry drops the previous texture and view — the free the
    // hypothesis is about. The new texture is built *before* the old one dies,
    // as it is in sugarloaf, so the peak live count is one above the slot count.
    slot.held = Some(Held { texture, view });

    // A frame boundary. Without a submission the queue's pending writes never
    // flush and without a maintain the dropped textures are never reclaimed —
    // either omission would manufacture growth that a real renderer does not
    // have.
    gpu.queue.submit(std::iter::empty());
    let _ = gpu.device.poll(wgpu::PollType::Poll);

    (created, started.elapsed())
}

fn snapshot(gpu: &Gpu) -> Option<Allocator> {
    let report = gpu.device.generate_allocator_report()?;
    Some(Allocator {
        allocations: report.allocations.len(),
        blocks: report.blocks.len(),
        allocated_bytes: report.total_allocated_bytes,
        reserved_bytes: report.total_reserved_bytes,
        largest_block_bytes: report
            .blocks
            .iter()
            .map(|block| block.size)
            .max()
            .unwrap_or(0),
    })
}

pub(super) fn run(options: Options) -> i32 {
    let gpu = match acquire(&options) {
        Ok(gpu) => gpu,
        Err(why) => {
            eprintln!("error: {why}");
            return 1;
        }
    };

    let start = Instant::now();
    let mut slots = schedule(&options, start);
    if slots.is_empty() {
        eprintln!("error: --cards 0 with no tray leaves nothing to upload");
        return 2;
    }

    header(&options, &gpu, &slots);

    let deadline = start + Duration::from_secs(options.seconds);
    let sample_every = Duration::from_secs(options.sample_seconds);
    let mut next_sample = start + sample_every;
    let mut samples = Vec::new();
    let mut uploads = 0u64;
    // Uploads that could not be delivered on time, so the report can say
    // whether the cadence it claims is the cadence it achieved.
    let mut behind = 0u64;
    let mut slowest_create = Duration::ZERO;
    let mut slowest_upload = Duration::ZERO;
    let mut worst_create = Duration::ZERO;
    let mut worst_upload = Duration::ZERO;

    row_header();
    let baseline = Sample {
        at: Duration::ZERO,
        uploads: 0,
        live: 0,
        allocator: snapshot(&gpu),
        slowest_create: Duration::ZERO,
        slowest_upload: Duration::ZERO,
    };
    row(&baseline);
    samples.push(baseline);

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let due = slots
            .iter()
            .enumerate()
            .min_by_key(|(_, slot)| slot.next_at)
            .map(|(index, _)| index)
            .unwrap_or(0);
        let wake = slots[due].next_at.min(next_sample).min(deadline);
        if wake > now {
            std::thread::sleep(wake - now);
        }

        let now = Instant::now();
        if now >= slots[due].next_at {
            let (created, whole) = upload(&gpu, &mut slots[due], uploads);
            uploads += 1;
            slowest_create = slowest_create.max(created);
            slowest_upload = slowest_upload.max(whole);
            worst_create = worst_create.max(created);
            worst_upload = worst_upload.max(whole);
            let interval = slots[due].interval;
            slots[due].next_at += interval;
            if slots[due].next_at <= now {
                // A machine that cannot keep up must not then burst to catch up:
                // that would be a different workload from the one measured.
                behind += 1;
                slots[due].next_at = now + interval;
            }
        }

        if now >= next_sample {
            // Drain first, so `reserved` means "with everything reclaimable
            // reclaimed" rather than "with an unknown amount still in flight".
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            let sample = Sample {
                at: now.duration_since(start),
                uploads,
                live: slots.iter().filter(|slot| slot.held.is_some()).count(),
                allocator: snapshot(&gpu),
                slowest_create,
                slowest_upload,
            };
            row(&sample);
            samples.push(sample);
            slowest_create = Duration::ZERO;
            slowest_upload = Duration::ZERO;
            next_sample += sample_every;
            if next_sample <= now {
                next_sample = now + sample_every;
            }
        }
    }

    report(
        &options,
        &gpu,
        &samples,
        uploads,
        behind,
        worst_create,
        worst_upload,
    );

    if let Some(path) = options.out.as_ref() {
        if let Err(error) = write_csv(path, &options, &gpu, &samples) {
            eprintln!("error: could not write {}: {error}", path.display());
            return 1;
        }
        println!("series written to {}", path.display());
    }
    0
}

fn header(options: &Options, gpu: &Gpu, slots: &[Slot]) {
    let scale = options.rate / MEASURED_RATE;
    println!("herdr upload-churn probe");
    println!(
        "  build     {} ({}, {})",
        crate::build_info::version(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "  adapter   {} [{:?}, {:?}] driver {} {}",
        gpu.adapter.name,
        gpu.adapter.backend,
        gpu.adapter.device_type,
        gpu.adapter.driver,
        gpu.adapter.driver_info
    );
    println!(
        "  device    memory hint {} ({})",
        options.hint.label(),
        options.hint.device_blocks()
    );
    println!(
        "  cadence   {:.2} uploads/s = {:.2}/s at {} + {:.2}/s across {} surfaces at {}",
        options.rate,
        MEASURED_TRAY_RATE * scale,
        options.tray,
        MEASURED_CARD_RATE * scale,
        options.cards,
        options.card
    );
    let per_second: f64 = slots
        .iter()
        .map(|slot| slot.size.bytes() as f64 / slot.interval.as_secs_f64())
        .sum();
    println!(
        "  bytes     {:.1} KiB per tray upload, {:.1} KiB per card upload, {:.2} MiB/s of texture",
        options.tray.bytes() as f64 / 1024.0,
        options.card.bytes() as f64 / 1024.0,
        per_second / (1024.0 * 1024.0)
    );
    println!(
        "  run       {} s, sampled every {} s, first {} s treated as warm-up",
        options.seconds,
        options.sample_seconds,
        SETTLE.as_secs()
    );
    println!();
}

fn row_header() {
    println!(
        "{:>7} {:>9} {:>5} {:>8} {:>7} {:>12} {:>12} {:>11} {:>11}",
        "t (s)",
        "uploads",
        "live",
        "allocs",
        "blocks",
        "allocated",
        "reserved",
        "create (ms)",
        "upload (ms)"
    );
}

fn row(sample: &Sample) {
    let (allocations, blocks, allocated, reserved) = match sample.allocator {
        Some(allocator) => (
            allocator.allocations.to_string(),
            allocator.blocks.to_string(),
            mib(allocator.allocated_bytes),
            mib(allocator.reserved_bytes),
        ),
        None => ("-".into(), "-".into(), "-".into(), "-".into()),
    };
    println!(
        "{:>7.0} {:>9} {:>5} {:>8} {:>7} {:>12} {:>12} {:>11.3} {:>11.3}",
        sample.at.as_secs_f64(),
        sample.uploads,
        sample.live,
        allocations,
        blocks,
        allocated,
        reserved,
        sample.slowest_create.as_secs_f64() * 1000.0,
        sample.slowest_upload.as_secs_f64() * 1000.0,
    );
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn report(
    options: &Options,
    gpu: &Gpu,
    samples: &[Sample],
    uploads: u64,
    behind: u64,
    worst_create: Duration,
    worst_upload: Duration,
) {
    let elapsed = samples
        .last()
        .map(|sample| sample.at.as_secs_f64())
        .unwrap_or(0.0);
    println!();
    println!(
        "delivered {uploads} uploads in {elapsed:.0} s ({:.2}/s; {behind} arrived late)",
        if elapsed > 0.0 {
            uploads as f64 / elapsed
        } else {
            0.0
        }
    );
    println!(
        "worst create_texture {:.3} ms, worst whole upload {:.3} ms",
        worst_create.as_secs_f64() * 1000.0,
        worst_upload.as_secs_f64() * 1000.0
    );

    match verdict(samples) {
        Verdict::Unavailable => println!(
            "\nVERDICT: inconclusive — the {:?} backend produces no allocator report, so nothing\n\
             about heap growth was measured. Only DX12 implements one in wgpu-hal 27; re-run this\n\
             on Windows with --gpu-backend dx12.",
            gpu.adapter.backend
        ),
        Verdict::TooShort => println!(
            "\nVERDICT: inconclusive — fewer than two samples past the {} s warm-up. Run longer\n\
             than --sample-seconds {} plus that warm-up.",
            SETTLE.as_secs(),
            options.sample_seconds
        ),
        Verdict::HarnessLeaked { from, to } => println!(
            "\nVERDICT: void — this probe's own live textures went from {from} to {to}, so the\n\
             allocator series is measuring a leak in the harness and not the allocator. Nothing\n\
             below the control is readable."
        ),
        Verdict::Flat { reserved, blocks } => println!(
            "\nVERDICT: flat — {} reserved across {blocks} block(s) at the end, no more than at\n\
             the start. Under this cadence the allocator settled into its heaps and stopped asking\n\
             the driver for more, so allocator growth is not the mechanism behind the stall.",
            mib(reserved)
        ),
        Verdict::Grew {
            reserved_from,
            reserved_to,
            blocks_from,
            blocks_to,
        } => println!(
            "\nVERDICT: grew — reserved {} -> {} and {blocks_from} -> {blocks_to} block(s) while\n\
             this probe held a constant number of live textures. The allocator keeps asking for\n\
             memory under the measured cadence, so the mechanism is real above the driver; what a\n\
             heap request *costs* is a separate question and needs the card in question.",
            mib(reserved_from),
            mib(reserved_to)
        ),
    }
}

fn write_csv(
    path: &std::path::Path,
    options: &Options,
    gpu: &Gpu,
    samples: &[Sample],
) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "# herdr bench upload-churn")?;
    writeln!(out, "# build,{}", crate::build_info::version())?;
    writeln!(out, "# os,{}", std::env::consts::OS)?;
    writeln!(out, "# adapter,{}", gpu.adapter.name)?;
    writeln!(out, "# backend,{:?}", gpu.adapter.backend)?;
    writeln!(out, "# device_type,{:?}", gpu.adapter.device_type)?;
    writeln!(
        out,
        "# driver,{} {}",
        gpu.adapter.driver, gpu.adapter.driver_info
    )?;
    writeln!(out, "# memory_hint,{}", options.hint.label())?;
    writeln!(out, "# memory_blocks,{}", options.hint.device_blocks())?;
    writeln!(out, "# rate_per_second,{}", options.rate)?;
    writeln!(out, "# tray_size,{}", options.tray)?;
    writeln!(out, "# card_size,{}", options.card)?;
    writeln!(out, "# cards,{}", options.cards)?;
    writeln!(out, "# seconds,{}", options.seconds)?;
    writeln!(
        out,
        "elapsed_s,uploads,live_textures,allocations,blocks,allocated_bytes,reserved_bytes,largest_block_bytes,slowest_create_us,slowest_upload_us"
    )?;
    for sample in samples {
        let (allocations, blocks, allocated, reserved, largest) = match sample.allocator {
            Some(allocator) => (
                allocator.allocations.to_string(),
                allocator.blocks.to_string(),
                allocator.allocated_bytes.to_string(),
                allocator.reserved_bytes.to_string(),
                allocator.largest_block_bytes.to_string(),
            ),
            None => (
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ),
        };
        writeln!(
            out,
            "{:.3},{},{},{allocations},{blocks},{allocated},{reserved},{largest},{},{}",
            sample.at.as_secs_f64(),
            sample.uploads,
            sample.live,
            sample.slowest_create.as_micros(),
            sample.slowest_upload.as_micros(),
        )?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn the_defaults_are_the_measured_cadence() {
        let options = parse(&[]).expect("no arguments is a valid run");
        assert_eq!(options.rate, MEASURED_RATE);
        assert_eq!(options.tray, DEFAULT_TRAY);
        assert_eq!(options.card, DEFAULT_CARD);
        assert_eq!(options.cards, DEFAULT_CARDS);
        assert_eq!(
            options.hint,
            MemoryHint::Performance,
            "the default must be the hint the renderer under investigation has, \
             not the one Herdr's own compute device asks for"
        );
    }

    /// The seven clocks have to add up to the rate that was measured, or the
    /// whole run is replaying a workload nobody observed.
    #[test]
    fn the_schedule_adds_up_to_the_requested_rate() {
        let options = parse(&[]).expect("defaults");
        let slots = schedule(&options, Instant::now());
        assert_eq!(slots.len(), DEFAULT_CARDS + 1);
        let total: f64 = slots
            .iter()
            .map(|slot| 1.0 / slot.interval.as_secs_f64())
            .sum();
        assert!(
            (total - MEASURED_RATE).abs() < 0.05,
            "the slots deliver {total} uploads/s, not the measured {MEASURED_RATE}"
        );
    }

    #[test]
    fn a_scaled_rate_scales_every_slot() {
        let options = parse(&args(&["--rate", "27.8"])).expect("twice the cadence");
        let slots = schedule(&options, Instant::now());
        let total: f64 = slots
            .iter()
            .map(|slot| 1.0 / slot.interval.as_secs_f64())
            .sum();
        assert!((total - 27.8).abs() < 0.1, "delivered {total} uploads/s");
    }

    /// Six cards firing on the same instant would be one burst rather than the
    /// trickle that was measured.
    #[test]
    fn same_period_surfaces_are_staggered() {
        let start = Instant::now();
        let options = parse(&[]).expect("defaults");
        let slots = schedule(&options, start);
        let cards: Vec<Instant> = slots
            .iter()
            .filter(|slot| slot.size == DEFAULT_CARD)
            .map(|slot| slot.next_at)
            .collect();
        for pair in cards.windows(2) {
            assert!(pair[1] > pair[0], "two cards start on the same instant");
        }
    }

    #[test]
    fn a_bad_argument_is_refused_rather_than_defaulted() {
        assert!(
            parse(&args(&["--seconds"])).is_err(),
            "a flag with no value"
        );
        assert!(parse(&args(&["--seconds", "0"])).is_err(), "an empty run");
        assert!(parse(&args(&["--rate", "0"])).is_err(), "no uploads at all");
        assert!(parse(&args(&["--card-size", "390"])).is_err(), "no height");
        assert!(parse(&args(&["--card-size", "0x126"])).is_err(), "no width");
        assert!(
            parse(&args(&["--memory-hint", "fast"])).is_err(),
            "not a hint"
        );
        assert!(
            parse(&args(&["--gpu-backend", "cuda"])).is_err(),
            "not a backend"
        );
        assert!(parse(&args(&["--nonsense"])).is_err(), "an unknown flag");
    }

    #[test]
    fn sizes_parse_either_way_round() {
        let options = parse(&args(&["--card-size", "12X34", "--tray-size", "56x78"]))
            .expect("both separators");
        assert_eq!(
            options.card,
            Size {
                width: 12,
                height: 34
            }
        );
        assert_eq!(
            options.tray,
            Size {
                width: 56,
                height: 78
            }
        );
    }

    fn sample(at: u64, live: usize, blocks: usize, reserved: u64) -> Sample {
        Sample {
            at: Duration::from_secs(at),
            uploads: at * 14,
            live,
            allocator: Some(Allocator {
                allocations: live,
                blocks,
                allocated_bytes: live as u64 * 200_000,
                reserved_bytes: reserved,
                largest_block_bytes: reserved,
            }),
            slowest_create: Duration::from_micros(50),
            slowest_upload: Duration::from_micros(200),
        }
    }

    /// The falsification the whole probe exists for: a settled allocator is a
    /// dead hypothesis.
    #[test]
    fn a_settled_allocator_reads_as_flat() {
        let samples = [
            sample(0, 0, 0, 0),
            sample(35, 7, 1, 8 << 20),
            sample(900, 7, 1, 8 << 20),
        ];
        assert_eq!(
            verdict(&samples),
            Verdict::Flat {
                reserved: 8 << 20,
                blocks: 1
            }
        );
    }

    #[test]
    fn a_growing_allocator_reads_as_grew() {
        let samples = [sample(35, 7, 1, 8 << 20), sample(900, 7, 4, 32 << 20)];
        assert!(matches!(
            verdict(&samples),
            Verdict::Grew {
                blocks_from: 1,
                blocks_to: 4,
                ..
            }
        ));
    }

    /// Growth this probe caused itself is not evidence about the allocator, and
    /// must not be reported as if it were.
    #[test]
    fn a_leaking_harness_voids_its_own_series() {
        let samples = [sample(35, 7, 1, 8 << 20), sample(900, 90, 4, 32 << 20)];
        assert_eq!(
            verdict(&samples),
            Verdict::HarnessLeaked { from: 7, to: 90 }
        );
    }

    /// Warm-up samples are not the steady state, so a run that never leaves it
    /// says so instead of comparing two points inside it.
    #[test]
    fn a_run_shorter_than_the_warm_up_is_inconclusive() {
        let samples = [sample(0, 0, 0, 0), sample(5, 7, 1, 8 << 20)];
        assert_eq!(verdict(&samples), Verdict::TooShort);
    }

    #[test]
    fn a_backend_with_no_report_is_inconclusive_not_flat() {
        let mut samples = [sample(35, 7, 1, 8 << 20), sample(900, 7, 1, 8 << 20)];
        for sample in &mut samples {
            sample.allocator = None;
        }
        assert_eq!(verdict(&samples), Verdict::Unavailable);
    }
}
