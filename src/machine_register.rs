//! The host machine's own state, and its recent past: CPU aggregate and per core, memory, swap,
//! and load average.
//!
//! The first register in this fork that is about the **substrate** rather than about the work. The
//! rest of the background scene mirrors the fleet — which projects exist, how big they are, how
//! they are doing; this one is about the machine the fleet is running on, and it is neither the
//! scene nor the fleet.
//!
//! ## Where the arithmetic lives, and why it lives here
//!
//! `crate::platform` reads raw counters and stops. A CPU fraction cannot exist in a single sample —
//! it is a ratio of two deltas of a cumulative counter — so computing one in the platform layer
//! would put a stateful problem inside the OS boundary and hand every platform its own chance to
//! get it wrong. This module holds the previous sample, does the subtraction once for every
//! platform, and is testable by handing it counters rather than by having a machine.
//!
//! ## Three rules carried over from the fleet orrery, which named them first
//!
//! - **No fabricated number, ever** (F21). On a platform this build does not read, or before the
//!   first pair of samples has landed, every quantity is `None` and the readout says why. Nothing
//!   is seeded from noise, defaulted to zero, or interpolated to fill a gap.
//! - **History is mandatory, not decoration** (A46(d)). A current value alone cannot say whether
//!   the machine is settling or climbing, and that difference is the whole reason anyone glances
//!   at this. Every continuous quantity carries its own recent past.
//! - **A stale sample is not a current one** (H12). When the newest sample is older than
//!   [`STALE_AFTER_INTERVALS`] sampling intervals, the register reports itself stalled rather than
//!   continuing to publish the last value as though it were now.
//!
//! ## Runtime/client boundary
//!
//! This is a shared runtime fact about the host, not TUI presentation state: it is owned by
//! `AppState`, exposed through the session API, and the scene is one client of it. See
//! `AGENTS.md`'s runtime/client guardrail.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::platform::MachineCounters;

/// How often the register takes a sample.
///
/// Two seconds, the fleet orrery's own interval. Fast enough that a machine going busy shows up
/// while the eye is still on it, slow enough that three small `/proc` reads are not a cost anyone
/// has to think about: at this cadence the whole register is well under a thousandth of a percent
/// of a core.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// How many sampling intervals a sample may be old before the register reports itself stalled.
///
/// Three, so one missed sample is not an outage: the register is sampled from the same tick loop
/// everything else runs on, and that loop is allowed to be busy occasionally.
pub(crate) const STALE_AFTER_INTERVALS: u32 = 3;

/// How many samples of history each quantity keeps.
///
/// Sixty at [`SAMPLE_INTERVAL`] is two minutes — long enough to show a machine settling after a
/// build, short enough that the whole register is a few kilobytes however many cores the host has.
pub(crate) const HISTORY_SAMPLES: usize = 60;

/// Which quantities the register carries. Ordered as they are read out.
///
/// A fixed set rather than a map: every one of these is a different question about the machine,
/// and a caller that wants "all of them" should get exactly these rather than whatever the last
/// sample happened to contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Quantity {
    /// Busy fraction across every CPU.
    Cpu,
    /// Memory in use as a fraction of total — *used* being total minus available, so the kernel's
    /// own cache is not reported as pressure.
    Memory,
    /// Swap in use as a fraction of total. A machine with no swap has a total of zero, which is a
    /// real answer: the fraction is zero and the quantity is present.
    Swap,
    /// One-minute load average, normalised by core count so it reads on the same `0.0..=1.0` scale
    /// as the rest — at `1.0` there is one runnable task per core.
    Load,
}

impl Quantity {
    pub(crate) const ALL: [Self; 4] = [Self::Cpu, Self::Memory, Self::Swap, Self::Load];

    /// The word this quantity draws as. Short enough for a corner, and a word rather than only a
    /// colour so the readout survives being read as text.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "mem",
            Self::Swap => "swap",
            Self::Load => "load",
        }
    }
}

/// One quantity's current value and its recent past, oldest sample first.
#[derive(Debug, Clone, Default)]
pub(crate) struct Series {
    history: VecDeque<f32>,
}

impl Series {
    /// The newest sample, or `None` if nothing has been recorded yet.
    pub(crate) fn current(&self) -> Option<f32> {
        self.history.back().copied()
    }

    /// Every sample held, oldest first.
    pub(crate) fn history(&self) -> impl ExactSizeIterator<Item = f32> + '_ {
        self.history.iter().copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.history.len()
    }

    fn push(&mut self, value: f32) {
        if self.history.len() == HISTORY_SAMPLES {
            self.history.pop_front();
        }
        self.history.push_back(value.clamp(0.0, 1.0));
    }
}

/// Why the register has nothing to show, when it has nothing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Absence {
    /// This build does not read the host's state on this platform.
    Unsupported,
    /// Read, but not yet twice — a CPU fraction is a ratio of two deltas, so the first sample
    /// produces no reading at all. Resolves on its own one interval later.
    AwaitingSecondSample,
    /// The newest sample is older than [`STALE_AFTER_INTERVALS`] intervals. Reported rather than
    /// papered over: continuing to draw a stale value as though it were current is the specific
    /// dishonesty H12 names.
    Stalled,
}

impl Absence {
    /// The reason, in the readout's own words. A corner that is empty has to say why it is empty.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::Unsupported => "the host's own state is not read on this platform",
            Self::AwaitingSecondSample => "waiting for a second sample",
            Self::Stalled => "the machine feed has stalled",
        }
    }
}

/// The host machine's state and its recent past.
#[derive(Debug, Clone, Default)]
pub(crate) struct MachineRegister {
    series: [Series; Quantity::ALL.len()],
    /// One series per logical CPU, in the OS's own core order. A core the OS stopped reporting
    /// keeps its slot rather than shifting every core after it along.
    cores: Vec<Series>,
    previous: Option<MachineCounters>,
    last_sample: Option<Instant>,
    sources: Vec<&'static str>,
    unsupported: bool,
    generation: u64,
}

impl MachineRegister {
    /// Whether it is time to take another sample.
    pub(crate) fn is_due(&self, now: Instant) -> bool {
        match self.last_sample {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= SAMPLE_INTERVAL,
        }
    }

    /// Take a sample from `counters`, which is whatever [`crate::platform::read_machine_counters`]
    /// returned.
    ///
    /// Returns whether anything a reader can see actually moved, so a caller can skip the work a
    /// changed register implies without diffing it.
    pub(crate) fn sample(&mut self, counters: Option<MachineCounters>, now: Instant) -> bool {
        let Some(counters) = counters else {
            let was = std::mem::replace(&mut self.unsupported, true);
            if !was {
                self.generation = self.generation.wrapping_add(1);
            }
            return false;
        };
        self.unsupported = false;
        self.sources = counters.sources.clone();

        // Every quantity but CPU is absolute and readable from one sample. CPU is a ratio of two
        // deltas, so it needs the previous read — which is exactly why the platform layer hands
        // over counters and this module does the subtraction.
        let previous = self.previous.replace(counters.clone());
        self.last_sample = Some(now);

        let mut moved = false;
        if let Some(fraction) = fraction_of(counters.memory_kib) {
            self.series[Quantity::Memory as usize].push(fraction);
            moved = true;
        }
        if let Some(fraction) = fraction_of(counters.swap_kib) {
            self.series[Quantity::Swap as usize].push(fraction);
            moved = true;
        }
        if let Some(load) = counters.load_average_1m {
            // Normalised by core count, so this reads on the same scale as everything beside it:
            // at 1.0 there is one runnable task per core, whatever the core count is. Saturating
            // rather than running off the end — a load of eight per core is not eight times more
            // legible than a load of two.
            let cores = counters.cpu_per_core.len().max(1) as f32;
            self.series[Quantity::Load as usize].push((load / cores).clamp(0.0, 1.0));
            moved = true;
        }

        let Some(previous) = previous else {
            return moved;
        };
        if let Some(fraction) = busy_fraction(previous.cpu_total, counters.cpu_total) {
            self.series[Quantity::Cpu as usize].push(fraction);
            moved = true;
        }
        // A core that vanishes from the OS's report keeps its slot: shifting every core after it
        // along would silently re-label every subsequent series.
        if self.cores.len() < counters.cpu_per_core.len() {
            self.cores
                .resize(counters.cpu_per_core.len(), Series::default());
        }
        for (idx, core) in counters.cpu_per_core.iter().enumerate() {
            let was = previous.cpu_per_core.get(idx).copied().flatten();
            if let Some(fraction) = busy_fraction(was, *core) {
                self.cores[idx].push(fraction);
                moved = true;
            }
        }
        if moved {
            self.generation = self.generation.wrapping_add(1);
        }
        moved
    }

    /// Bumped every time a reading changes.
    ///
    /// Lets a caller that draws the register decide whether to redraw without comparing two
    /// histories — the same shape `background_scene_key` uses for the scene, and for the same
    /// reason: the alternative is re-rendering on a timer and diffing the result.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// One quantity's series.
    pub(crate) fn series(&self, quantity: Quantity) -> &Series {
        &self.series[quantity as usize]
    }

    /// Every logical CPU's own series, in the OS's core order.
    pub(crate) fn cores(&self) -> &[Series] {
        &self.cores
    }

    /// The files these numbers were read from. A reader who wants to check a number has to be told
    /// where to check it.
    pub(crate) fn sources(&self) -> &[&'static str] {
        &self.sources
    }

    /// How old the newest sample is.
    pub(crate) fn age(&self, now: Instant) -> Option<Duration> {
        self.last_sample
            .map(|last| now.saturating_duration_since(last))
    }

    /// Why there is nothing to draw, or `None` when there is.
    ///
    /// Checked in the order the causes actually occur, so the reason a reader is given is the one
    /// that is true rather than the first one that happens to match.
    pub(crate) fn absence(&self, now: Instant) -> Option<Absence> {
        if self.unsupported || self.last_sample.is_none() {
            return Some(Absence::Unsupported).filter(|_| self.unsupported || self.is_empty());
        }
        if let Some(age) = self.age(now) {
            if age > SAMPLE_INTERVAL * STALE_AFTER_INTERVALS {
                return Some(Absence::Stalled);
            }
        }
        if self.is_empty() {
            return Some(Absence::AwaitingSecondSample);
        }
        None
    }

    /// Whether any quantity has a reading at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.series.iter().all(|s| s.len() == 0) && self.cores.iter().all(|s| s.len() == 0)
    }
}

/// A used/total pair as a fraction. A total of zero is a real machine state — no swap configured —
/// and reads as zero rather than as a missing quantity.
fn fraction_of(pair: Option<(u64, u64)>) -> Option<f32> {
    let (used, total) = pair?;
    if total == 0 {
        return Some(0.0);
    }
    Some((used as f32 / total as f32).clamp(0.0, 1.0))
}

/// The busy fraction between two cumulative `(busy, total)` reads.
///
/// `None` when either sample is missing or the counters did not advance — a window with no elapsed
/// time has no fraction in it, and inventing one (zero, say) would put a fabricated number in the
/// readout on exactly the sample where nothing is known. Counters that go *backwards* (a suspended
/// machine, a container's counters being reset) are treated the same way rather than producing a
/// nonsense ratio.
fn busy_fraction(previous: Option<(u64, u64)>, current: Option<(u64, u64)>) -> Option<f32> {
    let (was_busy, was_total) = previous?;
    let (busy, total) = current?;
    let total_delta = total.checked_sub(was_total)?;
    let busy_delta = busy.checked_sub(was_busy)?;
    if total_delta == 0 {
        return None;
    }
    Some((busy_delta as f32 / total_delta as f32).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(busy: u64, total: u64, cores: &[(u64, u64)]) -> MachineCounters {
        MachineCounters {
            cpu_total: Some((busy, total)),
            cpu_per_core: cores.iter().map(|c| Some(*c)).collect(),
            memory_kib: Some((4_000_000, 16_000_000)),
            swap_kib: Some((0, 8_000_000)),
            load_average_1m: Some(2.0),
            sources: vec!["/proc/stat", "/proc/meminfo", "/proc/loadavg"],
        }
    }

    fn at(seconds: u64) -> Instant {
        // A fixed origin plus an offset, so every test's clock is its own and none of them read a
        // real one.
        Instant::now() + Duration::from_secs(seconds)
    }

    #[test]
    fn a_cpu_fraction_needs_two_samples_and_says_so_until_it_has_them() {
        // F21's hardest case: the very first sample. A cumulative counter alone carries no rate,
        // and the tempting thing to publish is zero — which is a fabricated number about a machine
        // that might be at full tilt.
        let mut register = MachineRegister::default();
        let start = Instant::now();
        register.sample(Some(counters(1_000, 10_000, &[(500, 5_000)])), start);

        assert_eq!(register.series(Quantity::Cpu).current(), None);
        assert!(register.cores().iter().all(|core| core.current().is_none()));
        // ...while the quantities that *are* readable from one sample are already there, because
        // withholding those would be its own kind of dishonesty.
        assert_eq!(register.series(Quantity::Memory).current(), Some(0.25));

        // Half the window busy.
        register.sample(
            Some(counters(6_000, 20_000, &[(3_000, 10_000)])),
            start + SAMPLE_INTERVAL,
        );
        assert_eq!(register.series(Quantity::Cpu).current(), Some(0.5));
        assert_eq!(register.cores()[0].current(), Some(0.5));
    }

    #[test]
    fn nothing_is_read_on_a_platform_this_build_does_not_read() {
        // Not "zero", not "idle" — absent, with a reason. A plausible number invented from nothing
        // is worse than an empty corner that says why it is empty.
        let mut register = MachineRegister::default();
        assert!(!register.sample(None, Instant::now()));
        assert_eq!(register.absence(Instant::now()), Some(Absence::Unsupported));
        for quantity in Quantity::ALL {
            assert_eq!(register.series(quantity).current(), None);
            assert_eq!(register.series(quantity).len(), 0);
        }
        assert!(register.cores().is_empty());
        assert!(!Absence::Unsupported.reason().is_empty());
    }

    #[test]
    fn a_stale_sample_is_reported_rather_than_drawn_as_current() {
        // H12: when a sample is older than three intervals the register says the feed has stalled
        // rather than continuing to draw the last value as though it were now. The value is still
        // *held* — the history is real and stays real — it is the claim that it is current that
        // is withdrawn.
        let mut register = MachineRegister::default();
        let start = Instant::now();
        register.sample(Some(counters(0, 1_000, &[(0, 500)])), start);
        register.sample(
            Some(counters(500, 2_000, &[(250, 1_000)])),
            start + SAMPLE_INTERVAL,
        );
        assert_eq!(register.absence(start + SAMPLE_INTERVAL), None);

        // A hair past three intervals: "older than" is strictly greater, so exactly three is
        // still current and the boundary is asserted rather than assumed.
        let stale = start
            + SAMPLE_INTERVAL
            + SAMPLE_INTERVAL * STALE_AFTER_INTERVALS
            + Duration::from_millis(1);
        assert_eq!(
            register.absence(start + SAMPLE_INTERVAL + SAMPLE_INTERVAL * STALE_AFTER_INTERVALS),
            None,
            "exactly three intervals is not yet stale"
        );
        assert_eq!(register.absence(stale), Some(Absence::Stalled));
        assert!(register.series(Quantity::Cpu).current().is_some());
        // ...and one missed sample is not an outage.
        assert_eq!(register.absence(start + SAMPLE_INTERVAL * 3), None);
    }

    #[test]
    fn history_is_kept_bounded_and_in_order() {
        // A46(d): history is what makes the readout legible and is therefore mandatory — a current
        // value alone cannot say whether the machine is settling or climbing.
        let mut register = MachineRegister::default();
        let start = Instant::now();
        let mut busy = 0u64;
        let mut total = 0u64;
        for step in 0..HISTORY_SAMPLES * 2 {
            // A machine climbing steadily: each window is busier than the last.
            let window = 1_000u64;
            let fraction = (step % 10) as u64 * 100;
            busy += fraction;
            total += window;
            register.sample(
                Some(counters(busy, total, &[(busy / 2, total / 2)])),
                start + SAMPLE_INTERVAL * step as u32,
            );
        }
        let series = register.series(Quantity::Cpu);
        assert_eq!(series.len(), HISTORY_SAMPLES, "history is not bounded");

        // Oldest first, and it really is the *recent* past: the last value written is the one
        // `current` reports.
        let held: Vec<f32> = series.history().collect();
        assert_eq!(held.last().copied(), series.current());
        assert!(held.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    #[test]
    fn counters_that_go_backwards_produce_no_reading_rather_than_a_nonsense_one() {
        // A suspended machine, or a container whose counters were reset. The ratio is meaningless,
        // so there is no reading — not a clamped one, and certainly not a wrapped one.
        let mut register = MachineRegister::default();
        let start = Instant::now();
        register.sample(Some(counters(9_000, 10_000, &[(4_500, 5_000)])), start);
        register.sample(Some(counters(10, 100, &[(5, 50)])), start + SAMPLE_INTERVAL);
        assert_eq!(register.series(Quantity::Cpu).current(), None);

        // ...and a window where the counters did not move at all is the same case: no time
        // elapsed, so there is no fraction in it.
        assert_eq!(busy_fraction(Some((5, 10)), Some((5, 10))), None);
    }

    #[test]
    fn a_machine_with_no_swap_reads_zero_rather_than_missing() {
        // Zero of zero is a real machine state and a real answer. Reporting it as "unknown" would
        // put a gap in the readout for the most common desktop configuration there is.
        assert_eq!(fraction_of(Some((0, 0))), Some(0.0));
        assert_eq!(fraction_of(None), None);
        assert_eq!(fraction_of(Some((3, 4))), Some(0.75));
    }

    #[test]
    fn load_average_is_normalised_by_core_count() {
        // So it reads on the same 0..1 scale as everything beside it: at 1.0 there is one runnable
        // task per core, whatever the core count is. A raw load of 2 means very different things
        // on a 2-core box and a 64-core one.
        let mut register = MachineRegister::default();
        let start = Instant::now();
        let cores: Vec<(u64, u64)> = (0..8).map(|_| (0, 100)).collect();
        register.sample(Some(counters(0, 800, &cores)), start);
        assert_eq!(register.series(Quantity::Load).current(), Some(2.0 / 8.0));
    }

    #[test]
    fn a_core_that_stops_being_reported_keeps_its_slot() {
        // The per-core row is the hard case: shifting every core after a vanished one along would
        // silently re-label every series after it, so core 5's history would become core 4's.
        let mut register = MachineRegister::default();
        let start = Instant::now();
        let four: Vec<(u64, u64)> = (0..4).map(|i| (i * 10, 100)).collect();
        register.sample(Some(counters(0, 400, &four)), start);
        register.sample(
            Some(counters(
                200,
                800,
                &[(50, 200), (60, 200), (70, 200), (80, 200)],
            )),
            start + SAMPLE_INTERVAL,
        );
        assert_eq!(register.cores().len(), 4);

        let mut fewer = counters(400, 1_200, &[(100, 300), (120, 300)]);
        fewer.cpu_per_core.push(None);
        fewer.cpu_per_core.push(None);
        register.sample(Some(fewer), start + SAMPLE_INTERVAL * 2);
        assert_eq!(register.cores().len(), 4, "a core lost its slot");
        // The two that reported still advanced; the two that did not kept what they had rather
        // than being pushed a zero.
        assert_eq!(register.cores()[3].len(), 1);
    }

    #[test]
    fn sampling_is_due_on_its_own_interval_and_not_before() {
        let mut register = MachineRegister::default();
        let start = Instant::now();
        assert!(register.is_due(start), "the first sample is always due");
        register.sample(Some(counters(0, 100, &[])), start);
        assert!(!register.is_due(start + SAMPLE_INTERVAL / 2));
        assert!(register.is_due(start + SAMPLE_INTERVAL));
    }

    #[test]
    fn a_sample_that_changes_nothing_does_not_move_the_generation() {
        // The generation is what a caller redraws on. A machine sitting perfectly still would
        // otherwise redraw its corner every two seconds forever, which on a terminal where nothing
        // has changed is a repaint nobody asked for — and this register runs whether or not the
        // scene is even switched on.
        let mut register = MachineRegister::default();
        let start = Instant::now();

        // A platform this build does not read moves the generation exactly once, on the way in,
        // and never again.
        assert_eq!(register.generation(), 0);
        register.sample(None, start);
        let unsupported = register.generation();
        assert_ne!(unsupported, 0, "becoming unsupported is itself a change");
        register.sample(None, start + SAMPLE_INTERVAL);
        assert_eq!(
            register.generation(),
            unsupported,
            "an unchanged absence kept moving the generation"
        );

        // ...and a real reading moves it once per reading, not once per sample.
        let mut live = MachineRegister::default();
        live.sample(Some(counters(0, 1_000, &[(0, 500)])), start);
        let after_first = live.generation();
        live.sample(
            Some(counters(500, 2_000, &[(250, 1_000)])),
            start + SAMPLE_INTERVAL,
        );
        assert!(
            live.generation() > after_first,
            "a new reading did not register"
        );
    }

    #[test]
    fn every_quantity_names_itself() {
        // A word and not only a position, because the readout has to survive being read as text.
        let labels: Vec<&str> = Quantity::ALL.iter().map(|q| q.label()).collect();
        assert_eq!(labels, ["cpu", "mem", "swap", "load"]);
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(unique.len(), labels.len(), "two quantities share a label");
        let _ = at(0);
    }
}
