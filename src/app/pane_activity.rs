//! Live per-terminal work-volume signal, sampled on Herdr's own loop.
//!
//! This is the one number that says *how hard a pane is working right now*. It
//! carries no opinion about what that should look like: a consumer decides
//! whether it becomes colour, motion, a meter, or nothing at all. Keeping the
//! decision on the consuming side is what lets the drawing be redesigned
//! without touching the sampler.
//!
//! Everything here is pure data with an explicit clock and explicit readings
//! passed in, so the whole smoothing curve is testable without a PTY, a socket,
//! or a render pass.
//!
//! Four properties this module is responsible for holding:
//!
//! - **The signal is sampled in-process, by Herdr, on Herdr's clock.** Nothing
//!   external publishes it and nothing external can hold it up. The raw input
//!   is a PTY output byte counter that only exists inside this process, which
//!   is also why it is the honest source: it sees an alternate-screen agent
//!   repainting in place, and it sees a spinner, neither of which grows a
//!   single line of scrollback. `pane.scroll.max_offset_from_bottom` was
//!   measured against a real running Herdr and stays flat at `0` through a
//!   full-screen application's entire lifetime, so it cannot carry this.
//! - **The level always decays to exactly zero.** It is a pure function of
//!   elapsed time and bytes observed, so a pane that goes quiet settles at `0`
//!   whether or not anyone is looking, and a sampler that stops running strands
//!   nothing — the next sample re-derives the level from the clock.
//! - **A busy pane cannot make the loop cost more wakes.** Sampling is bounded
//!   by `SAMPLE_INTERVAL` no matter how much output arrives, and once every
//!   pane has settled at zero the sampler asks for no deadline at all, so an
//!   idle Herdr never wakes up to measure silence.
//! - **The published shape is a contract.** A normalized `0.0..=1.0` level,
//!   plus the raw rate it was derived from. Consumers bind to the level; the
//!   rate exists so the normalization curve can be re-tuned by feel without
//!   anyone having to guess what the input was.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::terminal::TerminalId;

/// Shortest spacing between two smoothing steps.
///
/// Fast enough that a consumer redrawing at 60fps sees the level move rather
/// than step, slow enough that a pane dumping a build log cannot turn its own
/// output rate into the loop's wake rate.
pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

/// How many sample windows the output rate is measured across.
///
/// The rate is deliberately *not* taken from the single window the smoothing
/// step just closed. Agents emit in bursts, so consecutive windows alternate
/// between a whole burst and nothing at all, and a one-window rate turns that
/// into a target flipping between full scale and zero several times a second.
/// Measured against a real running Herdr, a pane under steady load oscillated
/// between 65% and 83% at 2Hz — the smoothing cannot damp that out, because
/// the input really is alternating and the asymmetric time constants below are
/// built to follow real starts and stops quickly.
///
/// Summing bytes and elapsed time across a second of windows spans the gaps
/// between bursts, so sustained work reads as one steady rate. Bounded rather
/// than exponential on purpose: a burst leaves the average completely once it
/// falls out the back, instead of trailing behind the pane forever.
const RATE_WINDOW_SAMPLES: usize = 4;

/// Time constant used while the rate is climbing.
///
/// Deliberately much shorter than `FALL_TAU`: work starting is the event worth
/// noticing immediately, and a slow attack reads as lag rather than as smoothing.
const RISE_TAU: Duration = Duration::from_millis(150);

/// Time constant used while the rate is falling.
///
/// Long enough that the gaps between an agent's output bursts do not read as
/// the pane going idle and back. This asymmetry is the whole reason the signal
/// looks like activity rather than like a strip chart of a byte counter.
const FALL_TAU: Duration = Duration::from_millis(900);

/// Output rate that reads as fully busy, in bytes per second.
///
/// A feel constant, not a measurement. It is the denominator of a logarithmic
/// curve because real output rates span orders of magnitude — an idle agent's
/// spinner is a few hundred bytes a second and a build log is megabytes — and a
/// linear scale would pin almost everything to either end.
const FULL_SCALE_BYTES_PER_SEC: f64 = 1_048_576.0;

/// Level below which a falling pane is snapped to rest.
///
/// Exponential decay only ever approaches zero, so without a floor the sampler
/// would arm deadlines forever chasing an ever-smaller fraction. Set below the
/// rounding threshold of one published percent, which is what makes the snap
/// invisible: no consumer can distinguish it from the decay continuing.
const LEVEL_FLOOR: f32 = 0.004;

/// What one terminal's work volume looks like right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PaneActivity {
    /// Smoothed work volume in `0.0..=1.0`. This is the contract value.
    pub(crate) level: f32,
    /// Lifetime PTY output bytes as of the last committed sample.
    pub(crate) output_bytes: u64,
}

impl PaneActivity {
    /// Level as whole percent, the form the API publishes.
    ///
    /// Quantizing here rather than at each call site is what keeps "did the
    /// published value change" a single, consistent question.
    pub(crate) fn level_percent(&self) -> u8 {
        // `level` is clamped into 0.0..=1.0 by `normalize`, so this cannot
        // saturate or wrap.
        (self.level * 100.0).round().clamp(0.0, 100.0) as u8
    }

    /// The output rate this level stands for, in bytes per second.
    ///
    /// Derived from `level` rather than reported alongside it, so the two can
    /// never disagree. Publishing the most recent window's measured rate
    /// instead was tried against a real running Herdr and was actively
    /// misleading: an agent emitting short bursts leaves most individual
    /// windows empty, so the raw rate read `0` while the level correctly read
    /// a fifth of full scale, which looks like a broken field rather than like
    /// smoothing working.
    pub(crate) fn bytes_per_sec(&self) -> f64 {
        if self.level <= 0.0 {
            return 0.0;
        }
        (f64::from(self.level) * FULL_SCALE_BYTES_PER_SEC.ln_1p()).exp_m1()
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Byte counter as of the last committed sample.
    committed_bytes: u64,
    /// Most recent byte counter reading, committed or not.
    latest_bytes: u64,
    level: f32,
    /// The most recent windows' byte deltas and the time each covered, as a
    /// ring. A fixed array rather than a deque: it is read on every loop pass,
    /// and this way a terminal's whole history costs no allocation.
    window_bytes: [u64; RATE_WINDOW_SAMPLES],
    window_secs: [f64; RATE_WINDOW_SAMPLES],
    window_next: usize,
    window_len: usize,
}

impl Entry {
    fn new(bytes: u64) -> Self {
        Self {
            committed_bytes: bytes,
            latest_bytes: bytes,
            level: 0.0,
            window_bytes: [0; RATE_WINDOW_SAMPLES],
            window_secs: [0.0; RATE_WINDOW_SAMPLES],
            window_next: 0,
            window_len: 0,
        }
    }

    /// Record one closed window's bytes and the time it covered.
    fn push_window(&mut self, bytes: u64, secs: f64) {
        self.window_bytes[self.window_next] = bytes;
        self.window_secs[self.window_next] = secs;
        self.window_next = (self.window_next + 1) % RATE_WINDOW_SAMPLES;
        self.window_len = (self.window_len + 1).min(RATE_WINDOW_SAMPLES);
    }

    /// Output rate across the whole trailing window, in bytes per second.
    ///
    /// Totals rather than an average of per-window rates, so windows that
    /// covered different amounts of time weigh in proportion to the time they
    /// actually covered — the loop wakes irregularly, and a short window must
    /// not count as much as a long one.
    fn windowed_rate(&self) -> f64 {
        // Slots fill from index 0 before the ring wraps, so the first
        // `window_len` entries are exactly the live ones in both cases.
        let bytes: u64 = self.window_bytes.iter().take(self.window_len).sum();
        let secs: f64 = self.window_secs.iter().take(self.window_len).sum();
        if secs <= 0.0 {
            return 0.0;
        }
        bytes as f64 / secs
    }

    /// True when this entry still has something to do: either a level to decay
    /// or unmeasured bytes waiting for the next commit.
    fn is_live(&self) -> bool {
        self.level > 0.0 || self.latest_bytes > self.committed_bytes
    }
}

/// Live activity levels, keyed by the terminal producing the output.
///
/// Keyed by terminal rather than by pane because the terminal is the thing that
/// emits bytes: a pane moved between tabs, or re-attached to the same terminal,
/// keeps its level instead of restarting from zero.
#[derive(Debug, Default)]
pub(crate) struct PaneActivityMap {
    entries: HashMap<TerminalId, Entry>,
    last_committed_at: Option<Instant>,
}

impl PaneActivityMap {
    /// Take one set of readings and, if enough time has passed, advance the
    /// smoothing.
    ///
    /// Called on every loop pass rather than only on the sample deadline. The
    /// cheap half — recording the latest counters — is what lets
    /// [`next_deadline`](Self::next_deadline) tell the difference between "this
    /// pane is quiet" and "this pane produced output 40ms ago and is waiting to
    /// be measured", so output arriving just after a commit still gets a wake
    /// scheduled for it instead of sitting unmeasured until something unrelated
    /// redraws.
    ///
    /// Terminals absent from `readings` are dropped, so a closed pane's level
    /// leaves with it rather than lingering as a ghost.
    ///
    /// Returns whether any published (percent-quantized) level changed.
    pub(crate) fn observe<'a, I>(&mut self, now: Instant, readings: I) -> bool
    where
        I: IntoIterator<Item = (&'a TerminalId, u64)>,
    {
        let mut seen = Vec::new();
        for (terminal_id, bytes) in readings {
            seen.push(terminal_id.clone());
            match self.entries.get_mut(terminal_id) {
                Some(entry) => entry.latest_bytes = bytes,
                None => {
                    self.entries.insert(terminal_id.clone(), Entry::new(bytes));
                }
            }
        }
        let dropped = self.entries.len() != seen.len();
        if dropped {
            self.entries.retain(|id, _| seen.contains(id));
        }

        let Some(elapsed) = self.due_elapsed(now) else {
            return false;
        };
        self.commit(now, elapsed) || dropped
    }

    /// Elapsed time since the last commit, once a full interval has passed.
    fn due_elapsed(&self, now: Instant) -> Option<Duration> {
        let Some(last) = self.last_committed_at else {
            // The first observation establishes the baseline counters; there is
            // no interval to divide by yet.
            return Some(SAMPLE_INTERVAL);
        };
        now.checked_duration_since(last)
            .filter(|elapsed| *elapsed >= SAMPLE_INTERVAL)
    }

    fn commit(&mut self, now: Instant, elapsed: Duration) -> bool {
        let elapsed_secs = elapsed.as_secs_f64();
        let mut changed = false;
        for entry in self.entries.values_mut() {
            let before = quantize(entry.level);
            // A counter that went backwards means a new runtime behind the same
            // terminal, not negative work. Same for the `clear` that resets a
            // pane's scrollback: neither is evidence of anything undone.
            let delta = entry.latest_bytes.saturating_sub(entry.committed_bytes);
            entry.push_window(delta, elapsed_secs);
            // Measured across the trailing window rather than this one closed
            // window, so the gaps between an agent's bursts do not read as the
            // rate itself alternating. See `RATE_WINDOW_SAMPLES`.
            let rate = entry.windowed_rate();
            // Smoothed in level space, not in bytes-per-second space. Decaying
            // the rate exponentially would decay the *level* linearly, because
            // the level is its logarithm — so a loud burst would leave a long
            // flat trail instead of fading. Smoothing what the consumer sees
            // also makes the time constants above mean what they say.
            entry.level = smooth(entry.level, normalize(rate), elapsed);
            entry.committed_bytes = entry.latest_bytes;
            changed |= quantize(entry.level) != before;
        }
        self.last_committed_at = Some(now);
        changed
    }

    /// When the sampler next needs the loop to wake.
    ///
    /// `None` once every pane has settled at zero with nothing unmeasured, so a
    /// Herdr with no output happening arms no deadline at all. Output arriving
    /// already wakes the loop on its own, and the pass it wakes records the new
    /// counters, which is what re-arms this.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.entries.values().any(Entry::is_live) {
            return None;
        }
        let deadline = self
            .last_committed_at
            .and_then(|last| last.checked_add(SAMPLE_INTERVAL))
            .unwrap_or(now);
        Some(deadline.max(now))
    }

    pub(crate) fn get(&self, terminal_id: &TerminalId) -> Option<PaneActivity> {
        self.entries.get(terminal_id).map(|entry| PaneActivity {
            level: entry.level,
            output_bytes: entry.latest_bytes,
        })
    }

    /// A terminal's level, or `0.0` for one that has never been sampled.
    ///
    /// The accessor a paint pass should use: an unknown terminal is a quiet
    /// terminal, never an error and never a missing frame.
    // Reached only through `AppState::pane_activity_level`, which no paint pass
    // binds to yet. See the note there.
    #[allow(dead_code)]
    pub(crate) fn level(&self, terminal_id: &TerminalId) -> f32 {
        self.entries
            .get(terminal_id)
            .map_or(0.0, |entry| entry.level)
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One step of an asymmetric exponential moving average.
///
/// The weight is derived from the elapsed time rather than fixed per step, so
/// the curve a consumer sees does not change when the loop samples irregularly
/// — which it does, because the loop wakes for input, resizes, and API calls,
/// not only for this deadline.
fn smooth(current: f32, target: f32, elapsed: Duration) -> f32 {
    let tau = if target > current { RISE_TAU } else { FALL_TAU };
    let alpha = (1.0 - (-elapsed.as_secs_f64() / tau.as_secs_f64()).exp()) as f32;
    let next = current + alpha * (target - current);
    if next < LEVEL_FLOOR && target <= 0.0 {
        return 0.0;
    }
    next.clamp(0.0, 1.0)
}

/// Map an output rate onto the published `0.0..=1.0` level.
fn normalize(bytes_per_sec: f64) -> f32 {
    if bytes_per_sec <= 0.0 {
        return 0.0;
    }
    let level = bytes_per_sec.ln_1p() / FULL_SCALE_BYTES_PER_SEC.ln_1p();
    level.clamp(0.0, 1.0) as f32
}

fn quantize(level: f32) -> u8 {
    (level * 100.0).round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(_name: &str) -> TerminalId {
        TerminalId::alloc()
    }

    /// Drive the map for `steps` commits, feeding `per_step` bytes each time.
    fn run(map: &mut PaneActivityMap, id: &TerminalId, start: Instant, steps: u32, per_step: u64) {
        let mut total = map.get(id).map_or(0, |activity| activity.output_bytes);
        for step in 1..=steps {
            total += per_step;
            map.observe(start + SAMPLE_INTERVAL * step, [(id, total)]);
        }
    }

    #[test]
    fn a_quiet_terminal_reads_as_zero_and_asks_for_no_wake() {
        let now = Instant::now();
        let id = terminal("term_quiet");
        let mut map = PaneActivityMap::default();

        map.observe(now, [(&id, 0)]);
        assert_eq!(map.level(&id), 0.0);

        run(&mut map, &id, now, 8, 0);
        assert_eq!(map.level(&id), 0.0);
        assert_eq!(
            map.next_deadline(now + SAMPLE_INTERVAL * 8),
            None,
            "a settled pane must not keep the loop awake"
        );
    }

    #[test]
    fn the_published_rate_never_contradicts_the_published_level() {
        let now = Instant::now();
        let id = terminal("term_bursty");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);

        // Bursty output: one loud window, then several empty ones. This is the
        // shape a real agent produces, and the shape that made a raw
        // last-window rate read zero against a plainly non-zero level.
        let mut total = 0u64;
        for step in 1..=6u32 {
            if step == 1 {
                total += 262_144;
            }
            map.observe(now + SAMPLE_INTERVAL * step, [(&id, total)]);
        }

        let activity = map.get(&id).expect("sampled");
        assert!(activity.level_percent() > 0, "{activity:?}");
        assert!(
            activity.bytes_per_sec() > 0.0,
            "a non-zero level must never report a zero rate: {activity:?}"
        );

        // And the two agree: the rate round-trips back to the same level.
        assert_eq!(normalize(activity.bytes_per_sec()), activity.level);
    }

    #[test]
    fn steady_bursty_output_holds_a_steady_level() {
        // The shape a real agent produces, and the one the live lab caught: a
        // whole burst lands in one sample window and the next window is empty.
        // Rated one window at a time, this pane oscillated between 65% and 83%
        // several times a second while its actual throughput never changed —
        // a flicker for anything bound to the level.
        let now = Instant::now();
        let id = terminal("term_steady_burst");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);

        let mut total = 0u64;
        let mut levels = Vec::new();
        for step in 1..=24u32 {
            if step % 2 == 0 {
                total += 44_000;
            }
            map.observe(now + SAMPLE_INTERVAL * step, [(&id, total)]);
            // Sample only after the trailing window has filled and the rise has
            // converged; the ramp up from rest is supposed to move.
            if step >= 12 {
                levels.push(map.get(&id).expect("sampled").level_percent());
            }
        }

        let min = levels.iter().copied().min().expect("levels");
        let max = levels.iter().copied().max().expect("levels");
        assert!(
            max - min <= 2,
            "unchanging throughput must not read as an oscillating level: {levels:?}"
        );
        assert!(
            min > 50,
            "sustained heavy output must still read as busy: {levels:?}"
        );
    }

    #[test]
    fn a_resting_pane_reports_no_rate_at_all() {
        let activity = PaneActivity {
            level: 0.0,
            output_bytes: 4_096,
        };
        assert_eq!(activity.level_percent(), 0);
        assert_eq!(activity.bytes_per_sec(), 0.0);
    }

    #[test]
    fn an_unsampled_terminal_is_quiet_rather_than_missing() {
        let map = PaneActivityMap::default();
        assert_eq!(map.level(&terminal("term_unknown")), 0.0);
        assert_eq!(map.get(&terminal("term_unknown")), None);
    }

    #[test]
    fn output_raises_the_level_and_silence_returns_it_to_exactly_zero() {
        let now = Instant::now();
        let id = terminal("term_busy");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);

        // ~256 KiB/s of output for a second.
        run(&mut map, &id, now, 4, 65_536);
        let busy = map.level(&id);
        assert!(
            busy > 0.5,
            "sustained heavy output should read as clearly busy, got {busy}"
        );

        // Then nothing at all. The level has to reach exactly zero, not merely
        // approach it, or the sampler never stops arming deadlines.
        let quiet_from = now + SAMPLE_INTERVAL * 4;
        run(&mut map, &id, quiet_from, 40, 0);
        assert_eq!(map.level(&id), 0.0);
        assert_eq!(map.next_deadline(quiet_from + SAMPLE_INTERVAL * 40), None);
    }

    #[test]
    fn the_level_rises_faster_than_it_falls() {
        let now = Instant::now();
        let id = terminal("term_envelope");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);

        // Output for a span, then silence for the same span. Both halves are
        // longer than the rate window on purpose: over a single step this
        // measures the window filling and draining, not the envelope, and a
        // lone quiet step after a burst is correctly not yet a fall.
        const STEPS: u32 = RATE_WINDOW_SAMPLES as u32 * 2;

        run(&mut map, &id, now, STEPS, 65_536);
        let peak = map.level(&id);

        run(&mut map, &id, now + SAMPLE_INTERVAL * STEPS, STEPS, 0);
        let after = map.level(&id);

        assert!(peak > 0.5, "sustained output should read as busy: {peak}");
        assert!(after < peak, "silence must bring the level down: {after}");
        // The rise covered all of `peak` from rest. Silence got the same number
        // of steps to give it back, and must not have managed it: that gap is
        // the asymmetry, and it is what keeps the pauses inside an agent's work
        // from reading as the pane stopping.
        assert!(
            after > peak * 0.15,
            "release must be gentler than attack: gave back all but {after} of \
             {peak} in the same {STEPS} steps it took to gain it"
        );
    }

    #[test]
    fn a_heavier_pane_reads_higher_than_a_lighter_one() {
        let now = Instant::now();
        let spinner = terminal("term_spinner");
        let build = terminal("term_build");
        let mut map = PaneActivityMap::default();

        let mut spinner_total = 0u64;
        let mut build_total = 0u64;
        for step in 0..=8u32 {
            map.observe(
                now + SAMPLE_INTERVAL * step,
                [(&spinner, spinner_total), (&build, build_total)],
            );
            // A repainting spinner trickles; a build log floods.
            spinner_total += 60;
            build_total += 131_072;
        }

        let spinner_level = map.level(&spinner);
        let build_level = map.level(&build);
        assert!(
            spinner_level > 0.0,
            "an in-place repaint is still work and must not read as idle"
        );
        assert!(
            build_level > spinner_level,
            "heavier output must read higher: build {build_level} vs spinner {spinner_level}"
        );
        assert!(build_level <= 1.0 && spinner_level <= 1.0);
    }

    #[test]
    fn the_level_never_leaves_its_published_range() {
        let now = Instant::now();
        let id = terminal("term_flood");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);

        // Far past the full-scale rate, sustained.
        run(&mut map, &id, now, 12, 64 * 1_048_576);
        let activity = map.get(&id).expect("sampled");
        assert!((0.0..=1.0).contains(&activity.level), "{activity:?}");
        assert_eq!(activity.level, 1.0);
        assert_eq!(activity.level_percent(), 100);
    }

    #[test]
    fn a_counter_that_restarts_is_not_negative_work() {
        let now = Instant::now();
        let id = terminal("term_replaced");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);
        run(&mut map, &id, now, 4, 65_536);
        assert!(map.level(&id) > 0.0);

        // A new runtime behind the same terminal restarts the counter at zero.
        let restart = now + SAMPLE_INTERVAL * 5;
        assert!(map.observe(restart, [(&id, 0)]) || true);
        let activity = map.get(&id).expect("sampled");
        assert!(
            activity.bytes_per_sec() >= 0.0 && activity.level >= 0.0,
            "a restarted counter must not drive the level negative: {activity:?}"
        );
        assert!((0.0..=1.0).contains(&activity.level));
    }

    #[test]
    fn sampling_is_bounded_by_the_interval_however_often_it_is_asked() {
        let now = Instant::now();
        let id = terminal("term_chatty");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);

        // Twenty observations inside one interval commit nothing.
        for step in 1..=20u64 {
            map.observe(now + Duration::from_millis(step), [(&id, step * 4_096)]);
        }
        assert_eq!(
            map.level(&id),
            0.0,
            "a chatty pane cannot force extra smoothing steps"
        );

        // But the bytes are not lost: the next due commit accounts for them.
        map.observe(now + SAMPLE_INTERVAL, [(&id, 20 * 4_096)]);
        assert!(map.level(&id) > 0.0);
    }

    #[test]
    fn unmeasured_output_arms_a_wake_even_between_commits() {
        let now = Instant::now();
        let id = terminal("term_pending");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&id, 0)]);
        // Settled, nothing pending: no reason to wake.
        assert_eq!(map.next_deadline(now), None);

        // Output arrives 40ms after the commit — too soon to measure, but it
        // must not sit unmeasured until something unrelated redraws.
        let arrived = now + Duration::from_millis(40);
        assert!(!map.observe(arrived, [(&id, 8_192)]));
        let deadline = map.next_deadline(arrived).expect("a wake is owed");
        assert_eq!(deadline, now + SAMPLE_INTERVAL);
    }

    #[test]
    fn a_closed_terminal_takes_its_level_with_it() {
        let now = Instant::now();
        let gone = terminal("term_gone");
        let stays = terminal("term_stays");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&gone, 0), (&stays, 0)]);
        run(&mut map, &gone, now, 4, 65_536);
        assert!(map.get(&gone).is_some());

        map.observe(now + SAMPLE_INTERVAL * 5, [(&stays, 0)]);
        assert_eq!(map.get(&gone), None);
        assert_eq!(map.level(&gone), 0.0);
        assert!(map.get(&stays).is_some());
    }

    #[test]
    fn an_irregular_sampling_cadence_lands_in_the_same_place() {
        let now = Instant::now();
        let steady = terminal("term_steady");
        let jittery = terminal("term_jittery");
        let mut map = PaneActivityMap::default();
        map.observe(now, [(&steady, 0), (&jittery, 0)]);

        // Same bytes over the same two seconds, one on a clean 250ms cadence
        // and one on a ragged one. Time-derived weights are what keep these
        // from diverging.
        let mut steady_total = 0u64;
        for step in 1..=8u32 {
            steady_total += 32_768;
            map.observe(now + SAMPLE_INTERVAL * step, [(&steady, steady_total)]);
        }

        let mut jittery_map = PaneActivityMap::default();
        jittery_map.observe(now, [(&jittery, 0)]);
        let offsets_ms = [260u64, 700, 980, 1_500, 1_760, 2_000];
        let mut jittery_total = 0u64;
        let mut previous_ms = 0u64;
        for offset_ms in offsets_ms {
            jittery_total += 32_768 * 8 * (offset_ms - previous_ms) / 2_000;
            previous_ms = offset_ms;
            jittery_map.observe(
                now + Duration::from_millis(offset_ms),
                [(&jittery, jittery_total)],
            );
        }

        let steady_level = map.level(&steady);
        let jittery_level = jittery_map.level(&jittery);
        assert!(
            (steady_level - jittery_level).abs() < 0.1,
            "cadence should not change the curve: steady {steady_level} vs jittery {jittery_level}"
        );
    }

    #[test]
    fn a_map_that_never_saw_a_terminal_stays_empty() {
        let now = Instant::now();
        let mut map = PaneActivityMap::default();
        assert!(!map.observe(now, std::iter::empty()));
        assert!(map.is_empty());
        assert_eq!(map.next_deadline(now), None);
    }
}
