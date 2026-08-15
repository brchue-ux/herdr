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
//! - **Asteroid (bug/failure) impacts** read `AppState::pending_effects`; their failure producer
//!   remains separate work.
//! - **Ask-win comets** read that same pane-identity path. Claude's bottom-buffer detector emits
//!   one when it observes a newly-visible green success circle, behind a fleet-wide governor.
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
//!
//! ## How big each body is drawn
//!
//! Not a trigger but a standing fact, and the one other thing this module reads from the fleet: a
//! project's body is sized by the [`FILES_TOKEN`] workspace metadata token — its tracked files at
//! HEAD — placed in [`solar_system::BodySize`]'s register. An unmeasured project is floored rather
//! than drawn at nothing, and the sun and worker bodies are out of the register entirely.

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
/// How long the rays an impact throws off stay visible. A flash, not a scar: a small fraction of
/// [`CRATER_FADE`], so the burst is over long before the mark it leaves has begun to fade.
const EJECTA_FADE: Duration = Duration::from_millis(1100);
/// How long a non-win streak-shower comet takes to cross the scene.
const COMET_FLIGHT: Duration = Duration::from_millis(2200);
/// The ask tier's life. CI and merge scale upward from this exact pre-tier comet lifetime.
const COMET_PASS_FLIGHT: Duration = Duration::from_millis(1250);
/// How long a streak-milestone shower's comets stay staggered across, so they read as a burst
/// rather than one simultaneous flash.
const SHOWER_STAGGER: Duration = Duration::from_millis(260);
/// Comets in one streak-milestone shower.
const SHOWER_SIZE: usize = 6;
/// How far a crossing comet's exit angle may wander from the point diametrically opposite its
/// entry, in radians. Applies to the quiet green-pass tier, unchanged from round 1.
const CROSS_CHORD_JITTER: f32 = 0.3 * std::f32::consts::PI;
/// The *smallest* deflection a shower comet's exit angle takes from diametrically opposite its
/// entry, in radians.
///
/// A chord between opposite edges runs through the middle of the scene, which is exactly where the
/// sun is — six of them at once all radiate through it. Forcing a minimum deflection pushes every
/// chord off-centre by a margin that clears the sun's disk instead of leaving it to chance; see
/// this module's `a_shower_spreads_its_comets_clear_of_the_sun` test, which measures that clearance
/// against the sun's real drawn radius.
const SHOWER_MIN_CHORD_DEFLECTION: f32 = 0.62;
/// How much further past [`SHOWER_MIN_CHORD_DEFLECTION`] a shower comet's exit angle may wander,
/// in radians — the part that keeps the six comets from tracing one repeated arc.
const SHOWER_CHORD_JITTER: f32 = 0.75;

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

/// How a comet moves, chosen by *what triggered it* rather than applied uniformly.
///
/// The three triggers this module already distinguishes are three different kinds of event, and
/// the storyboard's approved pick (`data/decisions/2026-08-08-ambient-storyboard-picks-autonomous.md`,
/// firstmate home) is to let the motion say which one happened — information the scene already has
/// for free. This resolves the "motion pattern round 1 left open" note that used to sit in
/// [`spawn_comet`].
#[derive(Debug, Clone)]
enum CometMotion {
    /// An ask completion or green CI pass: straight across the scene, no destination.
    Pass,
    /// A PR merge / clean landing: flies in from the edge and straight into the body the work
    /// landed on. Carried as a [`CardRow`] rather than a body index because the tree is rebuilt
    /// every pass — the index is re-resolved at draw time, exactly like an asteroid's target.
    Arrival { target: CardRow },
    /// One comet of a quality-streak milestone shower: crosses on a deliberately off-centre chord
    /// so the whole shower fans out instead of converging on the sun. `index` is the comet's place
    /// within its own shower, which is what alternates the chords to either side.
    Shower { index: usize },
}

#[derive(Debug, Clone)]
struct ActiveComet {
    started_at: Instant,
    flight: Duration,
    start: (f32, f32),
    end: (f32, f32),
    /// Set only for [`CometMotion::Arrival`]; a crossing comet flies to `end` unchanged.
    target: Option<CardRow>,
    magnitude: f32,
    tier: solar_system::WinTier,
    trail: std::sync::Arc<Vec<(f32, f32)>>,
    last_trail_sample_at: Option<Instant>,
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
    seen_success: HashMap<PaneId, Instant>,
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

    /// Drop only the comet layer, leaving independent impact effects and transition latches.
    pub(crate) fn forget_comets(&mut self) -> bool {
        let had_any = !self.comets.is_empty();
        self.comets.clear();
        had_any
    }
}

/// The workspace metadata token carrying a project's size: its tracked files at HEAD, as a plain
/// decimal count.
///
/// Same measure and same name as the fleet orrery's bridge already publishes for the web scene
/// (`git ls-tree -r HEAD --name-only`, counted), so a project reads the same size in both places,
/// and it rides the existing `workspace.report_metadata` path exactly like `lifecycle`/`severity`,
/// `outcome`, `streak` and `quota_5h` do — no new transport for one more fleet fact.
///
/// **Absent is not zero.** A project with no token, no HEAD to count, or an unparseable value is
/// [`solar_system::BodySize::Unmeasured`], which the register floors into a real body rather than
/// collapsing to a dot — see [`solar_system::BodySize`]'s own doc.
pub(crate) const FILES_TOKEN: &str = "files";

/// How much of its own orbit each body in the fleet has completed, and therefore how worn its
/// track is.
///
/// **This is the one part of the scene that is not regenerated from scratch.** The ambient loop is
/// baked and rebaked; a groove is wear in the ground, and wear that vanished every time the scene
/// was rebuilt would not be wear at all — it would be a decoration that happened to look like it.
///
/// ## What survives a regeneration, exactly
///
/// Keyed by **fleet identity** ([`crate::anim::CardRow`]) rather than by body index, so:
///
/// - A **resize** keeps every track. Nothing about the fleet changed; only the pixels did.
/// - A **body joining or leaving** keeps every *other* track. Indices shift on a topology change
///   and identities do not, which is the whole reason the key is what it is — keyed by index, one
///   pane closing would hand its accumulated wear to whichever body slid into its slot.
/// - A body that **leaves** loses its wear, and gets none back if it returns. A groove is how much
///   has passed *here*, and a body that left took its orbit with it.
/// - A **server restart** starts every track bare. This is in-memory state about a running
///   session, not something the fleet publishes, and inventing a past for a scene that has just
///   started would be the same fabrication the machine register refuses one module over.
#[derive(Debug, Default)]
pub(crate) struct OrbitTracks {
    /// Revolutions completed, per body still in the fleet.
    revolutions: HashMap<CardRow, f32>,
    /// When the accumulation was last advanced.
    advanced_at: Option<Instant>,
}

impl OrbitTracks {
    /// Advance every live body's revolution count to `now`, and forget every body that has left.
    ///
    /// `bodies` is each live body's identity paired with the revolutions it completes per animation
    /// loop — the rate `crate::solar_system` already resolved from its mass.
    ///
    /// Returns whether any body's *drawn* wear step moved, which is the only kind of change that
    /// can reach the picture: the counts advance continuously and the steps do not, so a caller
    /// that rebaked whenever a count moved would rebake every tick forever.
    pub(crate) fn advance<'a>(
        &mut self,
        bodies: impl Iterator<Item = (&'a CardRow, f32)>,
        now: Instant,
    ) -> bool {
        let elapsed = self
            .advanced_at
            .map(|last| now.saturating_duration_since(last))
            .unwrap_or(Duration::ZERO);
        self.advanced_at = Some(now);
        let loops = elapsed.as_secs_f32() / (LOOP_DURATION_MS as f32 / 1_000.0);

        let mut moved = false;
        let mut live: HashMap<CardRow, f32> = HashMap::new();
        for (row, per_loop) in bodies {
            let was = self.revolutions.get(row).copied().unwrap_or(0.0);
            let now_at = was + per_loop * loops;
            if solar_system::OrbitWear::of(was) != solar_system::OrbitWear::of(now_at) {
                moved = true;
            }
            live.insert(row.clone(), now_at);
        }
        // A body that has left is gone from `live` by construction, which is what forgets it.
        // Deliberately *not* reported as a change: a body joining or leaving already moves
        // `background_scene_key` through the node list itself, and reporting it here as well would
        // be a second rebake trigger for the same event.
        self.revolutions = live;
        moved
    }

    /// One body's drawn wear, `0.0..=1.0`.
    pub(crate) fn wear(&self, row: &CardRow) -> f32 {
        solar_system::OrbitWear::of(self.revolutions.get(row).copied().unwrap_or(0.0)).fraction()
    }

    /// Revolutions completed by one body — the raw count behind the drawn step.
    ///
    /// The caller its own doc was waiting for arrived: `crate::ui::sidebar::body_register` prints
    /// `N revs` on a row's orbit line, and the *drawn* step is four quantized rungs under a square
    /// root — a readout derived from it would say `2 revs` for anything between one and a few
    /// hundred. The scene still reads the step and nothing else.
    pub(crate) fn revolutions(&self, row: &CardRow) -> f32 {
        self.revolutions.get(row).copied().unwrap_or(0.0)
    }

    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.revolutions.len()
    }
}

/// The ambient tier: one mote per unit of work a body's own agent actually did.
///
/// ## What an "event" is here, stated rather than assumed
///
/// The fleet orrery's ambient tier counts **shell commands per body**. herdr does not count those
/// — libghostty-vt reports OSC 133 semantic prompt state, so the marks exist, but nothing in this
/// codebase tallies them. What herdr *does* hold natively, per body, with no bridge and no
/// publisher, is `crate::app::pane_activity::PaneActivity::output_bytes`: lifetime PTY output
/// bytes. That is the real per-body work register this fork has, so it is the one the tier is fed
/// from, quantized into discrete events by [`BYTES_PER_EVENT`].
///
/// The substitution is recorded rather than smuggled through: if a command counter lands later,
/// this tier changes what it reads and nothing else — the transform, the accounting and the bound
/// below are all about *counts*, whatever is being counted.
///
/// ## Every mote traces to one event
///
/// No mote is emitted by a timer, a loop, or a decorative oscillator. Motes emitted equals events
/// consumed, exactly, at every cadence — which is why the counters are kept rather than derived,
/// and why [`Self::emitted`] and [`Self::consumed`] are separate fields that a test can compare.
#[derive(Debug, Default)]
pub(crate) struct AmbientMotes {
    /// Per body: events consumed, motes emitted, and the raw count the transform reads.
    bodies: HashMap<CardRow, MoteTally>,
    /// The byte counter each body was last seen at, so only *new* work is consumed.
    seen_bytes: HashMap<CardRow, u64>,
}

#[derive(Debug, Default, Clone, Copy)]
struct MoteTally {
    consumed: u64,
    emitted: u64,
}

/// How much PTY output one ambient event is.
///
/// 16 KiB — roughly a screenful of an agent's own output, so a mote is "this body did a thing"
/// rather than "this body printed a character". Large enough that a chatty build does not stud its
/// whole orbit in a second, small enough that a quiet body still earns one.
pub(crate) const BYTES_PER_EVENT: u64 = 16 * 1024;

/// The attribution transform's three constants, ported exactly.
///
/// `GAMMA` compresses, `FLOOR` keeps a silent body legible, and `C0` keeps a zero count finite.
/// The worked example on the card: raw counts 400:200:60:10:2:0 — a 200x spread with two bodies
/// that would otherwise go black — become shares spread 4.1x with every body visible.
pub(crate) const MOTE_GAMMA: f64 = 0.45;
pub(crate) const MOTE_FLOOR: f64 = 0.30;
pub(crate) const MOTE_C0: f64 = 0.5;

/// Each body's share of the ambient tier's light, from the real per-body counts.
///
/// **Order-preserving, and that is the property that makes it honest.** It is a monotone function
/// of the real count and of nothing else, so it can compress the truth but can never reorder it:
/// the busiest body is always the brightest.
pub(crate) fn mote_shares(counts: &[u64]) -> Vec<f32> {
    if counts.is_empty() {
        return Vec::new();
    }
    let n = counts.len() as f64;
    let weights: Vec<f64> = counts
        .iter()
        .map(|c| (*c as f64 + MOTE_C0).powf(MOTE_GAMMA))
        .collect();
    let total: f64 = weights.iter().sum();
    weights
        .iter()
        .map(|w| {
            let share = if total > 0.0 {
                MOTE_FLOOR / n + (1.0 - MOTE_FLOOR) * (w / total)
            } else {
                1.0 / n
            };
            share as f32
        })
        .collect()
}

impl AmbientMotes {
    /// Consume every body's new work and emit one mote per event.
    ///
    /// `bodies` is each live body's identity and its lifetime output-byte counter. A body that has
    /// left is forgotten; a body that joins starts from wherever its counter already is, so
    /// attaching to a pane that has been running for an hour does not emit an hour of motes in one
    /// pass.
    pub(crate) fn consume<'a>(&mut self, bodies: impl Iterator<Item = (&'a CardRow, u64)>) -> bool {
        let mut live: HashMap<CardRow, MoteTally> = HashMap::new();
        let mut seen: HashMap<CardRow, u64> = HashMap::new();
        let mut emitted_any = false;

        for (row, bytes) in bodies {
            let tally = self.bodies.get(row).copied().unwrap_or_default();
            let mut tally = tally;
            match self.seen_bytes.get(row).copied() {
                // A body already being watched: everything new since last time is work it did.
                // A counter that went backwards (a pane replaced under the same identity) is
                // treated as a fresh start rather than as a huge negative.
                Some(was) if bytes >= was => {
                    let events = (bytes - was) / BYTES_PER_EVENT;
                    if events > 0 {
                        tally.consumed += events;
                        // One mote per event. Not "some motes when busy" — the whole accounting
                        // rests on this line being an equality.
                        tally.emitted += events;
                        emitted_any = true;
                        // Only what was actually consumed is marked seen, so the remainder is
                        // carried rather than discarded.
                        seen.insert(row.clone(), was + events * BYTES_PER_EVENT);
                    } else {
                        seen.insert(row.clone(), was);
                    }
                }
                _ => {
                    seen.insert(row.clone(), bytes);
                }
            }
            live.insert(row.clone(), tally);
        }

        self.bodies = live;
        self.seen_bytes = seen;
        emitted_any
    }

    /// How many motes one body carries.
    pub(crate) fn motes(&self, row: &CardRow) -> u64 {
        self.bodies.get(row).map(|t| t.emitted).unwrap_or(0)
    }

    /// Events consumed and motes emitted across the whole fleet. Equal, always.
    pub(crate) fn accounting(&self) -> (u64, u64) {
        self.bodies
            .values()
            .fold((0, 0), |(c, e), t| (c + t.consumed, e + t.emitted))
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
    tree_nodes_with_tracks(app, &app.orbit_tracks)
}

/// [`tree_nodes`], against a given track layer — so a test can hand one in and a caller that has
/// borrowed the tracks mutably does not have to give them back first.
pub(crate) fn tree_nodes_with_tracks(
    app: &crate::app::state::AppState,
    tracks: &OrbitTracks,
) -> (Vec<solar_system::TreeNode>, Vec<CardRow>) {
    tree_nodes_with(app, tracks, &app.ambient_motes)
}

/// [`tree_nodes`], against given track and mote layers.
pub(crate) fn tree_nodes_with(
    app: &crate::app::state::AppState,
    tracks: &OrbitTracks,
    motes: &AmbientMotes,
) -> (Vec<solar_system::TreeNode>, Vec<CardRow>) {
    let agents = crate::ui::sidebar::sidebar_agent_entries(app);
    let entries = crate::ui::sidebar::workspace_list_entries_whole_fleet(app);

    let mut nodes = Vec::with_capacity(entries.len());
    let mut identity = Vec::with_capacity(entries.len());
    let mut path: Vec<usize> = Vec::new();
    // The one node that is the sun, once the walk has found it.
    let mut sun: Option<usize> = None;

    for entry in &entries {
        let depth = entry.depth() as usize;
        path.truncate(depth);
        let parent = path.last().copied();

        let Some(facts) = row_for_entry(app, entry, &agents) else {
            // A row this module cannot resolve to a colour (a dangling index between a pane
            // closing and the next tree rebuild) is skipped rather than drawn wrong — the next
            // pass, once the tree has settled, draws it correctly instead.
            continue;
        };

        // **There is exactly one sun, whatever the tree's shape.** Depth alone used to decide this,
        // so *every* root drew as a star at orbit radius zero — and a second mate working in its own
        // checkout is a root. Two suns stacked on the frame's centre is not a second star; it is one
        // star with a hole punched through it, and the mate that owns the second one loses its place
        // in the register entirely. The first root the walk reaches is the first mate and is the sun;
        // every later root is a second mate like any other, orbiting it.
        let kind = match (depth, sun) {
            (0, None) => solar_system::BodyKind::Sun,
            (0, Some(_)) | (1, _) => solar_system::BodyKind::Planet,
            _ => solar_system::BodyKind::Moon,
        };
        let parent = match (depth, sun) {
            (0, Some(sun)) => Some(sun),
            _ => parent,
        };

        nodes.push(solar_system::TreeNode {
            parent,
            kind,
            label: facts.label,
            hue: facts.hue,
            severity: facts.severity,
            size: facts.size,
            streak: facts.streak,
            // The one fact here that is not read fresh from the fleet: how much of its own orbit
            // this body has already travelled, which is accumulated across every regeneration of
            // the scene. See [`OrbitTracks`].
            wear: tracks.wear(&facts.row),
            // One mote per unit of work this body's own agent actually did. The share is filled
            // in below, once every body's count is known — it is a share *of the fleet*, so it
            // cannot be resolved one body at a time.
            motes: motes.motes(&facts.row).min(u64::from(u32::MAX)) as u32,
            mote_share: 0.0,
        });
        identity.push(facts.row);
        if kind == solar_system::BodyKind::Sun {
            sun = Some(nodes.len() - 1);
        }
        path.push(nodes.len() - 1);
    }

    // The attribution transform is a share of the whole fleet's traffic, so it is applied once
    // the roster is complete rather than per body as they are walked.
    let counts: Vec<u64> = nodes.iter().map(|node| u64::from(node.motes)).collect();
    for (node, share) in nodes.iter_mut().zip(mote_shares(&counts)) {
        node.mote_share = share;
    }

    (nodes, identity)
}

/// The size register reading for one workspace, from [`FILES_TOKEN`].
///
/// A missing or malformed token is [`solar_system::BodySize::Unmeasured`] rather than
/// `Files(0)` — the two are deliberately different, and only the former is honest about a
/// project nobody has measured yet.
///
/// Reads through [`crate::metadata_tokens::MetadataTokens::get`] rather than `values()`: this runs
/// per workspace on every topology change, and materialising the whole token map to look at one
/// key is exactly the aggregate-state collection the scene's hot paths are not allowed to do.
fn work_size(workspace: &crate::workspace::Workspace) -> solar_system::BodySize {
    workspace
        .metadata_tokens
        .get(FILES_TOKEN)
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .map_or(solar_system::BodySize::Unmeasured, |files| {
            solar_system::BodySize::Files(files)
        })
}

/// The already-resolved fleet facts one sidebar row contributes to the scene: what it is, and the
/// four registers the renderer draws it from.
///
/// A struct rather than a tuple because the last two members are both plain numbers with no
/// natural order between them, and a five-tuple of `(CardRow, f32, Severity, BodySize, f32)` is
/// where a caller silently swaps two of them.
struct RowFacts {
    row: CardRow,
    /// What this body is called in the sky — the Space's own display name. Empty for a worker,
    /// which the reference does not caption.
    label: solar_system::SceneLabel,
    hue: f32,
    severity: Severity,
    size: solar_system::BodySize,
    /// The quality-streak expression, `0.0..=1.0` — see [`streak_expression`].
    streak: f32,
}

/// How hot a Space's published quality streak is reading, as the `0.0..=1.0` expression
/// `src/solar_system.rs` draws rings and gas swell from.
///
/// Measured against `crate::quality_streak`'s **own** published bands rather than against the
/// artifact's integer counter, because herdr's register is a different quantity: a decayed score
/// in five named bands, not a count of consecutive wins. The mapping is the one the bands already
/// state — nothing shows below the `Low` threshold, because `Ember` is documented as "barely
/// alight" and a body that expresses a streak nobody would call alight is lying about the fleet;
/// full expression at the `Hot` threshold, which the bands describe as reached by sustained work
/// with no ceiling above it.
///
/// This is the same shape as the artifact's A20 floor (*"visible accumulation begins only at 4"*),
/// stated in herdr's units instead of imported in the orrery's.
fn streak_expression(workspace: &crate::workspace::Workspace) -> f32 {
    const FLOOR: f64 = 8.0;
    const CEILING: f64 = 38.0;

    // Narrow `get`s rather than `values()`: this runs per workspace on every topology change, and
    // materialising the whole token map to read two keys is exactly the aggregate-state collection
    // the scene's hot paths are not allowed to do — same reasoning as `work_size`.
    let Some(readout) = workspace
        .metadata_tokens
        .get(crate::quality_streak::STREAK_TOKEN)
        .and_then(crate::quality_streak::parse)
    else {
        return 0.0;
    };
    let half_lives = crate::quality_streak::half_lives(
        workspace
            .metadata_tokens
            .get(crate::quality_streak::HALF_LIFE_TOKEN),
    );
    let value = crate::quality_streak::decayed(readout, half_lives, std::time::SystemTime::now());
    (((value - FLOOR) / (CEILING - FLOOR)) as f32).clamp(0.0, 1.0)
}

fn row_for_entry(
    app: &crate::app::state::AppState,
    entry: &crate::ui::sidebar::WorkspaceListEntry,
    agents: &[crate::ui::AgentPanelEntry],
) -> Option<RowFacts> {
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
            Some(RowFacts {
                row: CardRow::Space(workspace.id.clone()),
                // The same name the sidebar's own row prints, so the sky and the tree name the same
                // project the same way.
                label: solar_system::SceneLabel::new(
                    &workspace.display_name_from_terminals(&app.terminals),
                ),
                hue,
                severity,
                size: work_size(workspace),
                streak: streak_expression(workspace),
            })
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
            // A worker is not a project, so it stays out of the size register entirely and keeps
            // its tier's fixed radius — the same reason the sun is out of it. It is out of the
            // streak register for the same reason: the captain's correction puts the streak
            // expression on second mates, and worker streak stays on the left panel's
            // wire-border glow where it already lives.
            Some(RowFacts {
                row: CardRow::Agent(detail.pane_id),
                // A worker carries no caption: the reference labels its mates and its sun and
                // leaves the workers bare, and at a worker's drawn size the caption would be longer
                // than the body it names.
                label: solar_system::SceneLabel::EMPTY,
                hue,
                severity,
                size: solar_system::BodySize::Fixed,
                streak: 0.0,
            })
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
        match effect.kind {
            crate::app::pending_effects::EffectKind::PaneIssue => {
                if state.asteroids.contains_key(&effect.pane_id)
                    || !identity.contains(&CardRow::Agent(effect.pane_id))
                {
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
            crate::app::pending_effects::EffectKind::PaneSuccess => {
                let already_seen = state
                    .seen_success
                    .get(&effect.pane_id)
                    .is_some_and(|seen| *seen >= effect.spawned_at);
                state.seen_success.insert(effect.pane_id, effect.spawned_at);
                if already_seen || !app.background_comets.enabled {
                    continue;
                }
                if let Some(ws_idx) = app
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.pane_state(effect.pane_id).is_some())
                {
                    spawn_comet(
                        state,
                        ws_idx,
                        now,
                        0.15,
                        solar_system::WinTier::Ask,
                        CometMotion::Pass,
                    );
                }
            }
        }
    }

    for (ws_idx, workspace) in app.workspaces.iter().enumerate() {
        let ws_id = &workspace.id;

        if let Some(outcome) = workspace.metadata_tokens.get("outcome") {
            let is_landing = outcome == "pr_merged" || outcome == "landed";
            let already_seen = state.seen_outcome.get(ws_id).map(String::as_str) == Some(outcome);
            if app.background_comets.enabled && is_landing && !already_seen {
                // A landing has a *place* it landed, so this tier alone flies into it rather than
                // past it — the workspace's own body in the scene.
                spawn_comet(
                    state,
                    ws_idx,
                    now,
                    work_size_magnitude(workspace),
                    solar_system::WinTier::Merge,
                    CometMotion::Arrival {
                        target: CardRow::Space(ws_id.clone()),
                    },
                );
            }
            state
                .seen_outcome
                .insert(ws_id.clone(), outcome.to_string());
        }

        if let Some(counts) = workspace.cached_pull_requests {
            let all_clear = counts.checks_failing == 0 && counts.checks_pending == 0;
            let had_outstanding = state.seen_checks_clear.get(ws_id) == Some(&false);
            if app.background_comets.enabled && all_clear && had_outstanding {
                // The quiet, high-frequency tier: every green pass gets a star, kept small, dim
                // and quick so it never competes with a landing or a streak shower.
                spawn_comet(
                    state,
                    ws_idx,
                    now,
                    0.15,
                    solar_system::WinTier::Ci,
                    CometMotion::Pass,
                );
            }
            state.seen_checks_clear.insert(ws_id.clone(), all_clear);
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
                if app.background_comets.enabled && previous.is_some_and(|prev| band > prev) {
                    // A milestone is an accumulated streak, not one landed thing, so it reads as
                    // a shower of several comets rather than one bigger one — fanned wide, since
                    // six comets all crossing through the middle would converge on the sun.
                    for i in 0..SHOWER_SIZE {
                        let stagger = SHOWER_STAGGER * i as u32;
                        spawn_comet(
                            state,
                            ws_idx,
                            now + stagger,
                            0.9,
                            solar_system::WinTier::Ask,
                            CometMotion::Shower { index: i },
                        );
                    }
                }
                state.seen_streak_band.insert(ws_id.clone(), band);
            }
        }
    }
}

/// The work-size magnitude a landing/merge comet scales by, `0.0..=1.0`.
///
/// This used to be a fixed mid tier, because nothing published a real size and a guess dressed up
/// as data is worse than an admitted default. [`FILES_TOKEN`] is that publisher, so a landing on a
/// big project now arrives as a bigger comet — read through the *same* register the body it lands
/// on is sized by ([`solar_system::BodySize::register_fraction`]) rather than a second scale that
/// could disagree with the picture, and floored for an unmeasured project exactly as the body is.
fn work_size_magnitude(workspace: &crate::workspace::Workspace) -> f32 {
    work_size(workspace).register_fraction()
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
    tier: solar_system::WinTier,
    motion: CometMotion,
) {
    // Varies with how many comets are already in flight this pass, so a shower's own several
    // calls (same `ws_idx`, same rough `started_at`) do not all pick the same path.
    let seed = (ws_idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (state.comets.len() as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let entry_edge = pseudo_angle(seed);
    let start = edge_point(entry_edge);
    let opposite = entry_edge + std::f32::consts::PI;
    // Every tier still enters from an edge — what differs is where it goes, and how long it takes
    // to get there. See [`CometMotion`].
    let (flight, target, exit_angle) = match &motion {
        CometMotion::Pass => (
            COMET_PASS_FLIGHT.mul_f32(tier.life_scale()),
            None,
            opposite
                + (pseudo_angle(seed ^ 7) / std::f32::consts::TAU - 0.5) * 2.0 * CROSS_CHORD_JITTER,
        ),
        CometMotion::Arrival { target } => (
            COMET_PASS_FLIGHT.mul_f32(tier.life_scale()),
            Some(target.clone()),
            opposite,
        ),
        CometMotion::Shower { index } => {
            // Alternating sides, each already deflected clear of the middle, is what turns six
            // chords into a fan rather than six near-copies of the same line through the sun.
            let side = if index % 2 == 0 { 1.0 } else { -1.0 };
            let spread = pseudo_angle(seed ^ 0x5157) / std::f32::consts::TAU;
            (
                COMET_FLIGHT,
                None,
                opposite + side * (SHOWER_MIN_CHORD_DEFLECTION + spread * SHOWER_CHORD_JITTER),
            )
        }
    };
    state.comets.push(ActiveComet {
        started_at,
        flight,
        start,
        // An arrival overrides this with the live body position at draw time; kept as a real
        // crossing endpoint anyway so a target that has since left the tree degrades to the
        // round-1 crossing rather than to a comet flying at nothing.
        end: edge_point(exit_angle),
        target,
        magnitude: magnitude.clamp(0.05, 1.0),
        tier,
        trail: std::sync::Arc::new(Vec::new()),
        last_trail_sample_at: None,
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
    scene: Option<(&solar_system::SceneLayout, f32)>,
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
                    push_ejecta(&mut effects, target, angle, severity, 0.0);
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
                // The rays live on their own much shorter clock inside the same lifecycle, so a
                // strike that happened a minute ago still shows its scar and no dust.
                if elapsed < EJECTA_FADE {
                    let ejecta_age = elapsed.as_secs_f32() / EJECTA_FADE.as_secs_f32();
                    push_ejecta(&mut effects, target, angle_on_surface, severity, ejecta_age);
                }
                true
            }
        }
    });

    // The fainter echo on a struck moon's parent planet is not tracked as a second lifecycle
    // here — `src/solar_system.rs::frame` derives it directly from each crater above via its own
    // `BodyLayout::parent` (which this module, working from a flat `identity` list, does not
    // have), scaled by `RIPPLE_FADE_RATIO` against the same age fraction already computed above.

    state.comets.retain_mut(|comet| {
        let elapsed = now.saturating_duration_since(comet.started_at);
        if elapsed >= comet.flight {
            return false;
        }
        let progress = elapsed.as_secs_f32() / comet.flight.as_secs_f32();
        let target = comet
            .target
            .as_ref()
            .and_then(|row| identity.iter().position(|candidate| candidate == row));
        let end = target
            .and_then(|idx| {
                scene.and_then(|(layout, phase)| layout.body_position_normalized(idx, phase))
            })
            .unwrap_or(comet.end);
        if comet.last_trail_sample_at.is_none_or(|last| now > last) {
            let trail = std::sync::Arc::make_mut(&mut comet.trail);
            trail.push(solar_system::comet_position_normalized(
                comet.tier,
                comet.start,
                end,
                progress,
            ));
            // The longest tier lives for about 196 frames at 60 Hz. This is deliberately a fixed
            // safety ceiling, not a visual tail length; the renderer measures the real recorded
            // path backward until the tier's pixel-length cap is reached.
            if trail.len() > 256 {
                trail.remove(0);
            }
            comet.last_trail_sample_at = Some(now);
        }
        effects.comets.push(solar_system::Comet {
            start: comet.start,
            end: comet.end,
            // Re-resolved every pass against the *current* tree, exactly like an asteroid's
            // target: a landing whose workspace has since gone finishes its flight as a plain
            // crossing rather than vanishing or aiming at whatever now holds that index.
            target,
            magnitude: comet.magnitude,
            tier: comet.tier,
            trail: std::sync::Arc::clone(&comet.trail),
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

/// Queue the burst of rays a strike throws off, on the same body and at the same point on its
/// surface as the crater that strike leaves behind — the two are one event drawn on two clocks.
fn push_ejecta(
    effects: &mut solar_system::SceneEffects,
    target: usize,
    angle: f32,
    severity: Severity,
    age: f32,
) {
    effects.ejecta.push(solar_system::Ejecta {
        body: target,
        angle_on_surface: angle,
        severity,
        age,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace carrying one `files` token value, or none at all.
    fn workspace_with_files(raw: Option<&str>) -> crate::workspace::Workspace {
        let mut workspace = crate::workspace::Workspace::test_new("sized");
        if let Some(raw) = raw {
            workspace.metadata_tokens.patch(
                HashMap::from([(FILES_TOKEN.to_string(), Some(raw.to_string()))]),
                None,
                Instant::now(),
            );
        }
        workspace
    }

    #[test]
    fn a_published_file_count_reaches_the_scene_as_a_measured_size() {
        assert_eq!(
            work_size(&workspace_with_files(Some("2470"))),
            solar_system::BodySize::Files(2470)
        );
        // Publishers write with `printf`/`wc -l` and friends, so a trailing newline is the normal
        // case rather than the exotic one.
        assert_eq!(
            work_size(&workspace_with_files(Some(" 2470\n"))),
            solar_system::BodySize::Files(2470)
        );
    }

    #[test]
    fn an_unpublished_or_unreadable_size_is_unmeasured_never_zero() {
        // The distinction the whole floor exists for: `Files(0)` is a claim about a project,
        // `Unmeasured` is an admission about the fleet, and only one of them is true here.
        for raw in [None, Some(""), Some("unknown"), Some("-3"), Some("12.5")] {
            assert_eq!(
                work_size(&workspace_with_files(raw)),
                solar_system::BodySize::Unmeasured,
                "{raw:?} is not a file count"
            );
        }
        assert_eq!(
            work_size(&workspace_with_files(Some("0"))),
            solar_system::BodySize::Files(0),
            "a real, published zero is still a measurement"
        );
    }

    #[test]
    fn a_landing_on_a_big_project_arrives_as_a_bigger_comet() {
        let big = work_size_magnitude(&workspace_with_files(Some("2470")));
        let tiny = work_size_magnitude(&workspace_with_files(Some("2")));
        let unmeasured = work_size_magnitude(&workspace_with_files(None));

        assert!(big > tiny, "{big} vs {tiny}");
        // The comet reads the same register its target body is drawn by, floor included, so an
        // unmeasured project's landing is a real comet rather than a vanishing one.
        assert!(unmeasured > 0.0 && unmeasured <= tiny);
        assert!(big <= 1.0);
    }

    #[test]
    fn ci_green_transition_is_edge_triggered_once() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces
            .push(crate::workspace::Workspace::test_new("fleet"));
        let mut state = BackgroundEffectsState::default();
        let now = Instant::now();
        let (_, identity) = tree_nodes(&app);
        let green = crate::forge::PullRequestCounts::default();
        let red = crate::forge::PullRequestCounts {
            checks_failing: 1,
            ..Default::default()
        };

        // Discovering an already-green fleet establishes the baseline. It is not a transition,
        // and continuing to observe the same green fact must stay silent forever.
        app.workspaces[0].cached_pull_requests = Some(green);
        spawn_new_effects(&app, &mut state, &identity, now);
        assert_eq!(state.comets.len(), 0);
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(16));
        assert_eq!(state.comets.len(), 0, "steady green re-fired the CI comet");

        // One outstanding reading followed by green is the edge. Exactly one comet is admitted,
        // and later green readings do not turn that one transition into a timer.
        app.workspaces[0].cached_pull_requests = Some(red);
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(32));
        assert_eq!(state.comets.len(), 0);
        app.workspaces[0].cached_pull_requests = Some(green);
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(48));
        assert_eq!(state.comets.len(), 1, "red-to-green did not fire once");
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(64));
        assert_eq!(
            state.comets.len(),
            1,
            "the same red-to-green transition fired more than once"
        );
    }

    #[test]
    fn a_pending_pane_success_emits_one_ask_comet() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces
            .push(crate::workspace::Workspace::test_new("fleet"));
        let pane_id = app.workspaces[0].focused_pane_id().expect("root pane");
        let now = Instant::now();
        app.pending_effects.record_ask(pane_id, now, 1.0);
        let (_, identity) = tree_nodes(&app);
        let mut state = BackgroundEffectsState::default();

        spawn_new_effects(&app, &mut state, &identity, now);
        assert_eq!(state.comets.len(), 1);
        assert_eq!(state.comets[0].tier, solar_system::WinTier::Ask);
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(16));
        assert_eq!(
            state.comets.len(),
            1,
            "one pending success replayed every tick"
        );
    }

    #[test]
    fn measure_fixed_comet_count_at_captain_fleet_shape() {
        let mut app = fleet_app(11);
        let green = crate::forge::PullRequestCounts::default();
        let red = crate::forge::PullRequestCounts {
            checks_failing: 1,
            ..Default::default()
        };
        for workspace in &mut app.workspaces {
            workspace.cached_pull_requests = Some(green);
        }
        let (_, identity) = tree_nodes(&app);
        let mut state = BackgroundEffectsState::default();
        let start = Instant::now();

        for tick in 0..300 {
            let now = start + Duration::from_millis(16 * tick);
            spawn_new_effects(&app, &mut state, &identity, now);
            advance_and_build_effects(&mut state, &identity, None, now);
        }
        let steady_green = state.comets.len();

        for workspace in &mut app.workspaces {
            workspace.cached_pull_requests = Some(red);
        }
        spawn_new_effects(&app, &mut state, &identity, start + Duration::from_secs(5));
        for workspace in &mut app.workspaces {
            workspace.cached_pull_requests = Some(green);
        }
        spawn_new_effects(
            &app,
            &mut state,
            &identity,
            start + Duration::from_secs(5) + Duration::from_millis(16),
        );
        let on_transition = state.comets.len();

        for tick in 1..300 {
            let now = start + Duration::from_secs(5) + Duration::from_millis(16 * (tick + 1));
            spawn_new_effects(&app, &mut state, &identity, now);
            advance_and_build_effects(&mut state, &identity, None, now);
        }
        let settled_green = state.comets.len();
        eprintln!(
            "MEASURE captain fleet (sun + 11 mates, 300 ticks): before=948 live; fixed steady={steady_green}; red-to-green={on_transition}; settled={settled_green}"
        );

        assert_eq!(steady_green, 0);
        assert_eq!(on_transition, 12);
        assert_eq!(settled_green, 0);
    }

    #[test]
    fn comet_switch_suppresses_wins_without_replaying_them_when_reenabled() {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces
            .push(crate::workspace::Workspace::test_new("fleet"));
        app.background_comets.enabled = false;
        let mut state = BackgroundEffectsState::default();
        let (_, identity) = tree_nodes(&app);
        let now = Instant::now();
        app.workspaces[0].cached_pull_requests = Some(crate::forge::PullRequestCounts {
            checks_failing: 1,
            ..Default::default()
        });
        spawn_new_effects(&app, &mut state, &identity, now);
        app.workspaces[0].cached_pull_requests = Some(Default::default());
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(16));
        assert!(state.comets.is_empty());

        app.background_comets.enabled = true;
        spawn_new_effects(&app, &mut state, &identity, now + Duration::from_millis(32));
        assert!(
            state.comets.is_empty(),
            "re-enabling replayed a transition seen while off"
        );
    }

    /// Nest `ws_idx` under `owner`, the way `workspace report-metadata --token owner=...` does —
    /// the one fact that makes a Space a planet in this scene rather than a second sun.
    fn publish(app: &mut crate::app::state::AppState, ws_idx: usize, key: &str, value: &str) {
        app.workspaces[ws_idx].metadata_tokens.patch(
            HashMap::from([(key.to_string(), Some(value.to_string()))]),
            None,
            Instant::now(),
        );
    }

    /// A fleet of `count` Spaces under one root, all measured, so every body has a real rate.
    fn fleet_app(count: usize) -> crate::app::state::AppState {
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        app.workspaces
            .push(crate::workspace::Workspace::test_new("fleet"));
        for i in 0..count {
            app.workspaces
                .push(crate::workspace::Workspace::test_new(&format!("mate{i}")));
            publish(
                &mut app,
                i + 1,
                crate::app::agent_tree::OWNER_TOKEN,
                "fleet",
            );
            publish(
                &mut app,
                i + 1,
                FILES_TOKEN,
                &((i as u32 + 1) * 300).to_string(),
            );
        }
        app
    }

    fn rates(app: &crate::app::state::AppState) -> Vec<(CardRow, f32)> {
        let (nodes, identity) = tree_nodes(app);
        identity
            .into_iter()
            .zip(nodes)
            .map(|(row, node)| (row, node.kind.revolutions_per_loop(node.size)))
            .collect()
    }

    fn advance(tracks: &mut OrbitTracks, app: &crate::app::state::AppState, at: Instant) -> bool {
        let rates = rates(app);
        tracks.advance(rates.iter().map(|(row, rate)| (row, *rate)), at)
    }

    #[test]
    fn every_mote_traces_to_one_event_and_no_mote_traces_to_a_timer() {
        // The accounting the whole tier rests on: no ambient element is emitted by a timer, a
        // loop, or a decorative oscillator. Motes emitted equals events consumed, exactly, at
        // every cadence — so the two are counted separately and compared rather than one being
        // derived from the other.
        let mut motes = AmbientMotes::default();
        let row = CardRow::Agent(crate::layout::PaneId::alloc());

        // A body arriving mid-life starts from where its counter is: a pane already an hour into a
        // build must not stud its whole orbit for work nobody was watching.
        motes.consume([(&row, 500 * BYTES_PER_EVENT)].into_iter());
        assert_eq!(motes.accounting(), (0, 0));
        assert_eq!(motes.motes(&row), 0);

        // Then it does three events' worth of work.
        motes.consume([(&row, 503 * BYTES_PER_EVENT)].into_iter());
        assert_eq!(motes.accounting(), (3, 3));

        // Ticking with nothing new emits nothing — this is the "no timer" half, and it is the one
        // a tier like this gets wrong.
        for _ in 0..50 {
            assert!(!motes.consume([(&row, 503 * BYTES_PER_EVENT)].into_iter()));
        }
        assert_eq!(motes.accounting(), (3, 3));

        // A partial event is carried, not discarded and not rounded up.
        motes.consume([(&row, 503 * BYTES_PER_EVENT + BYTES_PER_EVENT / 2)].into_iter());
        assert_eq!(motes.accounting(), (3, 3));
        motes.consume([(&row, 505 * BYTES_PER_EVENT)].into_iter());
        assert_eq!(
            motes.accounting(),
            (5, 5),
            "the carried half-event was lost"
        );

        // A counter that goes backwards — a pane replaced under the same identity — is a fresh
        // start rather than a huge negative or a wrap.
        motes.consume([(&row, 3 * BYTES_PER_EVENT)].into_iter());
        assert_eq!(motes.accounting(), (5, 5));
        motes.consume([(&row, 4 * BYTES_PER_EVENT)].into_iter());
        assert_eq!(motes.accounting(), (6, 6));
    }

    #[test]
    fn the_attribution_transform_compresses_the_truth_but_never_reorders_it() {
        // The property that makes it honest: it is a monotone function of the real count and of
        // nothing else, so the busiest body is always the brightest.
        //
        // The card's own worked example — a 200x spread with two bodies that would go black.
        let counts = [400u64, 200, 60, 10, 2, 0];
        let shares = mote_shares(&counts);

        // Order preserved, exactly.
        for i in 1..shares.len() {
            assert!(
                shares[i - 1] > shares[i],
                "share {} is not above share {i}: {shares:?}",
                i - 1
            );
        }
        // Compressed: a 200x spread in the counts becomes a few-fold spread in the shares.
        let spread = shares[0] / shares[shares.len() - 1];
        assert!(
            (3.0..6.0).contains(&spread),
            "a 200x count spread became a {spread:.1}x share spread"
        );
        // Every body visible — including the two that did nothing at all, which is what the floor
        // is for.
        assert!(shares.iter().all(|s| *s > 0.0));
        assert!(
            *shares.last().expect("a share") > 0.02,
            "a silent body is invisible: {shares:?}"
        );
        // The shares are a partition of the tier's light, so they sum to one.
        let total: f32 = shares.iter().sum();
        assert!((total - 1.0).abs() < 1e-4, "shares sum to {total}");

        // Monotone in general, not only on the worked example.
        let rising: Vec<u64> = (0..40).map(|i| i * i * 7).collect();
        let rising_shares = mote_shares(&rising);
        for i in 1..rising_shares.len() {
            assert!(rising_shares[i] >= rising_shares[i - 1]);
        }
        // Equal counts get equal shares, so nothing is being tie-broken by position.
        let flat = mote_shares(&[9, 9, 9, 9]);
        assert!(flat.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-6));
        assert!(mote_shares(&[]).is_empty());
    }

    #[test]
    fn a_track_wears_with_the_revolutions_the_body_has_actually_completed() {
        // Density is revolutions completed — a groove means "how much has passed here", so a body
        // that has been round its orbit a thousand times has a deeper one than a body that has
        // been round twice.
        let app = fleet_app(2);
        let mut tracks = OrbitTracks::default();
        let start = Instant::now();
        assert!(!advance(&mut tracks, &app, start), "nothing has passed yet");

        let (_, identity) = tree_nodes(&app);
        let mate = identity
            .iter()
            .find(|row| matches!(row, CardRow::Space(_)) && **row != identity[0])
            .cloned()
            .expect("a second mate");
        assert_eq!(tracks.wear(&mate), 0.0);

        // A minute of orbiting, then an hour.
        advance(&mut tracks, &app, start + Duration::from_secs(60));
        let after_a_minute = tracks.wear(&mate);
        advance(&mut tracks, &app, start + Duration::from_secs(3_600));
        let after_an_hour = tracks.wear(&mate);

        assert!(after_an_hour > after_a_minute, "the track stopped wearing");
        assert!(after_an_hour <= 1.0, "wear ran past full");
        // ...and it saturates rather than running away: a groove that kept deepening forever would
        // end up the loudest thing in the frame.
        advance(&mut tracks, &app, start + Duration::from_secs(86_400 * 7));
        let after_a_week = tracks.wear(&mate);
        advance(&mut tracks, &app, start + Duration::from_secs(86_400 * 30));
        assert_eq!(tracks.wear(&mate), after_a_week);
    }

    #[test]
    fn wear_survives_the_scene_being_rebuilt_and_a_body_joining_or_leaving() {
        // The point of the layer. The ambient loop is baked and rebaked; a groove is wear in the
        // ground, and wear that vanished on every rebuild would be a decoration that happened to
        // look like wear.
        let mut app = fleet_app(3);
        let mut tracks = OrbitTracks::default();
        let start = Instant::now();
        advance(&mut tracks, &app, start);
        advance(&mut tracks, &app, start + Duration::from_secs(3_600));

        let (_, identity) = tree_nodes(&app);
        let survivor = identity[1].clone();
        let leaver = identity[3].clone();
        let before = tracks.revolutions(&survivor);
        assert!(before > 0.0);
        assert!(tracks.revolutions(&leaver) > 0.0);

        // A rebuild of the scene changes nothing at all: this is not read from the layout.
        let (_, identity_again) = tree_nodes(&app);
        assert_eq!(identity_again, identity);
        assert_eq!(tracks.revolutions(&survivor), before);

        // A body leaves. Indices shift and identities do not, which is the whole reason the key is
        // an identity: keyed by index, the survivor would inherit whatever slid into its slot.
        app.workspaces.pop();
        advance(&mut tracks, &app, start + Duration::from_secs(3_601));
        assert!(
            tracks.revolutions(&survivor) >= before,
            "a body leaving took another body's wear with it"
        );
        assert_eq!(
            tracks.revolutions(&leaver),
            0.0,
            "a body that left kept its groove"
        );
        assert_eq!(tracks.tracked(), 3, "the leaver is still being tracked");

        // ...and a body joining starts bare rather than inheriting anyone's past.
        app.workspaces
            .push(crate::workspace::Workspace::test_new("newcomer"));
        publish(&mut app, 3, crate::app::agent_tree::OWNER_TOKEN, "fleet");
        publish(&mut app, 3, FILES_TOKEN, "50");
        advance(&mut tracks, &app, start + Duration::from_secs(3_602));
        let (_, joined) = tree_nodes(&app);
        let newcomer = joined.last().cloned().expect("the new body");
        assert!(
            tracks.revolutions(&newcomer) < before / 100.0,
            "a body that joined inherited a past it did not have"
        );
        assert!(tracks.revolutions(&survivor) >= before);
    }

    #[test]
    fn advancing_only_reports_a_change_when_the_drawn_wear_actually_moves() {
        // The counts advance continuously and the drawn steps do not. Reporting the former would
        // rebake the whole ambient loop on every tick, forever — which is the entire reason the
        // wear is quantized in the first place.
        let app = fleet_app(2);
        let mut tracks = OrbitTracks::default();
        let start = Instant::now();
        advance(&mut tracks, &app, start);

        let mut reported = 0;
        for step in 1..=600u32 {
            if advance(
                &mut tracks,
                &app,
                start + Duration::from_millis(u64::from(step) * 100),
            ) {
                reported += 1;
            }
        }
        // A minute of ticking at 10Hz: the wear has genuinely moved a step or two, and nowhere
        // near six hundred times.
        assert!(
            reported <= solar_system::OrbitWear::STEPS as usize,
            "{reported} rebakes reported in one minute of ticking"
        );
    }

    #[test]
    fn a_published_quality_streak_reaches_the_body_the_scene_draws() {
        // The whole path, end to end: the `streak` token `fm-quality-event.sh` already publishes,
        // through `crate::quality_streak`'s own decay and bands, into a mate's ring and gas swell.
        // Nothing about the body types is a fixture — the fleet's real file counts rank them.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        for name in ["fleet", "cold", "warm", "hot"] {
            app.workspaces
                .push(crate::workspace::Workspace::test_new(name));
        }
        for ws_idx in 1..=3 {
            publish(
                &mut app,
                ws_idx,
                crate::app::agent_tree::OWNER_TOKEN,
                "fleet",
            );
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Published *now*, so read-time decay is nil and the score under test is the score.
        publish(
            &mut app,
            1,
            crate::quality_streak::STREAK_TOKEN,
            &format!("2.0@{now}"),
        );
        publish(
            &mut app,
            2,
            crate::quality_streak::STREAK_TOKEN,
            &format!("23.0@{now}"),
        );
        publish(
            &mut app,
            3,
            crate::quality_streak::STREAK_TOKEN,
            &format!("60.0@{now}"),
        );

        let (nodes, identity) = tree_nodes(&app);
        let streak_of = |name: &str| {
            let id = &app
                .workspaces
                .iter()
                .find(|ws| ws.custom_name.as_deref() == Some(name))
                .expect("workspace")
                .id;
            let idx = identity
                .iter()
                .position(|row| row == &CardRow::Space(id.clone()))
                .expect("the scene draws a body for every Space in the tree");
            nodes[idx].streak
        };

        // An `Ember` streak is documented as "barely alight", so it expresses nothing: a body that
        // showed a streak nobody would call alight would be lying about the fleet.
        assert_eq!(streak_of("cold"), 0.0);
        // A `Steady` one sits partway up, and a score past `Hot` saturates rather than running on.
        assert!(
            (0.1..0.9).contains(&streak_of("warm")),
            "a steady streak expressed {}",
            streak_of("warm")
        );
        assert_eq!(streak_of("hot"), 1.0);

        // A worker is out of the register entirely — the captain's correction puts the streak
        // expression on second mates, and worker streak stays on the sidebar's wire-border glow.
        assert!(nodes
            .iter()
            .all(|node| node.kind != solar_system::BodyKind::Moon || node.streak == 0.0));
    }

    #[test]
    fn a_decayed_streak_expresses_less_than_the_score_it_was_published_at() {
        // The token is durable and carries the instant it was true, so a fleet that stopped
        // winning a fortnight ago must not still be drawing a full ring. Read through the same
        // decay every other surface reads it through, rather than a second copy of the rule.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        for name in ["fleet", "fresh", "stale"] {
            app.workspaces
                .push(crate::workspace::Workspace::test_new(name));
        }
        for ws_idx in 1..=2 {
            publish(
                &mut app,
                ws_idx,
                crate::app::agent_tree::OWNER_TOKEN,
                "fleet",
            );
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let a_fortnight_ago = now.saturating_sub(14 * 86_400);
        publish(
            &mut app,
            1,
            crate::quality_streak::STREAK_TOKEN,
            &format!("30.0@{now}"),
        );
        publish(
            &mut app,
            2,
            crate::quality_streak::STREAK_TOKEN,
            &format!("30.0@{a_fortnight_ago}"),
        );

        let (nodes, _) = tree_nodes(&app);
        let mates: Vec<f32> = nodes
            .iter()
            .filter(|node| node.kind == solar_system::BodyKind::Planet)
            .map(|node| node.streak)
            .collect();
        assert_eq!(mates.len(), 2);
        assert!(
            mates[0] > mates[1],
            "a fortnight-old streak still expresses as much as a fresh one: {mates:?}"
        );
        // Two win half-lives at the default 5 days is well under the floor, so it reads as nothing
        // rather than as a little — which is the honest answer for a fleet that stopped.
        assert_eq!(mates[1], 0.0);
    }

    #[test]
    fn a_second_root_is_a_second_mate_rather_than_a_second_sun() {
        // A second mate working in its own checkout publishes no `owner`, so the fleet tree draws it
        // as a **root** — and every root used to map to the star tier at orbit radius zero, which
        // put two suns on the same pixel and took the second mate out of the size and period
        // registers entirely. There is one sun, and it is the first root the walk reaches.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        for name in ["fleet", "owned", "standalone"] {
            app.workspaces
                .push(crate::workspace::Workspace::test_new(name));
        }
        publish(&mut app, 1, crate::app::agent_tree::OWNER_TOKEN, "fleet");
        publish(&mut app, 1, FILES_TOKEN, "900");
        // ...and nothing at all on the third, which is what makes it a root.
        publish(&mut app, 2, FILES_TOKEN, "400");

        let (nodes, _) = tree_nodes(&app);
        let suns = nodes
            .iter()
            .filter(|node| node.kind == solar_system::BodyKind::Sun)
            .count();
        assert_eq!(suns, 1, "{suns} suns in a fleet that has one first mate");

        let sun = nodes
            .iter()
            .position(|node| node.kind == solar_system::BodyKind::Sun)
            .expect("a sun");
        // The standalone mate is a planet, and it orbits the sun rather than sitting on it — which
        // is also what puts it back on the ladder, in the register, and under a groove.
        let standalone = nodes
            .iter()
            .rposition(|node| node.kind == solar_system::BodyKind::Planet)
            .expect("a second planet");
        assert_eq!(nodes[standalone].parent, Some(sun));
        assert_eq!(nodes[standalone].size, solar_system::BodySize::Files(400));

        // And the picture agrees: no two bodies share the sun's own position.
        let layout = solar_system::build_layout(&nodes, 1_600, 900);
        let at_the_sun = (0..nodes.len())
            .filter(|idx| layout.body_position(*idx, 0.0) == layout.body_position(sun, 0.0))
            .count();
        assert_eq!(
            at_the_sun, 1,
            "{at_the_sun} bodies are drawn on the sun's own centre"
        );
    }

    #[test]
    fn a_spaces_own_name_reaches_its_caption_in_the_sky() {
        // The caption is drawn into the baked scene, so the name has to arrive as a fleet fact on
        // the node rather than being read back out of a renderer. A worker carries none: the
        // reference captions its mates and its sun and leaves the workers bare.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        for name in ["fleet", "no-mistakes"] {
            app.workspaces
                .push(crate::workspace::Workspace::test_new(name));
        }
        publish(&mut app, 1, crate::app::agent_tree::OWNER_TOKEN, "fleet");

        let (nodes, _) = tree_nodes(&app);
        let mate = nodes
            .iter()
            .find(|node| node.kind == solar_system::BodyKind::Planet)
            .expect("a mate");
        assert_eq!(mate.label.as_str(), "no-mistakes");
        let sun = nodes
            .iter()
            .find(|node| node.kind == solar_system::BodyKind::Sun)
            .expect("a sun");
        assert_eq!(sun.label.as_str(), "fleet");
    }

    #[test]
    fn a_published_size_reaches_the_body_the_scene_draws() {
        // The whole path, end to end: a `files` token on a real workspace, through the same fleet
        // tree the sidebar draws, into a radius. Three projects under one root — one tiny, one the
        // size of this checkout, and one nobody has measured.
        let mut app = crate::app::state::AppState::test_new();
        app.workspaces.clear();
        for name in ["fleet", "tiny", "big", "unmeasured"] {
            app.workspaces
                .push(crate::workspace::Workspace::test_new(name));
        }
        for ws_idx in 1..=3 {
            publish(
                &mut app,
                ws_idx,
                crate::app::agent_tree::OWNER_TOKEN,
                "fleet",
            );
        }
        publish(&mut app, 1, FILES_TOKEN, "2");
        publish(&mut app, 2, FILES_TOKEN, "2470");

        let (nodes, identity) = tree_nodes(&app);
        let layout = solar_system::build_layout(&nodes, 1_000, 1_000);
        let radius = |name: &str| {
            let id = &app
                .workspaces
                .iter()
                .find(|ws| ws.custom_name.as_deref() == Some(name))
                .expect("workspace")
                .id;
            let idx = identity
                .iter()
                .position(|row| row == &CardRow::Space(id.clone()))
                .expect("the scene draws a body for every Space in the tree");
            layout.body_radius_px(idx).expect("a body has a radius")
        };

        assert_eq!(
            nodes[0].kind,
            solar_system::BodyKind::Sun,
            "the root Space is the sun, and the three below it are its planets"
        );
        assert!(
            radius("big") > radius("tiny") * 1.5,
            "a 2,470-file project should visibly outdraw a 2-file one: {} vs {}",
            radius("big"),
            radius("tiny")
        );
        // Nobody published a size for this one, and it is still a planet: floored, not collapsed,
        // and not silently treated as a project with no files in it.
        assert_eq!(
            radius("unmeasured"),
            radius("tiny").min(radius("unmeasured"))
        );
        assert!(
            radius("unmeasured") > radius("big") * 0.4,
            "an unmeasured project drew {} against a measured {}",
            radius("unmeasured"),
            radius("big")
        );
    }

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

    /// Shortest distance from `p` to the segment `a`..`b`.
    fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
        let (abx, aby) = (b.0 - a.0, b.1 - a.1);
        let len_sq = abx * abx + aby * aby;
        let t = if len_sq <= f32::EPSILON {
            0.0
        } else {
            (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len_sq).clamp(0.0, 1.0)
        };
        let (cx, cy) = (a.0 + abx * t, a.1 + aby * t);
        ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
    }

    /// The scene's real 1440p target, so this measures the geometry the captain actually sees.
    const SCENE: (f32, f32) = (2560.0, 1440.0);

    fn to_px(point: (f32, f32)) -> (f32, f32) {
        (point.0 * SCENE.0, point.1 * SCENE.1)
    }

    /// F4: a milestone shower must fan out, not converge. Six comets all crossing between opposite
    /// edges all run through the middle of the scene, which is exactly where the sun is — this
    /// measures each comet's real path against the sun's real drawn radius rather than trusting
    /// the jitter to have spread them.
    #[test]
    fn a_shower_spreads_its_comets_clear_of_the_sun() {
        let mut state = BackgroundEffectsState::default();
        let now = Instant::now();
        for index in 0..SHOWER_SIZE {
            spawn_comet(
                &mut state,
                3,
                now,
                0.9,
                solar_system::WinTier::Ask,
                CometMotion::Shower { index },
            );
        }
        assert_eq!(state.comets.len(), SHOWER_SIZE);

        let sun = (SCENE.0 / 2.0, SCENE.1 / 2.0);
        // `BodyKind::Sun::fixed_radius_fraction()` against `min(width, height)`, which is the
        // radius the sun's own disk is drawn at in this scene.
        let sun_radius = 0.050 * SCENE.1;

        let mut sides = (false, false);
        for comet in &state.comets {
            let clearance = distance_to_segment(sun, to_px(comet.start), to_px(comet.end));
            assert!(
                clearance > sun_radius,
                "a shower comet passes within {clearance:.0}px of the sun's {sun_radius:.0}px disk"
            );
            // Which side of the sun it went by — a fan needs both.
            let (ax, ay) = (comet.end.0 - comet.start.0, comet.end.1 - comet.start.1);
            let cross =
                ax * (sun.1 / SCENE.1 - comet.start.1) - ay * (sun.0 / SCENE.0 - comet.start.0);
            if cross > 0.0 {
                sides.0 = true;
            } else {
                sides.1 = true;
            }
        }
        assert!(
            sides.0 && sides.1,
            "every shower comet went past the sun on the same side — that is an arc, not a shower"
        );
    }

    /// Q3: the three triggers are three different kinds of event, so they must not all move the
    /// same way. This asserts the two facts that distinguish them mechanically — where the comet
    /// is headed, and how long it takes to get there.
    #[test]
    fn each_comet_tier_moves_differently() {
        let now = Instant::now();
        let landed_on = CardRow::Space("ws-landed".to_string());

        let mut pass = BackgroundEffectsState::default();
        spawn_comet(
            &mut pass,
            1,
            now,
            0.15,
            solar_system::WinTier::Ask,
            CometMotion::Pass,
        );
        let mut arrival = BackgroundEffectsState::default();
        spawn_comet(
            &mut arrival,
            1,
            now,
            0.6,
            solar_system::WinTier::Merge,
            CometMotion::Arrival {
                target: landed_on.clone(),
            },
        );
        let mut shower = BackgroundEffectsState::default();
        spawn_comet(
            &mut shower,
            1,
            now,
            0.9,
            solar_system::WinTier::Ask,
            CometMotion::Shower { index: 0 },
        );

        // A green pass has no destination and is the quickest of the three.
        assert!(pass.comets[0].target.is_none());
        assert!(pass.comets[0].flight < shower.comets[0].flight);
        assert!(pass.comets[0].flight < arrival.comets[0].flight);

        // A landing flies into the body it landed on, and is the slowest — the arrival is the
        // thing worth watching.
        assert_eq!(arrival.comets[0].target.as_ref(), Some(&landed_on));
        assert!(arrival.comets[0].flight > shower.comets[0].flight);

        // A shower comet crosses like a pass but on a deliberately off-centre chord.
        assert!(shower.comets[0].target.is_none());
        assert_ne!(shower.comets[0].end, pass.comets[0].end);
    }

    /// An arrival's endpoint is a body index re-resolved against the *current* tree every pass,
    /// so a landing whose workspace has since disappeared finishes as a plain crossing rather
    /// than aiming at whatever now happens to hold that index.
    #[test]
    fn an_arrival_re_resolves_its_target_and_degrades_to_a_crossing() {
        let now = Instant::now();
        let target = CardRow::Space("ws-landed".to_string());
        let mut state = BackgroundEffectsState::default();
        spawn_comet(
            &mut state,
            1,
            now,
            0.6,
            solar_system::WinTier::Merge,
            CometMotion::Arrival {
                target: target.clone(),
            },
        );

        let present = [CardRow::Space("other".to_string()), target.clone()];
        let effects = advance_and_build_effects(&mut state.clone(), &present, None, now);
        assert_eq!(effects.comets[0].target, Some(1));

        let gone = [CardRow::Space("other".to_string())];
        let effects = advance_and_build_effects(&mut state.clone(), &gone, None, now);
        assert_eq!(effects.comets[0].target, None);
        // The crossing endpoint it was spawned with is still there to fall back on.
        assert!(effects.comets[0].end.0.is_finite());
    }

    /// Q2: the rays are a flash and the crater they leave is a scar. One impact drives both, but
    /// on clocks far enough apart that the burst is long gone while the mark is still legible.
    #[test]
    fn a_strike_throws_rays_that_are_gone_long_before_its_crater() {
        let pane = PaneId::from_raw(7);
        let identity = [CardRow::Agent(pane)];
        let now = Instant::now();
        let mut state = BackgroundEffectsState::default();
        state.asteroids.insert(
            pane,
            AsteroidLifecycle::Cratering {
                started_at: now,
                severity: Severity::Critical,
                angle_on_surface: 0.9,
            },
        );

        let at_impact = advance_and_build_effects(&mut state.clone(), &identity, None, now);
        assert_eq!(at_impact.craters.len(), 1);
        assert_eq!(at_impact.ejecta.len(), 1);
        assert_eq!(at_impact.ejecta[0].angle_on_surface, 0.9);
        assert_eq!(at_impact.ejecta[0].body, at_impact.craters[0].body);

        // Halfway through the burst: both live, the rays already well into their own fade.
        let mid =
            advance_and_build_effects(&mut state.clone(), &identity, None, now + EJECTA_FADE / 2);
        assert_eq!(mid.ejecta.len(), 1);
        assert!(mid.ejecta[0].age > 0.4 && mid.ejecta[0].age < 0.6);
        assert!(
            mid.craters[0].age < 0.02,
            "the scar has barely begun to fade"
        );

        // A strike from ten seconds ago still shows its scar and no dust at all.
        let later = advance_and_build_effects(
            &mut state.clone(),
            &identity,
            None,
            now + Duration::from_secs(10),
        );
        assert_eq!(later.craters.len(), 1);
        assert!(later.ejecta.is_empty());
    }

    #[test]
    fn pseudo_angle_is_deterministic_and_in_range() {
        let a = pseudo_angle(42);
        let b = pseudo_angle(42);
        assert_eq!(a, b);
        assert!((0.0..std::f32::consts::TAU).contains(&a));
    }
}
