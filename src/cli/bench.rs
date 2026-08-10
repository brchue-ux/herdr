//! `herdr bench cards` — what card rasterisation costs on *this* machine.
//!
//! # Why a subcommand and not a `cargo bench`
//!
//! The question this answers is a property of a machine: a GPU round trip is
//! measured, not assumed (`crate::gpu::bloom::Calibration`), and the machine
//! whose number matters is the captain's Windows box with an RX 6900 XT. A
//! `cargo bench` or an `#[ignore]`d test needs the source tree, a toolchain and
//! a compile on that box. A subcommand rides the binary that is already being
//! shipped there, needs no server, no session and no fleet, and runs in one
//! command.
//!
//! # What it measures
//!
//! Whole frames through [`crate::ui::sidebar::image_card::rasterise_card_scene`]
//! — the real client entry point — with the bloom backend pinned first to the
//! CPU and then to the GPU. So each number is an end-to-end frame: layout, text
//! shaping, glyph rasterisation, bloom, the card itself, and the PNG encode that
//! puts it on the wire. The bloom is the only stage the two runs do differently,
//! which is exactly the comparison `src/gpu/` is for; everything around it is
//! held identical so the difference between the two columns is attributable.
//!
//! # It reports what it ran on
//!
//! Every run prints the adapter wgpu actually picked, and a GPU run that
//! declined — no adapter, a driver that refused a device, a build without the
//! `gpu-raster` feature — says so instead of quietly reporting CPU timings under
//! a GPU heading. `crate::gpu::bloom::TILES_COMPOSED` is the positive control:
//! it counts tiles the compute pass really composed, and a "GPU" run that
//! composed none did not use one.

use std::time::{Duration, Instant};

use crate::ui::sidebar::image_card::bench as workload;

/// Frames drawn before the clock starts, per backend.
///
/// The first frame of either backend pays for things no later frame pays for:
/// the font search and face load, the glyph cache filling, and on the GPU side
/// adapter enumeration, device creation, shader compilation and the two-point
/// calibration — tens of milliseconds that are a property of process start, not
/// of throughput.
const DEFAULT_WARMUP: usize = 5;
/// Timed frames per backend. Enough that the p99 is a real sample and not the
/// worst of a handful.
const DEFAULT_FRAMES: usize = 200;

pub(super) fn run_bench_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("cards") => match parse(&args[1..]) {
            Ok(options) => Ok(run_cards(options)),
            Err(message) => {
                eprintln!("error: {message}");
                eprintln!("{USAGE}");
                Ok(2)
            }
        },
        Some("help" | "--help" | "-h") | None => {
            println!("{USAGE}");
            Ok(0)
        }
        Some(other) => {
            eprintln!("error: unknown bench target '{other}'");
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

const USAGE: &str = "usage: herdr bench cards [--cards N] [--frames N] [--warmup N]\n\
                     \x20                        [--backend cpu|gpu|both] [--panel-cols N]\n\
                     \x20                        [--cell-width PX] [--cell-height PX]";

/// Which backends to run, in the order they are reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Cpu,
    Gpu,
}

impl Backend {
    fn label(self) -> &'static str {
        match self {
            Backend::Cpu => "cpu",
            Backend::Gpu => "gpu",
        }
    }
}

struct Options {
    fleet: workload::Fleet,
    frames: usize,
    warmup: usize,
    backends: Vec<Backend>,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut fleet = workload::Fleet::default_fleet();
    let mut frames = DEFAULT_FRAMES;
    let mut warmup = DEFAULT_WARMUP;
    let mut backends = vec![Backend::Cpu, Backend::Gpu];

    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--cards" => fleet.cards = number(value(args, &mut index)?, flag)?,
            "--frames" => frames = number(value(args, &mut index)?, flag)?,
            "--warmup" => warmup = number(value(args, &mut index)?, flag)?,
            "--panel-cols" => fleet.panel_cols = number(value(args, &mut index)?, flag)?,
            "--cell-width" => fleet.cell.width_px = number(value(args, &mut index)?, flag)?,
            "--cell-height" => fleet.cell.height_px = number(value(args, &mut index)?, flag)?,
            "--backend" => {
                backends = match value(args, &mut index)? {
                    "cpu" => vec![Backend::Cpu],
                    "gpu" => vec![Backend::Gpu],
                    "both" => vec![Backend::Cpu, Backend::Gpu],
                    other => {
                        return Err(format!("--backend must be cpu, gpu or both, not {other}"))
                    }
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
        index += 1;
    }

    if fleet.cards == 0 {
        return Err("--cards must be at least 1".into());
    }
    if frames == 0 {
        return Err("--frames must be at least 1".into());
    }
    Ok(Options {
        fleet,
        frames,
        warmup,
        backends,
    })
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
        .map_err(|_| format!("{flag} needs a non-negative whole number, not '{raw}'"))
}

fn run_cards(options: Options) -> i32 {
    let fleet = options.fleet;
    let scene = match workload::scene(fleet) {
        Ok(scene) => scene,
        Err(why) => {
            eprintln!("error: {why}");
            return 1;
        }
    };

    println!("herdr card rasterisation benchmark");
    println!(
        "  build     {} ({}, {})",
        crate::build_info::version(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "  fleet     {} cards, {}x{} px cells, {}-column panel",
        fleet.cards, fleet.cell.width_px, fleet.cell.height_px, fleet.panel_cols
    );
    println!(
        "  cores     {} available, up to {} used per frame by the card threads",
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1),
        crate::ui::sidebar::image_card::raster_threads_for_bench(fleet.cards)
    );
    println!(
        "  adapter   {}",
        crate::gpu::bloom::adapter_description()
            .unwrap_or_else(|| "none — every run below is on the CPU".into())
    );
    println!(
        "  frames    {} timed per backend, after {} warm-up",
        options.frames, options.warmup
    );
    println!();

    let mut runs = Vec::new();
    for backend in &options.backends {
        match measure(*backend, &scene, &options) {
            Ok(run) => runs.push(run),
            Err(()) => {
                eprintln!(
                    "error: a frame failed to rasterise on the {} backend",
                    backend.label()
                );
                return 1;
            }
        }
    }

    report(&runs);
    0
}

/// One backend's measured run.
struct Run {
    backend: Backend,
    frames: usize,
    /// Every timed frame, sorted, for the percentiles.
    sorted: Vec<Duration>,
    wall: Duration,
    cards: usize,
    pixels: u64,
    bytes: usize,
    /// Tiles the compute pass actually composed during this run. Zero on a GPU
    /// run means the GPU never ran.
    tiles_composed: u64,
}

fn measure(
    backend: Backend,
    scene: &crate::ui::sidebar::image_card::CardScene,
    options: &Options,
) -> Result<Run, ()> {
    crate::gpu::pin_backend(backend == Backend::Gpu);

    let mut last = None;
    for _ in 0..options.warmup {
        last = Some(workload::draw(scene, options.fleet.cell)?);
    }

    let composed_before =
        crate::gpu::bloom::TILES_COMPOSED.load(std::sync::atomic::Ordering::Relaxed);
    let mut timings = Vec::with_capacity(options.frames);
    let wall_at = Instant::now();
    for _ in 0..options.frames {
        let at = Instant::now();
        let frame = workload::draw(scene, options.fleet.cell)?;
        timings.push(at.elapsed());
        last = Some(frame);
    }
    let wall = wall_at.elapsed();
    let composed = crate::gpu::bloom::TILES_COMPOSED
        .load(std::sync::atomic::Ordering::Relaxed)
        .saturating_sub(composed_before);

    // Warm-up runs at least once even when `--warmup 0`, because the
    // accounting below has to come from a frame.
    let frame = match last {
        Some(frame) => frame,
        None => workload::draw(scene, options.fleet.cell)?,
    };
    timings.sort_unstable();
    Ok(Run {
        backend,
        frames: options.frames,
        sorted: timings,
        wall,
        cards: frame.cards,
        pixels: frame.pixels,
        bytes: frame.bytes,
        tiles_composed: composed,
    })
}

impl Run {
    /// The frame time at `fraction` through the sorted samples. Nearest-rank,
    /// which for a few hundred samples is the honest reading and needs no
    /// interpolation nobody would be able to check.
    fn percentile(&self, fraction: f64) -> Duration {
        if self.sorted.is_empty() {
            return Duration::ZERO;
        }
        let rank = ((self.sorted.len() as f64) * fraction).ceil() as usize;
        self.sorted[rank.clamp(1, self.sorted.len()) - 1]
    }

    fn fps(&self) -> f64 {
        if self.wall.is_zero() {
            return 0.0;
        }
        self.frames as f64 / self.wall.as_secs_f64()
    }

    /// Whether this run drew what its label says it drew.
    fn honest(&self) -> bool {
        match self.backend {
            Backend::Cpu => self.tiles_composed == 0,
            Backend::Gpu => self.tiles_composed > 0,
        }
    }
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn report(runs: &[Run]) {
    println!(
        "{:<8} {:>7} {:>10} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "backend", "frames", "wall (s)", "fps", "p50 (ms)", "p95 (ms)", "p99 (ms)", "max (ms)"
    );
    for run in runs {
        println!(
            "{:<8} {:>7} {:>10.3} {:>9.1} {:>9.3} {:>9.3} {:>9.3} {:>9.3}",
            run.backend.label(),
            run.frames,
            run.wall.as_secs_f64(),
            run.fps(),
            ms(run.percentile(0.50)),
            ms(run.percentile(0.95)),
            ms(run.percentile(0.99)),
            ms(*run.sorted.last().unwrap_or(&Duration::ZERO)),
        );
    }
    println!();

    if let Some(run) = runs.first() {
        println!(
            "per frame: {} cards, {:.2} Mpx of card image, {:.1} KiB encoded",
            run.cards,
            run.pixels as f64 / 1_000_000.0,
            run.bytes as f64 / 1024.0
        );
    }

    // Stated on every run, not only when it disagrees with the label: "the GPU
    // was used" is the one claim in this report that the timings themselves
    // cannot evidence, and a reader should not have to trust the absence of a
    // warning for it.
    for run in runs {
        println!(
            "{:<10} {} tiles composed on the compute pass across {} timed frames",
            format!("{}:", run.backend.label()),
            run.tiles_composed,
            run.frames
        );
    }

    // The comparison, only when both halves of it were actually run.
    let cpu = runs.iter().find(|run| run.backend == Backend::Cpu);
    let gpu = runs.iter().find(|run| run.backend == Backend::Gpu);
    if let (Some(cpu), Some(gpu)) = (cpu, gpu) {
        let p50 = ms(gpu.percentile(0.50));
        if p50 > 0.0 {
            println!(
                "speedup:   {:.2}x at p50 ({:.3} ms -> {:.3} ms per frame)",
                ms(cpu.percentile(0.50)) / p50,
                ms(cpu.percentile(0.50)),
                p50
            );
        }
    }

    for run in runs {
        if run.honest() {
            continue;
        }
        match run.backend {
            // The one that matters. Everything about the GPU path declines
            // silently by design — that is what makes it safe — so a run
            // labelled `gpu` that composed no tiles is measuring the CPU.
            Backend::Gpu => println!(
                "\nWARNING: the gpu run composed no tiles on the compute pass, so its numbers\n\
                 are the CPU path's. This machine has no usable adapter, this build has no\n\
                 gpu-raster feature, or the pass declined — check the adapter line above."
            ),
            Backend::Cpu => println!(
                "\nWARNING: the cpu run composed {} tiles on the compute pass, so its numbers\n\
                 are not a clean CPU baseline.",
                run.tiles_composed
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_a_full_sidebar_of_cards() {
        let options = parse(&[]).expect("no arguments is a valid run");
        assert_eq!(options.frames, DEFAULT_FRAMES);
        assert_eq!(options.warmup, DEFAULT_WARMUP);
        assert_eq!(options.backends, vec![Backend::Cpu, Backend::Gpu]);
        assert!(options.fleet.cards > 1);
    }

    #[test]
    fn options_are_parsed() {
        let args: Vec<String> = ["--cards", "40", "--frames", "3", "--backend", "gpu"]
            .iter()
            .map(|arg| arg.to_string())
            .collect();
        let options = parse(&args).expect("a valid argument list");
        assert_eq!(options.fleet.cards, 40);
        assert_eq!(options.frames, 3);
        assert_eq!(options.backends, vec![Backend::Gpu]);
    }

    #[test]
    fn a_bad_argument_is_refused_rather_than_defaulted() {
        assert!(
            parse(&["--cards".to_string()]).is_err(),
            "a flag with no value"
        );
        assert!(
            parse(&["--cards".to_string(), "lots".to_string()]).is_err(),
            "a flag with a value that is not a number"
        );
        assert!(
            parse(&["--nonsense".to_string()]).is_err(),
            "an unknown flag"
        );
        assert!(
            parse(&["--frames".to_string(), "0".to_string()]).is_err(),
            "a run with no timed frames"
        );
    }

    /// Nearest-rank, and the p100 is the slowest sample rather than one past the
    /// end of the list.
    #[test]
    fn percentiles_land_on_real_samples() {
        let run = Run {
            backend: Backend::Cpu,
            frames: 100,
            sorted: (1..=100).map(Duration::from_millis).collect(),
            wall: Duration::from_secs(1),
            cards: 12,
            pixels: 0,
            bytes: 0,
            tiles_composed: 0,
        };
        assert_eq!(run.percentile(0.50), Duration::from_millis(50));
        assert_eq!(run.percentile(0.95), Duration::from_millis(95));
        assert_eq!(run.percentile(0.99), Duration::from_millis(99));
        assert_eq!(run.percentile(1.0), Duration::from_millis(100));
    }

    /// A GPU run that composed nothing is not a GPU run, and the report says so
    /// rather than printing a speedup off two CPU columns.
    #[test]
    fn a_gpu_run_that_composed_nothing_is_not_honest() {
        let mut run = Run {
            backend: Backend::Gpu,
            frames: 1,
            sorted: vec![Duration::from_millis(1)],
            wall: Duration::from_millis(1),
            cards: 1,
            pixels: 0,
            bytes: 0,
            tiles_composed: 0,
        };
        assert!(!run.honest());
        run.tiles_composed = 12;
        assert!(run.honest());
    }
}
