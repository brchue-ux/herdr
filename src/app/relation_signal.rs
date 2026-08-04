//! Transient relation signals between runtime entities.
//!
//! A relation signal says *what happened between two workspaces* — one handed
//! work to another, one finished — and nothing about how that should look. The
//! publisher describes the fleet; Herdr decides whether that becomes a moving
//! charge on a branch line, a brief emphasis on the state icon, or nothing at
//! all. Keeping the decision on this side is what lets the drawing be redesigned
//! without changing a single publisher.
//!
//! Everything here is pure data with an explicit clock passed in, so the whole
//! lifecycle is testable without a PTY, a socket, or a render pass.
//!
//! Three properties this module is responsible for holding:
//!
//! - **A signal is decoration over state that is already correct.** It carries
//!   no information of its own. Dropping every frame of it must leave a row
//!   identical to its unsignalled rendering — same characters, same columns,
//!   same widths — which is why a signal resolves only over the connector's own
//!   decorative cells and never over a label, a symbol that means something, or
//!   a width the layout was computed from.
//! - **A signal always dies.** `expires_at` is set when it is accepted and is
//!   never extended, so a signal that is never drawn — collapsed sidebar, mobile
//!   layout, scrolled-off row, a client that went away — still disappears on
//!   schedule and can never strand a row mid-travel.
//! - **A publisher cannot make a row cost more frames.** Reporting is as cheap
//!   and as capped as `workspace.report_metadata` — one coalesced repaint
//!   request, already rate-limited by the loop's own minimum render interval —
//!   and beyond that a row refuses to restart its travel until the travel has
//!   moved. The number of frames a signal costs is a property of Herdr's clock,
//!   not of how chatty the publisher is.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Lifetime used when a publisher does not supply `ttl_ms`.
///
/// 800 ms puts the charge on a connector cell for about 200 ms — long enough to
/// read as a deliberate movement rather than a flicker, short enough that the
/// row is back to its settled appearance before anyone reaches for it. Divided
/// across `SIGNAL_POSITIONS` it also spends 25 ms per position, which is the
/// cadence the charge behaviour asks for; that is not a coincidence, it is
/// [`positions_for`] choosing the finest step the lifetime can pay for.
pub(crate) const DEFAULT_SIGNAL_TTL: Duration = Duration::from_millis(800);
/// Shortest lifetime that still leaves room for a visible travel.
pub(crate) const MIN_SIGNAL_TTL: Duration = Duration::from_millis(120);
/// Longest lifetime a publisher can ask for. Bounds how long one signal can
/// hold a row's branch line, whatever `ttl_ms` says.
pub(crate) const MAX_SIGNAL_TTL: Duration = Duration::from_millis(5_000);
/// Cap on concurrently live signals. Reached only by a publisher signalling
/// more distinct rows than a sidebar can show at once; the oldest is dropped.
pub(crate) const MAX_LIVE_SIGNALS: usize = 32;

/// Cells of the branch-line prefix a charge travels through before it reaches
/// the row's state icon. Matches the `├─ ` / `└─ ` connector drawn by the
/// sidebar; the icon is the last cell of the route.
pub(crate) const CONNECTOR_CELLS: u8 = 3;
/// Cells on the route: every connector cell, then the state icon.
pub(crate) const SIGNAL_STOPS: u8 = CONNECTOR_CELLS + 1;

/// Positions a charge resolves inside one cell of the route.
///
/// Eight, because eight is what the horizontal block ramp can actually draw
/// (`CHARGE_BLOCKS` in [`crate::anim::behaviour`]). Asking for more would move
/// the charge to places no cell could show it, which by this crate's own rule
/// is not a frame; asking for fewer would throw away resolution the glyph set
/// is offering for free. Four cells at eight positions each is thirty-two
/// distinguishable places along a route the cell grid alone offers four.
pub(crate) const SIGNAL_SUBSTEPS: u16 = 8;

/// Finest the whole route is ever quantized to.
pub(crate) const SIGNAL_POSITIONS: u16 = SIGNAL_STOPS as u16 * SIGNAL_SUBSTEPS;

/// What happened between two workspaces.
///
/// This is the *vocabulary*: four kinds because four is what a fleet actually
/// reports about a relation, and because a reader who has to tell them apart by
/// motion alone is being asked to time an 800 ms animation. Which colour each
/// one draws in is the sidebar's decision, not this module's — see
/// `relation_signal_ink` there. What lives here is only which way a kind
/// travels, because that is a fact about the relation rather than about the
/// drawing: work handed to a row arrives at it, and anything a row reports
/// about its own outcome leaves from it toward the trunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationSignalKind {
    /// Work moved from one workspace to another, and is starting there.
    Transfer,
    /// A workspace finished the work it was given.
    Completed,
    /// A workspace failed the work it was given.
    Failed,
    /// A workspace has nothing left to do and has gone quiet.
    Idle,
}

impl RelationSignalKind {
    /// Which way along the branch line this kind travels.
    fn direction(self) -> SignalDirection {
        match self {
            Self::Transfer => SignalDirection::Toward,
            Self::Completed | Self::Failed | Self::Idle => SignalDirection::Away,
        }
    }
}

/// Which way along its branch line a signal travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalDirection {
    /// From the trunk of the branch line toward the row's own state icon.
    Toward,
    /// From the row's state icon back out to the trunk.
    Away,
}

/// Where a signal has reached on the row that carries it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RelationSignalPhase {
    /// What is being signalled, which is what the drawing colours by.
    pub(crate) kind: RelationSignalKind,
    pub(crate) direction: SignalDirection,
    /// How far along the route the charge has reached, in `0.0..=1.0`, in
    /// *draw order*: `0.0` is the trunk end of the connector and `1.0` is the
    /// row's own state icon, whichever way the charge is actually travelling.
    ///
    /// Direction is already folded in, so a renderer walks its cells left to
    /// right and never has to know which way the charge is going — the only
    /// thing that reads `direction` is prose.
    ///
    /// Continuous rather than a cell index on purpose. A whole-cell index can
    /// only ever say which of three cells is lit, and three positions over
    /// 800 ms is five moves a second: the difference between a charge moving
    /// and a charge stepping is entirely in the places between the cells.
    pub(crate) progress: f32,
}

/// One accepted signal, mid-flight.
#[derive(Debug, Clone)]
pub(crate) struct RelationSignal {
    kind: RelationSignalKind,
    /// Canonical id of the workspace whose row draws this signal.
    ///
    /// The workspace at the other end is validated when the report is accepted
    /// and then deliberately not kept: both directions travel the carrier's own
    /// branch line, whose trunk already *is* the relation being drawn.
    carrier_workspace_id: String,
    started_at: Instant,
    expires_at: Instant,
    /// Quantized position along the route, in `0..=positions`, recomputed by
    /// the runtime tick so that drawing stays a pure read of state.
    position: u16,
    /// How many positions this signal's own lifetime affords.
    ///
    /// Per signal rather than global because a publisher chooses the lifetime:
    /// a 5 s charge and a 120 ms one both travel the same route, and the short
    /// one has to give up resolution rather than ask the loop for frames it
    /// could not draw. Held on the signal so `advance` and `next_deadline`
    /// cannot disagree about it.
    positions: u16,
}

impl RelationSignal {
    pub(crate) fn carrier_workspace_id(&self) -> &str {
        &self.carrier_workspace_id
    }

    fn lifetime(&self) -> Duration {
        self.expires_at - self.started_at
    }

    fn phase(&self) -> RelationSignalPhase {
        let travelled = f32::from(self.position) / f32::from(self.positions.max(1));
        RelationSignalPhase {
            kind: self.kind,
            direction: self.kind.direction(),
            progress: match self.kind.direction() {
                SignalDirection::Toward => travelled,
                SignalDirection::Away => 1.0 - travelled,
            },
        }
    }

    /// Length of one whole cell of the route.
    ///
    /// The coalescing window, and deliberately still a *cell* rather than a
    /// sub-cell step: the ceiling it enforces is "a publisher cannot restart a
    /// row's travel before that travel has visibly gone somewhere", and going
    /// an eighth of a cell is not visibly going somewhere. Never zero, because
    /// `accept` clamps the lifetime to at least `MIN_SIGNAL_TTL`.
    fn step(&self) -> Duration {
        self.lifetime() / u32::from(SIGNAL_STOPS)
    }

    fn position_at(&self, now: Instant) -> u16 {
        if now <= self.started_at {
            return 0;
        }
        let elapsed = now - self.started_at;
        let travelled =
            elapsed.as_nanos() * u128::from(self.positions) / self.lifetime().as_nanos().max(1);
        u16::try_from(travelled)
            .unwrap_or(self.positions)
            .min(self.positions)
    }

    /// When this signal next changes what it draws: its next position boundary,
    /// or its expiry once it has arrived.
    fn next_change_at(&self) -> Instant {
        let reached = f64::from(self.position.saturating_add(1)) / f64::from(self.positions.max(1));
        let boundary = self
            .started_at
            .checked_add(self.lifetime().mul_f64(reached))
            .unwrap_or(self.expires_at);
        boundary.min(self.expires_at)
    }
}

/// How many positions a charge of this lifetime resolves.
///
/// Capped so one step is never shorter than a frame of the charge's own
/// declared cadence: a signal asking to move faster than it can be drawn would
/// only wake the loop for frames nobody sees, which is the same rule the
/// animation engine holds for its own behaviours. A very short lifetime
/// therefore degrades to fewer, coarser positions rather than to a spin —
/// and at `MIN_SIGNAL_TTL` that floor is exactly one position per cell, which
/// is the old whole-cell travel, arrived at from the other direction.
fn positions_for(lifetime: Duration) -> u16 {
    let affordable = lifetime.as_millis()
        / crate::anim::behaviour::CHARGE_FRAME_INTERVAL
            .as_millis()
            .max(1);
    let affordable = u16::try_from(affordable).unwrap_or(SIGNAL_POSITIONS);
    affordable.clamp(u16::from(SIGNAL_STOPS), SIGNAL_POSITIONS)
}

/// Why a report was not turned into a live signal. All of these are reported to
/// the publisher as success: a signal that cannot be drawn is not an error, it
/// is simply nothing happening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalDropped {
    /// `seq` was at or behind the last one accepted from this source.
    StaleSequence,
    /// Too many distinct sources are already tracking sequences.
    SourceLimit,
    /// The row is already travelling a signal that has not finished its first
    /// stop, so this report was folded into it.
    Coalesced,
}

/// Live relation signals, plus the per-source sequence watermarks that make
/// reporting idempotent.
#[derive(Debug, Default)]
pub(crate) struct RelationSignals {
    live: Vec<RelationSignal>,
    sequences: HashMap<String, u64>,
}

impl RelationSignals {
    /// Clamps a publisher-supplied lifetime into the range Herdr will animate
    /// over. A publisher can make a signal shorter or longer; it cannot make it
    /// permanent, and it cannot make it too short to be seen.
    pub(crate) fn effective_ttl(ttl: Option<Duration>) -> Duration {
        ttl.unwrap_or(DEFAULT_SIGNAL_TTL)
            .clamp(MIN_SIGNAL_TTL, MAX_SIGNAL_TTL)
    }

    /// Records a signal against the row that will carry it.
    ///
    /// `carrier_workspace_id` and `peer_workspace_id` are canonical workspace
    /// ids the caller has already resolved, so an id that no longer exists is
    /// filtered out before it reaches here rather than being stored and
    /// dangling.
    ///
    /// Replaces any signal already travelling that row rather than queueing
    /// behind it — a queue would animate history.
    ///
    /// A replacement that arrives before the signal it replaces has finished its
    /// first stop is coalesced into it instead. That is the per-row ceiling: a
    /// publisher can report as often as it likes and still cannot restart a
    /// row's travel faster than the travel itself moves, so the number of frames
    /// a row costs stays a property of Herdr's clock. Coalescing is per row, not
    /// per source, so a fan-out that signals four different rows in one breath
    /// still gets four signals.
    pub(crate) fn accept(
        &mut self,
        source: &str,
        seq: Option<u64>,
        kind: RelationSignalKind,
        carrier_workspace_id: String,
        ttl: Option<Duration>,
        now: Instant,
    ) -> Result<(), SignalDropped> {
        match crate::metadata_tokens::accept_sequence(&mut self.sequences, source, seq) {
            Ok(true) => {}
            Ok(false) => return Err(SignalDropped::StaleSequence),
            Err(()) => return Err(SignalDropped::SourceLimit),
        }

        let lifetime = Self::effective_ttl(ttl);
        let signal = RelationSignal {
            kind,
            carrier_workspace_id,
            started_at: now,
            expires_at: now + lifetime,
            position: 0,
            positions: positions_for(lifetime),
        };

        if let Some(existing) = self
            .live
            .iter_mut()
            .find(|live| live.carrier_workspace_id == signal.carrier_workspace_id)
        {
            if now.saturating_duration_since(existing.started_at) < existing.step() {
                return Err(SignalDropped::Coalesced);
            }
            *existing = signal;
            return Ok(());
        }

        if self.live.len() >= MAX_LIVE_SIGNALS {
            self.live.remove(0);
        }
        self.live.push(signal);
        Ok(())
    }

    /// Drops expired signals and moves the survivors to the position they are
    /// due at `now`. Returns true when anything a row draws changed.
    ///
    /// Called on every loop iteration, not only while the animation clock is
    /// armed, so expiry does not depend on anyone looking.
    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let before = self.live.len();
        self.live.retain(|signal| now < signal.expires_at);
        let mut changed = self.live.len() != before;
        for signal in &mut self.live {
            let position = signal.position_at(now);
            if position != signal.position {
                signal.position = position;
                changed = true;
            }
        }
        if self.live.is_empty() {
            self.sequences.shrink_to_fit();
        }
        changed
    }

    /// Earliest moment a live signal changes what it draws, or expires.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.live.iter().map(RelationSignal::next_change_at).min()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &RelationSignal> {
        self.live.iter()
    }

    /// Where the signal on `workspace_id` has reached, if one is travelling it.
    pub(crate) fn phase_for_workspace(&self, workspace_id: &str) -> Option<RelationSignalPhase> {
        self.live
            .iter()
            .find(|signal| signal.carrier_workspace_id == workspace_id)
            .map(RelationSignal::phase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(signals: &mut RelationSignals, kind: RelationSignalKind, now: Instant) {
        signals
            .accept("firstmate", None, kind, "w4N".into(), None, now)
            .expect("unsequenced report is always accepted");
    }

    /// Every position the charge is at over its whole lifetime, sampled at the
    /// finest cadence anything downstream can draw.
    fn travel(kind: RelationSignalKind, now: Instant) -> Vec<f32> {
        let mut signals = RelationSignals::default();
        accepted(&mut signals, kind, now);
        let frame = crate::anim::behaviour::CHARGE_FRAME_INTERVAL;
        let mut seen = Vec::new();
        let mut elapsed = Duration::ZERO;
        while elapsed < DEFAULT_SIGNAL_TTL {
            signals.advance(now + elapsed);
            match signals.phase_for_workspace("w4N") {
                Some(phase) => seen.push(phase.progress),
                None => break,
            }
            elapsed += frame;
        }
        seen
    }

    fn distinct(positions: &[f32]) -> usize {
        let mut steps: Vec<i32> = positions.iter().map(|at| (at * 1_000.0) as i32).collect();
        steps.dedup();
        steps.len()
    }

    #[test]
    fn a_transfer_travels_the_connector_toward_the_state_icon() {
        let seen = travel(RelationSignalKind::Transfer, Instant::now());
        assert!(
            seen.windows(2).all(|pair| pair[1] >= pair[0]),
            "a transfer only ever moves toward the icon: {seen:?}"
        );
        assert!(seen.first().copied().expect("live at once") < 0.05);
        assert!(seen.last().copied().expect("live throughout") > 0.9);
    }

    #[test]
    fn the_travel_resolves_finer_than_one_position_per_cell() {
        // The whole point of the sub-cell model: four cells used to mean four
        // places to be, which over a default lifetime is five moves a second
        // and reads as stepping. A cell's worth of eighths is what makes it
        // read as moving instead.
        let resolved = distinct(&travel(RelationSignalKind::Transfer, Instant::now()));
        assert!(
            resolved > usize::from(SIGNAL_STOPS),
            "a charge that only resolves {resolved} places over {SIGNAL_STOPS} cells is stepping"
        );
        assert_eq!(
            positions_for(DEFAULT_SIGNAL_TTL),
            SIGNAL_POSITIONS,
            "the default lifetime should afford the finest travel there is"
        );
    }

    #[test]
    fn a_completion_runs_the_same_route_backwards() {
        let now = Instant::now();
        let seen = travel(RelationSignalKind::Completed, now);
        assert!(
            seen.windows(2).all(|pair| pair[1] <= pair[0]),
            "a completion only ever moves toward the trunk: {seen:?}"
        );
        assert!(seen.first().copied().expect("live at once") > 0.95);
        assert!(seen.last().copied().expect("live throughout") < 0.1);

        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Completed, now);
        assert_eq!(
            signals.phase_for_workspace("w4N").expect("live").direction,
            SignalDirection::Away
        );
    }

    #[test]
    fn every_kind_travels_the_way_its_meaning_does() {
        let now = Instant::now();
        // Work handed to a row arrives at it; anything a row reports about its
        // own outcome leaves it. Getting this backwards would draw a failure as
        // though someone had just sent the row a failure to work on.
        for (kind, expected) in [
            (RelationSignalKind::Transfer, SignalDirection::Toward),
            (RelationSignalKind::Completed, SignalDirection::Away),
            (RelationSignalKind::Failed, SignalDirection::Away),
            (RelationSignalKind::Idle, SignalDirection::Away),
        ] {
            let mut signals = RelationSignals::default();
            accepted(&mut signals, kind, now);
            assert_eq!(
                signals.phase_for_workspace("w4N").expect("live").direction,
                expected,
                "{kind:?} travels the wrong way"
            );
            assert_eq!(
                signals.phase_for_workspace("w4N").expect("live").kind,
                kind,
                "the phase has to carry the kind, or nothing downstream can colour it"
            );
        }
    }

    #[test]
    fn a_lifetime_too_short_to_draw_gives_up_resolution_rather_than_frames() {
        // A publisher can ask for a 120ms charge. It must not answer by asking
        // the loop for a frame every 4ms; it answers by moving in coarser
        // steps, which at the floor is exactly the whole-cell travel this
        // model replaced.
        let coarse = positions_for(MIN_SIGNAL_TTL);
        assert_eq!(coarse, u16::from(SIGNAL_STOPS));
        assert!(
            MIN_SIGNAL_TTL / u32::from(coarse) >= crate::anim::behaviour::CHARGE_FRAME_INTERVAL,
            "a step must never be shorter than a frame anyone could draw"
        );
        // And every lifetime in between lands somewhere sensible in between.
        assert!(positions_for(Duration::from_millis(400)) > coarse);
        assert!(positions_for(MAX_SIGNAL_TTL) == SIGNAL_POSITIONS);
    }

    #[test]
    fn a_signal_expires_even_though_nothing_ever_advanced_it() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Transfer, now);
        assert!(!signals.is_empty());

        // One single advance long after the fact, standing in for a sidebar
        // that was collapsed for the whole lifetime.
        assert!(signals.advance(now + DEFAULT_SIGNAL_TTL));
        assert!(signals.is_empty());
        assert_eq!(signals.phase_for_workspace("w4N"), None);
        assert_eq!(signals.next_deadline(), None);
    }

    #[test]
    fn a_stale_sequence_is_dropped_and_leaves_the_live_signal_alone() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        signals
            .accept(
                "firstmate",
                Some(7),
                RelationSignalKind::Transfer,
                "w4N".into(),
                None,
                now,
            )
            .expect("first sequenced report");

        assert_eq!(
            signals.accept(
                "firstmate",
                Some(7),
                RelationSignalKind::Completed,
                "w4N".into(),
                None,
                now,
            ),
            Err(SignalDropped::StaleSequence),
        );
        assert_eq!(
            signals.accept(
                "firstmate",
                Some(6),
                RelationSignalKind::Completed,
                "w4N".into(),
                None,
                now,
            ),
            Err(SignalDropped::StaleSequence),
        );

        // The live signal is still the transfer: a replayed report never
        // rewrites what is already travelling.
        assert_eq!(
            signals
                .phase_for_workspace("w4N")
                .map(|phase| phase.direction),
            Some(SignalDirection::Toward),
        );
        // A different source keeps its own watermark.
        assert!(signals
            .accept(
                "secondmate",
                Some(1),
                RelationSignalKind::Completed,
                "w4A".into(),
                None,
                now,
            )
            .is_ok());
    }

    #[test]
    fn a_second_signal_on_the_same_row_replaces_rather_than_queues() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Transfer, now);

        let step = DEFAULT_SIGNAL_TTL / u32::from(SIGNAL_STOPS);
        signals.advance(now + step * 2);
        let halfway = signals.phase_for_workspace("w4N").expect("live").progress;
        assert!((0.4..0.6).contains(&halfway), "half-way along: {halfway}");

        let later = now + step * 2;
        accepted(&mut signals, RelationSignalKind::Completed, later);
        assert_eq!(signals.iter().count(), 1);
        // The replacement starts from its own beginning, so no history is
        // animated: a completion never resumes a transfer's position.
        signals.advance(later);
        let phase = signals.phase_for_workspace("w4N").expect("live");
        assert_eq!(phase.direction, SignalDirection::Away);
        assert_eq!(phase.progress, 1.0, "an outbound charge starts at the icon");
    }

    #[test]
    fn a_publisher_cannot_hold_a_row_forever_or_flash_it_invisibly() {
        assert_eq!(
            RelationSignals::effective_ttl(Some(Duration::from_secs(3600))),
            MAX_SIGNAL_TTL,
        );
        assert_eq!(
            RelationSignals::effective_ttl(Some(Duration::from_millis(1))),
            MIN_SIGNAL_TTL,
        );
        assert_eq!(RelationSignals::effective_ttl(None), DEFAULT_SIGNAL_TTL);
        // Even the shortest lifetime still resolves every cell distinctly.
        const { assert!(MIN_SIGNAL_TTL.as_millis() >= SIGNAL_STOPS as u128) };
    }

    #[test]
    fn a_chatty_publisher_cannot_restart_a_row_faster_than_the_row_moves() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Transfer, now);
        let step = DEFAULT_SIGNAL_TTL / u32::from(SIGNAL_STOPS);

        // Everything inside the first stop folds into the signal already
        // travelling, so the row keeps moving instead of being pinned at its
        // starting cell forever.
        for offset in [0, 1, 50, 199] {
            assert_eq!(
                signals.accept(
                    "firstmate",
                    None,
                    RelationSignalKind::Transfer,
                    "w4N".into(),
                    None,
                    now + Duration::from_millis(offset),
                ),
                Err(SignalDropped::Coalesced),
            );
        }
        signals.advance(now + step + Duration::from_millis(1));
        let moved = signals.phase_for_workspace("w4N").expect("live").progress;
        assert!(
            moved > 0.2,
            "the row kept travelling rather than being pinned: {moved}"
        );

        // Past the first stop a genuinely new report is honoured.
        assert!(signals
            .accept(
                "firstmate",
                None,
                RelationSignalKind::Transfer,
                "w4N".into(),
                None,
                now + step + Duration::from_millis(1),
            )
            .is_ok());

        // Coalescing is per row: a fan-out to other rows is never swallowed.
        for other in ["w4A", "w4B", "w4C"] {
            assert!(signals
                .accept(
                    "firstmate",
                    None,
                    RelationSignalKind::Transfer,
                    other.into(),
                    None,
                    now,
                )
                .is_ok());
        }
        assert_eq!(signals.iter().count(), 4);
    }

    #[test]
    fn live_signals_stay_bounded_under_a_chatty_publisher() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        for index in 0..(MAX_LIVE_SIGNALS * 3) {
            signals
                .accept(
                    "firstmate",
                    None,
                    RelationSignalKind::Transfer,
                    format!("w{index}"),
                    None,
                    now,
                )
                .expect("unsequenced report is always accepted");
            assert!(signals.iter().count() <= MAX_LIVE_SIGNALS);
        }
        assert_eq!(signals.iter().count(), MAX_LIVE_SIGNALS);
    }

    #[test]
    fn the_next_deadline_is_the_next_visible_change() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Transfer, now);

        let step = DEFAULT_SIGNAL_TTL / u32::from(SIGNAL_POSITIONS);
        assert_eq!(signals.next_deadline(), Some(now + step));

        signals.advance(now + step);
        assert_eq!(signals.next_deadline(), Some(now + step * 2));

        // Once it has arrived the only thing left to wake for is expiry.
        signals.advance(now + DEFAULT_SIGNAL_TTL - Duration::from_millis(1));
        assert_eq!(signals.next_deadline(), Some(now + DEFAULT_SIGNAL_TTL));
    }
}
