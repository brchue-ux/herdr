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
//!   identical to its unsignalled rendering, which is why a signal only ever
//!   resolves to a *style*, never to a symbol, a width, or a label.
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

/// Lifetime used when a publisher does not supply `ttl_ms`. Divided across
/// `SIGNAL_STOPS` it puts each stop at 200 ms — long enough to read as a
/// deliberate movement rather than a flicker, short enough that the row is back
/// to its settled appearance before anyone reaches for it.
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
/// sidebar; the icon is the stop after the last of them.
pub(crate) const CONNECTOR_CELLS: u8 = 3;
/// Total stops on the route: every connector cell, then the state icon.
pub(crate) const SIGNAL_STOPS: u8 = CONNECTOR_CELLS + 1;

/// What happened between two workspaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelationSignalKind {
    /// Work moved from one workspace to another.
    Transfer,
    /// A workspace finished the work it was given.
    Completed,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelationSignalPhase {
    pub(crate) direction: SignalDirection,
    /// `0..CONNECTOR_CELLS` index a connector cell left to right;
    /// `CONNECTOR_CELLS` is the state icon at the end of the line.
    pub(crate) cell: u8,
}

impl RelationSignalPhase {
    /// True when the charge is sitting on the row's state icon.
    pub(crate) fn is_at_state_icon(&self) -> bool {
        self.cell >= CONNECTOR_CELLS
    }

    /// The connector cell the charge occupies, if it is on the connector.
    pub(crate) fn connector_cell(&self) -> Option<u8> {
        (self.cell < CONNECTOR_CELLS).then_some(self.cell)
    }
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
    /// Stop index in `0..SIGNAL_STOPS`, recomputed by the runtime tick so that
    /// drawing stays a pure read of state.
    stop: u8,
}

impl RelationSignal {
    pub(crate) fn carrier_workspace_id(&self) -> &str {
        &self.carrier_workspace_id
    }

    fn direction(&self) -> SignalDirection {
        match self.kind {
            RelationSignalKind::Transfer => SignalDirection::Toward,
            RelationSignalKind::Completed => SignalDirection::Away,
        }
    }

    fn phase(&self) -> RelationSignalPhase {
        let cell = match self.direction() {
            SignalDirection::Toward => self.stop,
            SignalDirection::Away => SIGNAL_STOPS - 1 - self.stop,
        };
        RelationSignalPhase {
            direction: self.direction(),
            cell,
        }
    }

    /// Length of one stop. Never zero: `accept` clamps the lifetime to at least
    /// `MIN_SIGNAL_TTL`, which is comfortably more than `SIGNAL_STOPS` ticks.
    fn step(&self) -> Duration {
        (self.expires_at - self.started_at) / u32::from(SIGNAL_STOPS)
    }

    fn stop_at(&self, now: Instant) -> u8 {
        if now <= self.started_at {
            return 0;
        }
        let elapsed = now - self.started_at;
        let step = self.step();
        let stop = elapsed.as_nanos() / step.as_nanos().max(1);
        u8::try_from(stop)
            .unwrap_or(SIGNAL_STOPS - 1)
            .min(SIGNAL_STOPS - 1)
    }

    /// When this signal next changes what it draws: its next stop boundary, or
    /// its expiry once it is on the last stop.
    fn next_change_at(&self) -> Instant {
        let boundary = self
            .started_at
            .checked_add(self.step() * u32::from(self.stop + 1))
            .unwrap_or(self.expires_at);
        boundary.min(self.expires_at)
    }
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
            stop: 0,
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

    /// Drops expired signals and moves the survivors to the stop they are due
    /// at `now`. Returns true when anything a row draws changed.
    ///
    /// Called on every loop iteration, not only while the animation clock is
    /// armed, so expiry does not depend on anyone looking.
    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let before = self.live.len();
        self.live.retain(|signal| now < signal.expires_at);
        let mut changed = self.live.len() != before;
        for signal in &mut self.live {
            let stop = signal.stop_at(now);
            if stop != signal.stop {
                signal.stop = stop;
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

    #[test]
    fn a_transfer_travels_the_connector_and_lands_on_the_state_icon() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Transfer, now);

        let step = DEFAULT_SIGNAL_TTL / u32::from(SIGNAL_STOPS);
        let cells: Vec<u8> = (0..SIGNAL_STOPS)
            .map(|stop| {
                signals.advance(now + step * u32::from(stop) + Duration::from_millis(1));
                signals.phase_for_workspace("w4N").expect("live").cell
            })
            .collect();

        assert_eq!(cells, vec![0, 1, 2, CONNECTOR_CELLS]);
        assert!(signals
            .phase_for_workspace("w4N")
            .expect("live")
            .is_at_state_icon());
    }

    #[test]
    fn a_completion_runs_the_same_route_backwards() {
        let now = Instant::now();
        let mut signals = RelationSignals::default();
        accepted(&mut signals, RelationSignalKind::Completed, now);

        let step = DEFAULT_SIGNAL_TTL / u32::from(SIGNAL_STOPS);
        let cells: Vec<u8> = (0..SIGNAL_STOPS)
            .map(|stop| {
                signals.advance(now + step * u32::from(stop) + Duration::from_millis(1));
                signals.phase_for_workspace("w4N").expect("live").cell
            })
            .collect();

        assert_eq!(cells, vec![CONNECTOR_CELLS, 2, 1, 0]);
        assert_eq!(
            signals.phase_for_workspace("w4N").expect("live").direction,
            SignalDirection::Away
        );
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
        assert_eq!(signals.phase_for_workspace("w4N").expect("live").cell, 2);

        let later = now + step * 2;
        accepted(&mut signals, RelationSignalKind::Completed, later);
        assert_eq!(signals.iter().count(), 1);
        // The replacement starts from its own beginning, so no history is
        // animated: a completion never resumes a transfer's position.
        signals.advance(later);
        let phase = signals.phase_for_workspace("w4N").expect("live");
        assert_eq!(phase.direction, SignalDirection::Away);
        assert!(phase.is_at_state_icon());
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
        // Even the shortest lifetime still resolves every stop distinctly.
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
        assert_eq!(signals.phase_for_workspace("w4N").expect("live").cell, 1);

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

        let step = DEFAULT_SIGNAL_TTL / u32::from(SIGNAL_STOPS);
        assert_eq!(signals.next_deadline(), Some(now + step));

        signals.advance(now + step);
        assert_eq!(signals.next_deadline(), Some(now + step * 2));

        // On the last stop the only thing left to wake for is expiry.
        signals.advance(now + step * 3);
        assert_eq!(signals.next_deadline(), Some(now + DEFAULT_SIGNAL_TTL));
    }
}
