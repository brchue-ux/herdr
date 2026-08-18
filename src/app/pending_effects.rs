//! Identity-scoped triggers for bounded-lifetime visual effects.
//!
//! This is deliberately the smallest possible slice of a much larger feature:
//! an arbitrary internal event ("a bug was detected in this pane") spawning a
//! transient visual effect ("a meteor at this card") somewhere on screen.
//! Three things that already exist in this codebase were each a near miss —
//! [`crate::events::AppEvent`] is a closed, exhaustively-matched enum with no
//! subscription model but the right single dispatch point
//! (`AppState::handle_app_event`); the public API's `EventHub` is pull-based
//! and carries no location; the fork's [`crate::anim::Animator`] is
//! membership-driven ("this row exists right now"), not spawn-driven
//! ("something happened here, live for a bit, then gone"). This module is the
//! missing spawn-driven piece, built as a sibling to `Animator` rather than a
//! new variant inside it: it follows the same single-writer/many-reader
//! discipline ([`Self::live`] is a pure `&self` read, exactly like
//! [`crate::anim::Animator::frame`]; writes stay in this module), but
//! holds identity only — never a screen coordinate. Resolving an identity to
//! an on-screen position is client-side, at render time, same as every other
//! visual element in this app; see this repo's own runtime/client boundary
//! rule in `CLAUDE.md`.
//!
//! The background scene reads [`PendingEffects::live`] and resolves pane identity to its current
//! on-screen body when drawing an impact or ask-win comet.

use std::time::{Duration, Instant};

use crate::layout::PaneId;

/// Which kind of transient visual trigger fired.
///
/// Adding a trigger kind is an explicit variant here plus a matching
/// [`crate::events::AppEvent`] producer — this module has no
/// generic registration API, deliberately mirroring how `AppEvent` itself
/// works (see this module's own doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectKind {
    /// A bug or failure was detected in a pane's own output or state.
    PaneIssue,
    /// Claude displayed a green success circle for a completed ask.
    PaneSuccess,
}

impl EffectKind {
    /// How long an entry stays live after it is recorded, before it is
    /// treated as expired.
    ///
    /// One fixed constant per kind, not a caller-supplied duration.
    fn ttl(self) -> Duration {
        match self {
            Self::PaneIssue | Self::PaneSuccess => Duration::from_millis(2_000),
        }
    }
}

/// One pending trigger: which pane it happened to, which kind of effect it
/// asked for, and when.
///
/// Deliberately carries no screen coordinate — see this module's own doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingEffect {
    pub(crate) pane_id: PaneId,
    pub(crate) kind: EffectKind,
    pub(crate) spawned_at: Instant,
}

impl PendingEffect {
    fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.spawned_at) >= self.kind.ttl()
    }
}

/// Every not-yet-expired visual-effect trigger, across every pane.
///
/// A single shared instance is meant to live on `AppState`, sibling to
/// `AppState::anim`, populated from the one place every `AppEvent` producer
/// already has a path into: `AppState::handle_app_event`.
#[derive(Debug, Clone, Default)]
pub(crate) struct PendingEffects {
    entries: Vec<PendingEffect>,
    /// Fleet-wide ask governor. It outlives the short pending-entry TTL so a busy pane cannot
    /// defeat the limit merely by waiting for the renderer to consume an entry.
    last_ask_at: Option<Instant>,
}

impl PendingEffects {
    #[cfg(unix)]
    pub(crate) fn ask_governor_age(&self, now: Instant) -> Option<Duration> {
        self.last_ask_at
            .map(|last| now.saturating_duration_since(last))
    }

    #[cfg(unix)]
    pub(crate) fn restore_ask_governor_age(&mut self, age: Option<Duration>, now: Instant) {
        self.last_ask_at = age.map(|age| now.checked_sub(age).unwrap_or(now));
    }

    /// Record a freshly fired trigger.
    ///
    /// Prunes anything already expired first, so this module's own memory
    /// cannot grow without bound purely from triggers nobody ever reads.
    pub(crate) fn record(&mut self, pane_id: PaneId, kind: EffectKind, now: Instant) {
        self.prune_expired(now);
        self.entries.push(PendingEffect {
            pane_id,
            kind,
            spawned_at: now,
        });
    }

    /// Record an ask win when the fleet-wide governor permits it.
    ///
    /// At the default scalar, no more than one ask comet is admitted per minute across every
    /// pane (60/hour). The scalar multiplies that rate by dividing the minimum interval.
    pub(crate) fn record_ask(&mut self, pane_id: PaneId, now: Instant, rate: f32) -> bool {
        let rate = if rate.is_finite() {
            rate.clamp(
                crate::config::CometsConfig::RATE_MIN,
                crate::config::CometsConfig::RATE_MAX,
            )
        } else {
            1.0
        };
        let interval = Duration::from_secs_f32(60.0 / rate);
        if self
            .last_ask_at
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return false;
        }
        self.last_ask_at = Some(now);
        self.record(pane_id, EffectKind::PaneSuccess, now);
        true
    }

    /// Every entry still within its TTL, oldest first.
    ///
    /// A pure `&self` read, deliberately: this is the same multi-client
    /// contract [`crate::anim::Animator::frame`] already establishes — as
    /// many attached clients as want to ask for this list on their own render
    /// pass can, without racing each other or silently dropping an entry a
    /// slower asker still needed. This filters defensively by TTL on every
    /// call rather than trusting an entry was already pruned, since nothing
    /// guarantees `Self::prune_expired` ran between two reads.
    pub(crate) fn live(&self, now: Instant) -> Vec<PendingEffect> {
        self.entries
            .iter()
            .copied()
            .filter(|entry| !entry.is_expired(now))
            .collect()
    }

    /// Drop every entry whose TTL has closed.
    ///
    /// The single writer half of the discipline `Self::live` depends on:
    /// mutation happens here and in `Self::record` only, never from a read.
    pub(crate) fn prune_expired(&mut self, now: Instant) {
        self.entries.retain(|entry| !entry.is_expired(now));
    }

    /// How many entries this module is currently holding onto, expired or
    /// not — a test-only way to observe pruning directly rather than only
    /// through `Self::live`'s own defensive filter.
    #[cfg(test)]
    pub(crate) fn raw_len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> PaneId {
        PaneId::alloc()
    }

    #[test]
    fn a_recorded_trigger_is_live_with_its_identity_kind_and_spawn_time() {
        let mut effects = PendingEffects::default();
        let now = Instant::now();
        let mine = pane();

        effects.record(mine, EffectKind::PaneIssue, now);

        let live = effects.live(now);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].pane_id, mine);
        assert_eq!(live[0].kind, EffectKind::PaneIssue);
        assert_eq!(live[0].spawned_at, now);
    }

    #[test]
    fn ttl_expiry_removes_the_entry() {
        let mut effects = PendingEffects::default();
        let now = Instant::now();
        let mine = pane();
        let ttl = EffectKind::PaneIssue.ttl();

        effects.record(mine, EffectKind::PaneIssue, now);

        // Still within its TTL: live() still reports it.
        assert_eq!(effects.live(now + ttl - Duration::from_millis(1)).len(), 1);

        // TTL closed: live() no longer reports it...
        assert!(effects.live(now + ttl).is_empty());
        // ...but nothing has swept internal storage yet, since only a read
        // happened so far.
        assert_eq!(effects.raw_len(), 1);

        // An explicit prune actually removes it.
        effects.prune_expired(now + ttl);
        assert_eq!(effects.raw_len(), 0);
    }

    #[test]
    fn recording_a_new_trigger_prunes_expired_ones_first() {
        let mut effects = PendingEffects::default();
        let now = Instant::now();
        let ttl = EffectKind::PaneIssue.ttl();

        effects.record(pane(), EffectKind::PaneIssue, now);
        effects.record(pane(), EffectKind::PaneIssue, now + ttl);

        // The first trigger's TTL has closed by the time the second is
        // recorded, so recording swept it away rather than accumulating it
        // forever.
        assert_eq!(effects.raw_len(), 1);
    }

    #[test]
    fn a_second_clients_read_never_mutates_state() {
        let mut effects = PendingEffects::default();
        let now = Instant::now();
        let ttl = EffectKind::PaneIssue.ttl();
        effects.record(pane(), EffectKind::PaneIssue, now);

        // Past the TTL, so a naive "read prunes" implementation would shrink
        // storage on the very first read.
        let read_time = now + ttl;
        let first_client_read = effects.live(read_time);
        let raw_len_after_first_read = effects.raw_len();
        let second_client_read = effects.live(read_time);

        assert!(first_client_read.is_empty());
        assert!(second_client_read.is_empty());
        assert_eq!(
            raw_len_after_first_read,
            effects.raw_len(),
            "a read must never mutate storage, so two reads must agree on raw_len"
        );
        assert_eq!(
            effects.raw_len(),
            1,
            "the entry is still only removed by an explicit prune/record, not by a read"
        );
    }

    #[test]
    fn no_triggers_is_no_live_entries() {
        let effects = PendingEffects::default();
        assert!(effects.live(Instant::now()).is_empty());
    }

    #[test]
    fn ask_governor_caps_a_busy_fleet_at_sixty_per_hour() {
        let mut effects = PendingEffects::default();
        let panes: Vec<_> = (0..13).map(|_| pane()).collect();
        let start = Instant::now();
        let mut admitted = 0;

        for second in 0..3_600 {
            let now = start + Duration::from_secs(second);
            for &pane_id in &panes {
                admitted += usize::from(effects.record_ask(pane_id, now, 1.0));
            }
        }

        eprintln!(
            "MEASURE ask governor: 46,800 markers/hour across 13 panes -> {admitted} comets/hour"
        );
        assert_eq!(admitted, 60);
    }

    #[test]
    fn ask_rate_scalar_changes_frequency_not_the_off_switch() {
        let start = Instant::now();
        let pane_id = pane();
        let mut half = PendingEffects::default();
        let mut double = PendingEffects::default();

        assert!(half.record_ask(pane_id, start, 0.5));
        assert!(!half.record_ask(pane_id, start + Duration::from_secs(119), 0.5));
        assert!(half.record_ask(pane_id, start + Duration::from_secs(120), 0.5));
        assert!(double.record_ask(pane_id, start, 2.0));
        assert!(double.record_ask(pane_id, start + Duration::from_secs(30), 2.0));
    }

    #[test]
    #[cfg(unix)]
    fn ask_governor_age_survives_a_process_clock_handoff() {
        let source_now = Instant::now();
        let mut source = PendingEffects::default();
        assert!(source.record_ask(pane(), source_now - Duration::from_secs(20), 1.0));

        let age = source.ask_governor_age(source_now);
        let imported_now = source_now + Duration::from_secs(5);
        let mut imported = PendingEffects::default();
        imported.restore_ask_governor_age(age, imported_now);
        assert!(!imported.record_ask(pane(), imported_now + Duration::from_secs(39), 1.0));
        assert!(imported.record_ask(pane(), imported_now + Duration::from_secs(40), 1.0));
    }

    #[test]
    fn identical_success_text_can_emit_again_after_the_governor_interval() {
        let mut visible = None;
        assert_eq!(
            crate::detect::diff_new_success_markers(Vec::new(), &mut visible),
            0
        );
        let first =
            crate::detect::diff_new_success_markers(vec!["● Done".to_string()], &mut visible);
        let mut effects = PendingEffects::default();
        let start = Instant::now();
        assert_eq!(first, 1);
        assert!(effects.record_ask(pane(), start, 1.0));

        let second = crate::detect::diff_new_success_markers(
            vec!["● Done".to_string(), "● Done".to_string()],
            &mut visible,
        );
        assert_eq!(second, 1);
        assert!(effects.record_ask(pane(), start + Duration::from_secs(60), 1.0));
    }
}
