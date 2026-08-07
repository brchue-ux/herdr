//! Identity-scoped triggers for a bounded-lifetime visual effect, waiting for
//! a renderer that does not exist yet.
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
//! [`crate::anim::Animator::frame`]; only [`Self::record`] mutates), but
//! holds identity only — never a screen coordinate. Resolving an identity to
//! an on-screen position is client-side, at render time, same as every other
//! visual element in this app; see this repo's own runtime/client boundary
//! rule in `CLAUDE.md`.
//!
//! Nothing reads [`PendingEffects::live`] yet. That is intentional: this is
//! plumbing for a captain-designed rendering layer that lands as its own,
//! later task.

use std::time::{Duration, Instant};

use crate::layout::PaneId;

/// Which kind of transient visual trigger fired.
///
/// Exactly one variant exists today because it is the only trigger kind this
/// task's brief asks to wire: a bug or failure detected in a pane's own
/// output or state. Adding a new trigger kind later is a new variant here
/// plus a new [`crate::events::AppEvent`] producer — this module has no
/// generic registration API, deliberately mirroring how `AppEvent` itself
/// works (see this module's own doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffectKind {
    /// A bug or failure was detected in a pane's own output or state.
    PaneIssue,
}

impl EffectKind {
    /// How long an entry stays live after it is recorded, before it is
    /// treated as expired.
    ///
    /// One fixed constant per kind, not a caller-supplied duration: nothing
    /// outside this module reads this list yet, so there is no consumer with
    /// an opinion on the number to plumb through.
    fn ttl(self) -> Duration {
        match self {
            Self::PaneIssue => Duration::from_millis(2_000),
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
}

impl PendingEffects {
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

    /// Every entry still within its TTL, oldest first.
    ///
    /// A pure `&self` read, deliberately: this is the same multi-client
    /// contract [`crate::anim::Animator::frame`] already establishes — as
    /// many attached clients as want to ask for this list on their own render
    /// pass can, without racing each other or silently dropping an entry a
    /// slower asker still needed. This filters defensively by TTL on every
    /// call rather than trusting an entry was already pruned, since nothing
    /// guarantees `Self::prune_expired` ran between two reads.
    // Next step: a renderer reading this list lands as its own, later task —
    // see this module's doc. Exercised by tests only until then.
    #[allow(dead_code)]
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
}
