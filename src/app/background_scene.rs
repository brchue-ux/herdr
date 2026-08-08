//! AppState glue for the persistent whole-terminal background scene:
//! `[experimental] persistent_background`.
//!
//! Mirrors the split `src/ui/sidebar/particle_background.rs` has with
//! `src/particle_field.rs` — `src/solar_system.rs` is the pure `(layout, effects, phase) ->
//! RGBA8` generator, this module is everything that depends on live `AppState`: deriving the
//! scene's shape from the same owner tree the sidebar draws
//! (`crate::ui::sidebar::workspace_list_entries_whole_fleet`), resolving colour from the existing
//! `crate::app::lifecycle` hue/severity channel, and turning real triggers — a pane's own
//! `PendingEffects` entry, a fleet-published `outcome`/`streak` token, a workspace's PR checks
//! going green — into the asteroid/crater/comet effects `solar_system::SceneEffects` describes.
//!
//! ## Persistence: state-derived, not history-derived
//!
//! Matches the already-decided fade-clean model
//! (`data/decisions/2026-08-06-persistent-background-and-shooting-star-design.md`, firstmate
//! home): an effect's presence and age are recomputed from *current* wall-clock time against a
//! *recorded start*, never accumulated. A struck moon holds at most one asteroid/crater lifecycle
//! at a time, keyed by the pane that reported the issue — a second bug on an already-cratering
//! moon extends nothing and does not queue a second crater, since the existing crater is already
//! communicating "this one has a problem" and a history of exactly how many times is not part of
//! what this scene draws.
//!
//! ## What actually drives each trigger today
//!
//! - **Asteroid (bug/failure) impacts** read `AppState::pending_effects`
//!   (`crate::app::pending_effects`), which is real, live plumbing — but its only current
//!   producer is test-only (see that module's own doc); a screen-detection producer that decides
//!   "this pane's output is a failure" is separate, not-yet-built work. This module is ready the
//!   moment that producer lands.
//! - **PR-merge / clean-landing comets** read the `outcome` workspace metadata token
//!   (`herdr-outcome-publisher`, live today) via a value-transition check, since the token is
//!   durable rather than momentary and carries no publish timestamp of its own.
//! - **Green-test-pass comets** read `Workspace::cached_pull_requests`' `checks_failing`/
//!   `checks_pending` counts — already-live GitHub PR-check polling, independent of any
//!   fleet-side publisher — firing on the edge where both counts return to zero after having been
//!   positive.
//! - **Quality-streak-milestone comet showers** read the `streak`/`streak_hl` workspace tokens
//!   the fleet's `fm-quality-event.sh` publishes, through [`crate::quality_streak`] — which owns
//!   the token contract, the read-time decay and the band table for every surface that draws
//!   them, so a shower fires on the same band the sidebar's own flame readout is showing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::anim::cell::Severity;
use crate::anim::CardRow;
use crate::layout::PaneId;
use crate::solar_system;

/// How long each frame of the ambient orbit loop is shown once playback is armed. Slower than
/// `src/ui/sidebar/particle_background.rs`'s 100ms — planets read as majestic and slow, not
/// twitchy, and the loop is long besides ([`crate::solar_system::FRAME_COUNT`] frames).
pub(crate) const FRAME_GAP_MS: u32 = 140;

/// How long one full ambient-loop cycle plays for — every body lands back on its starting angle
/// after this many milliseconds. The effects overlay derives its own animation phase from this,
/// so a crater/comet drawn on a separate pass still roughly agrees with where the terminal's own
/// autonomously-played-back loop visually is.
pub(crate) const LOOP_DURATION_MS: u64 = FRAME_GAP_MS as u64 * solar_system::FRAME_COUNT as u64;

/// How long an asteroid takes to fly from its approach point to impact.
const ASTEROID_FLIGHT: Duration = Duration::from_millis(900);
/// How long a struck moon's crater takes to fully fade once the asteroid lands.
///
/// Deliberately much longer than [`ASTEROID_FLIGHT`] — round 1's "gradual crater fade over time,
/// not an instant clear" (`data/decisions/2026-08-07-terminal-background-visual-execution-round1.md`,
/// firstmate home).
const CRATER_FADE: Duration = Duration::from_secs(45);
/// How long a comet takes to cross the scene.
const COMET_FLIGHT: Duration = Duration::from_millis(2200);
/// How long a streak-milestone shower's comets stay staggered across, so they read as a burst
/// rather than one simultaneous flash.
const SHOWER_STAGGER: Duration = Duration::from_millis(260);
/// Comets in one streak-milestone shower.
const SHOWER_SIZE: usize = 6;

/// Where a bug-impact effect on one pane currently is: still travelling, or already landed and
/// fading. Kept as one state per pane rather than a list, so a second trigger on an
/// already-cratering moon is a no-op — see this module's own doc.
#[derive(Debug, Clone, Copy)]
enum AsteroidLifecycle {
    Flying {
        started_at: Instant,
        severity: Severity,
        approach_angle: f32,
    },
    Cratering {
        started_at: Instant,
        severity: Severity,
        angle_on_surface: f32,
    },
}

#[derive(Debug, Clone, Copy)]
struct ActiveComet {
    started_at: Instant,
    start: (f32, f32),
    end: (f32, f32),
    magnitude: f32,
}

/// Everything the background scene's event-driven overlay needs to remember between frames.
/// Sibling to `AppState::pending_effects` and `AppState::anim` in spirit: presentation state,
/// safe to forget wholesale whenever nobody is watching (see
/// `App::observe_background_effects`'s `has_viewers` gate).
#[derive(Debug, Clone, Default)]
pub(crate) struct BackgroundEffectsState {
    asteroids: HashMap<PaneId, AsteroidLifecycle>,
    comets: Vec<ActiveComet>,
    seen_outcome: HashMap<String, String>,
    seen_streak_band: HashMap<String, crate::quality_streak::FlameBand>,
    seen_checks_clear: HashMap<String, bool>,
}

impl BackgroundEffectsState {
    /// True while at least one effect is live — the gate `App::observe_background_effects` uses
    /// to decide whether it is still worth regenerating the overlay layer at all.
    pub(crate) fn is_live(&self) -> bool {
        !self.asteroids.is_empty() || !self.comets.is_empty()
    }

    /// Drop every remembered effect and transition marker. Used when nobody is watching (mirrors
    /// `Animator::forget_all`) and when the feature flag turns off.
    pub(crate) fn forget_all(&mut self) -> bool {
        let had_any = !self.asteroids.is_empty() || !self.comets.is_empty();
        self.asteroids.clear();
        self.comets.clear();
        // Transition markers are *not* cleared: forgetting them would replay every already-seen
        // outcome/streak/checks state as a fresh transition the next time this module runs,
        // which is exactly the double-fire this state exists to prevent.
        had_any
    }
}

/// Every node of the whole-fleet owner tree, in the exact shape `src/solar_system.rs` wants, plus
/// the identity each index resolves back to.
///
/// Reuses `crate::ui::sidebar::workspace_list_entries_whole_fleet` — the same walk the sidebar
/// itself draws from — rather than re-deriving parentage: this scene is a mirror of that tree,
/// not a second opinion about its shape. Parent-of-index is reconstructed from the flat list's
/// `depth` field alone (nearest preceding entry one depth shallower), which is valid because that
/// walk is guaranteed parent-before-children.
pub(crate) fn tree_nodes(
    app: &crate::app::state::AppState,
) -> (Vec<solar_system::TreeNode>, Vec<CardRow>) {
    let agents = crate::ui::sidebar::sidebar_agent_entries(app);
    let entries = crate::ui::sidebar::workspace_list_entries_whole_fleet(app);

    let mut nodes = Vec::with_capacity(entries.len());
    let mut identity = Vec::with_capacity(entries.len());
    let mut path: Vec<usize> = Vec::new();

    for entry in &entries {
        let depth = entry.depth() as usize;
        path.truncate(depth);
        let parent = path.last().copied();

        let Some((row, hue, severity)) = row_for_entry(app, entry, &agents) else {
            // A row this module cannot resolve to a colour (a dangling index between a pane
            // closing and the next tree rebuild) is skipped rather than drawn wrong — the next
            // pass, once the tree has settled, draws it correctly instead.
            continue;
        };

        let kind = match depth {
            0 => solar_system::BodyKind::Sun,
            1 => solar_system::BodyKind::Planet,
            _ => solar_system::BodyKind::Moon,
        };

        nodes.push(solar_system::TreeNode {
            parent,
            kind,
            hue,
            severity,
        });
        identity.push(row);
        path.push(nodes.len() - 1);
    }

    (nodes, identity)
}

fn row_for_entry(
    app: &crate::app::state::AppState,
    entry: &crate::ui::sidebar::WorkspaceListEntry,
    agents: &[crate::ui::AgentPanelEntry],
) -> Option<(CardRow, f32, Severity)> {
    match entry {
        crate::ui::sidebar::WorkspaceListEntry::Workspace { ws_idx, .. } => {
            let workspace = app.workspaces.get(*ws_idx)?;
            let tokens = workspace.metadata_tokens.values();
            let (state, _seen) = workspace.aggregate_state(&app.terminals);
            let stage = crate::app::lifecycle::stage(
                tokens
                    .get(crate::app::lifecycle::STAGE_TOKEN)
                    .map(String::as_str),
                state,
            );
            let severity = crate::app::lifecycle::severity(
                tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            );
            let hue = stage.hue(&app.palette, &app.host_terminal_theme);
            Some((CardRow::Space(workspace.id.clone()), hue, severity))
        }
        crate::ui::sidebar::WorkspaceListEntry::Agent { entry_idx, .. } => {
            let detail = agents.get(*entry_idx)?;
            let stage = crate::app::lifecycle::stage(
                detail
                    .tokens
                    .get(crate::app::lifecycle::STAGE_TOKEN)
                    .map(String::as_str),
                detail.state,
            );
            let severity = crate::app::lifecycle::severity(
                detail
                    .tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            );
            let hue = stage.hue(&app.palette, &app.host_terminal_theme);
            Some((CardRow::Agent(detail.pane_id), hue, severity))
        }
    }
}

/// The ambient loop's current animation phase, `0.0..=2*PI`, purely as a function of how long ago
/// it was (re)generated — see [`LOOP_DURATION_MS`].
pub(crate) fn phase_at(generated_at: Instant, now: Instant) -> f32 {
    let elapsed_ms = now.saturating_duration_since(generated_at).as_millis() as u64;
    let progress = (elapsed_ms % LOOP_DURATION_MS) as f32 / LOOP_DURATION_MS as f32;
    progress * std::f32::consts::TAU
}

/// A stable, deterministic angle in `0..=2*PI` derived from `seed` — used to pick where on a
/// body's surface a crater lands, or which edges a comet crosses between, without touching a
/// random number generator (`crate::solar_system`'s whole render path is pure, and this module
/// keeps the same discipline: same inputs, same picture).
fn pseudo_angle(seed: u64) -> f32 {
    let mut h = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 33;
    h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h ^= h >> 33;
    (h % 1_000_000) as f32 / 1_000_000.0 * std::f32::consts::TAU
}

/// Look for freshly-fired triggers and start a lifecycle for each one this module has not already
/// started. Pure state mutation — no rendering. `identity` is `tree_nodes`'s own output, so a
/// pane/workspace this pass cannot place in the scene cannot spawn an effect either.
pub(crate) fn spawn_new_effects(
    app: &crate::app::state::AppState,
    state: &mut BackgroundEffectsState,
    identity: &[CardRow],
    now: Instant,
) {
    for effect in app.pending_effects.live(now) {
        let crate::app::pending_effects::EffectKind::PaneIssue = effect.kind;
        if state.asteroids.contains_key(&effect.pane_id) {
            continue;
        }
        if !identity.contains(&CardRow::Agent(effect.pane_id)) {
            continue;
        }
        let severity = pane_severity(app, effect.pane_id);
        let seed = effect.pane_id.raw() as u64;
        state.asteroids.insert(
            effect.pane_id,
            AsteroidLifecycle::Flying {
                started_at: now,
                severity,
                approach_angle: pseudo_angle(seed),
            },
        );
    }

    for (ws_idx, workspace) in app.workspaces.iter().enumerate() {
        let ws_id = &workspace.id;

        if let Some(outcome) = workspace.metadata_tokens.get("outcome") {
            let is_landing = outcome == "pr_merged" || outcome == "landed";
            let already_seen = state.seen_outcome.get(ws_id).map(String::as_str) == Some(outcome);
            if is_landing && !already_seen {
                spawn_comet(state, ws_idx, now, work_size_magnitude(workspace));
            }
            state
                .seen_outcome
                .insert(ws_id.clone(), outcome.to_string());
        }

        if let Some(counts) = workspace.cached_pull_requests {
            let all_clear = counts.checks_failing == 0 && counts.checks_pending == 0;
            let had_outstanding = state.seen_checks_clear.get(ws_id) == Some(&false);
            if all_clear && had_outstanding {
                // The quiet, high-frequency tier: every green pass gets a star, kept small and
                // dim so it never competes with a landing or a streak shower.
                spawn_comet(state, ws_idx, now, 0.15);
            }
            state.seen_checks_clear.insert(ws_id.clone(), !all_clear);
        }

        // The band is read from the *decayed* score, through the one module that owns that
        // arithmetic (`crate::quality_streak`) rather than a second copy of the table here: a
        // milestone is a streak the fleet has right now, and a score read undecayed would fire a
        // shower for heat that had already faded — including on the frame after a cold start,
        // where nothing has ticked for however long Herdr was stopped.
        if let Some(streak) = workspace
            .metadata_tokens
            .get(crate::quality_streak::STREAK_TOKEN)
        {
            if let Some(readout) = crate::quality_streak::parse(streak) {
                let band = crate::quality_streak::FlameBand::of(crate::quality_streak::decayed(
                    readout,
                    crate::quality_streak::half_lives(
                        workspace
                            .metadata_tokens
                            .get(crate::quality_streak::HALF_LIFE_TOKEN),
                    ),
                    app.wall_now,
                ));
                let previous = state.seen_streak_band.get(ws_id).copied();
                if previous.is_some_and(|prev| band > prev) {
                    // A milestone is an accumulated streak, not one landed thing, so it reads as
                    // a shower of several comets rather than one bigger one.
                    for i in 0..SHOWER_SIZE {
                        let stagger = SHOWER_STAGGER * i as u32;
                        spawn_comet(state, ws_idx, now + stagger, 0.9);
                    }
                }
                state.seen_streak_band.insert(ws_id.clone(), band);
            }
        }
    }
}

/// The work-size magnitude a landing/merge comet scales by, `0.0..=1.0`.
///
/// No fleet publisher carries per-event size yet (see this module's own doc) — `outcome`-token
/// landings read as the mid tier (`M`, normalised `1.00` of the `0.25/0.50/1.00/1.75` scale,
/// itself renormalised into `0.0..=1.0`) until one does, which is a deliberately visible, honest
/// default rather than a guess dressed up as data.
fn work_size_magnitude(_workspace: &crate::workspace::Workspace) -> f32 {
    1.00 / 1.75
}

fn pane_severity(app: &crate::app::state::AppState, pane_id: PaneId) -> Severity {
    crate::ui::sidebar::sidebar_agent_entries(app)
        .into_iter()
        .find(|entry| entry.pane_id == pane_id)
        .map(|entry| {
            crate::app::lifecycle::severity(
                entry
                    .tokens
                    .get(crate::app::lifecycle::SEVERITY_TOKEN)
                    .map(String::as_str),
            )
        })
        .unwrap_or_default()
}

fn spawn_comet(
    state: &mut BackgroundEffectsState,
    ws_idx: usize,
    started_at: Instant,
    magnitude: f32,
) {
    // Varies with how many comets are already in flight this pass, so a shower's own several
    // calls (same `ws_idx`, same rough `started_at`) do not all pick the same path.
    let seed = (ws_idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (state.comets.len() as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    // Crossing the scene: the motion pattern round 1 left open
    // (`data/decisions/2026-08-07-terminal-background-visual-execution-round1.md`, firstmate
    // home) — picked here as the simplest reading of "comet/shooting star" that needs no
    // specific target body and naturally scales to a multi-comet shower.
    let entry_edge = pseudo_angle(seed);
    let start = edge_point(entry_edge);
    let end = edge_point(
        entry_edge + std::f32::consts::PI + (pseudo_angle(seed ^ 7) - std::f32::consts::PI) * 0.3,
    );
    state.comets.push(ActiveComet {
        started_at,
        start,
        end,
        magnitude: magnitude.clamp(0.05, 1.0),
    });
}

/// A point on the scene's own border, picked by angle from centre — used to send a comet in
/// from one edge and out through roughly the opposite one.
fn edge_point(angle: f32) -> (f32, f32) {
    let (s, c) = angle.sin_cos();
    // Push out to a unit square's border along this direction from the centre.
    let scale = 0.5 / s.abs().max(c.abs()).max(0.0001);
    (0.5 + c * scale, 0.5 + s * scale)
}

/// Advance every remembered lifecycle against `now`, drop anything fully finished, and build the
/// `solar_system::SceneEffects` this frame renders. `identity` must be the same slice
/// `tree_nodes` produced this pass, so a body index always resolves to the row it currently
/// means.
pub(crate) fn advance_and_build_effects(
    state: &mut BackgroundEffectsState,
    identity: &[CardRow],
    now: Instant,
) -> solar_system::SceneEffects {
    let mut effects = solar_system::SceneEffects::default();

    state.asteroids.retain(|pane_id, lifecycle| {
        let Some(target) = identity
            .iter()
            .position(|row| *row == CardRow::Agent(*pane_id))
        else {
            return false;
        };
        match *lifecycle {
            AsteroidLifecycle::Flying {
                started_at,
                severity,
                approach_angle,
            } => {
                let elapsed = now.saturating_duration_since(started_at);
                if elapsed >= ASTEROID_FLIGHT {
                    let angle = approach_angle;
                    *lifecycle = AsteroidLifecycle::Cratering {
                        started_at: now,
                        severity,
                        angle_on_surface: angle,
                    };
                    push_crater(&mut effects, target, angle, severity, 0.0);
                } else {
                    let progress = elapsed.as_secs_f32() / ASTEROID_FLIGHT.as_secs_f32();
                    effects.asteroids.push(solar_system::AsteroidInFlight {
                        target,
                        severity,
                        progress: progress.clamp(0.0, 1.0),
                        approach_angle,
                    });
                }
                true
            }
            AsteroidLifecycle::Cratering {
                started_at,
                severity,
                angle_on_surface,
            } => {
                let elapsed = now.saturating_duration_since(started_at);
                if elapsed >= CRATER_FADE {
                    return false;
                }
                let age = elapsed.as_secs_f32() / CRATER_FADE.as_secs_f32();
                push_crater(&mut effects, target, angle_on_surface, severity, age);
                true
            }
        }
    });

    // The fainter echo on a struck moon's parent planet is not tracked as a second lifecycle
    // here — `src/solar_system.rs::frame` derives it directly from each crater above via its own
    // `BodyLayout::parent` (which this module, working from a flat `identity` list, does not
    // have), scaled by `RIPPLE_FADE_RATIO` against the same age fraction already computed above.

    state.comets.retain(|comet| {
        let elapsed = now.saturating_duration_since(comet.started_at);
        if elapsed >= COMET_FLIGHT {
            return false;
        }
        let progress = elapsed.as_secs_f32() / COMET_FLIGHT.as_secs_f32();
        effects.comets.push(solar_system::Comet {
            start: comet.start,
            end: comet.end,
            magnitude: comet.magnitude,
            progress: progress.clamp(0.0, 1.0),
        });
        true
    });

    effects
}

fn push_crater(
    effects: &mut solar_system::SceneEffects,
    target: usize,
    angle: f32,
    severity: Severity,
    age: f32,
) {
    effects.craters.push(solar_system::Crater {
        body: target,
        angle_on_surface: angle,
        severity,
        age,
        // Never a ripple: the struck moon's own crater is what this module tracks and advances.
        // `src/solar_system.rs::frame` derives the fainter parent-planet echo from this crater
        // directly — see this function's callers' doc.
        is_ripple: false,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The milestone rule this module holds is *crossing into a higher band*,
    /// so all it needs from the shared vocabulary is that the bands order by
    /// heat. The bands themselves, and the decay that places a score in one,
    /// are `crate::quality_streak`'s to test.
    #[test]
    fn flame_band_orders_by_score() {
        use crate::quality_streak::FlameBand;
        assert!(FlameBand::of(-5.0) < FlameBand::of(2.0));
        assert!(FlameBand::of(2.0) < FlameBand::of(15.0));
        assert!(FlameBand::of(15.0) < FlameBand::of(25.0));
        assert!(FlameBand::of(25.0) < FlameBand::of(50.0));
    }

    #[test]
    fn pseudo_angle_is_deterministic_and_in_range() {
        let a = pseudo_angle(42);
        let b = pseudo_angle(42);
        assert_eq!(a, b);
        assert!((0.0..std::f32::consts::TAU).contains(&a));
    }
}
