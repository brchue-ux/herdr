//! `herdr bench combined` — what a whole animated frame costs under churn.
//!
//! # Why a second benchmark rather than a bigger first one
//!
//! `herdr bench cards` holds everything still and varies one thing: the bloom
//! backend. That is what makes its two columns comparable, and it is also what
//! makes it a *micro*-benchmark — a settled fleet, redrawn whole, with nothing
//! else on the machine.
//!
//! Herdr does not run that frame. The frame it runs has panes arriving and
//! leaving continuously, the sidebar's rows sliding as the tree opens and
//! closes under them, trunk segments extending and retracting beside them, and a
//! whole-terminal ambient scene animating behind all of it — every one of those
//! contending for the same cores at the same instant. Four subsystems that are
//! each affordable alone are not obviously affordable together, and nothing
//! measures them together.
//!
//! # The four loads, and that they are the real ones
//!
//! - **Churn.** Panes spawn and are torn down all through the run, through the
//!   real [`crate::anim::Animator`] with the real
//!   [`crate::app::state::sidebar_row_lifecycle_from`] life, so arrivals mount
//!   and departures play their dismount out before the row is dropped. A card
//!   is rasterised through [`rasterise_card_scene`] *against the previous
//!   frame's layers*, which is the entry point and the carry-forward a real
//!   client has. `CARDS_RASTERISED` says how many the matcher declined to carry,
//!   so a run cannot claim churn it did not create.
//! - **Row motion.** [`crate::ui::sidebar::motion::row_offsets`] and
//!   `cell_offsets`, off the engine's own progress, reaching the rasteriser as
//!   `CardScene::offsets` — the same field a client fills from
//!   `WorkspaceCardArea::motion_cells`.
//! - **Trunk and branch.** One [`crate::anim::ElementId::trunk_segment`] per row
//!   standing beside a still-open ancestor column, on the trunk lifecycle, so
//!   segments mount and retract on their own clock as the tree's shape changes
//!   under them.
//! - **The ambient wash.** [`crate::solar_system`], at the terminal's full size:
//!   `effects_frame_png` every frame, and the whole `loop_frames_png` loop again
//!   whenever churn changes the fleet's shape — which is what
//!   `App::observe_background_scene` really does, because
//!   `background_scene_key` hashes the node list. That regeneration is the
//!   single most expensive thing churn causes and it is why this benchmark
//!   exists; it is timed and counted as its own line rather than folded into the
//!   frame percentiles it does not belong to.
//!
//! # Why the clock is simulated
//!
//! Animation advances by `--frame-ms` per frame rather than by however long the
//! frame took. A machine that draws at half the speed would otherwise see half
//! as many arrivals, tear down half as many panes and regenerate the ambient
//! loop half as often — it would be measured on a *lighter* workload for being
//! slower, which is exactly backwards. A simulated clock puts the identical
//! sequence of events through every machine, which is what makes two runs on two
//! boxes the same question.
//!
//! The consequence is stated rather than hidden: a run's wall clock is not
//! `frames × frame-ms`, and the report says how far the two came apart. That
//! ratio *is* the result — under 1.0 the machine kept up with the frame budget,
//! over it the machine did not.
//!
//! # The gate this deliberately does not go through
//!
//! `HostTerminalKind::draws_ambient_wash` decides whether a *terminal* is handed
//! a wash. It is answered by terminal identification, over a handshake there is
//! no client here to perform. What it gates is delivery, not cost: the frames
//! are built by the functions called below whatever it answers, and those are
//! what a benchmark is for. The card path is pinned to `Kitty` for the same
//! reason `bench cards` pins it — it picks the encoder.

use std::time::{Duration, Instant};

use crate::ui::sidebar::image_card::bench as workload;

/// Timed frames. Enough that the p99 is a real sample, and enough simulated time
/// at the default cadence — about sixteen seconds — for the churn to reach a
/// steady state rather than measuring a fleet still filling up.
const DEFAULT_FRAMES: usize = 1000;
/// Frames drawn before the clock starts. Longer than `bench cards`' warm-up
/// because more first-frame costs exist here: the font and glyph caches, the GPU
/// adapter and its calibration, *and* the first ambient loop.
const DEFAULT_WARMUP: usize = 10;
/// Simulated milliseconds per frame. 16 ms is the 60 Hz budget a frame is
/// trying to fit inside, which makes the wall-clock ratio in the report read
/// directly as "did this machine keep up".
const DEFAULT_FRAME_MS: u64 = 16;
/// Panes torn down and replaced per simulated second.
///
/// At the default 448 ms arrival and 308 ms departure, six a second keeps
/// roughly three rows mid-arrival and two mid-departure at every instant, so
/// the panel is never still and the ambient loop is regenerating throughout.
const DEFAULT_CHURN_PER_SEC: f64 = 6.0;
/// The terminal the wash is drawn across, in cells. A full-screen window on the
/// captain's own display.
const DEFAULT_SCREEN_COLS: u16 = 210;
const DEFAULT_SCREEN_ROWS: u16 = 56;

pub(super) const USAGE: &str =
    "usage: herdr bench combined [--cards N] [--frames N] [--warmup N]\n\
     \x20                           [--churn-per-sec R] [--frame-ms MS]\n\
     \x20                           [--backend cpu|gpu|both] [--panel-cols N]\n\
     \x20                           [--cell-width PX] [--cell-height PX]\n\
     \x20                           [--screen-cols N] [--screen-rows N]\n\
     \x20                           [--wash on|off] [--ambient-loop on|off]";

pub(super) struct Options {
    pub(super) fleet: workload::Fleet,
    pub(super) frames: usize,
    pub(super) warmup: usize,
    pub(super) frame_ms: u64,
    pub(super) churn_per_sec: f64,
    pub(super) screen_cols: u16,
    pub(super) screen_rows: u16,
    pub(super) wash: bool,
    pub(super) ambient_loop: bool,
    pub(super) backends: Vec<super::Backend>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fleet: workload::Fleet::default_fleet(),
            frames: DEFAULT_FRAMES,
            warmup: DEFAULT_WARMUP,
            frame_ms: DEFAULT_FRAME_MS,
            churn_per_sec: DEFAULT_CHURN_PER_SEC,
            screen_cols: DEFAULT_SCREEN_COLS,
            screen_rows: DEFAULT_SCREEN_ROWS,
            wash: true,
            ambient_loop: true,
            backends: vec![super::Backend::Cpu, super::Backend::Gpu],
        }
    }
}

pub(super) fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();

    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--cards" => {
                options.fleet.cards = super::number(super::value(args, &mut index)?, flag)?
            }
            "--frames" => options.frames = super::number(super::value(args, &mut index)?, flag)?,
            "--warmup" => options.warmup = super::number(super::value(args, &mut index)?, flag)?,
            "--frame-ms" => {
                options.frame_ms = super::number(super::value(args, &mut index)?, flag)?
            }
            "--churn-per-sec" => {
                options.churn_per_sec = rate(super::value(args, &mut index)?, flag)?
            }
            "--panel-cols" => {
                options.fleet.panel_cols = super::number(super::value(args, &mut index)?, flag)?
            }
            "--cell-width" => {
                options.fleet.cell.width_px = super::number(super::value(args, &mut index)?, flag)?
            }
            "--cell-height" => {
                options.fleet.cell.height_px = super::number(super::value(args, &mut index)?, flag)?
            }
            "--screen-cols" => {
                options.screen_cols = super::number(super::value(args, &mut index)?, flag)?
            }
            "--screen-rows" => {
                options.screen_rows = super::number(super::value(args, &mut index)?, flag)?
            }
            "--wash" => options.wash = switch(super::value(args, &mut index)?, flag)?,
            "--ambient-loop" => {
                options.ambient_loop = switch(super::value(args, &mut index)?, flag)?
            }
            "--backend" => options.backends = super::backends(super::value(args, &mut index)?)?,
            other => return Err(format!("unknown option {other}")),
        }
        index += 1;
    }

    if options.fleet.cards == 0 {
        return Err("--cards must be at least 1".into());
    }
    if options.frames == 0 {
        return Err("--frames must be at least 1".into());
    }
    if options.frame_ms == 0 {
        return Err("--frame-ms must be at least 1".into());
    }
    if !options.churn_per_sec.is_finite() || options.churn_per_sec < 0.0 {
        return Err("--churn-per-sec must be zero or more".into());
    }
    Ok(options)
}

/// A churn rate. Its own parser rather than [`super::number`]'s, which says
/// "whole number" — and a churn rate is the one flag here that is usefully
/// fractional, because a rate slower than one pane a second is still a rate.
fn rate(raw: &str, flag: &str) -> Result<f64, String> {
    raw.parse()
        .map_err(|_| format!("{flag} needs a number, not '{raw}'"))
}

fn switch(raw: &str, flag: &str) -> Result<bool, String> {
    match raw {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        other => Err(format!("{flag} must be on or off, not '{other}'")),
    }
}

// ---------------------------------------------------------------------------
// The fleet under churn
// ---------------------------------------------------------------------------

/// One row of the panel, alive or on its way out.
#[derive(Debug, Clone)]
struct Row {
    /// Which card it draws — see [`workload::LiveRow::seed`].
    seed: usize,
    pane: crate::layout::PaneId,
    /// True once its pane has been torn down. It keeps its slot, and is still
    /// drawn, until the engine has played its departure out and retired it —
    /// which is exactly what a real panel does, and is why a departure costs
    /// anything at all.
    leaving: bool,
}

impl Row {
    fn element(&self) -> crate::anim::ElementId {
        crate::anim::ElementId::agent_row(self.pane)
    }

    fn depth(&self) -> u8 {
        workload::depth_of(self.seed)
    }
}

/// The panel's rows, and the churn that keeps them turning over.
struct Fleet {
    rows: Vec<Row>,
    /// Live rows this fleet is trying to hold. Churn replaces rows rather than
    /// growing or shrinking the fleet, so a run measures a *panel* under churn
    /// and not a panel filling up.
    capacity: usize,
    next_seed: usize,
    /// Fractional churn events carried between frames, so a rate that is not a
    /// whole number of events per frame still comes out right over a run.
    pending: f64,
    spawned: u64,
    torn_down: u64,
}

impl Fleet {
    fn new(capacity: usize) -> Self {
        Self {
            rows: Vec::with_capacity(capacity + 8),
            capacity,
            next_seed: 0,
            pending: 0.0,
            spawned: 0,
            torn_down: 0,
        }
    }

    fn spawn(&mut self) {
        let seed = self.next_seed;
        self.next_seed += 1;
        self.spawned += 1;
        self.rows.push(Row {
            seed,
            // A fresh id every time, exactly as `PaneId::alloc` gives a real
            // pane: it is what makes an arrival a new element to the engine
            // rather than the previous tenant of the slot coming back.
            pane: crate::layout::PaneId::from_raw(
                u32::try_from(seed).unwrap_or(u32::MAX).wrapping_add(1),
            ),
            leaving: false,
        });
    }

    /// Retire the longest-standing live row, which is the one a fleet turning
    /// over would finish with first.
    fn tear_down(&mut self) {
        if let Some(row) = self.rows.iter_mut().find(|row| !row.leaving) {
            row.leaving = true;
            self.torn_down += 1;
        }
    }

    /// Fire whatever churn `elapsed` worth of simulated time has earned.
    ///
    /// Below capacity a fleet only fills; at capacity every event is a tear-down
    /// *and* a spawn, so one row is leaving while another arrives and the panel
    /// is never momentarily still.
    fn churn(&mut self, per_sec: f64, frame: Duration) {
        let live = self.rows.iter().filter(|row| !row.leaving).count();
        if live < self.capacity {
            self.spawn();
            return;
        }
        self.pending += per_sec * frame.as_secs_f64();
        while self.pending >= 1.0 {
            self.pending -= 1.0;
            self.tear_down();
            self.spawn();
        }
    }

    /// Elements to publish this pass: the live rows only. A row that has been
    /// torn down is *absent* from the membership set, which is what puts it into
    /// its dismount.
    fn members(&self) -> Vec<(crate::anim::ElementId, crate::anim::behaviour::DriveInputs)> {
        self.rows
            .iter()
            .filter(|row| !row.leaving)
            .map(|row| {
                (
                    row.element(),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect()
    }

    /// One segment per row standing beside a still-open ancestor column — the
    /// shape `crate::ui::sidebar_trunk_segment_members` publishes, over the
    /// synthetic tree's own depths.
    fn trunk_members(&self) -> Vec<(crate::anim::ElementId, crate::anim::behaviour::DriveInputs)> {
        self.rows
            .iter()
            .filter(|row| !row.leaving && row.depth() >= 2)
            .map(|row| {
                (
                    crate::anim::ElementId::trunk_segment(crate::anim::CardRow::Agent(row.pane), 1),
                    crate::anim::behaviour::DriveInputs::default(),
                )
            })
            .collect()
    }

    /// Drop rows the engine has finished with. Until then a departing row is
    /// still drawn, still placed, and still displacing the rows under it.
    fn retire_finished(&mut self, anim: &crate::anim::Animator) {
        self.rows
            .retain(|row| !row.leaving || anim.frame(&row.element(), None).is_some());
    }

    /// The fleet as the ambient scene sees it: a sun per root, a planet per
    /// mate, a moon per worker.
    fn nodes(&self) -> Vec<crate::solar_system::TreeNode> {
        let mut nodes = Vec::with_capacity(self.rows.len());
        let mut last_root: Option<usize> = None;
        let mut last_planet: Option<usize> = None;
        for row in &self.rows {
            let (kind, parent) = match row.depth() {
                0 => (crate::solar_system::BodyKind::Sun, None),
                1 => (crate::solar_system::BodyKind::Planet, last_root),
                _ => (crate::solar_system::BodyKind::Moon, last_planet),
            };
            match kind {
                crate::solar_system::BodyKind::Sun => last_root = Some(nodes.len()),
                crate::solar_system::BodyKind::Planet => last_planet = Some(nodes.len()),
                crate::solar_system::BodyKind::Moon => {}
            }
            nodes.push(crate::solar_system::TreeNode {
                parent,
                kind,
                // Spread over the wheel by seed rather than all one colour: hue
                // is what the body's whole palette is resolved from.
                hue: (row.seed % 360) as f32,
                severity: match row.seed % 4 {
                    0 => crate::anim::cell::Severity::Clear,
                    1 => crate::anim::cell::Severity::Mild,
                    2 => crate::anim::cell::Severity::Serious,
                    _ => crate::anim::cell::Severity::Critical,
                },
                // Out of the project-size register on purpose: this rig measures how render cost
                // scales with body *count*, so every run should draw the same body sizes as every
                // other one rather than a spread that moves with whatever fixture built the rows.
                size: crate::solar_system::BodySize::Fixed,
                // ...and out of the streak register for the same reason: a ring that widens and a
                // gas giant that swells are both size changes, and this rig holds size fixed so
                // the number it reports is body *count* and nothing else.
                streak: 0.0,
                // No accumulated wear either: this rig measures render cost against body count,
                // and a track is a fixed annulus per body rather than something that scales with
                // the fleet in a different way.
                wear: 0.0,
                // No ambient motes either: this rig measures render cost against body count.
                motes: 0,
                mote_share: 0.0,
            });
        }
        nodes
    }
}

// ---------------------------------------------------------------------------
// Running it
// ---------------------------------------------------------------------------

/// One stage's timings across a run. Named rather than positional because the
/// report prints them as rows and a reader has to be able to tell which is which.
struct Stage {
    name: &'static str,
    samples: Vec<Duration>,
}

impl Stage {
    fn new(name: &'static str, frames: usize) -> Self {
        Self {
            name,
            samples: Vec::with_capacity(frames),
        }
    }

    fn sorted(&self) -> Vec<Duration> {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        sorted
    }

    fn total(&self) -> Duration {
        self.samples.iter().sum()
    }
}

/// Nearest-rank, matching `bench cards` exactly so the two reports' percentile
/// columns mean the same thing.
fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = ((sorted.len() as f64) * fraction).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// One backend's measured run under the combined load.
struct Run {
    backend: super::Backend,
    frames: usize,
    wall: Duration,
    /// Simulated time the run represents — `frames × frame-ms`.
    simulated: Duration,
    stages: Vec<Stage>,
    total: Stage,
    spawned: u64,
    torn_down: u64,
    cards_rasterised: u64,
    tiles_composed: u64,
    ambient_regens: u64,
    ambient_time: Duration,
    /// The last frame's accounting, for the per-frame line.
    cards: usize,
    bytes: usize,
    pixels: u64,
    wash_bytes: usize,
}

impl Run {
    fn fps(&self) -> f64 {
        if self.wall.is_zero() {
            return 0.0;
        }
        self.frames as f64 / self.wall.as_secs_f64()
    }

    /// How much of the frame budget the machine actually spent. Under 1.0 it
    /// kept up; over it, it did not.
    fn budget_ratio(&self) -> f64 {
        if self.simulated.is_zero() {
            return 0.0;
        }
        self.wall.as_secs_f64() / self.simulated.as_secs_f64()
    }

    /// Whether this run drew the bloom the way its label says.
    fn honest(&self) -> bool {
        match self.backend {
            super::Backend::Cpu => self.tiles_composed == 0,
            super::Backend::Gpu => self.tiles_composed > 0,
        }
    }
}

/// Everything [`run`] works out once from the options and every frame then
/// needs. A struct rather than eight more parameters threaded through two call
/// layers, and the place the panel's own size is written down: it is not the
/// fleet's size, and the difference is what a departing row stands in.
struct Rig {
    /// The panel the tree is laid out on — the live fleet plus room for the
    /// rows on their way out.
    panel: workload::Fleet,
    row_span_px: f32,
    panel_width_px: f32,
    wash_size: (u32, u32),
    row_lifecycle: crate::anim::Lifecycle,
    trunk_lifecycle: crate::anim::Lifecycle,
}

pub(super) fn run(options: Options) -> i32 {
    // Motion is what a churn benchmark is *for*, so it is configured on rather
    // than left at the shipped default of `none`. `moves` is passed to the
    // lifecycle directly for the same reason: the host gate it normally folds in
    // (`AppState::sidebar_rows_move`) asks about a terminal there is no client
    // here to have.
    let mut animation = crate::config::SidebarAnimationConfig {
        row_motion: crate::config::SidebarRowMotion::Slide,
        ..Default::default()
    };
    animation.row_enter = crate::config::SidebarTokenEmphasis::Dissolve;
    animation.row_exit = crate::config::SidebarTokenEmphasis::Dissolve;

    let row_lifecycle = crate::app::state::sidebar_row_lifecycle_from(
        &animation,
        &crate::config::AgentsSidebarConfig::default(),
        &crate::config::SpacesSidebarConfig::default(),
        &crate::config::SidebarCardsConfig::default(),
        true,
        true,
    );
    let trunk_lifecycle = crate::app::state::sidebar_trunk_lifecycle_from(&animation);

    // A torn-down row keeps its slot until its departure has played out, so the
    // panel has to be taller than the fleet is wide — otherwise the first frame
    // with a leaver on it lays a card out past the bottom of the panel and the
    // whole run dies with "a card failed to rasterise". How many can be in
    // flight at once is the churn rate times how long a departure lasts, plus
    // slack for the rounding at both ends.
    let departing = (options.churn_per_sec * animation.row_exit_ms as f64 / 1000.0).ceil();
    let headroom = (departing as usize).saturating_add(2);
    let panel = workload::Fleet {
        cards: options.fleet.cards.saturating_add(headroom),
        ..options.fleet
    };

    let (row_span_px, panel_width_px) = match workload::panel_metrics(panel) {
        Ok(metrics) => metrics,
        Err(why) => {
            eprintln!("error: {why}");
            return 1;
        }
    };

    let cell = options.fleet.cell;
    let wash_size = (
        u32::from(options.screen_cols) * cell.width_px,
        u32::from(options.screen_rows) * cell.height_px,
    );
    let rig = Rig {
        panel,
        row_span_px,
        panel_width_px,
        wash_size,
        row_lifecycle,
        trunk_lifecycle,
    };

    println!("herdr combined-load stress benchmark");
    println!(
        "  build     {} ({}, {})",
        crate::build_info::version(),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "  fleet     {} cards held live, {}x{} px cells, {}-column panel",
        options.fleet.cards, cell.width_px, cell.height_px, options.fleet.panel_cols
    );
    println!(
        "  churn     {:.1} panes/s replaced, arrivals {} ms, departures {} ms, sliding",
        options.churn_per_sec, animation.row_enter_ms, animation.row_exit_ms
    );
    println!(
        "  wash      {}",
        if options.wash {
            format!(
                "{}x{} cells = {}x{} px, ambient loop {}",
                options.screen_cols,
                options.screen_rows,
                wash_size.0,
                wash_size.1,
                if options.ambient_loop {
                    format!(
                        "rebuilt per churn ({} frames)",
                        crate::solar_system::FRAME_COUNT
                    )
                } else {
                    "off".to_string()
                }
            )
        } else {
            "off".to_string()
        }
    );
    println!(
        "  cores     {} available, up to {} used per frame by the card threads",
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1),
        crate::ui::sidebar::image_card::raster_threads_for_bench(options.fleet.cards)
    );
    println!(
        "  adapter   {}",
        crate::gpu::bloom::adapter_description()
            .unwrap_or_else(|| "none — every run below is on the CPU".into())
    );
    println!(
        "  frames    {} timed per backend after {} warm-up, {} ms of simulated time each",
        options.frames, options.warmup, options.frame_ms
    );
    println!();

    let mut runs = Vec::new();
    for backend in &options.backends {
        match measure(*backend, &options, &rig) {
            Ok(run) => runs.push(run),
            Err(why) => {
                eprintln!("error: {why}");
                return 1;
            }
        }
    }

    report(&runs, &options);
    0
}

/// Everything one frame of the combined load costs, kept between frames.
struct Live {
    fleet: Fleet,
    anim: crate::anim::Animator,
    held: Vec<crate::ui::sidebar::image_card::SidebarCardLayer>,
    layout: Option<crate::solar_system::SceneLayout>,
    scene_key: u64,
    /// When the ambient loop standing right now was generated. The wash's phase
    /// is measured from it, exactly as `App::observe_background_effects` measures
    /// it from `background_scene_generated_at`, so a rebuild restarts the loop
    /// rather than dropping it in mid-orbit.
    generated_at: Instant,
    legibility: Option<crate::app::background_legibility::LegibilityGrid>,
    ambient_regens: u64,
    ambient_time: Duration,
    wash_bytes: usize,
}

fn measure(backend: super::Backend, options: &Options, rig: &Rig) -> Result<Run, String> {
    crate::gpu::pin_backend(backend == super::Backend::Gpu);

    let start = Instant::now();
    let frame_step = Duration::from_millis(options.frame_ms);
    let mut live = Live {
        fleet: Fleet::new(options.fleet.cards),
        anim: crate::anim::Animator::default(),
        held: Vec::new(),
        layout: None,
        scene_key: 0,
        generated_at: start,
        legibility: None,
        ambient_regens: 0,
        ambient_time: Duration::ZERO,
        wash_bytes: 0,
    };

    let mut step = 0u64;
    let mut last = None;
    for _ in 0..options.warmup {
        let (drawn, _) = frame(
            &mut live,
            options,
            rig,
            start + frame_step * step as u32,
            frame_step,
            None,
        )
        .map_err(|why| format!("a warm-up frame failed: {why}"))?;
        last = Some(drawn);
        step += 1;
    }

    // Counted from the end of the warm-up, so the first ambient loop and the
    // fleet filling up are not charged to the timed window.
    let spawned_before = live.fleet.spawned;
    let torn_before = live.fleet.torn_down;
    let regens_before = live.ambient_regens;
    let ambient_before = live.ambient_time;
    let composed_before =
        crate::gpu::bloom::TILES_COMPOSED.load(std::sync::atomic::Ordering::Relaxed);
    let rasterised_before =
        crate::ui::sidebar::image_card::CARDS_RASTERISED.load(std::sync::atomic::Ordering::Relaxed);

    let mut stages = vec![
        Stage::new("tree", options.frames),
        Stage::new("cards", options.frames),
        Stage::new("wash", options.frames),
        Stage::new("legibility", options.frames),
    ];
    let mut total = Stage::new("frame", options.frames);

    let wall_at = Instant::now();
    for _ in 0..options.frames {
        let at = Instant::now();
        let (drawn, regen) = frame(
            &mut live,
            options,
            rig,
            start + frame_step * step as u32,
            frame_step,
            Some(&mut stages),
        )
        .map_err(|why| format!("a timed frame failed: {why}"))?;
        // Less the ambient rebuild, so this column is the frame every frame
        // draws. The wall clock below still has it, because the machine did.
        total.samples.push(at.elapsed().saturating_sub(regen));
        last = Some(drawn);
        step += 1;
    }
    let wall = wall_at.elapsed();

    let drawn = match last {
        Some(drawn) => drawn,
        None => return Err("a run with no frames drew nothing to account for".into()),
    };

    Ok(Run {
        backend,
        frames: options.frames,
        wall,
        simulated: frame_step * u32::try_from(options.frames).unwrap_or(u32::MAX),
        stages,
        total,
        spawned: live.fleet.spawned - spawned_before,
        torn_down: live.fleet.torn_down - torn_before,
        cards_rasterised: crate::ui::sidebar::image_card::CARDS_RASTERISED
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(rasterised_before),
        tiles_composed: crate::gpu::bloom::TILES_COMPOSED
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(composed_before),
        ambient_regens: live.ambient_regens - regens_before,
        ambient_time: live.ambient_time.saturating_sub(ambient_before),
        cards: drawn.cards,
        bytes: drawn.bytes,
        pixels: drawn.pixels,
        wash_bytes: live.wash_bytes,
    })
}

/// One whole frame of the combined load.
///
/// `stages`, when present, is where each stage's own time is recorded; a warm-up
/// frame passes `None` and does the identical work untimed.
fn frame(
    live: &mut Live,
    options: &Options,
    rig: &Rig,
    now: Instant,
    frame_step: Duration,
    mut stages: Option<&mut Vec<Stage>>,
) -> Result<(workload::Frame, Duration), String> {
    let mut record = |index: usize, taken: Duration| {
        if let Some(stages) = stages.as_deref_mut() {
            stages[index].samples.push(taken);
        }
    };

    // ---- the tree: churn, the engine, and where the motion puts each row ----
    let at = Instant::now();
    live.fleet.churn(options.churn_per_sec, frame_step);
    live.anim.observe(
        now,
        crate::anim::Family::AgentRow,
        &rig.row_lifecycle,
        live.fleet.members(),
    );
    live.anim.observe(
        now,
        crate::anim::Family::TrunkSegment,
        &rig.trunk_lifecycle,
        live.fleet.trunk_members(),
    );
    live.fleet.retire_finished(&live.anim);

    let lives: Vec<crate::ui::sidebar::motion::RowLife> = live
        .fleet
        .rows
        .iter()
        .map(|row| crate::ui::sidebar::motion::RowLife {
            height_px: rig.row_span_px,
            settle: crate::ui::sidebar::motion::settle_in(&live.anim, &row.element()),
        })
        .collect();
    let offsets = crate::ui::sidebar::motion::cell_offsets(
        &crate::ui::sidebar::motion::row_offsets(&lives, rig.panel_width_px),
        f32::from(u16::try_from(options.fleet.cell.width_px).unwrap_or(u16::MAX)),
        f32::from(u16::try_from(options.fleet.cell.height_px).unwrap_or(u16::MAX)),
    );
    let rows: Vec<workload::LiveRow> = live
        .fleet
        .rows
        .iter()
        .zip(offsets)
        .map(|(row, motion)| workload::LiveRow {
            seed: row.seed,
            motion,
        })
        .collect();
    record(0, at.elapsed());

    // ---- the cards ----
    let at = Instant::now();
    let scene = workload::scene_of(rig.panel, &rows).map_err(|why| why.to_string())?;
    let (drawn, layers) = workload::draw_over(&scene, options.fleet.cell, &live.held)
        .map_err(|()| "a card failed to rasterise".to_string())?;
    if let Some(layers) = layers {
        live.held = layers;
    }
    record(1, at.elapsed());

    // ---- the ambient wash ----
    let at = Instant::now();
    // The rebuild's own time, taken out of this frame's wash sample below. It is
    // occasional and enormous, and leaving it in would put a 200 ms spike into a
    // percentile that describes neither the frames that pay it nor the ones that
    // do not — while the report claims it is charged separately.
    let mut regen_taken = Duration::ZERO;
    let mut drawn_wash = None;
    if options.wash {
        let nodes = live.fleet.nodes();
        let key = scene_key(&nodes, rig.wash_size);
        if key != live.scene_key || live.layout.is_none() {
            live.scene_key = key;
            live.generated_at = now;
            let regen_at = Instant::now();
            let layout =
                crate::solar_system::build_layout(&nodes, rig.wash_size.0, rig.wash_size.1);
            if options.ambient_loop && !layout.is_empty() {
                let frames =
                    crate::solar_system::loop_frames_png(&layout, crate::solar_system::FRAME_COUNT);
                live.wash_bytes = frames.iter().map(Vec::len).sum();
            }
            regen_taken = regen_at.elapsed();
            live.ambient_time += regen_taken;
            live.ambient_regens += 1;
            live.layout = (!layout.is_empty()).then_some(layout);
        }
        if let Some(layout) = &live.layout {
            let phase = crate::app::background_scene::phase_at(live.generated_at, now);
            let effects = effects_over(layout, phase);
            let png = crate::solar_system::effects_frame_png(layout, &effects, phase);
            live.wash_bytes = live.wash_bytes.max(png.len());
            drawn_wash = Some((phase, effects));
        }
    }
    record(2, at.elapsed().saturating_sub(regen_taken));

    // ---- per-cell text legibility over whatever the wash just drew ----
    //
    // Handed this frame's real phase and real effects, not a resting scene: the
    // sampler's own coarser cadence is what decides how often it does the heavy
    // work, and giving it constants would let it decline every pass and report a
    // cost of zero for a stage that has one.
    let at = Instant::now();
    if let (Some(layout), Some((phase, effects))) = (&live.layout, &drawn_wash) {
        crate::app::background_legibility::observe(
            &mut live.legibility,
            layout,
            *phase,
            effects,
            // No machine corner in the rig: this measures how render cost scales with body count
            // under churn, and the corner is a fixed box that neither churns nor scales with it.
            None,
            options.fleet.cell.width_px,
            options.fleet.cell.height_px,
            now,
        );
    }
    record(3, at.elapsed());

    Ok((drawn, regen_taken))
}

/// `App::observe_background_scene`'s own key, over the two things that can move
/// here: the fleet's shape and the canvas it is drawn on.
fn scene_key(nodes: &[crate::solar_system::TreeNode], size: (u32, u32)) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    size.hash(&mut hasher);
    nodes.len().hash(&mut hasher);
    for node in nodes {
        node.parent.hash(&mut hasher);
        (node.kind as u8).hash(&mut hasher);
        node.hue.to_bits().hash(&mut hasher);
        node.severity.hash(&mut hasher);
    }
    hasher.finish()
}

/// The transient overlay a fleet under churn keeps alive: something is always
/// arriving somewhere, so there is always an asteroid in flight and a crater
/// still fading.
fn effects_over(
    layout: &crate::solar_system::SceneLayout,
    phase: f32,
) -> crate::solar_system::SceneEffects {
    let bodies = layout.body_count();
    if bodies == 0 {
        return crate::solar_system::SceneEffects::default();
    }
    // Walked off the wash's own phase rather than a counter, so the overlay is
    // the same picture at the same simulated instant on every machine.
    let progress = (phase / std::f32::consts::TAU).fract().clamp(0.0, 1.0);
    let tick = (phase * 1000.0) as usize;
    crate::solar_system::SceneEffects {
        asteroids: vec![crate::solar_system::AsteroidInFlight {
            target: tick % bodies,
            severity: crate::anim::cell::Severity::Serious,
            progress,
            approach_angle: phase,
        }],
        craters: vec![crate::solar_system::Crater {
            body: (tick / 3) % bodies,
            angle_on_surface: phase,
            severity: crate::anim::cell::Severity::Mild,
            age: progress,
            is_ripple: false,
        }],
        ejecta: Vec::new(),
        comets: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

fn report(runs: &[Run], options: &Options) {
    for run in runs {
        println!("{} backend", run.backend.label());
        println!(
            "  {:<12} {:>7} {:>10} {:>9} {:>9} {:>9} {:>9}",
            "stage", "frames", "share", "p50 (ms)", "p95 (ms)", "p99 (ms)", "max (ms)"
        );
        let whole = run.total.total().as_secs_f64().max(f64::MIN_POSITIVE);
        for stage in run.stages.iter().chain(std::iter::once(&run.total)) {
            let sorted = stage.sorted();
            println!(
                "  {:<12} {:>7} {:>9.1}% {:>9.3} {:>9.3} {:>9.3} {:>9.3}",
                stage.name,
                sorted.len(),
                100.0 * stage.total().as_secs_f64() / whole,
                ms(percentile(&sorted, 0.50)),
                ms(percentile(&sorted, 0.95)),
                ms(percentile(&sorted, 0.99)),
                ms(*sorted.last().unwrap_or(&Duration::ZERO)),
            );
        }
        println!(
            "  {:.1} fps over {:.3} s of wall clock for {:.3} s of simulated time ({:.2}x the \
             frame budget)",
            run.fps(),
            run.wall.as_secs_f64(),
            run.simulated.as_secs_f64(),
            run.budget_ratio(),
        );
        println!(
            "  churn: {} panes spawned, {} torn down, {} cards rasterised ({:.2} per frame; the \
             rest were carried)",
            run.spawned,
            run.torn_down,
            run.cards_rasterised,
            run.cards_rasterised as f64 / run.frames.max(1) as f64,
        );
        if options.wash && options.ambient_loop {
            println!(
                "  wash:  {} ambient loop rebuilds costing {:.3} s in total ({:.1} ms each), \
                 charged to the wall clock but not to the frame times above",
                run.ambient_regens,
                run.ambient_time.as_secs_f64(),
                ms(run.ambient_time) / run.ambient_regens.max(1) as f64,
            );
        } else if options.wash {
            println!(
                "  wash:  the fleet's shape changed {} times; the ambient loop was not rebuilt \
                 for any of them (--ambient-loop off)",
                run.ambient_regens,
            );
        }
        println!(
            "  per frame: {} cards, {:.2} Mpx of card image, {:.1} KiB of card PNG, \
             {:.1} KiB of wash PNG",
            run.cards,
            run.pixels as f64 / 1_000_000.0,
            run.bytes as f64 / 1024.0,
            run.wash_bytes as f64 / 1024.0,
        );
        println!(
            "  {} tiles composed on the compute pass",
            run.tiles_composed
        );
        println!();
    }

    let cpu = runs.iter().find(|run| run.backend == super::Backend::Cpu);
    let gpu = runs.iter().find(|run| run.backend == super::Backend::Gpu);
    if let (Some(cpu), Some(gpu)) = (cpu, gpu) {
        let p50 = ms(percentile(&gpu.total.sorted(), 0.50));
        if p50 > 0.0 {
            println!(
                "speedup:   {:.2}x at p50 on the whole frame ({:.3} ms -> {:.3} ms)",
                ms(percentile(&cpu.total.sorted(), 0.50)) / p50,
                ms(percentile(&cpu.total.sorted(), 0.50)),
                p50
            );
        }
    }

    // Two claims the timings cannot evidence for themselves, stated on every
    // run rather than only when they fail: that the GPU ran, and that the churn
    // this says it measured actually redrew anything.
    for run in runs {
        if !run.honest() {
            match run.backend {
                super::Backend::Gpu => println!(
                    "\nWARNING: the gpu run composed no tiles on the compute pass, so its numbers\n\
                     are the CPU path's. This machine has no usable adapter, this build has no\n\
                     gpu-raster feature, or the pass declined — check the adapter line above."
                ),
                super::Backend::Cpu => println!(
                    "\nWARNING: the cpu run composed {} tiles on the compute pass, so its numbers\n\
                     are not a clean CPU baseline.",
                    run.tiles_composed
                ),
            }
        }
        if run.torn_down == 0 && options.churn_per_sec > 0.0 {
            println!(
                "\nWARNING: no pane was torn down in the timed window, so this measured a settled\n\
                 fleet and not churn. Raise --churn-per-sec or --frames."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_a_churning_fleet_with_a_wash_behind_it() {
        let options = parse(&[]).expect("no arguments is a valid run");
        assert_eq!(options.frames, DEFAULT_FRAMES);
        assert!(options.churn_per_sec > 0.0);
        assert!(options.wash);
        assert!(options.ambient_loop);
        assert_eq!(
            options.backends,
            vec![super::super::Backend::Cpu, super::super::Backend::Gpu]
        );
    }

    #[test]
    fn options_are_parsed() {
        let args: Vec<String> = [
            "--cards",
            "40",
            "--frames",
            "3",
            "--churn-per-sec",
            "12.5",
            "--wash",
            "off",
            "--backend",
            "gpu",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        let options = parse(&args).expect("a valid argument list");
        assert_eq!(options.fleet.cards, 40);
        assert_eq!(options.frames, 3);
        assert!((options.churn_per_sec - 12.5).abs() < f64::EPSILON);
        assert!(!options.wash);
        assert_eq!(options.backends, vec![super::super::Backend::Gpu]);
    }

    #[test]
    fn a_bad_argument_is_refused_rather_than_defaulted() {
        assert!(parse(&["--churn-per-sec".to_string()]).is_err(), "no value");
        assert!(
            parse(&["--churn-per-sec".to_string(), "lots".to_string()]).is_err(),
            "not a number"
        );
        assert!(
            parse(&["--wash".to_string(), "maybe".to_string()]).is_err(),
            "not a switch"
        );
        assert!(
            parse(&["--frame-ms".to_string(), "0".to_string()]).is_err(),
            "a frame that advances no clock would never churn"
        );
        assert!(parse(&["--nonsense".to_string()]).is_err(), "unknown flag");
    }

    /// The churn model's contract: a fleet fills to capacity, then holds there
    /// while turning over. Without this a long run would either grow without
    /// bound or drain to nothing, and either way stop measuring what it claims.
    #[test]
    fn churn_fills_to_capacity_and_then_replaces_rather_than_growing() {
        let mut fleet = Fleet::new(4);
        // Ten milliseconds a frame, so a hundred frames is exactly one
        // simulated second and the rate can be asserted without the fractional
        // carry the default cadence deliberately keeps.
        let step = Duration::from_millis(10);
        for _ in 0..4 {
            fleet.churn(6.0, step);
        }
        assert_eq!(fleet.rows.len(), 4, "the fleet filled");
        assert_eq!(fleet.torn_down, 0, "nothing left on the way up");

        for _ in 0..100 {
            fleet.churn(6.0, step);
        }
        assert_eq!(fleet.torn_down, 6, "six replaced in a simulated second");
        assert_eq!(
            fleet.rows.iter().filter(|row| !row.leaving).count(),
            4,
            "still exactly capacity live"
        );
        assert!(
            fleet.rows.len() > 4,
            "the rows that left are still drawn until the engine retires them"
        );
    }

    /// Every arriving row is a *new* element. A fleet that reused pane ids would
    /// hand the engine an element it already had, no arrival would mount, and
    /// the benchmark would quietly measure a settled panel.
    #[test]
    fn a_replacement_row_is_a_new_element() {
        let mut fleet = Fleet::new(1);
        let step = Duration::from_millis(10);
        fleet.churn(6.0, step);
        let first = fleet.rows[0].element();
        for _ in 0..100 {
            fleet.churn(6.0, step);
        }
        assert!(fleet.torn_down > 0, "something turned over");
        assert!(
            fleet
                .rows
                .iter()
                .filter(|row| !row.leaving)
                .all(|row| row.element() != first),
            "a live row is still carrying the retired row's identity"
        );
    }

    /// A departing row keeps its slot until the engine retires it, and is gone
    /// once it has. A row dropped early cuts its exit short; one never dropped
    /// is a leak that would make every later frame more expensive than the last.
    #[test]
    fn a_departed_row_is_dropped_only_once_the_engine_is_done_with_it() {
        let mut fleet = Fleet::new(1);
        fleet.churn(6.0, Duration::from_millis(16));

        let mut anim = crate::anim::Animator::default();
        let lifecycle = crate::app::state::sidebar_trunk_lifecycle_from(
            &crate::config::SidebarAnimationConfig {
                row_exit: crate::config::SidebarTokenEmphasis::Dissolve,
                row_exit_ms: 200,
                ..Default::default()
            },
        );
        let now = Instant::now();
        // Admitted while it is live, or there would be no element for its
        // departure to play on and nothing for this test to be about.
        anim.observe(
            now,
            crate::anim::Family::AgentRow,
            &lifecycle,
            fleet.members(),
        );
        assert_eq!(fleet.rows.len(), 1, "the row is live and tracked");

        fleet.tear_down();
        anim.observe(
            now,
            crate::anim::Family::AgentRow,
            &lifecycle,
            fleet.members(),
        );
        fleet.retire_finished(&anim);
        assert_eq!(
            fleet.rows.len(),
            1,
            "the row is still drawn while its departure plays"
        );

        anim.advance(now + Duration::from_millis(400));
        fleet.retire_finished(&anim);
        assert!(fleet.rows.is_empty(), "and gone once the engine is done");
    }

    /// The ambient loop is rebuilt when the fleet's shape changes, which is what
    /// makes churn expensive. A key that did not move with the fleet would make
    /// this benchmark's whole wash story a single cached rebuild.
    #[test]
    fn the_scene_key_moves_when_the_fleet_does() {
        let mut fleet = Fleet::new(3);
        let step = Duration::from_millis(10);
        for _ in 0..3 {
            fleet.churn(6.0, step);
        }
        let before = scene_key(&fleet.nodes(), (1920, 1080));
        assert_eq!(
            before,
            scene_key(&fleet.nodes(), (1920, 1080)),
            "the same fleet keys the same"
        );

        for _ in 0..100 {
            fleet.churn(6.0, step);
        }
        assert_ne!(
            before,
            scene_key(&fleet.nodes(), (1920, 1080)),
            "a fleet that has turned over is a different scene"
        );
    }

    #[test]
    fn percentiles_land_on_real_samples() {
        let sorted: Vec<Duration> = (1..=100).map(Duration::from_millis).collect();
        assert_eq!(percentile(&sorted, 0.50), Duration::from_millis(50));
        assert_eq!(percentile(&sorted, 0.99), Duration::from_millis(99));
        assert_eq!(percentile(&sorted, 1.0), Duration::from_millis(100));
        assert_eq!(percentile(&[], 0.5), Duration::ZERO);
    }
}
