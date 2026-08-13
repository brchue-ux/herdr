//! Pure whole-terminal background-scene frame generator: firstmate as the sun, second mates as
//! planets, workers as moons — mirroring the same owner tree the sidebar already draws, not a
//! separate picture (`data/decisions/2026-08-06-persistent-background-and-shooting-star-design.md`,
//! firstmate home). `(phase, width, height, &SceneLayout, &SceneEffects) -> RGBA8`, no I/O, no
//! terminal, matching this project's "state is separated from runtime / render is pure"
//! principle — see `src/particle_field.rs` for the sibling generator this one deliberately does
//! not share code with (a swarm of thousands of soft dust particles and a dozen shaded spheres
//! are different rendering problems; forcing one model to serve both would have compromised
//! either).
//!
//! Visual style is settled by
//! `data/decisions/2026-08-07-terminal-background-visual-execution-round1.md` (firstmate home):
//! high-fidelity realism, not abstract glowing nodes — each planet/moon is a Lambertian-shaded
//! sphere lit from the sun's own on-screen position (so a body's terminator always points away
//! from the sun, which is free realism this scene's own geometry supplies), with limb darkening
//! and a cheap procedural surface texture. Colour is unchanged from the rest of the fork: hue
//! carries lifecycle stage, intensity carries severity
//! (`data/decisions/2026-08-04-scaling-and-storyboard-answers.md` section 5), resolved through
//! the exact same [`crate::anim::cell::signal_ink`] every sidebar card already uses. This module
//! only chooses the surface those channels render onto. The one exemption is the sun, which
//! holds a fixed warm star colour ([`SUN_STAR_RGB01`]) instead of resolving through that channel
//! — see `data/decisions/2026-08-10-sun-fixed-star-color.md` (firstmate home) for why a star is
//! not severity-coded. Planets and moons are unaffected.
//!
//! An impact is a solid asteroid, not a glowing meteor — no tail, realistic rock-coloured, sized
//! by severity — that leaves a crater on the struck moon fading gradually over
//! [`CRATER_FADE`], with a fainter, shorter-lived echo on the parent planet so trouble reads one
//! level up without drilling in. A win is a comet crossing the whole scene, brightness and tail
//! length scaled by the work-size model
//! (`data/herdr-severity-and-weighting/report.md` "Why 7", firstmate home): weight 1/2/4/7
//! normalised to 0.25/0.50/1.00/1.75. A quality-streak milestone launches several at once — a
//! shower, not one bigger comet — which is what makes a milestone read as a different *kind* of
//! event at a glance rather than just a bigger version of a single landing.
//!
//! This module places bodies and shades pixels only. Deciding *when* an asteroid or comet fires,
//! resolving fleet identity to a body, and caching the generated frame all live in
//! `src/app/background_scene.rs` — mirroring the split between this module and
//! `src/ui/sidebar/particle_background.rs` for the particle field.

use std::f32::consts::PI;

use crate::anim::cell::{signal_ink, Severity};

/// Deep-space canvas colour bodies and glow are composited onto. Not the terminal theme's own
/// backdrop — this scene is its own place, not a wash behind the sidebar's panel colour, so it
/// gets its own fixed surface for [`signal_ink`] to measure severity's lightness distance
/// against.
const SPACE_SURFACE: (u8, u8, u8) = (6, 8, 16);

/// The sun's fixed star colour, as `0.0..=1.0` floats — a warm white with a yellow cast, the
/// colour a G-type star reads as in the realistic space photography this scene is styled after
/// (`data/decisions/2026-08-07-terminal-background-visual-execution-round1.md`, firstmate home).
///
/// Deliberately *not* [`severity_rgb01`]: the captain's ruling in
/// `data/decisions/2026-08-10-sun-fixed-star-color.md` is that a star does not change colour to
/// match whatever a planet near it is doing, so tying the sun to the shared hue=stage channel
/// made an idle fleet render a green sun beside an identically green planet. This is a sun-only
/// exemption — planets and moons keep hue=stage/intensity=severity exactly as before.
///
/// Kept slightly under pure white so [`shade_surface`]'s self-luminous limb brightening (up to
/// `1.05`) still has headroom to lift the core rather than clipping the whole disk flat.
const SUN_STAR_RGB01: (f32, f32, f32) = (1.0, 0.945, 0.835);

/// Cap on how many row bands a frame's background pass splits across.
///
/// Mirrors `src/particle_field.rs`'s `FIELD_MAX_THREADS` and the same reasoning: this process
/// hosts a fleet of agent panes, so generation must not be free to take the whole machine.
const FIELD_MAX_THREADS: usize = 6;

fn field_threads(rows: usize) -> usize {
    if rows < 2 {
        return 1;
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    rows.min(FIELD_MAX_THREADS).min((cores / 2).max(1))
}

/// Which kind of body this node draws as. Carries no colour or size of its own — those are
/// resolved per-node from lifecycle/severity and [`Self::radius_fraction`] — because two bodies
/// of the same kind can be in wildly different states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BodyKind {
    Sun,
    Planet,
    Moon,
}

/// The top of the size register, in tracked files: a project at or above this draws at its tier's
/// maximum radius, and the register saturates rather than running away.
///
/// `5,000` is the fleet orrery's own stated top — 2.0x the largest checkout measured on the box
/// this scene was designed against (herdr itself, 2,470 files).
const FILES_CEIL: u32 = 5_000;

/// The baseline mass, in files, every project is floored at before its own files are added.
///
/// Ported verbatim from the orrery's solved constant rather than re-derived, so herdr and the
/// artifact place the same project at the same point of the same register. It is what makes the
/// band 2.38x wide from floor to ceiling — `((FILE_FLOOR + FILES_CEIL) / FILE_FLOOR).cbrt()` — and
/// it is why an unmeasured project is still a body: a project the fleet has not measured is not a
/// project with no files, and the floor is what absorbs the difference.
const FILE_FLOOR: f32 = 398.42;

/// How big a body is in the project-size register — the quantity that decides where inside its
/// tier's band it draws.
///
/// The register is *tracked files at HEAD*, the same measure the fleet orrery's bridge publishes
/// (`git ls-tree -r HEAD --name-only`, counted), so a project reads the same size in both places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum BodySize {
    /// Outside the register, because this body is not a project and so has no file count to be
    /// compared with: the sun (the fleet's router *to* the projects, not one of them — the
    /// captain's ruling in `data/decisions/2026-08-12-project-size-driven-sizing.md`, firstmate
    /// home) and every worker body (a pane is not a checkout). Draws at its tier's fixed radius,
    /// exactly as it did before the register existed.
    #[default]
    Fixed,
    /// A project whose size the fleet has not published — no token yet, no HEAD to count, or a
    /// value that did not parse. Draws at the register's floor, **not** at zero: see
    /// [`FILE_FLOOR`].
    Unmeasured,
    /// A project measured at this many tracked files at HEAD.
    Files(u32),
}

impl BodySize {
    /// Where this size sits in the register, as a fraction of the register's top —
    /// `0.4195..=1.0`, never zero.
    ///
    /// Volume ~ mass, so the cube root is what made mass legible as size in the first place; that
    /// is the part deliberately carried over unchanged. [`BodySize::Unmeasured`] and
    /// [`BodySize::Fixed`] both contribute no files of their own and so land exactly on the floor,
    /// which is the whole point of having one.
    pub(crate) fn register_fraction(self) -> f32 {
        let files = match self {
            Self::Files(files) => files.min(FILES_CEIL) as f32,
            Self::Unmeasured | Self::Fixed => 0.0,
        };
        ((FILE_FLOOR + files) / (FILE_FLOOR + FILES_CEIL as f32)).cbrt()
    }
}

/// The sun's radius, as a fraction of `min(width, height)`. Locked: the sun is out of the
/// project-size register entirely, so this is the one radius in the scene that is a constant
/// rather than a solved bound.
const SUN_RADIUS_FRACTION: f32 = 0.050;

/// The largest a second mate ever draws — half the sun's, so no planet rivals the sun whatever it
/// grows to. F16's first clause. (The orrery carries a perspective factor into that bound; herdr
/// draws no perspective, so half is half.)
const MATE_RADIUS_CEIL: f32 = SUN_RADIUS_FRACTION / 2.0;

/// How far under F16's exact `r_min / 2` the moon ceiling is placed.
///
/// Sitting *exactly* on the bound makes the largest moon and the smallest planet an exact 2:1
/// tie, which survives neither float rounding at an arbitrary scene size nor the round-to-whole-
/// pixels every body goes through before anyone sees it. A hair inside keeps the ordering the
/// rule is about legible on screen and keeps the rule itself provable in one direction.
const MOON_HEADROOM: f32 = 0.98;

/// A worker moon's radius as a share of the moon tier's ceiling.
///
/// Carried over from the pre-register pair (`0.009` against `0.01125`): a pane is not a checkout,
/// so a worker draws at a fixed radius, and it draws a little under a maxed-out nested project
/// rather than at the same size as one. Re-solving the ceiling moves the worker with it, which is
/// what keeps that relationship a proportion rather than a coincidence of two constants.
const WORKER_MOON_OF_CEILING: f32 = 0.8;

/// The *smallest* a second mate ever draws: the planet tier's ceiling taken at the project-size
/// register's own floor, which is where an unmeasured or empty checkout lands ([`FILE_FLOOR`]).
///
/// This is the quantity F16's moon clause is stated against — `r_min`, not the planet ceiling —
/// and it is why the moon bound cannot be written as a flat constant: it is a function of the
/// register's floor, so any change to [`FILE_FLOOR`] or [`FILES_CEIL`] moves it.
fn mate_radius_floor() -> f32 {
    MATE_RADIUS_CEIL * BodySize::Unmeasured.register_fraction()
}

/// F16's moon clause, solved rather than chosen: *"no second mate's drawn radius ... falls below
/// TWICE the largest worker moon's AT EQUAL DEPTH"*.
///
/// Both tiers carry the same depth law in [`build_layout`] (a planet is always at tree depth 1 and
/// so never nested at all; a moon at depth 2 has the same nesting factor of `1.0`, and deeper
/// moons only ever shrink), so at equal depth the factor cancels exactly as the commitment says it
/// does and the ratio reduces to `mate_radius_floor() / MOON_R_MAX`. Requiring that ratio to be at
/// least 2 is one division.
///
/// The previous flat `0.01125` was picked to hold a 0.45 moon:planet proportion against the
/// planet *ceiling*, which is the wrong end of the band: measured against the floor a maxed-out
/// nested project drew at `1.07x` the smallest planet — a moon visibly outdrawing a planet, the
/// exact thing F16 exists to forbid. Deriving the ceiling from the floor makes the violation
/// unreachable instead of merely fixed once.
fn moon_radius_ceil() -> f32 {
    mate_radius_floor() * 0.5 * MOON_HEADROOM
}

impl BodyKind {
    /// The largest this tier ever draws, as a fraction of `min(width, height)` — the top of its
    /// band, reached by a project at [`FILES_CEIL`] files or more.
    fn max_radius_fraction(self) -> f32 {
        match self {
            Self::Sun => SUN_RADIUS_FRACTION,
            Self::Planet => MATE_RADIUS_CEIL,
            Self::Moon => moon_radius_ceil(),
        }
    }

    /// Body pixel radius for a body outside the register, as a fraction of `min(width, height)` —
    /// the sun, and every worker body.
    fn fixed_radius_fraction(self) -> f32 {
        match self {
            Self::Sun => SUN_RADIUS_FRACTION,
            Self::Planet => 0.020,
            Self::Moon => moon_radius_ceil() * WORKER_MOON_OF_CEILING,
        }
    }

    /// Body pixel radius, as a fraction of `min(width, height)`.
    ///
    /// A project is placed inside its tier's band by `size`; everything else draws at
    /// [`Self::fixed_radius_fraction`]. The planet tier is unmoved by the register's arrival: a
    /// 2,470-file project (the largest real checkout this was measured against) draws at `0.0202`,
    /// within a percent of the flat `0.020` every planet used to get. The moon tier is *not*
    /// unmoved, and deliberately — see [`moon_radius_ceil`] for the bound it now answers to.
    fn radius_fraction(self, size: BodySize) -> f32 {
        match (self, size) {
            // The sun is locked out of the register by decision, whatever a caller hands it: it
            // routes to projects rather than being one, so there is nothing to compare it against.
            (Self::Sun, _) | (_, BodySize::Fixed) => self.fixed_radius_fraction(),
            (_, BodySize::Unmeasured | BodySize::Files(_)) => {
                self.max_radius_fraction() * size.register_fraction()
            }
        }
    }

    /// How far this body orbits from its parent, as a fraction of `min(width, height)`. Unused
    /// for [`Self::Sun`], which never orbits anything.
    fn orbit_radius_fraction(self) -> f32 {
        match self {
            Self::Sun => 0.0,
            Self::Planet => 0.34,
            Self::Moon => 0.055,
        }
    }

    /// Full orbits completed per animation loop by a body **outside** the size register — the sun,
    /// and every worker. Unchanged from before period read mass, for the same reason
    /// [`Self::fixed_radius_fraction`] is: a pane is not a checkout, so there is no mass to read.
    ///
    /// Every body lands back on its starting angle after [`FRAME_COUNT`] samples — the same
    /// seamless-loop contract `src/particle_field.rs::loop_frames` already relies on — which is
    /// why every value here and in [`Self::revolution_band`] is a whole number.
    fn fixed_revolutions_per_loop(self) -> f32 {
        match self {
            Self::Sun => 0.0,
            Self::Planet => 1.0,
            Self::Moon => 4.0,
        }
    }

    /// The slowest and fastest whole number of revolutions per loop this tier draws: the heaviest
    /// project in the tier at the slow end, the lightest at the fast end.
    ///
    /// Whole numbers because of the loop contract, and a *narrow* band of them because of
    /// [`FRAME_COUNT`]: a body doing `R` revolutions per loop is sampled `36/R` times per
    /// revolution, so the fast end of each band is set by where the orbit stops reading as a
    /// circle and starts reading as a polygon, not by how much spread would be nice to have.
    /// A worker's own 4 sits inside the moon band, so nothing about the composition moves.
    fn revolution_band(self) -> (f32, f32) {
        match self {
            Self::Sun => (0.0, 0.0),
            Self::Planet => (1.0, 3.0),
            Self::Moon => (3.0, 5.0),
        }
    }

    /// Full orbits completed per animation loop, for a body of this kind at this size.
    ///
    /// **Period is a consequence of mass, not a cadence.** The fleet orrery states the law as
    /// `T = k · a^1.5 · m^0.5` — Kepler's third, with a mass term — so rate goes as
    /// `(a · m^0.5)^-1` and the heaviest body in a tier is its *slowest*. That is the opposite of
    /// "big things move fast", and it is the whole reading: a mate that has grown into the biggest
    /// checkout in the fleet comes round with a weight the small ones do not have.
    ///
    /// `m` is the register's own mass, and [`BodySize::register_fraction`] is its cube root, so
    /// `m^0.5` is `register_fraction^1.5` and the law reduces to `rate ∝ (a · f^1.5)^-1`.
    ///
    /// **`a` cancels inside a tier, and that is a fact about herdr's ladder rather than a
    /// shortcut.** The orrery spreads its mates over a ladder of orbital distances, so its `a`
    /// term does real work per body. herdr draws one ring per tier — every second mate at
    /// [`Self::orbit_radius_fraction`], every worker at its own — so within a tier `a` is a
    /// constant and divides straight out of the normalisation below. The `a^1.5` separation has
    /// not been dropped: it is exactly what [`Self::revolution_band`]'s two bands *are*, which is
    /// also the artifact's own conclusion — *"distance from the sun stays what it has always been:
    /// the ladder's fixed spacing, which is the field's composition. Mass is read off radius and
    /// period."*
    fn revolutions_per_loop(self, size: BodySize) -> f32 {
        let (slowest, fastest) = self.revolution_band();
        match (self, size) {
            // The sun does not orbit, and a body with no mass to read keeps its tier's own rate.
            (Self::Sun, _) | (_, BodySize::Fixed) => self.fixed_revolutions_per_loop(),
            (_, BodySize::Unmeasured | BodySize::Files(_)) => {
                // `f^-1.5`, normalised so the register's ceiling lands on the slow end of the band
                // and its floor on the fast end. Normalised rather than scaled, so the law's own
                // curvature survives the mapping — the band is not a linear restatement of `f`.
                let rate = |fraction: f32| fraction.max(1e-4).powf(-1.5);
                let heaviest = rate(1.0);
                let lightest = rate(BodySize::Unmeasured.register_fraction());
                let span = (lightest - heaviest).max(1e-6);
                let t = clamp01((rate(size.register_fraction()) - heaviest) / span);
                mix(slowest, fastest, t).round()
            }
        }
    }
}

/// Which surface a body carries.
///
/// The captain's binding correction, in the fleet orrery's own words: *"Second mates are the gas
/// giants and the ringed planets. Body type is theirs and is binding."* A worker is a moon and the
/// firstmate is a star, so neither has one — [`Self::Plain`] is not a third kind of planet, it is
/// the absence of the question.
///
/// Derived per layout build and never stored on a [`TreeNode`], because the rule is a *ranking*:
/// see [`assign_body_types`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BodyType {
    /// The sun, and every moon.
    Plain,
    /// A second mate carrying few, long workstreams: no ring, deep cloud banding, and a streak
    /// that swells the gas rather than brightening anything.
    Gas,
    /// A second mate carrying many concurrent workers: the ring is the traffic of its own moons,
    /// and a streak brightens and widens it.
    Ringed,
}

/// How many bodies in three are gas giants: two.
///
/// A54's rule, ported with its reasoning intact — a mate is a gas giant *unless* its rank is
/// `2 mod 3`. That is the captain's "even distribution" read as even **spacing** rather than as
/// 50/50, because the same sentence asked for more gas planets and a 50/50 split adds none.
const RINGED_RANK_MODULUS: usize = 3;
const RINGED_RANK_REMAINDER: usize = 2;

/// How many second mates the ring seats.
///
/// A41(a): *"orbit slots are composition — they are the spacing the whole field is built on — so
/// the capacity is a card number and not a code accident."* Eight is the fleet orrery's own
/// `ORBIT_LADDER` length, and it is a decision rather than a limit found by measurement.
pub(crate) const ORBIT_LADDER_SLOTS: usize = 8;

/// Which second mates the ring seats when the roster exceeds [`ORBIT_LADDER_SLOTS`], and how many
/// it could not.
///
/// A42: the mates that orbit are the ones with the **largest project size** — files at HEAD, the
/// same register the radius band already draws mass from. Not roster order, not arrival, not
/// event volume.
///
/// The key is [`BodySize::register_fraction`] rather than the raw file count, and that is A42(b)
/// rather than convenience: an unmeasured project is not a zero-file project, the floor is what
/// absorbs it, and ranking it on the raw field would put it *below* zero where it could never win
/// a slot at all. It ranks where it is **drawn** — at the floor — so the selection and the picture
/// agree about how big it is. Ties keep the roster's own order (A42(c)), so two identical
/// snapshots can never seat different bodies.
///
/// A dropped mate takes its own workers with it: a worker orbits its mate, and a mate that is not
/// in the picture has nothing for its workers to orbit.
///
/// Returns one flag per node — whether it is drawn — alongside the seated and overflow counts.
fn seat_the_ladder(nodes: &[TreeNode]) -> (Vec<bool>, usize, usize) {
    let mut mates: Vec<(usize, f32)> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.kind == BodyKind::Planet)
        .map(|(idx, node)| (idx, node.size.register_fraction()))
        .collect();
    let total = mates.len();
    mates.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut seated = vec![true; nodes.len()];
    for (idx, _) in mates.into_iter().skip(ORBIT_LADDER_SLOTS) {
        seated[idx] = false;
    }
    // A dropped mate's subtree goes with it. Nodes arrive parent-before-child, and the same
    // defensive fixed-point pass the depth walk uses keeps this right if that ever stops holding.
    for _ in 0..nodes.len() {
        let mut changed = false;
        for (idx, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                if parent < nodes.len() && parent != idx && !seated[parent] && seated[idx] {
                    seated[idx] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let shown = total.min(ORBIT_LADDER_SLOTS);
    (seated, shown, total.saturating_sub(ORBIT_LADDER_SLOTS))
}

/// Assign a [`BodyType`] to every node, by rank in the project-size register.
///
/// Ranked by [`BodySize::register_fraction`] — the same key the size band already uses, so a mate
/// with no published size ranks where it is *drawn* (at the floor) rather than below zero — with
/// roster order breaking ties, which keeps the assignment stable for a fleet of equal-sized
/// projects instead of shuffling on every rebuild.
///
/// **Recomputed, never stored** (A54(b)): a rule that is even only for the fleet that happened to
/// exist at bake time is not a rule, and this fleet's roster changes under it. [`build_layout`] is
/// that recompute — it already runs on every topology change, which is exactly the event that can
/// move a rank.
fn assign_body_types(nodes: &[TreeNode], seated: &[bool]) -> Vec<BodyType> {
    let mut ranked: Vec<(usize, f32)> = nodes
        .iter()
        .enumerate()
        .filter(|(idx, node)| {
            // Only what is on the ring gets a type: an unseated mate is not in the picture, and
            // letting it hold a rank would push a ring off a mate that *is*.
            node.kind == BodyKind::Planet && seated.get(*idx).copied().unwrap_or(true)
        })
        .map(|(idx, node)| (idx, node.size.register_fraction()))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut types = vec![BodyType::Plain; nodes.len()];
    for (rank, (idx, _)) in ranked.into_iter().enumerate() {
        types[idx] = if rank % RINGED_RANK_MODULUS == RINGED_RANK_REMAINDER {
            BodyType::Ringed
        } else {
            BodyType::Gas
        };
    }
    types
}

/// One node of the fleet's owner tree, exactly as `src/app/background_scene.rs` derived it from
/// `crate::ui::sidebar::workspace_list_entries_whole_fleet` — this module knows nothing about
/// panes, workspaces or tokens, only shape and already-resolved colour, size and streak facts.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TreeNode {
    /// Index into the same slice, or `None` for a root (the sun tier).
    pub(crate) parent: Option<usize>,
    pub(crate) kind: BodyKind,
    /// Lifecycle-stage hue in degrees, from `crate::app::lifecycle::stage(...).hue(...)`.
    pub(crate) hue: f32,
    pub(crate) severity: Severity,
    /// Where this body sits in the project-size register — [`BodySize::Fixed`] for anything that
    /// is not a project.
    pub(crate) size: BodySize,
    /// How hot this mate's quality streak is reading, `0.0..=1.0`, already resolved against
    /// `crate::quality_streak`'s own published bands by `src/app/background_scene.rs` — this
    /// module never sees a raw score or a clock.
    ///
    /// `0.0` for anything with no streak to read, which is every worker and the sun.
    pub(crate) streak: f32,
}

/// One body's static placement facts, resolved once per topology change (mirrors
/// `App::observe_sidebar_particle_field`'s "regenerate on resize, not per tick" cadence — a body
/// added or removed is the equivalent event here).
#[derive(Debug, Clone, Copy)]
struct BodyLayout {
    parent: Option<usize>,
    kind: BodyKind,
    body_type: BodyType,
    hue: f32,
    severity: Severity,
    /// This mate's already-resolved streak expression, `0.0..=1.0` — see [`TreeNode::streak`].
    streak: f32,
    /// Whether this body is drawn at all. `false` for a second mate the ring had no slot for, and
    /// for everything under it — see [`seat_the_ladder`].
    seated: bool,
    /// Whole orbits this body completes per animation loop, resolved from its own mass — see
    /// [`BodyKind::revolutions_per_loop`]. Held here rather than re-derived per frame because
    /// [`SceneLayout::position`] is called per body per frame and recursively up every parent
    /// chain, and a `powf` per call on that path is work with no reason to be there.
    revolutions_per_loop: f32,
    /// Angle this body sits at within its parent's ring at `phase == 0`.
    base_angle: f32,
    orbit_radius_px: f32,
    body_radius_px: f32,
}

/// The streak expression's full range, in the artifact's own units: `visStreakCapped` runs
/// `0..=11`, and every rate below is quoted per unit of it. herdr resolves its own streak to a
/// `0.0..=1.0` fraction instead (its register is a decayed score in named bands, not a count of
/// consecutive wins), so each ported rate is multiplied through by this once, here, rather than
/// eleven constants being silently rescaled one at a time.
const STREAK_UNITS: f32 = 11.0;

/// Where a ring begins, as a multiple of its planet's own radius. Ports `ringGeom`'s `inner`.
const RING_INNER: f32 = 1.40;

/// Where a ring ends at no streak at all, and how far a full streak pushes that out. Ports
/// `ringGeom`'s `outer: 1.78 + visStreakCapped * 0.070` — *"streak brightens and thickens the
/// ring"*, and thickening it is what makes a sustained streak read from across the room.
const RING_OUTER: f32 = 1.78;
const RING_OUTER_PER_STREAK: f32 = 0.070 * STREAK_UNITS;

/// How flat the ring's ellipse is drawn. The orrery's `RING_SQUASH` — the ring plane is tilted
/// about the horizontal axis by `acos(0.34)`, roughly 70 degrees, which is what makes the ring
/// read as a plane the body sits *in* rather than a halo drawn round it.
const RING_SQUASH: f32 = 0.34;

/// Ice and dust, straight off the orrery's own palette. Deliberately not severity-coded: a ring is
/// material, and the body's own colour already carries what state the mate is in — the same
/// exemption, and for the same reason, as [`SUN_STAR_RGB01`].
const RING_RGB01: (f32, f32, f32) = (212.0 / 255.0, 198.0 / 255.0, 170.0 / 255.0);

/// A ring's brightness at no streak, and how far a full streak lifts it. Ports `drawRing`'s
/// `bright = 0.30 + visStreakCapped * 0.055`.
const RING_BRIGHT: f32 = 0.30;
const RING_BRIGHT_PER_STREAK: f32 = 0.055 * STREAK_UNITS;

/// How far a full streak swells a gas giant, as a multiple of its register radius. Ports the
/// artifact's `swell = 1 + visStreakCapped * 0.0295`.
///
/// F16 is explicit that this swell is *inside* the size bound rather than outside it: it is
/// clamped, and the clamp is the stated price. [`build_layout`] applies that clamp, so a gas giant
/// on a long streak grows toward the planet ceiling and stops there rather than through it.
const GAS_SWELL_PER_STREAK: f32 = 0.0295 * STREAK_UNITS;

/// How many latitudinal cloud bands each planet type carries. The orrery's `m.type==='gas'?7:5` —
/// a ringed planet is banded too, just less deeply, which is most of what separates a second mate
/// from a worker moon at a glance.
const GAS_BAND_COUNT: f32 = 7.0;
const RINGED_BAND_COUNT: f32 = 5.0;

/// How much of the rocky surface mottling a gas giant keeps. The orrery's roughness pair
/// (`0.42` for gas against `0.72` for everything else) as a ratio, because this renderer's mottle
/// amplitude is already solved and only its *depth* differs by body type: cloud decks are smoother
/// than rock, and a gas giant that mottles like a moon reads as a big moon.
const GAS_MOTTLE_SCALE: f32 = 0.42 / 0.72;

impl BodyType {
    /// This type's band count, or `None` for a body that carries no bands at all.
    fn band_count(self) -> Option<f32> {
        match self {
            Self::Plain => None,
            Self::Gas => Some(GAS_BAND_COUNT),
            Self::Ringed => Some(RINGED_BAND_COUNT),
        }
    }

    fn mottle_scale(self) -> f32 {
        match self {
            Self::Gas => GAS_MOTTLE_SCALE,
            Self::Plain | Self::Ringed => 1.0,
        }
    }
}

impl BodyLayout {
    /// This body's ring, inner and outer radius in pixels, or `None` if it does not carry one.
    fn ring_radii_px(&self) -> Option<(f32, f32)> {
        if self.body_type != BodyType::Ringed {
            return None;
        }
        Some((
            self.body_radius_px * RING_INNER,
            self.body_radius_px * (RING_OUTER + RING_OUTER_PER_STREAK * clamp01(self.streak)),
        ))
    }
}

/// Every body's static placement for one frame size, ready to be evaluated at any phase.
#[derive(Debug, Clone)]
pub(crate) struct SceneLayout {
    bodies: Vec<BodyLayout>,
    width: u32,
    height: u32,
    /// Second mates the ring seated, and second mates it had no slot for. Counted rather than
    /// silently dropped: an instrument that quietly shows fewer bodies than the fleet has is lying
    /// by omission (A41(b)). Surfaced in the scene by [`draw_overflow_mark`] and reported through
    /// the session API by `App::observe_background_scene`.
    mates_seated: usize,
    mates_beyond_ladder: usize,
}

/// Place every node of `nodes` into a scene sized `width` x `height`.
///
/// Siblings under the same parent are spread evenly around that parent's ring, offset by the
/// parent's own base angle so a whole subtree does not start all bodies pointing the same
/// direction. Orbit radius and body radius both shrink per extra depth past moon-tier (a
/// worker's own delegated worker, say), so a deeper chain stays inside the canvas instead of
/// spiralling off it.
pub(crate) fn build_layout(nodes: &[TreeNode], width: u32, height: u32) -> SceneLayout {
    let scale = width.min(height) as f32;

    // Both rules are recomputed here rather than carried on a node, because this is the recompute:
    // `build_layout` runs on exactly the topology changes that can move a rank or a seat.
    let (seated, mates_seated, mates_beyond_ladder) = seat_the_ladder(nodes);
    let types = assign_body_types(nodes, &seated);

    // Siblings, counting only what is drawn. A42(d): *"the slot is composition"* — the selected
    // set is seated into the ladder's own slots, so eight mates chosen out of seventeen are spread
    // evenly around the ring rather than left holding eight of seventeen positions with nine gaps
    // where the dropped ones used to be. Spreading over the roster instead makes the ring visibly
    // lopsided, which is the ladder's spacing being spent on a fact the picture is not showing.
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (idx, node) in nodes.iter().enumerate() {
        if !seated[idx] {
            continue;
        }
        if let Some(parent) = node.parent {
            if parent < nodes.len() && parent != idx {
                children[parent].push(idx);
            }
        }
    }

    let mut depth: Vec<u32> = vec![0; nodes.len()];
    // Nodes are expected in parent-before-child order (the caller derives them from a
    // depth-first tree walk); a defensive fixed-point pass keeps this correct even if that
    // ever stops holding, at the cost of nothing on the common path.
    for _ in 0..nodes.len() {
        let mut changed = false;
        for (idx, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                if parent < nodes.len() && parent != idx {
                    let next = depth[parent] + 1;
                    if next > depth[idx] {
                        depth[idx] = next;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut bodies: Vec<BodyLayout> = Vec::with_capacity(nodes.len());
    for (idx, node) in nodes.iter().enumerate() {
        let siblings = node
            .parent
            .map(|parent| children[parent].as_slice())
            .unwrap_or(&[]);
        let sibling_index = siblings.iter().position(|&s| s == idx).unwrap_or(0);
        let sibling_count = siblings.len().max(1);
        let parent_angle = node
            .parent
            .and_then(|parent| bodies.get(parent))
            .map(|body| body.base_angle)
            .unwrap_or(0.0);
        let base_angle = parent_angle + (sibling_index as f32 / sibling_count as f32) * 2.0 * PI;

        // Depth past moon-tier (depth 2) shrinks both radii geometrically so a worker's own
        // delegated worker nests visibly inside its parent's ring instead of overshooting it.
        let extra_depth = depth[idx].saturating_sub(2);
        let nesting = 0.62f32.powi(extra_depth as i32);

        let body_type = types.get(idx).copied().unwrap_or(BodyType::Plain);
        let streak = clamp01(node.streak);

        // A gas giant on a streak swells. F16 puts that swell *inside* the size bound rather than
        // outside it, so it is clamped at the planet ceiling — a mate that has been winning for a
        // month grows toward the ceiling and stops, it does not grow into the sun's half of the
        // frame. The clamp is the stated price of the mechanism, not an oversight in it.
        let swell = if body_type == BodyType::Gas {
            1.0 + GAS_SWELL_PER_STREAK * streak
        } else {
            1.0
        };
        let radius_fraction =
            (node.kind.radius_fraction(node.size) * swell).min(node.kind.max_radius_fraction());

        bodies.push(BodyLayout {
            parent: node.parent,
            kind: node.kind,
            body_type,
            hue: node.hue,
            severity: node.severity,
            streak,
            seated: seated.get(idx).copied().unwrap_or(true),
            revolutions_per_loop: node.kind.revolutions_per_loop(node.size),
            base_angle,
            orbit_radius_px: node.kind.orbit_radius_fraction() * scale * nesting,
            body_radius_px: radius_fraction * scale * nesting.max(0.35),
        });
    }

    SceneLayout {
        bodies,
        width,
        height,
        mates_seated,
        mates_beyond_ladder,
    }
}

impl SceneLayout {
    /// How many bodies this scene draws.
    ///
    /// For a caller building a [`SceneEffects`] over it: every effect names the
    /// body it lands on by index, so one that does not know how many there are
    /// can only guess. `herdr bench combined` is the caller — everything in
    /// production derives its effects from the fleet the layout was built from
    /// and already knows.
    pub(crate) fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// Second mates the ring seated, and second mates it had no slot for — the pair
    /// `App::observe_background_scene` reports through the session API so the overflow can be read
    /// as a number rather than counted off the picture.
    pub(crate) fn ladder_occupancy(&self) -> (usize, usize) {
        (self.mates_seated, self.mates_beyond_ladder)
    }

    /// Whether this body is drawn at all. An effect that lands on a body the ring had no slot for
    /// has nothing to land on, so every effect path checks this before drawing.
    fn is_seated(&self, idx: usize) -> bool {
        self.bodies.get(idx).is_some_and(|body| body.seated)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bodies.is_empty()
    }

    /// The radius one body is drawn at, for a caller checking that a fleet fact reached the
    /// picture. `src/app/background_scene.rs` owns the whole path from a workspace's tokens to
    /// these nodes, so its tests are the ones that can prove a published size ends up as a size.
    #[cfg(test)]
    pub(crate) fn body_radius_px(&self, idx: usize) -> Option<f32> {
        self.bodies.get(idx).map(|body| body.body_radius_px)
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// This body's on-screen centre at `phase` (`0.0..=2*PI` covering one full animation loop).
    fn position(&self, idx: usize, phase: f32) -> (f32, f32) {
        let center = (self.width as f32 / 2.0, self.height as f32 / 2.0);
        let Some(body) = self.bodies.get(idx) else {
            return center;
        };
        let parent_pos = body
            .parent
            .map(|parent| self.position(parent, phase))
            .unwrap_or(center);
        if body.orbit_radius_px <= 0.0 {
            return parent_pos;
        }
        let angle = body.base_angle + phase * body.revolutions_per_loop;
        (
            parent_pos.0 + body.orbit_radius_px * angle.cos(),
            parent_pos.1 + body.orbit_radius_px * angle.sin(),
        )
    }
}

/// A struck moon's fading crater, and the fainter echo on its parent planet.
///
/// `age` is `0.0` the instant the asteroid lands and `1.0` once the crater has fully faded — a
/// caller-computed fraction of [`CRATER_FADE`]/[`RIPPLE_FADE`] rather than a raw duration, so
/// this module never reads a clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Crater {
    pub(crate) body: usize,
    pub(crate) angle_on_surface: f32,
    pub(crate) severity: Severity,
    /// `0.0` = just landed, `1.0` = fully faded.
    pub(crate) age: f32,
    /// Whether this is the fainter echo on a parent planet rather than the struck moon itself.
    pub(crate) is_ripple: bool,
}

/// An asteroid still travelling toward its target, before impact.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AsteroidInFlight {
    pub(crate) target: usize,
    pub(crate) severity: Severity,
    /// `0.0` = just fired, `1.0` = impact.
    pub(crate) progress: f32,
    /// Angle the rock approaches from, in the target's local frame.
    pub(crate) approach_angle: f32,
}

/// The burst of short rays thrown off an impact at the moment of strike.
///
/// A separate effect from [`Crater`] rather than a phase of it, because the two live on completely
/// different timescales — the rays are a sub-second flash, the scar they leave behind fades for
/// most of a minute — and because only the rays are drawn *outside* the struck body's own disk.
/// Like [`Crater::age`], `age` is a caller-resolved fraction so this module never reads a clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ejecta {
    pub(crate) body: usize,
    /// Where on the body the strike landed, in the same local frame [`Crater`] uses — the rays
    /// fan outward around this direction, so they read as thrown *from* the crater.
    pub(crate) angle_on_surface: f32,
    pub(crate) severity: Severity,
    /// `0.0` = the instant of impact, `1.0` = the last dust has faded.
    pub(crate) age: f32,
}

/// A comet crossing the whole scene. `start`/`end` are normalised `0.0..=1.0` scene coordinates;
/// `magnitude` is the already-resolved work-size intensity (`0.0..=1.0`, quiet green-test tier at
/// the bottom, a landed large task at the top) driving both brightness and tail length.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Comet {
    pub(crate) start: (f32, f32),
    pub(crate) end: (f32, f32),
    /// The body this comet is flying *into*, if it is an arrival rather than a crossing — a
    /// landing comet ends on the body the work landed on, which moves along its own orbit, so the
    /// endpoint has to be resolved per-frame here rather than frozen into `end` at spawn time.
    /// `None` (a crossing) uses `end` exactly as given, which is the round-1 behaviour unchanged.
    pub(crate) target: Option<usize>,
    pub(crate) magnitude: f32,
    /// `0.0` = just launched, `1.0` = crossed off-scene.
    pub(crate) progress: f32,
}

/// Every transient, event-driven overlay live on top of the ambient orbit scene this frame.
#[derive(Debug, Clone, Default)]
pub(crate) struct SceneEffects {
    pub(crate) asteroids: Vec<AsteroidInFlight>,
    pub(crate) craters: Vec<Crater>,
    pub(crate) ejecta: Vec<Ejecta>,
    pub(crate) comets: Vec<Comet>,
}

/// Deterministic 2D value noise in `0.0..=1.0`, for cheap procedural surface mottling. Not
/// physically anything — a fast hash-lattice noise with smoothstep interpolation, chosen because
/// a real Perlin/Simplex implementation buys nothing here: the texture only has to read as
/// "not a flat disk" at typical body pixel radii (a few dozen pixels), never as a rendered
/// planet photograph.
fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    fn hash(ix: i32, iy: i32, seed: u32) -> f32 {
        let mut h = (ix as u32)
            .wrapping_mul(374_761_393)
            .wrapping_add((iy as u32).wrapping_mul(668_265_263))
            .wrapping_add(seed.wrapping_mul(2_654_435_761));
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        h ^= h >> 16;
        (h & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
    }
    fn smooth(t: f32) -> f32 {
        t * t * (3.0 - 2.0 * t)
    }

    let x0 = x.floor();
    let y0 = y.floor();
    let tx = smooth(x - x0);
    let ty = smooth(y - y0);
    let (x0i, y0i) = (x0 as i32, y0 as i32);

    let a = hash(x0i, y0i, seed);
    let b = hash(x0i + 1, y0i, seed);
    let c = hash(x0i, y0i + 1, seed);
    let d = hash(x0i + 1, y0i + 1, seed);

    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

/// A stable per-body seed for its own surface texture, so two bodies of the same kind and
/// severity do not render as the literal same rock.
fn body_seed(idx: usize) -> u32 {
    (idx as u32).wrapping_mul(2_246_822_519).wrapping_add(1)
}

fn clamp01(v: f32) -> f32 {
    v.clamp(0.0, 1.0)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * clamp01(t)
}

/// `signal_ink` as `0.0..=1.0` floats rather than `u8` triples, since shading multiplies it by a
/// per-pixel lighting factor before quantizing back down once at the end.
fn severity_rgb01(hue: f32, severity: Severity) -> (f32, f32, f32) {
    let (r, g, b) = signal_ink(hue, severity, SPACE_SURFACE);
    (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}

/// Half-width of the terminator's transition, in the same cosine units the Lambert term is in.
///
/// A body whose day/night line is `lam.max(0.0)` has a hard edge no real body has. Inside this
/// band the term is remapped by a quadratic that is C1 at both joins — it meets the raw Lambert
/// at `+WIDTH` with slope 1 and reaches zero at `-WIDTH` with slope 0 — so nothing kinks where
/// the remap takes over and it is the identity everywhere outside the band. The orrery's own
/// `TERM_W`.
const TERMINATOR_WIDTH: f32 = 0.13;

/// How far the terminator's transition goes warm, red against blue. Grazing light travels a
/// longer path through an atmosphere and comes out red — which is why a terminator is warm and
/// why a sunset is. Applies only inside [`TERMINATOR_WIDTH`].
const TERMINATOR_WARMTH: f32 = 0.32;

/// Per-channel share of [`TERMINATOR_WARMTH`]: red up, green barely, blue down.
const TERMINATOR_WARM_TINT: (f32, f32, f32) = (1.0, -0.15, -0.55);

/// Peak brightness the atmospheric rim adds at the grazing sunlit limb, as a multiple of the
/// body's own colour.
///
/// The orrery's `RIM_A` of `0.62` against its 0–255 accumulator, carried into this module's
/// `0.0..=1.0` one — the same crescent, in the units this renderer shades in.
const RIM_GAIN: f32 = 0.62 * (150.0 / 255.0);

/// Per-channel tint of the rim: very slightly cool, because it is scattered light rather than the
/// surface's own colour.
const RIM_TINT: (f32, f32, f32) = (1.0, 0.985, 0.94);

/// Peak brightness planetshine adds on the night side, as a multiple of the body's own colour.
/// Light returned to a body from the rest of the system: small, and the only thing lighting the
/// night side that is not a flat fill. The orrery's `SHINE`.
const PLANETSHINE: f32 = 0.075;

/// Amplitude of the two banding harmonics, and the darker belt that breaks their regularity.
/// The orrery's `0.145` / `0.075` pair and its `Math.abs(Math.abs(latW)-0.26) < 0.075` belt.
const BAND_PRIMARY: f32 = 0.145;
const BAND_SECONDARY: f32 = 0.075;
const BAND_BELT_LATITUDE: f32 = 0.26;
const BAND_BELT_HALF_WIDTH: f32 = 0.075;
const BAND_BELT_DARKEN: f32 = 0.90;

/// Everything about *what a body is made of* that per-pixel shading needs, resolved once per body
/// rather than threaded through as five more positional arguments.
///
/// Exists because body types arrived: before them a body's surface was two facts (its colour and
/// whether it emits light) and a positional argument each was fine; a gas giant's cloud deck adds
/// band count and mottle depth, and a five-`f32`-and-two-`bool` call is where a caller starts
/// passing them in the wrong order.
#[derive(Debug, Clone, Copy)]
struct Surface {
    base: (f32, f32, f32),
    seed: u32,
    self_luminous: bool,
    /// Latitudinal cloud bands and how many, or `None` for a rocky body that carries none.
    bands: Option<f32>,
    /// How much of the rocky surface mottling this body keeps, `0.0..=1.0`.
    mottle_scale: f32,
}

/// Lambertian shading with a soft warm terminator, an atmospheric rim, planetshine, limb
/// darkening and a mottled texture, for one pixel `(dx, dy)` offset from a body's centre, `dist`
/// away, inside a body of `radius` pixels.
///
/// The rim, the terminator's softness and the night side's rim of returned light are the three
/// optical facts a real body has that a diffuse-shaded disk does not, and each is a function of
/// the same light vector everything else here already obeys rather than a garnish laid over it.
/// They are the fleet orrery's own A51(a)–(c), ported into this renderer's per-pixel shading —
/// there is no sprite bake here to hang them off, so they are computed where the Lambert term
/// already is, at no extra pass and inside the same bounding box.
///
/// `light_dir` is normalised and points *from the surface toward the light* (i.e. toward the
/// sun's on-screen position for a planet/moon; straight out of the screen for the sun itself,
/// which is self-luminous rather than lit).
fn shade_surface(
    dx: f32,
    dy: f32,
    radius: f32,
    light_dir: (f32, f32, f32),
    surface: Surface,
) -> (f32, f32, f32) {
    let Surface {
        base,
        seed,
        self_luminous,
        bands,
        mottle_scale,
    } = surface;
    let nx = dx / radius;
    let ny = dy / radius;
    let nz = (1.0 - (nx * nx + ny * ny)).max(0.0).sqrt();

    let texture = value_noise(nx * 4.0 + 7.0, ny * 4.0 + 3.0, seed) * 0.5
        + value_noise(nx * 11.0, ny * 11.0, seed.wrapping_add(97)) * 0.5;
    // A cloud deck is smoother than rock, so a gas giant keeps only part of the rocky mottle's
    // depth. A gas giant that mottles like a moon reads as a big moon.
    let mottle = mix(1.0 - (1.0 - 0.86) * mottle_scale, 1.0, texture);

    // Latitudinal cloud banding. `ny` *is* the latitude in this renderer's screen-oriented frame,
    // which is the same frame the terminator and rim are already solved in, so the bands stay
    // latitudinal without a second coordinate system. Two harmonics rather than one — one reads as
    // a stripe — plus one darker belt, which is what stops an evenly banded body from reading as
    // corduroy.
    let banding = match bands {
        None => 1.0,
        Some(count) => {
            let seed_phase = (seed & 0xFFFF) as f32 * (2.0 * PI / 65_536.0);
            let latitude = ny;
            let mut band = 1.0
                + BAND_PRIMARY * (latitude * count * PI + seed_phase).sin()
                + BAND_SECONDARY * (latitude * count * 2.7 * PI + seed_phase * 1.7).sin();
            if (latitude.abs() - BAND_BELT_LATITUDE).abs() < BAND_BELT_HALF_WIDTH {
                band *= BAND_BELT_DARKEN;
            }
            band
        }
    };
    let mottle = mottle * banding;

    if self_luminous {
        // Bright core, gentle limb brightening rather than darkening — a rough stand-in for
        // a star's own emitted light rather than reflected light.
        let limb = mix(0.80, 1.05, nz);
        let lit = limb * mottle;
        return (
            clamp01(base.0 * lit),
            clamp01(base.1 * lit),
            clamp01(base.2 * lit),
        );
    }

    let lambert = nx * light_dir.0 + ny * light_dir.1 + nz * light_dir.2;

    // A soft terminator, rather than `lambert.max(0.0)`'s hard day/night edge.
    let diffuse = if lambert >= TERMINATOR_WIDTH {
        lambert
    } else if lambert <= -TERMINATOR_WIDTH {
        0.0
    } else {
        let u = lambert + TERMINATOR_WIDTH;
        u * u / (4.0 * TERMINATOR_WIDTH)
    };

    let ambient = 0.10;
    let lit = ambient + diffuse * (1.0 - ambient);
    let limb_darkening = mix(0.55, 1.0, nz);
    let lit = lit * limb_darkening * mottle;

    // How far out toward the limb this pixel is, `0.0` at the centre and `1.0` at the edge —
    // squared, since that is what both terms below actually want.
    let radial_sq = nx * nx + ny * ny;

    // The atmospheric rim: a thin bright crescent where the line of sight grazes the atmosphere
    // and the surface normal is still lit. `radial_sq^5` is the limb falloff by repeated
    // multiply, which is the same curve without paying for a `powf` per pixel.
    let graze = radial_sq * radial_sq * radial_sq * radial_sq * radial_sq;
    let still_lit = (lambert * 0.72 + 0.28).max(0.0);
    let rim = RIM_GAIN * graze * still_lit * still_lit;

    // Planetshine: the night side is no longer just `ambient`. It carries a faint rim of light
    // returned to it from the rest of the system, strongest where the night side grazes the limb.
    let night = (-lambert).max(0.0);
    let shine = PLANETSHINE * night.sqrt() * (0.30 + 0.70 * radial_sq);

    // ...and the transition itself goes warm, which is only ever inside the terminator band.
    let warmth = if lambert.abs() < TERMINATOR_WIDTH {
        TERMINATOR_WARMTH * (1.0 - lambert.abs() / TERMINATOR_WIDTH)
    } else {
        0.0
    };
    let warm = |channel: f32| 1.0 + warmth * channel;

    (
        clamp01(base.0 * (lit * warm(TERMINATOR_WARM_TINT.0) + rim * RIM_TINT.0 + shine)),
        clamp01(base.1 * (lit * warm(TERMINATOR_WARM_TINT.1) + rim * RIM_TINT.1 + shine)),
        clamp01(base.2 * (lit * warm(TERMINATOR_WARM_TINT.2) + rim * RIM_TINT.2 + shine)),
    )
}

/// Standard "over" alpha compositing of straight (non-premultiplied) `src` at `alpha` onto `dst`
/// in place. `dst[3]` is `dst`'s own accumulated alpha — starting a buffer at `[0,0,0,0]` and
/// compositing only where something is actually drawn is what makes the effects overlay
/// (`effects_frame`) a real transparent layer rather than a second opaque copy of the scene.
fn blend(dst: &mut [f32; 4], src: (f32, f32, f32), alpha: f32) {
    let alpha = clamp01(alpha);
    let out_a = alpha + dst[3] * (1.0 - alpha);
    if out_a <= 0.0 {
        *dst = [0.0, 0.0, 0.0, 0.0];
        return;
    }
    dst[0] = (src.0 * alpha + dst[0] * dst[3] * (1.0 - alpha)) / out_a;
    dst[1] = (src.1 * alpha + dst[1] * dst[3] * (1.0 - alpha)) / out_a;
    dst[2] = (src.2 * alpha + dst[2] * dst[3] * (1.0 - alpha)) / out_a;
    dst[3] = out_a;
}

/// Deterministic starfield: a fixed number of point stars, positioned and dimmed from a fixed
/// seed rather than `width`/`height`, so the field does not visibly re-shuffle on every resize.
const STAR_COUNT: usize = 260;

/// Stellar colour-temperature classes, hottest first — the spread every star's colour is drawn
/// from.
///
/// The same six classes the fleet orrery's own `STAR_CLASS` table holds, ported rather than
/// re-derived so the two skies are the same sky. A real starfield carries a genuine spread of
/// colour temperature; one flat white at a few sizes is the lazy default both are refusing.
const STAR_CLASS: [(f32, f32, f32); 6] = [
    (0.745, 0.808, 1.000),
    (0.894, 0.922, 1.000),
    (0.980, 0.980, 0.988),
    (1.000, 0.965, 0.878),
    (1.000, 0.839, 0.667),
    (1.000, 0.737, 0.612),
];

/// How steeply a star's magnitude is skewed toward faint: `u^STAR_MAGNITUDE_SKEW` over a uniform
/// `u`, so the field is many faint stars and very few bright ones rather than an even scatter.
/// The orrery's own exponent.
const STAR_MAGNITUDE_SKEW: f32 = 3.4;

/// The floor and span a magnitude maps onto as drawn alpha — the faintest star is still a star,
/// and the brightest reaches its class colour whole.
const STAR_ALPHA: (f32, f32) = (0.16, 0.84);

/// The magnitude above which a star is one of "the brightest few" that scintillate. A sky where
/// every star twinkles is a screensaver, so this names a handful and leaves the rest steady.
///
/// The orrery cuts at `0.93` over roughly a thousand stars; [`STAR_COUNT`] is a quarter of that,
/// so the same *count* needs a lower cut. Measured against this field's own magnitudes, `0.80`
/// names eight of the two hundred and sixty — see this module's
/// `only_the_brightest_few_stars_scintillate` test, which holds the "few" rather than the number.
const STAR_SCINTILLATION_MAGNITUDE: f32 = 0.80;

/// Draw one body (disk, shading, and soft glow fringe) into `buf`, restricted to its own
/// bounding box rather than the whole frame — the cost lever that keeps a handful of shaded
/// spheres cheap even at 1440p, unlike `src/particle_field.rs`'s necessarily full-frame passes.
fn draw_body(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    center: (f32, f32),
    radius: f32,
    surface: Surface,
    light_dir: (f32, f32, f32),
) {
    let self_luminous = surface.self_luminous;
    let base = surface.base;
    let glow = radius * if self_luminous { 2.6 } else { 1.4 };

    let x0 = (center.0 - glow).floor().max(0.0) as i32;
    let x1 = (center.0 + glow).ceil().min(width as f32) as i32;
    let y0 = (center.1 - glow).floor().max(0.0) as i32;
    let y1 = (center.1 + glow).ceil().min(height as f32) as i32;

    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as f32 + 0.5 - center.0;
            let dy = py as f32 + 0.5 - center.1;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = py as usize * width as usize + px as usize;

            if dist <= radius {
                let aa = clamp01((radius - dist) * 1.5);
                let color = shade_surface(dx, dy, radius, light_dir, surface);
                blend(&mut buf[idx], color, aa.max(0.85));
            } else if dist <= glow {
                let t = 1.0 - (dist - radius) / (glow - radius).max(0.001);
                let alpha = t * t * if self_luminous { 0.55 } else { 0.22 };
                blend(&mut buf[idx], base, alpha);
            }
        }
    }
}

/// Where the unseated mates are marked, as a fraction of `min(width, height)` from the scene's
/// centre — outside every orbit, so the ladder's own spacing is untouched by the disclosure.
const OVERFLOW_ORBIT_FRACTION: f32 = 0.44;

/// The arc the marks are spread over, centred straight down. A compact fan rather than a full
/// ring: these are not bodies in orbit, they are a queue of mates the ring could not seat, and
/// spreading them evenly round the scene would read as exactly the thing they are not.
const OVERFLOW_FAN: f32 = 0.42;
const OVERFLOW_ANGLE: f32 = PI * 0.5;

/// One mark's radius in pixels, and its alpha. Deliberately smaller and dimmer than the smallest
/// body the scene can draw — a mark must not be mistakable for a seated body, or the disclosure
/// becomes the misreading it exists to prevent.
const OVERFLOW_MARK_PX: f32 = 1.6;
const OVERFLOW_MARK_ALPHA: f32 = 0.34;

/// Colour of an overflow mark: the ring's own cold ice, which is the scene's existing "material
/// rather than state" colour. Not severity-coded — these mates have states, and the whole point is
/// that the scene is *not* showing them.
const OVERFLOW_RGB01: (f32, f32, f32) = RING_RGB01;

/// Mark the second mates the ring had no slot for: one small, dim, unshaded mote each, on a short
/// arc outside every orbit.
///
/// A41(b): a ninth mate is *counted and dropped*, never a silent vanish — an instrument that
/// quietly shows fewer bodies than the fleet has is lying by omission. The exact count also goes
/// out over the session API, where it can be read as a number and argued with; this is the half
/// that is in the frame, so a viewer who is only looking at the scene still knows the ring is not
/// the whole fleet.
///
/// Absent at zero, because a disclosure of nothing is noise rather than population (A41(c)).
///
/// One mote per mate rather than a bar or a gauge: at these counts the marks are countable, which
/// makes the reading exact without this generator needing a font.
fn draw_overflow_mark(buf: &mut [[f32; 4]], width: u32, height: u32, beyond: usize, phase: f32) {
    if beyond == 0 || width == 0 || height == 0 {
        return;
    }
    let scale = width.min(height) as f32;
    let centre = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = OVERFLOW_ORBIT_FRACTION * scale;

    for i in 0..beyond {
        let spread = if beyond > 1 {
            i as f32 / (beyond - 1) as f32 - 0.5
        } else {
            0.0
        };
        let angle = OVERFLOW_ANGLE + spread * OVERFLOW_FAN;
        let (sin_a, cos_a) = angle.sin_cos();
        let px = centre.0 + cos_a * radius;
        let py = centre.1 + sin_a * radius;
        // A slow shared breath, so the queue reads as present rather than as dead pixels. One
        // whole cycle per loop, so it closes with everything else.
        let alpha = OVERFLOW_MARK_ALPHA * mix(0.7, 1.0, 0.5 + 0.5 * phase.sin());

        let x0 = (px - OVERFLOW_MARK_PX).floor().max(0.0) as i32;
        let x1 = (px + OVERFLOW_MARK_PX).ceil().min(width as f32) as i32;
        let y0 = (py - OVERFLOW_MARK_PX).floor().max(0.0) as i32;
        let y1 = (py + OVERFLOW_MARK_PX).ceil().min(height as f32) as i32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let dx = xx as f32 + 0.5 - px;
                let dy = yy as f32 + 0.5 - py;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > OVERFLOW_MARK_PX {
                    continue;
                }
                let idx = yy as usize * width as usize + xx as usize;
                blend(
                    &mut buf[idx],
                    OVERFLOW_RGB01,
                    alpha * clamp01(1.0 - dist / OVERFLOW_MARK_PX),
                );
            }
        }
    }
}

/// Which half of a ring is being drawn — the half behind the planet, or the half in front of it.
///
/// A ring is the one element in this scene that a body sits *inside*, so it cannot be one draw:
/// the far arc has to go down before the planet and the near arc after it, or the planet renders
/// as a disk with a bracelet painted over it. This renderer has no depth buffer, and it does not
/// need one — the ring's own parametric angle says which half a particle is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RingHalf {
    Back,
    Front,
}

/// How many ring particles per pixel of the ring's own outer circumference.
///
/// Density, not a count: the orrery draws a fixed 168 because its bodies are a fixed size, while
/// herdr's planets span the whole project-size register and a fixed count would make a large
/// mate's ring a dotted line and a small one's a solid smear.
///
/// Denser than the orrery's own one-per-2.3px, and for a reason that is herdr's rather than a
/// preference: a second mate here is 9 to 23 pixels across the radius against the orrery's ~35,
/// so the same *angular* density lands far fewer particles on the arc and the ring reads as a
/// dotted outline. Measured on the rendered frame at both ends of the register.
const RING_PARTICLES_PER_PX: f32 = 1.0 / 1.1;

/// Bounds on that count, so a ring at either end of the register is still a ring: enough particles
/// to close the ellipse at the smallest planet, and a ceiling so the largest cannot walk the whole
/// scene's cost up on its own.
const RING_PARTICLE_BOUNDS: (usize, usize) = (170, 520);

/// Full turns the ring's particle stream makes per animation loop.
///
/// An integer, for exactly the reason [`BodyKind::revolutions_per_loop`] is: the loop is baked
/// once and played forever, so anything that moves in it has to land back where it started after
/// [`FRAME_COUNT`] samples or the seam shows on every repeat. Two rather than one, because ring
/// traffic that keeps pace with the planet it orbits does not read as traffic.
const RING_REVOLUTIONS_PER_LOOP: f32 = 2.0;

/// Draw one half of a ringed planet's ring: a stream of ice-and-dust particles on a squashed
/// ellipse about the body's own centre.
///
/// Particles rather than a filled annulus, and deliberately: the ring is *the traffic of a mate's
/// own moons*, so it should read as many small things rather than as a painted band, and a
/// particle stream is also what lets the ring be occluded correctly by the body it circles for the
/// cost of one sign test. It is the cheaper of the two as well — a couple of hundred small dots
/// against an elliptical annulus's whole bounding box.
fn draw_ring(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    body: &BodyLayout,
    position: (f32, f32),
    half: RingHalf,
    phase: f32,
    seed: u32,
) {
    let Some((inner, outer)) = body.ring_radii_px() else {
        return;
    };
    if outer <= inner || outer <= 0.0 {
        return;
    }

    let count = ((outer * 2.0 * PI * RING_PARTICLES_PER_PX) as usize)
        .clamp(RING_PARTICLE_BOUNDS.0, RING_PARTICLE_BOUNDS.1);
    let bright = RING_BRIGHT + RING_BRIGHT_PER_STREAK * body.streak;
    let spin = phase * RING_REVOLUTIONS_PER_LOOP + body.base_angle;
    // A particle is drawn about a pixel across at the sizes this scene works at; below that it
    // stops being a particle and starts being noise.
    let dot = (outer * 0.028).max(0.9);

    for i in 0..count {
        let angle = (i as f32 / count as f32) * 2.0 * PI + spin;
        let (sin_a, cos_a) = angle.sin_cos();
        // The near half of the ellipse is the half swept below the centre on screen. One sign
        // test is the whole depth sort this element needs.
        if (sin_a > 0.0) != (half == RingHalf::Front) {
            continue;
        }

        // Where in the ring's own width this particle sits. Deterministic per particle and per
        // body, so the ring is a fixed object being rotated rather than a fresh scatter every
        // frame — which is what stops it boiling.
        let spread = value_noise(i as f32 * 1.37, 0.5, seed);
        let radius = inner + (outer - inner) * spread;
        let px = position.0 + cos_a * radius;
        let py = position.1 + sin_a * radius * RING_SQUASH;

        // Brighter where the ring is denser and toward its outer edge, which is what a real ring's
        // brightest arcs look like — a flat-alpha ring reads as a drawn outline.
        let alpha = (bright * (0.55 + 0.75 * spread)).min(0.92);
        if alpha <= 0.004 {
            continue;
        }

        let x0 = (px - dot).floor().max(0.0) as i32;
        let x1 = (px + dot).ceil().min(width as f32) as i32;
        let y0 = (py - dot).floor().max(0.0) as i32;
        let y1 = (py + dot).ceil().min(height as f32) as i32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let dx = xx as f32 + 0.5 - px;
                let dy = yy as f32 + 0.5 - py;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > dot {
                    continue;
                }
                let idx = yy as usize * width as usize + xx as usize;
                blend(&mut buf[idx], RING_RGB01, alpha * clamp01(1.0 - dist / dot));
            }
        }
    }
}

/// How long a struck moon's crater takes to fully fade, in the same `age` units [`Crater::age`]
/// is expressed in (a caller-resolved fraction, so this constant lives only in documentation on
/// the glue side — kept here as the one place the *visual* fade curve is defined).
pub(crate) const CRATER_MIN_ALPHA: f32 = 0.0;

/// Shape of the crater's fade against its own age: `(1 - age)^CRATER_FADE_EXPONENT`.
///
/// An exponent below `1.0` is an ease-out — the scar holds most of its opacity through the early
/// life of the fade and gives it all up late, instead of bleeding away linearly from the first
/// frame. Costs nothing (one `powf` per crater per frame, not per pixel) and spends none of the
/// 45s budget: the crater still clears at exactly `age == 1.0`, it is just legible for more of the
/// way there. `0.42` is the storyboard's own approved value
/// (`data/decisions/2026-08-08-ambient-storyboard-picks-autonomous.md`, firstmate home).
const CRATER_FADE_EXPONENT: f32 = 0.42;

/// How much faster a parent planet's ripple echo fades than the struck moon's own crater. Matches
/// `src/app/background_scene.rs`'s `CRATER_FADE` (45s) over `RIPPLE_FADE` (18s) — kept as a plain
/// ratio, not a second duration, so this module never has to read a clock to derive it.
const RIPPLE_FADE_RATIO: f32 = 2.5;

/// How strongly a parent planet's ripple echo draws relative to the struck moon's own crater.
///
/// Deliberately well above the round-1 value (`0.35`): the echo is drawn on a planet whose own
/// disk is brighter and larger than the moon's, and at `0.35` it measured as a low-single-digit
/// change in average patch alpha — present in the buffer, not visible on screen. The ripple still
/// has to read as the *fainter* half of the pair, so this stays below `1.0` and keeps the much
/// faster [`RIPPLE_FADE_RATIO`] fade that already distinguishes it.
const RIPPLE_STRENGTH: f32 = 0.62;

/// The shortest distance a crater's mark is allowed to fall off over, in pixels.
///
/// The mark fades linearly from its own centre to its own edge, which is right for a patch tens of
/// pixels across and wrong for one two pixels across: at that size the very first pixel centre is
/// already most of the way to the edge, so the darkest part of the mark is never sampled at all
/// and a critical strike renders as a faint smudge. That is not a tuning question, it is the
/// falloff being asked to resolve inside less than a pixel.
///
/// Flooring the falloff distance gives a small patch a solid core — which is also what a crater a
/// few pixels wide actually looks like, a pit rather than a smear — and changes nothing whatsoever
/// for any patch already wider than this. It became load-bearing when F16's bound re-solved the
/// moon tier down (see [`moon_radius_ceil`]): a worker moon's crater is now a couple of pixels
/// across at the real target resolution.
const CRATER_MIN_FALLOFF_PX: f32 = 3.0;

/// Draw a crater (or its fainter ripple echo) as a dark, irregular patch blended onto the
/// already-shaded body underneath it — craters darken and roughen a surface, they do not recolour
/// it, so this runs strictly after [`draw_body`] for the same body.
fn draw_crater(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    crater: &Crater,
    body: &BodyLayout,
    position: (f32, f32),
) {
    let fade = clamp01(1.0 - crater.age).powf(CRATER_FADE_EXPONENT);
    if fade <= CRATER_MIN_ALPHA {
        return;
    }
    // Severity scales both size and darkness, mirroring how the same channel already scales a
    // sidebar card's intensity — a mild problem leaves a small, shallow mark; a critical one
    // leaves a large, dark one.
    let severity_scale = mix(0.55, 1.0, crater.severity.amount());
    let strength = severity_scale
        * if crater.is_ripple {
            RIPPLE_STRENGTH
        } else {
            1.0
        };
    let patch_radius =
        body.body_radius_px * severity_scale * if crater.is_ripple { 0.55 } else { 0.42 };
    let cx = position.0 + body.body_radius_px * 0.35 * crater.angle_on_surface.cos();
    let cy = position.1 + body.body_radius_px * 0.35 * crater.angle_on_surface.sin();

    let x0 = (cx - patch_radius).floor().max(0.0) as i32;
    let x1 = (cx + patch_radius).ceil().min(width as f32) as i32;
    let y0 = (cy - patch_radius).floor().max(0.0) as i32;
    let y1 = (cy + patch_radius).ceil().min(height as f32) as i32;
    let seed = body_seed(crater.body).wrapping_add(919);

    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > patch_radius {
                continue;
            }
            let roughness = value_noise(dx / patch_radius * 3.0, dy / patch_radius * 3.0, seed);
            let edge = clamp01(1.0 - dist / patch_radius.max(CRATER_MIN_FALLOFF_PX));
            let alpha = edge * roughness * strength * fade * 0.85;
            let idx = py as usize * width as usize + px as usize;
            blend(&mut buf[idx], (0.05, 0.04, 0.04), alpha);
        }
    }
}

/// Colour of thrown-up impact dust: the same neutral rock tone [`draw_asteroid`] uses, lifted to
/// read as lit debris rather than a second silhouette. Deliberately hueless — the strike says
/// "something hit this", the body's own colour already says what state it is in.
const EJECTA_RGB: (f32, f32, f32) = (0.86, 0.80, 0.70);

/// How far the longest ray reaches past the impact point, as a multiple of the struck body's own
/// radius, at the mildest and most severe ends of the severity channel. A moon is only a dozen or
/// so pixels across at the real target resolution, so the rays have to leave the disk entirely to
/// be the thing that is readable from across the window — which is the whole reason the storyboard
/// picked "rock in + ejecta ray system" over the crater alone.
const EJECTA_REACH: (f32, f32) = (2.4, 4.6);

/// Rays in one burst, at the mildest and most severe ends of the severity channel.
const EJECTA_RAYS: (usize, usize) = (7, 13);

/// How wide the fan of rays opens around the outward normal at the impact point, in radians. Just
/// under a full hemisphere: rays thrown backwards through the body would draw on the wrong side of
/// a disk this renderer has no depth information about.
const EJECTA_SPREAD: f32 = PI * 0.92;

/// Draw the burst of short rays an impact throws off at the moment of strike: a fan of thin,
/// neutral-coloured streaks radiating outward from the crater, flying out and fading as they go.
///
/// Runs after [`draw_crater`] for the same body — the rays are dust *above* the surface, and they
/// deliberately extend past the body's own limb, which is what makes an impact on a 13px moon
/// visible at all.
fn draw_ejecta(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    ejecta: &Ejecta,
    body: &BodyLayout,
    position: (f32, f32),
) {
    // Rays are a flash, not a scar: quadratic so they are essentially gone well before the crater
    // they leave behind has begun to fade noticeably.
    let fade = clamp01(1.0 - ejecta.age);
    let alpha_scale = fade * fade;
    if alpha_scale <= 0.0 {
        return;
    }

    let amount = ejecta.severity.amount();
    let rays = mix(EJECTA_RAYS.0 as f32, EJECTA_RAYS.1 as f32, amount).round() as usize;
    let reach = body.body_radius_px * mix(EJECTA_REACH.0, EJECTA_REACH.1, amount);
    let seed = body_seed(ejecta.body).wrapping_add(5701);

    // The impact point, matching `draw_crater`'s own placement exactly so the rays leave from the
    // mark rather than from somewhere near it.
    let (sin_a, cos_a) = ejecta.angle_on_surface.sin_cos();
    let cx = position.0 + body.body_radius_px * 0.35 * cos_a;
    let cy = position.1 + body.body_radius_px * 0.35 * sin_a;

    // The whole burst flies outward over its life: the near end of every ray leaves the surface
    // and the far end runs ahead of it, so the fan expands rather than just dimming in place.
    let inner = body.body_radius_px * 0.2 + reach * 0.55 * ejecta.age;
    let ray_len = reach * mix(0.55, 0.22, ejecta.age);

    for i in 0..rays.max(1) {
        let spread_t = if rays > 1 {
            i as f32 / (rays - 1) as f32 - 0.5
        } else {
            0.0
        };
        // Even spacing would read as a mechanical starburst; the noise jitter keeps each body's
        // fan its own shape while staying fully deterministic, like every other seed here.
        let jitter = (value_noise(i as f32 * 1.7 + 0.5, 0.5, seed) - 0.5) * 0.34;
        let angle = ejecta.angle_on_surface + spread_t * EJECTA_SPREAD + jitter;
        let (rs, rc) = angle.sin_cos();
        // Length varies per ray for the same reason, and the shortest ray still clears the limb.
        let len = ray_len * mix(0.6, 1.0, value_noise(i as f32 * 3.1, 2.0, seed));

        let steps = (len.ceil() as i32).max(1);
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            let dist = inner + len * t;
            let px = cx + rc * dist;
            let py = cy + rs * dist;
            // Thin at the leading tip, thicker at the root, and thinning overall as it flies.
            let radius = mix(1.5, 0.5, t) * mix(1.0, 0.6, ejecta.age);
            // Brightest at the root, gone at the tip.
            let alpha = alpha_scale * mix(0.9, 0.0, t * t);
            if alpha <= 0.004 {
                continue;
            }

            let x0 = (px - radius).floor().max(0.0) as i32;
            let x1 = (px + radius).ceil().min(width as f32) as i32;
            let y0 = (py - radius).floor().max(0.0) as i32;
            let y1 = (py + radius).ceil().min(height as f32) as i32;
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let dx = xx as f32 + 0.5 - px;
                    let dy = yy as f32 + 0.5 - py;
                    let d = (dx * dx + dy * dy).sqrt();
                    let falloff = radius.max(0.5);
                    if d > falloff {
                        continue;
                    }
                    let idx = yy as usize * width as usize + xx as usize;
                    blend(
                        &mut buf[idx],
                        EJECTA_RGB,
                        alpha * clamp01(1.0 - d / falloff),
                    );
                }
            }
        }
    }
}

/// Draw an in-flight asteroid: a small, irregular, realistically neutral-coloured rock — no tail,
/// no hue — sized by severity, travelling toward its target.
fn draw_asteroid(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    center: (f32, f32),
    severity: Severity,
) {
    let size = mix(3.0, 7.0, severity.amount());
    let seed = 4242u32;
    let x0 = (center.0 - size).floor().max(0.0) as i32;
    let x1 = (center.0 + size).ceil().min(width as f32) as i32;
    let y0 = (center.1 - size).floor().max(0.0) as i32;
    let y1 = (center.1 + size).ceil().min(height as f32) as i32;

    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as f32 + 0.5 - center.0;
            let dy = py as f32 + 0.5 - center.1;
            let dist = (dx * dx + dy * dy).sqrt();
            let wobble = 0.75 + 0.25 * value_noise(dx * 0.8, dy * 0.8, seed);
            if dist > size * wobble {
                continue;
            }
            let shade = 0.35 + 0.35 * value_noise(dx * 1.3 + 5.0, dy * 1.3, seed);
            let idx = py as usize * width as usize + px as usize;
            blend(&mut buf[idx], (shade * 0.9, shade * 0.8, shade * 0.72), 1.0);
        }
    }
}

/// Draw a comet: a bright core plus a fading tail along its direction of travel.
///
/// `end` is passed in rather than read off `comet` because an arrival comet's endpoint is the
/// live position of an orbiting body, which only [`effects_frame`] can resolve — see
/// [`Comet::target`]. For a crossing comet it is exactly `comet.end`.
fn draw_comet(buf: &mut [[f32; 4]], width: u32, height: u32, comet: &Comet, end: (f32, f32)) {
    let pos = (
        mix(comet.start.0, end.0, comet.progress) * width as f32,
        mix(comet.start.1, end.1, comet.progress) * height as f32,
    );
    let dir = (end.0 - comet.start.0, end.1 - comet.start.1);
    let dir_len = (dir.0 * dir.0 + dir.1 * dir.1).sqrt().max(0.0001);
    let dir = (dir.0 / dir_len, dir.1 / dir_len);

    let core_radius = mix(1.4, 4.0, comet.magnitude);
    let tail_len = mix(18.0, 140.0, comet.magnitude) * (width.min(height) as f32 / 1440.0);
    let color = (1.0, 0.97, 0.86);

    let steps = (tail_len as i32).max(1);
    for step in 0..steps {
        let t = step as f32 / steps as f32;
        let px = pos.0 - dir.0 * t * tail_len;
        let py = pos.1 - dir.1 * t * tail_len;
        let radius = mix(core_radius, core_radius * 0.15, t);
        let alpha = mix(1.0, 0.0, t * t);
        let x0 = (px - radius).floor().max(0.0) as i32;
        let x1 = (px + radius).ceil().min(width as f32) as i32;
        let y0 = (py - radius).floor().max(0.0) as i32;
        let y1 = (py + radius).ceil().min(height as f32) as i32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let dx = xx as f32 + 0.5 - px;
                let dy = yy as f32 + 0.5 - py;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > radius.max(0.5) {
                    continue;
                }
                let idx = yy as usize * width as usize + xx as usize;
                blend(
                    &mut buf[idx],
                    color,
                    alpha * clamp01(1.0 - dist / radius.max(0.5)),
                );
            }
        }
    }
}

/// Samples per full animation loop. See [`BodyKind::revolutions_per_loop`] for why every body
/// lands back on its starting angle after this many samples regardless of its own orbital speed.
pub(crate) const FRAME_COUNT: usize = 36;

/// Render one frame of the ambient scene at `phase` (`0.0..=2*PI`, one full loop): starfield plus
/// every shaded body, fully opaque. No effects — this is what `App::observe_background_scene`
/// bakes into a slow-changing, cached animation loop
/// (`data/decisions/2026-08-06-persistent-background-and-shooting-star-design.md`'s fade-clean
/// model means transient effects cannot live in a loop that is only regenerated on a fleet/resize
/// change). See [`effects_frame`] for the asteroid/crater/comet overlay.
pub(crate) fn frame(layout: &SceneLayout, phase: f32) -> Vec<u8> {
    frame_inner(layout, phase, true)
}

/// [`frame`] with every ring suppressed and nothing else changed.
///
/// A test-only seam, and it earns its keep: it is the only way to isolate ring pixels *exactly*.
/// Differencing a ringed mate against a gas one instead looks like an isolate and is not — the two
/// types also band differently and a gas giant swells on a streak, so the difference carries three
/// mechanisms and the test would pass on any of them. Against this the difference is the ring and
/// provably nothing else, including inside the body's own disk, which is where the interesting
/// half of the question lives (does the near arc draw over the planet, and does the far arc not).
///
/// Free in production: `rings` is a constant at both call sites.
#[cfg(test)]
fn frame_without_rings(layout: &SceneLayout, phase: f32) -> Vec<u8> {
    frame_inner(layout, phase, false)
}

fn frame_inner(layout: &SceneLayout, phase: f32, rings: bool) -> Vec<u8> {
    let (width, height) = (layout.width, layout.height);
    let pixels = width as usize * height as usize;
    let mut buf = vec![[0.0f32; 4]; pixels];

    if width > 0 && height > 0 {
        render_bands(&mut buf, width, height, phase);
    }

    draw_overflow_mark(&mut buf, width, height, layout.mates_beyond_ladder, phase);

    let sun_pos = sun_position(layout, phase);
    for (idx, body) in layout.bodies.iter().enumerate() {
        // A mate the ring had no slot for is not in the picture, and neither is anything under it.
        // It is not gone, though — see [`draw_overflow_mark`].
        if !body.seated {
            continue;
        }
        let pos = layout.position(idx, phase);
        let seed = body_seed(idx);
        // Back arc, body, front arc — the ring is the one element the body sits *inside*, so it
        // straddles its own planet's draw rather than following it.
        if rings {
            draw_ring(
                &mut buf,
                width,
                height,
                body,
                pos,
                RingHalf::Back,
                phase,
                seed,
            );
        }
        draw_body(
            &mut buf,
            width,
            height,
            pos,
            body.body_radius_px,
            surface_of(body, seed),
            normalize3(light_dir_toward(sun_pos, pos, body.kind)),
        );
        if rings {
            draw_ring(
                &mut buf,
                width,
                height,
                body,
                pos,
                RingHalf::Front,
                phase,
                seed,
            );
        }
    }

    pack_rgba8(&buf, true)
}

/// What one laid-out body's surface is made of, for [`shade_surface`].
///
/// The sun is a star, not a severity-coded body: it holds one fixed warm colour regardless of
/// fleet state, while every other body still resolves through the shared hue/severity channel —
/// see [`SUN_STAR_RGB01`]. Bands and mottle depth come from the body's [`BodyType`], which is a
/// second mate's own binding fact and nothing a worker or a star has.
fn surface_of(body: &BodyLayout, seed: u32) -> Surface {
    let self_luminous = body.kind == BodyKind::Sun;
    Surface {
        base: if self_luminous {
            SUN_STAR_RGB01
        } else {
            severity_rgb01(body.hue, body.severity)
        },
        seed,
        self_luminous,
        bands: body.body_type.band_count(),
        mottle_scale: body.body_type.mottle_scale(),
    }
}

/// Render the event-driven overlay at `phase`: in-flight asteroids, fading craters (plus their
/// parent-planet ripple), travelling comets — nothing else. Starts fully transparent
/// (`[0,0,0,0]` everywhere) and only ever composites where an effect actually draws, so this
/// layer shows the ambient scene from [`frame`] straight through everywhere nothing is
/// happening, rather than being a second, redundant copy of it.
///
/// Craters still need `layout` for body position/radius (where on the moon's surface the mark
/// sits), but deliberately never calls [`draw_body`] — the struck body's own shading already
/// exists one layer below.
pub(crate) fn effects_frame(layout: &SceneLayout, effects: &SceneEffects, phase: f32) -> Vec<u8> {
    let (width, height) = (layout.width, layout.height);
    let pixels = width as usize * height as usize;
    let mut buf = vec![[0.0f32; 4]; pixels];

    // A struck moon's parent gets a fainter, faster-fading echo of the same crater — "a bug
    // strikes its own moon, with a fainter ripple reaching the parent planet" — derived here
    // rather than carried by the caller, since only this module's `BodyLayout::parent` knows
    // which body that is. Kept clock-free like the rest of this generator: the ripple's own age
    // is [`RIPPLE_FADE_RATIO`] applied to the crater's already-resolved age fraction, not a
    // second duration this module would have to read a clock to measure.
    for crater in &effects.craters {
        let Some(body) = layout
            .bodies
            .get(crater.body)
            .filter(|_| layout.is_seated(crater.body))
        else {
            continue;
        };
        let pos = layout.position(crater.body, phase);
        draw_crater(&mut buf, width, height, crater, body, pos);
        if let Some(parent_idx) = body.parent {
            if let Some(parent) = layout
                .bodies
                .get(parent_idx)
                .filter(|_| layout.is_seated(parent_idx))
            {
                let parent_pos = layout.position(parent_idx, phase);
                let ripple = Crater {
                    body: parent_idx,
                    angle_on_surface: crater.angle_on_surface,
                    severity: crater.severity,
                    age: (crater.age * RIPPLE_FADE_RATIO).min(1.0),
                    is_ripple: true,
                };
                draw_crater(&mut buf, width, height, &ripple, parent, parent_pos);
            }
        }
    }

    // After every crater, so the dust a strike throws up sits above the mark it left, and above
    // any other body's crater drawn this frame.
    for ejecta in &effects.ejecta {
        let Some(body) = layout
            .bodies
            .get(ejecta.body)
            .filter(|_| layout.is_seated(ejecta.body))
        else {
            continue;
        };
        let pos = layout.position(ejecta.body, phase);
        draw_ejecta(&mut buf, width, height, ejecta, body, pos);
    }

    for asteroid in &effects.asteroids {
        if let Some(target) = layout
            .bodies
            .get(asteroid.target)
            .filter(|_| layout.is_seated(asteroid.target))
        {
            let target_pos = layout.position(asteroid.target, phase);
            let approach_radius = target.body_radius_px * 6.0;
            let start = (
                target_pos.0 + approach_radius * asteroid.approach_angle.cos(),
                target_pos.1 + approach_radius * asteroid.approach_angle.sin(),
            );
            let pos = (
                mix(start.0, target_pos.0, asteroid.progress),
                mix(start.1, target_pos.1, asteroid.progress),
            );
            draw_asteroid(&mut buf, width, height, pos, asteroid.severity);
        }
    }

    for comet in &effects.comets {
        // An arrival comet ends on a body that is itself orbiting, so its endpoint is resolved
        // against this frame's `phase` rather than frozen at spawn time — that is what makes it
        // read as flying *into* the thing the work landed on rather than past where it used to be.
        let end = comet
            .target
            .filter(|idx| *idx < layout.bodies.len() && layout.is_seated(*idx))
            .map(|idx| {
                let pos = layout.position(idx, phase);
                (
                    pos.0 / (width.max(1)) as f32,
                    pos.1 / (height.max(1)) as f32,
                )
            })
            .unwrap_or(comet.end);
        draw_comet(&mut buf, width, height, comet, end);
    }

    pack_rgba8(&buf, false)
}

fn sun_position(layout: &SceneLayout, phase: f32) -> (f32, f32) {
    layout
        .bodies
        .iter()
        .position(|b| b.kind == BodyKind::Sun)
        .map(|idx| layout.position(idx, phase))
        .unwrap_or((layout.width as f32 / 2.0, layout.height as f32 / 2.0))
}

/// The direction a body at `pos` is lit from: straight out of the screen for the sun itself
/// (self-luminous, not lit by anything), otherwise toward the sun's own on-screen position — so a
/// planet or moon's terminator always points away from the sun, which is free realism this
/// scene's own geometry supplies rather than something hand-authored.
fn light_dir_toward(sun_pos: (f32, f32), pos: (f32, f32), kind: BodyKind) -> (f32, f32, f32) {
    if kind == BodyKind::Sun {
        return (0.0, 0.0, 1.0);
    }
    let to_sun = (sun_pos.0 - pos.0, sun_pos.1 - pos.1);
    let len = (to_sun.0 * to_sun.0 + to_sun.1 * to_sun.1)
        .sqrt()
        .max(0.001);
    (to_sun.0 / len, to_sun.1 / len, 0.55)
}

/// Quantize a straight-alpha `[f32; 4]` buffer down to RGBA8. `force_opaque` pins alpha to 255
/// regardless of the buffer's own accumulated alpha — used for the ambient scene, which is
/// always a full backdrop; the effects overlay instead emits each pixel's real alpha, which is
/// `0` everywhere nothing was drawn.
fn pack_rgba8(buf: &[[f32; 4]], force_opaque: bool) -> Vec<u8> {
    let mut out = vec![0u8; buf.len() * 4];
    for (i, px) in buf.iter().enumerate() {
        out[i * 4] = (clamp01(px[0]) * 255.0).round() as u8;
        out[i * 4 + 1] = (clamp01(px[1]) * 255.0).round() as u8;
        out[i * 4 + 2] = (clamp01(px[2]) * 255.0).round() as u8;
        out[i * 4 + 3] = if force_opaque {
            255
        } else {
            (clamp01(px[3]) * 255.0).round() as u8
        };
    }
    out
}

/// PNG-encode an RGBA8 buffer for the wire.
///
/// Whole-terminal frames are two to three orders of magnitude larger than the sidebar particle
/// wash's (`src/ui/sidebar/particle_background.rs`) small column, so raw RGBA — fine for that
/// small area — blows straight through `MAX_GRAPHICS_FRAME_SIZE`
/// (`src/server/headless.rs`, 32 MB) once multiplied across a whole animation loop: measured
/// live, a 36-frame 1440×810 loop at raw RGBA is ~224 MB and the server silently drops the
/// entire graphics payload for that pass. This scene's own content — a mostly flat/gradient
/// starfield background with a handful of small disks — is exactly what PNG compresses well:
/// the same loop measures ~3 MB PNG-encoded, comfortably under the cap even at the real 2560×1440
/// target. `Fast` compression is chosen over `Best`: the loop is generated on a resize/topology
/// change, never per tick, so encode time trades against a cache miss that is already rare rather
/// than against steady-state cost.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut encoder = png::Encoder::new(&mut buf, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    match encoder.write_header() {
        Ok(mut writer) => {
            if writer.write_image_data(rgba).is_err() {
                return Vec::new();
            }
        }
        Err(_) => return Vec::new(),
    }
    buf
}

fn normalize3(v: (f32, f32, f32)) -> (f32, f32, f32) {
    let len = (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt().max(0.0001);
    (v.0 / len, v.1 / len, v.2 / len)
}

/// Background gradient plus starfield, threaded across row bands — the one genuinely full-frame
/// pass this generator has, everything else (bodies, craters, asteroids, comets) is bounded to
/// its own small bounding box regardless of frame size.
fn render_bands(buf: &mut [[f32; 4]], width: u32, height: u32, phase: f32) {
    let rows = height as usize;
    let threads = field_threads(rows);
    if threads <= 1 {
        render_band(buf, width, height, 0, rows, phase);
        return;
    }

    let band_rows = rows.div_ceil(threads);
    let bands: Vec<&mut [[f32; 4]]> = buf.chunks_mut(width as usize * band_rows).collect();
    std::thread::scope(|scope| {
        for (band_idx, band) in bands.into_iter().enumerate() {
            let y0 = band_idx * band_rows;
            let y1 = (y0 + band_rows).min(rows);
            scope.spawn(move || {
                render_band(band, width, height, y0, y1, phase);
            });
        }
    });
}

fn render_band(buf: &mut [[f32; 4]], width: u32, height: u32, y0: usize, y1: usize, phase: f32) {
    let base = (
        SPACE_SURFACE.0 as f32 / 255.0,
        SPACE_SURFACE.1 as f32 / 255.0,
        SPACE_SURFACE.2 as f32 / 255.0,
    );
    // A faint vertical gradient (slightly lighter toward the centre row) so the canvas reads as
    // a depth cue rather than a flat fill, at negligible cost.
    for y in y0..y1 {
        let center_dist = ((y as f32 - height as f32 / 2.0) / (height as f32 / 2.0).max(1.0)).abs();
        let lift = mix(0.06, 0.0, center_dist);
        for x in 0..width as usize {
            let local_idx = (y - y0) * width as usize + x;
            buf[local_idx] = [base.0 + lift, base.1 + lift, base.2 + lift + 0.01, 1.0];
        }
    }
    splat_starfield_band(buf, width, height, y0, y1, phase);
}

fn splat_starfield_band(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    y0: usize,
    y1: usize,
    phase: f32,
) {
    for i in 0..STAR_COUNT {
        let lattice = i as f32 * 0.123;
        let sx = value_noise(lattice, 0.0, 11);
        let sy = value_noise(lattice, 100.0, 11);
        let py = (sy * height as f32) as usize;
        if py < y0 || py >= y1 {
            continue;
        }
        let px = (sx * width as f32) as usize;
        if px >= width as usize {
            continue;
        }

        let magnitude = star_magnitude(i);
        // Only the brightest few scintillate; everything else holds steady, which is what makes
        // the twinkle read as a property of those stars rather than as a shimmer over the sky.
        let twinkle = if magnitude > STAR_SCINTILLATION_MAGNITUDE {
            let offset = value_noise(lattice, 300.0, 11) * 2.0 * PI;
            0.75 + 0.25 * (phase + offset).sin()
        } else {
            1.0
        };
        let alpha = (STAR_ALPHA.0 + STAR_ALPHA.1 * magnitude) * twinkle;

        let class = value_noise(lattice, 400.0, 11) * STAR_CLASS.len() as f32;
        let class = STAR_CLASS[(class as usize).min(STAR_CLASS.len() - 1)];

        let local_idx = (py - y0) * width as usize + px;
        blend(&mut buf[local_idx], class, alpha);
    }
}

/// One star's magnitude, `0.0..=1.0` with `1.0` the brightest — a genuine distribution rather
/// than an even scatter, so most of the field is faint and a handful stand out.
///
/// A pure function of the star's index and this module's fixed seeds, exactly like its position:
/// nothing about the fleet reaches the star layer, and nothing about the frame size does either.
fn star_magnitude(index: usize) -> f32 {
    value_noise(index as f32 * 0.123, 200.0, 11).powf(STAR_MAGNITUDE_SKEW)
}

/// Sample one full ambient-scene animation loop, [`FRAME_COUNT`] frames, phase evenly spaced over
/// `0..=2*PI`. Mirrors `src/particle_field.rs::loop_frames`'s contract exactly, for the same
/// reason: this is what a caller wants to hand to Kitty's native animation-frame transport
/// (`src/kitty_graphics.rs`) and never re-generate until the topology or size actually changes.
pub(crate) fn loop_frames(layout: &SceneLayout, frame_count: usize) -> Vec<Vec<u8>> {
    (0..frame_count)
        .map(|i| {
            let phase = (i as f32 / frame_count as f32) * 2.0 * PI;
            frame(layout, phase)
        })
        .collect()
}

/// [`loop_frames`], PNG-encoded — what a caller actually hands to the wire. See [`encode_png`]
/// for why raw RGBA is not an option at whole-terminal size.
pub(crate) fn loop_frames_png(layout: &SceneLayout, frame_count: usize) -> Vec<Vec<u8>> {
    loop_frames(layout, frame_count)
        .into_iter()
        .map(|rgba| encode_png(layout.width, layout.height, &rgba))
        .collect()
}

/// [`effects_frame`], PNG-encoded — what a caller actually hands to the wire. PNG supports the
/// alpha channel [`effects_frame`] relies on for transparency, so this stays a real overlay.
pub(crate) fn effects_frame_png(
    layout: &SceneLayout,
    effects: &SceneEffects,
    phase: f32,
) -> Vec<u8> {
    let rgba = effects_frame(layout, effects, phase);
    encode_png(layout.width, layout.height, &rgba)
}

/// Average the composite (ambient + effects, alpha-over-composited) RGB colour under each
/// terminal cell, for `src/app/background_legibility.rs`'s per-cell text-contrast sampling.
///
/// `ambient`/`effects` are exactly [`frame`]'s and [`effects_frame`]'s own packed RGBA8 output,
/// sized `width`x`height`. Effects is composited over ambient using its own real alpha — exactly
/// as a client displaying both layers would show them — so a comet crossing a cell changes that
/// cell's sampled colour the same way it changes what's actually on screen. `cols`x`rows` is the
/// terminal-cell grid `width`x`height` divides into (the background scene canvas is always built
/// as an exact multiple of the host cell size, `App::observe_background_scene`).
pub(crate) fn sample_cell_backgrounds(
    ambient: &[u8],
    effects: &[u8],
    width: u32,
    height: u32,
    cell_width_px: u32,
    cell_height_px: u32,
    cols: u32,
    rows: u32,
) -> Vec<(u8, u8, u8)> {
    let cell_count = (cols as usize) * (rows as usize);
    if cell_width_px == 0 || cell_height_px == 0 || cell_count == 0 {
        return vec![SPACE_SURFACE; cell_count];
    }

    let mut sums = vec![[0u32; 3]; cell_count];
    let mut counts = vec![0u32; cell_count];

    for y in 0..height as usize {
        let row = ((y as u32 / cell_height_px).min(rows - 1)) as usize;
        for x in 0..width as usize {
            let col = ((x as u32 / cell_width_px).min(cols - 1)) as usize;
            let px_idx = (y * width as usize + x) * 4;

            let effects_alpha = f32::from(effects[px_idx + 3]) / 255.0;
            let composite = |channel: usize| {
                let ambient_c = f32::from(ambient[px_idx + channel]);
                let effects_c = f32::from(effects[px_idx + channel]);
                effects_c * effects_alpha + ambient_c * (1.0 - effects_alpha)
            };

            let cell_idx = row * cols as usize + col;
            let sum = &mut sums[cell_idx];
            sum[0] += composite(0) as u32;
            sum[1] += composite(1) as u32;
            sum[2] += composite(2) as u32;
            counts[cell_idx] += 1;
        }
    }

    sums.into_iter()
        .zip(counts)
        .map(|(sum, count)| {
            (
                sum[0]
                    .checked_div(count)
                    .unwrap_or(u32::from(SPACE_SURFACE.0)) as u8,
                sum[1]
                    .checked_div(count)
                    .unwrap_or(u32::from(SPACE_SURFACE.1)) as u8,
                sum[2]
                    .checked_div(count)
                    .unwrap_or(u32::from(SPACE_SURFACE.2)) as u8,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(parent: Option<usize>, kind: BodyKind) -> TreeNode {
        TreeNode {
            parent,
            kind,
            hue: 41.0,
            severity: Severity::Clear,
            size: BodySize::Fixed,
            streak: 0.0,
        }
    }

    /// A body inside the project-size register, for the tests that are about the register itself.
    fn sized(parent: Option<usize>, kind: BodyKind, size: BodySize) -> TreeNode {
        TreeNode {
            size,
            ..node(parent, kind)
        }
    }

    /// Hue of the `Done`/idle lifecycle stage — the green every body in a quiet fleet resolves
    /// to, and the case that exposed the green-sun-beside-a-green-planet bug.
    const IDLE_HUE: f32 = 115.0;
    /// Hue of the `Failed` stage, used to prove the sun ignores stage entirely.
    const FAILED_HUE: f32 = 343.0;

    fn body(parent: Option<usize>, kind: BodyKind, hue: f32, severity: Severity) -> TreeNode {
        TreeNode {
            parent,
            kind,
            hue,
            severity,
            size: BodySize::Fixed,
            streak: 0.0,
        }
    }

    /// A real spread of checkouts, as measured on the box this register was designed against: a
    /// two-file scratch project and herdr itself.
    const TINY_PROJECT: u32 = 2;
    const BIG_PROJECT: u32 = 2_470;

    /// The radius one body draws at, in a scene whose `min(width, height)` is exactly 1,000 — so a
    /// pixel radius reads directly as the fraction, times a thousand.
    fn radius_of(kind: BodyKind, size: BodySize) -> f32 {
        let nodes = [node(None, BodyKind::Sun), sized(Some(0), kind, size)];
        build_layout(&nodes, 1_000, 1_000).bodies[1].body_radius_px
    }

    #[test]
    fn a_bigger_project_draws_a_bigger_planet() {
        let tiny = radius_of(BodyKind::Planet, BodySize::Files(TINY_PROJECT));
        let big = radius_of(BodyKind::Planet, BodySize::Files(BIG_PROJECT));

        // Not merely ordered — *visibly* bigger. The whole point of the register is that the
        // difference survives being looked at, so a monotonicity assertion alone would pass on a
        // spread of a tenth of a pixel and prove nothing anyone could see.
        assert!(
            big > tiny * 1.5,
            "a 2,470-file project should visibly outdraw a 2-file one: {big} vs {tiny}"
        );
        assert!(
            radius_of(BodyKind::Planet, BodySize::Files(500))
                > radius_of(BodyKind::Planet, BodySize::Files(100)),
            "the register has to be monotonic in between, not only at its ends"
        );
    }

    #[test]
    fn an_unmeasured_project_is_floored_rather_than_collapsed() {
        let unmeasured = radius_of(BodyKind::Planet, BodySize::Unmeasured);
        let biggest = radius_of(BodyKind::Planet, BodySize::Files(FILES_CEIL));

        // A project nobody has measured is not a project with no files. It lands on the floor,
        // which is a real body — a little over 40% of the ceiling, ten pixels across at this
        // scene size — and never on zero.
        assert!(unmeasured > 0.0);
        assert!(
            unmeasured > biggest * 0.4,
            "the floor should be a body, not a dot: {unmeasured} against a ceiling of {biggest}"
        );
        // Zero files is the same case by construction, which is what makes the floor the one
        // place the two are absorbed.
        assert_eq!(unmeasured, radius_of(BodyKind::Planet, BodySize::Files(0)));
        assert!(radius_of(BodyKind::Moon, BodySize::Unmeasured) > 0.0);
    }

    #[test]
    fn the_register_saturates_at_its_stated_ceiling() {
        let at_ceiling = radius_of(BodyKind::Planet, BodySize::Files(FILES_CEIL));
        let far_past_it = radius_of(BodyKind::Planet, BodySize::Files(99_999));

        assert_eq!(at_ceiling, far_past_it);
        // And the ceiling is the bound that matters: no planet ever rivals the sun.
        assert!(at_ceiling <= radius_of(BodyKind::Sun, BodySize::Fixed) / 2.0);
    }

    /// The largest radius any moon can reach, over both of the two ways a moon is sized: a worker
    /// (outside the register, [`BodySize::Fixed`]) and a nested project maxed out at the
    /// register's ceiling. F16's clause names "the largest worker moon", so a scene where a
    /// *nested-project* moon could outdraw a worker one would still have to answer for it.
    fn largest_moon_radius() -> f32 {
        radius_of(BodyKind::Moon, BodySize::Fixed)
            .max(radius_of(BodyKind::Moon, BodySize::Files(FILES_CEIL)))
            .max(radius_of(BodyKind::Moon, BodySize::Files(u32::MAX)))
    }

    /// The smallest radius any planet can reach — the register's floor, which is where both an
    /// unmeasured project and an empty one land.
    fn smallest_planet_radius() -> f32 {
        radius_of(BodyKind::Planet, BodySize::Unmeasured)
            .min(radius_of(BodyKind::Planet, BodySize::Files(0)))
            .min(radius_of(BodyKind::Planet, BodySize::Files(1)))
    }

    #[test]
    fn no_moon_ever_draws_half_the_smallest_planet() {
        // F16's second clause, in its equal-depth reading: the smallest second mate on screen has
        // to be at least twice the largest worker moon. This is the assertion the flat `0.01125`
        // moon ceiling failed — it drew at 1.07x the register floor, so a maxed-out nested project
        // rendered as a moon *bigger than* a planet beside it.
        let moon = largest_moon_radius();
        let planet = smallest_planet_radius();
        assert!(
            planet >= moon * 2.0,
            "the smallest planet ({planet}) is not twice the largest moon ({moon}) — \
             a moon can outdraw a planet, which F16 forbids"
        );

        // ...and the bound is answered by *solving* it, not by leaving a chasm: the largest moon
        // still reaches most of the way to the bound it is held under, so the moon tier is as big
        // as F16 permits rather than shrunk into a dot to make the assertion easy.
        assert!(
            moon * 2.0 > planet * 0.9,
            "the moon ceiling ({moon}) is far under its own bound ({}) — re-solve it up",
            planet / 2.0
        );
    }

    #[test]
    fn the_moon_bound_tracks_the_register_rather_than_a_frozen_constant() {
        // The bound is a function of the register's floor, so the two tiers cannot drift apart the
        // way a pair of hand-picked constants can. Proving that means proving the moon ceiling is
        // *derived*: it must sit at exactly half the planet floor (less its stated headroom),
        // whatever those two happen to be.
        let expected = mate_radius_floor() * 0.5 * MOON_HEADROOM;
        assert!(
            (BodyKind::Moon.max_radius_fraction() - expected).abs() < 1e-9,
            "the moon ceiling has been pinned to a literal again"
        );
        // And a worker moon keeps its proportion to that ceiling rather than its own literal.
        assert!(
            (BodyKind::Moon.fixed_radius_fraction()
                - BodyKind::Moon.max_radius_fraction() * WORKER_MOON_OF_CEILING)
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn the_sun_and_the_workers_stay_out_of_the_register() {
        // The sun routes to projects rather than being one, so no size a caller hands it moves it.
        let locked = radius_of(BodyKind::Sun, BodySize::Fixed);
        assert_eq!(
            locked,
            radius_of(BodyKind::Sun, BodySize::Files(FILES_CEIL))
        );
        assert_eq!(locked, radius_of(BodyKind::Sun, BodySize::Unmeasured));

        // A worker is not a checkout either: a `Fixed` body ignores the register entirely and
        // draws one radius whatever it is handed.
        assert_eq!(radius_of(BodyKind::Planet, BodySize::Fixed), 20.0);
        let worker_moon = radius_of(BodyKind::Moon, BodySize::Fixed);
        assert_eq!(worker_moon, radius_of(BodyKind::Moon, BodySize::Fixed));
        assert!(worker_moon > 0.0);
    }

    #[test]
    fn the_biggest_real_checkout_still_draws_what_the_flat_constant_drew() {
        // The register spread the *planet* tier around what was already there rather than
        // rescaling it: the largest checkout measured on this box lands within ~1.5% of the flat
        // constant every planet used to get, so nothing about the composition moved to make room
        // for it. The moon tier is the one place that is no longer true, because F16's bound
        // turned out to be the thing the old flat moon constants were violating — see
        // `no_moon_ever_draws_half_the_smallest_planet`.
        let planet = radius_of(BodyKind::Planet, BodySize::Files(BIG_PROJECT));
        assert!((planet - 20.0).abs() < 20.0 * 0.015, "{planet}");
    }

    #[test]
    fn a_moon_is_still_a_body_at_the_real_target_resolution() {
        // Re-solving the ceiling down is only correct if what is left is still a rendered sphere.
        // At the captain's confirmed 1440p target the smallest moon in the scene — a worker, which
        // is the tier that shrank — has to keep enough pixels to carry the shading, terminator and
        // rim `shade_surface` puts on it, or the fix has traded one visual failure for another.
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
        ];
        let layout = build_layout(&nodes, 2560, 1440);
        let moon = layout.body_radius_px(2).unwrap_or(0.0);
        assert!(
            moon >= 4.0,
            "a worker moon draws {moon}px across the radius at 1440p — too few to shade"
        );
        // And it still clears its parent's own limb rather than orbiting inside it.
        let planet = layout.body_radius_px(1).unwrap_or(0.0);
        let orbit = BodyKind::Moon.orbit_radius_fraction() * 1440.0;
        assert!(orbit > planet + moon, "a moon orbits inside its own planet");
    }

    /// Sample one rendered pixel of `frame`'s RGBA8 output as `(r, g, b)`.
    fn pixel_at(rgba: &[u8], width: u32, pos: (f32, f32)) -> (u8, u8, u8) {
        let idx = (pos.1.round() as usize * width as usize + pos.0.round() as usize) * 4;
        (rgba[idx], rgba[idx + 1], rgba[idx + 2])
    }

    #[test]
    fn the_sun_holds_a_warm_star_colour_while_an_idle_planet_stays_green() {
        // The exact fleet that exposed the bug: everything idle, so every body resolved to the
        // same lifecycle green and the sun rendered as just another green disk.
        let nodes = [
            body(None, BodyKind::Sun, IDLE_HUE, Severity::Clear),
            body(Some(0), BodyKind::Planet, IDLE_HUE, Severity::Clear),
        ];
        let (width, height) = (400u32, 300u32);
        let layout = build_layout(&nodes, width, height);
        let phase = 0.0;
        let rgba = frame(&layout, phase);

        let sun = pixel_at(&rgba, width, layout.position(0, phase));
        let planet = pixel_at(&rgba, width, layout.position(1, phase));

        // The sun reads as a warm star: bright, and warm-biased rather than green-biased.
        assert!(
            sun.0 > sun.1 && sun.1 > sun.2,
            "sun should be warm (r > g > b), got {sun:?}"
        );
        assert!(sun.0 > 180, "sun should read as a bright star, got {sun:?}");

        // The planet is untouched by this change: still green-dominant for the idle stage.
        assert!(
            planet.1 > planet.0 && planet.1 > planet.2,
            "idle planet should still be green-dominant, got {planet:?}"
        );

        // And the two no longer collapse onto the same colour.
        let distance = (sun.0 as i32 - planet.0 as i32).abs()
            + (sun.1 as i32 - planet.1 as i32).abs()
            + (sun.2 as i32 - planet.2 as i32).abs();
        assert!(
            distance > 90,
            "sun {sun:?} and idle planet {planet:?} should not render as the same colour"
        );
    }

    #[test]
    fn the_sun_renders_the_same_colour_whatever_the_fleet_is_doing() {
        let (width, height) = (400u32, 300u32);
        let phase = 0.0;

        let idle = [
            body(None, BodyKind::Sun, IDLE_HUE, Severity::Clear),
            body(Some(0), BodyKind::Planet, IDLE_HUE, Severity::Clear),
        ];
        let failing = [
            body(None, BodyKind::Sun, FAILED_HUE, Severity::Critical),
            body(Some(0), BodyKind::Planet, FAILED_HUE, Severity::Critical),
        ];

        let idle_layout = build_layout(&idle, width, height);
        let failing_layout = build_layout(&failing, width, height);
        let idle_rgba = frame(&idle_layout, phase);
        let failing_rgba = frame(&failing_layout, phase);

        let idle_sun = pixel_at(&idle_rgba, width, idle_layout.position(0, phase));
        let failing_sun = pixel_at(&failing_rgba, width, failing_layout.position(0, phase));
        assert_eq!(
            idle_sun, failing_sun,
            "the sun's colour must not track lifecycle stage or severity"
        );

        // The planets, meanwhile, must still differ — the exemption is sun-only.
        let idle_planet = pixel_at(&idle_rgba, width, idle_layout.position(1, phase));
        let failing_planet = pixel_at(&failing_rgba, width, failing_layout.position(1, phase));
        assert_ne!(
            idle_planet, failing_planet,
            "planets must keep tracking lifecycle stage and severity"
        );
    }

    #[test]
    fn a_lone_sun_sits_at_the_frame_centre() {
        let nodes = [node(None, BodyKind::Sun)];
        let layout = build_layout(&nodes, 400, 300);
        assert_eq!(layout.position(0, 0.0), (200.0, 150.0));
        assert_eq!(layout.position(0, 3.0), (200.0, 150.0));
    }

    #[test]
    fn a_planet_orbits_the_sun_at_a_fixed_radius() {
        let nodes = [node(None, BodyKind::Sun), node(Some(0), BodyKind::Planet)];
        let layout = build_layout(&nodes, 1000, 1000);
        let sun = layout.position(0, 0.7);
        let planet = layout.position(1, 0.7);
        let dist = ((planet.0 - sun.0).powi(2) + (planet.1 - sun.1).powi(2)).sqrt();
        let expected = BodyKind::Planet.orbit_radius_fraction() * 1000.0;
        assert!(
            (dist - expected).abs() < 0.01,
            "dist={dist} expected={expected}"
        );
    }

    #[test]
    fn a_moon_orbits_its_planet_not_the_sun() {
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
        ];
        let layout = build_layout(&nodes, 1000, 1000);
        let planet = layout.position(1, 1.2);
        let moon = layout.position(2, 1.2);
        let dist = ((moon.0 - planet.0).powi(2) + (moon.1 - planet.1).powi(2)).sqrt();
        assert!(dist < BodyKind::Planet.orbit_radius_fraction() * 1000.0);
        assert!(dist > 0.0);
    }

    #[test]
    fn orbits_land_back_on_their_starting_angle_after_one_full_loop() {
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
        ];
        let layout = build_layout(&nodes, 800, 800);
        for idx in 0..nodes.len() {
            let start = layout.position(idx, 0.0);
            let looped = layout.position(idx, 2.0 * PI);
            assert!((start.0 - looped.0).abs() < 0.01);
            assert!((start.1 - looped.1).abs() < 0.01);
        }
    }

    #[test]
    fn frame_is_the_right_size_with_opaque_alpha() {
        let nodes = [node(None, BodyKind::Sun), node(Some(0), BodyKind::Planet)];
        let layout = build_layout(&nodes, 64, 48);
        let out = frame(&layout, 0.0);
        assert_eq!(out.len(), 64 * 48 * 4);
        assert!(
            out.chunks(4).all(|px| px[3] == 255),
            "the ambient scene is a full opaque backdrop"
        );
    }

    #[test]
    fn frame_is_deterministic_for_the_same_phase() {
        let nodes = [node(None, BodyKind::Sun), node(Some(0), BodyKind::Planet)];
        let layout = build_layout(&nodes, 96, 64);
        assert_eq!(frame(&layout, 1.5), frame(&layout, 1.5));
    }

    #[test]
    fn effects_frame_is_transparent_wherever_nothing_is_drawn() {
        let nodes = [node(None, BodyKind::Sun), node(Some(0), BodyKind::Planet)];
        let layout = build_layout(&nodes, 96, 64);
        let out = effects_frame(&layout, &SceneEffects::default(), 0.0);
        assert!(
            out.chunks(4).all(|px| px[3] == 0),
            "an overlay with nothing live must be fully transparent, not a second copy of the scene"
        );
    }

    #[test]
    fn frame_is_identical_across_thread_counts() {
        // `field_threads` reads real machine parallelism, so this asserts the *contract*
        // directly by calling the single-band and multi-band paths against the same buffer size
        // rather than trying to force a specific thread count.
        let mut threaded = vec![[0.0f32; 4]; 200 * 150];
        render_bands(&mut threaded, 200, 150, 0.9);
        let mut single = vec![[0.0f32; 4]; 200 * 150];
        render_band(&mut single, 200, 150, 0, 150, 0.9);
        assert_eq!(threaded, single);
    }

    #[test]
    fn the_starfield_is_many_faint_stars_and_a_few_bright_ones() {
        let magnitudes: Vec<f32> = (0..STAR_COUNT).map(star_magnitude).collect();
        let faint = magnitudes.iter().filter(|m| **m < 0.25).count();
        let bright = magnitudes.iter().filter(|m| **m > 0.8).count();

        // A real magnitude distribution, not an even scatter: the great majority of the field is
        // faint and only a handful stand out. An even scatter would put ~75% above 0.25.
        assert!(
            faint > STAR_COUNT / 2,
            "{faint} of {STAR_COUNT} stars are faint — the field is an even scatter, not a distribution"
        );
        assert!(
            (1..STAR_COUNT / 20).contains(&bright),
            "{bright} of {STAR_COUNT} stars are bright — 'very few' has to be some, and few"
        );
    }

    #[test]
    fn only_the_brightest_few_stars_scintillate() {
        let mut magnitudes: Vec<f32> = (0..STAR_COUNT).map(star_magnitude).collect();
        let scintillating = magnitudes
            .iter()
            .filter(|m| **m > STAR_SCINTILLATION_MAGNITUDE)
            .count();
        magnitudes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // The bar is the *few*, not the constant: a threshold that names none loses the twinkle
        // entirely, and one that names most of the sky is a shimmer over everything.
        assert!(
            (3..=20).contains(&scintillating),
            "{scintillating} of {STAR_COUNT} stars scintillate"
        );
        // And they really are the brightest ones — nothing below the top decile qualifies.
        let top_decile = magnitudes[STAR_COUNT - STAR_COUNT / 10];
        assert!(
            STAR_SCINTILLATION_MAGNITUDE >= top_decile,
            "the cut at {STAR_SCINTILLATION_MAGNITUDE} reaches below the top decile at {top_decile}"
        );
    }

    #[test]
    fn the_rendered_starfield_is_not_one_flat_white() {
        // Nothing but the void and the stars, so every pixel above the background is a star.
        let (width, height) = (600u32, 400u32);
        let layout = build_layout(&[], width, height);
        let rgba = frame(&layout, 0.0);

        let mut coloured = 0usize;
        let mut brightest = 0u8;
        let mut faintest = u8::MAX;
        for px in rgba.chunks_exact(4) {
            let (r, g, b) = (px[0], px[1], px[2]);
            let peak = r.max(g).max(b);
            // Comfortably above the void and its faint centre-row lift.
            if peak < 60 {
                continue;
            }
            brightest = brightest.max(peak);
            faintest = faintest.min(peak);
            if peak - r.min(g).min(b) >= 10 {
                coloured += 1;
            }
        }

        assert!(
            coloured >= 8,
            "only {coloured} star pixels carry any colour temperature at all"
        );
        assert!(
            brightest as u16 > faintest as u16 * 2,
            "every star drew at about the same brightness ({faintest}..{brightest})"
        );
    }

    /// Mean shaded colour across a short vertical run at horizontal position `nx` (in normalised
    /// body-radius units), so the surface texture's own mottling averages out and the shading
    /// term under test is what is left.
    ///
    /// Lit from `(1, 0, 0)` — straight along +x and in the screen plane. `light_dir_toward` always
    /// tilts a real body's light toward the eye, which puts the terminator off-centre; a purely
    /// in-plane light instead puts `lambert == nx` exactly, so a sample position names the point
    /// on the day/night curve it is testing.
    fn shaded_mean(nx: f32) -> (f32, f32, f32) {
        const RADIUS: f32 = 100.0;
        const BASE: (f32, f32, f32) = (0.8, 0.8, 0.8);
        let mut sum = (0.0, 0.0, 0.0);
        let samples = 9;
        for i in 0..samples {
            let ny = (i as f32 / (samples - 1) as f32 - 0.5) * 0.16;
            let (r, g, b) = shade_surface(
                nx * RADIUS,
                ny * RADIUS,
                RADIUS,
                (1.0, 0.0, 0.0),
                Surface {
                    base: BASE,
                    seed: 17,
                    self_luminous: false,
                    bands: None,
                    mottle_scale: 1.0,
                },
            );
            sum = (sum.0 + r, sum.1 + g, sum.2 + b);
        }
        let n = samples as f32;
        (sum.0 / n, sum.1 / n, sum.2 / n)
    }

    fn luminance(rgb: (f32, f32, f32)) -> f32 {
        rgb.0 * 0.299 + rgb.1 * 0.587 + rgb.2 * 0.114
    }

    #[test]
    fn the_sunlit_limb_carries_an_atmospheric_rim() {
        let limb = luminance(shaded_mean(0.97));
        let inside = luminance(shaded_mean(0.75));

        // Limb darkening alone makes the very edge of a body the *dimmest* part of its lit face.
        // The rim is what turns that edge into the brightest crescent on the body, which is the
        // single most photographic detail a planet carries.
        assert!(
            limb > inside * 1.25,
            "the sunlit limb ({limb:.3}) barely outdraws the face inside it ({inside:.3})"
        );
    }

    #[test]
    fn the_night_side_carries_returned_light_rather_than_a_flat_fill() {
        let night_limb = luminance(shaded_mean(-0.97));
        let deep_night = luminance(shaded_mean(-0.45));

        // Under a flat ambient fill this comparison runs the other way: limb darkening makes the
        // night limb the darkest place on the body. Planetshine reverses it, which is the whole
        // difference between a body in a system and a body in a lit box.
        assert!(
            night_limb > deep_night * 1.08,
            "the night limb ({night_limb:.3}) is no brighter than the night face ({deep_night:.3})"
        );
    }

    #[test]
    fn the_terminator_is_a_soft_warm_transition() {
        let terminator = shaded_mean(0.0);
        let day = shaded_mean(0.6);
        let night = luminance(shaded_mean(-0.35));

        // Soft: at the day/night line itself the surface is still measurably lit. A hard
        // `lambert.max(0.0)` puts it exactly on the night floor instead.
        assert!(
            luminance(terminator) > night * 1.15,
            "the terminator ({:.3}) sits on the night floor ({night:.3})",
            luminance(terminator)
        );
        // Warm: grazing light travels a longer path through an atmosphere and comes out red, and
        // the transition is the only place on the body that happens.
        let warm_ratio = |rgb: (f32, f32, f32)| rgb.0 / rgb.2;
        assert!(
            warm_ratio(terminator) > warm_ratio(day) * 1.3,
            "the terminator is no warmer than the day side: {:.3} against {:.3}",
            warm_ratio(terminator),
            warm_ratio(day)
        );
        // And the warmth stays inside the band — the day face keeps the body's own colour.
        assert!(
            (warm_ratio(day) - 1.0).abs() < 0.02,
            "the day side has been tinted too: {:.3}",
            warm_ratio(day)
        );
    }

    #[test]
    fn a_fresh_crater_darkens_its_moon_and_a_fully_faded_one_does_not() {
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
        ];
        let layout = build_layout(&nodes, 600, 600);
        let fresh = SceneEffects {
            craters: vec![Crater {
                body: 2,
                angle_on_surface: 0.0,
                severity: Severity::Critical,
                age: 0.0,
                is_ripple: false,
            }],
            ..Default::default()
        };
        let faded = SceneEffects {
            craters: vec![Crater {
                body: 2,
                angle_on_surface: 0.0,
                severity: Severity::Critical,
                age: 1.0,
                is_ripple: false,
            }],
            ..Default::default()
        };
        let clean = SceneEffects::default();
        let fresh_frame = effects_frame(&layout, &fresh, 0.0);
        let faded_frame = effects_frame(&layout, &faded, 0.0);
        let clean_frame = effects_frame(&layout, &clean, 0.0);
        assert_ne!(
            fresh_frame, clean_frame,
            "a fresh crater must change the frame"
        );
        assert_eq!(
            faded_frame, clean_frame,
            "a fully faded crater must match no crater at all"
        );
    }

    #[test]
    fn a_moons_crater_also_leaves_a_fainter_echo_on_its_parent_planet() {
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
        ];
        let layout = build_layout(&nodes, 600, 600);
        let struck = SceneEffects {
            craters: vec![Crater {
                body: 2,
                angle_on_surface: 0.5,
                severity: Severity::Critical,
                age: 0.0,
                is_ripple: false,
            }],
            ..Default::default()
        };
        let clean = SceneEffects::default();

        // Render only the planet's own bounding box: nothing about the moon's crater itself
        // should be visible there, only its ripple.
        let struck_frame = effects_frame(&layout, &struck, 0.0);
        let clean_frame = effects_frame(&layout, &clean, 0.0);
        let planet_pos = layout.position(1, 0.0);
        let planet_radius = layout.bodies[1].body_radius_px;
        let mut differs_near_planet = false;
        let cx = planet_pos.0 as i32;
        let cy = planet_pos.1 as i32;
        let r = (planet_radius * 1.2) as i32;
        for y in (cy - r).max(0)..(cy + r).min(600) {
            for x in (cx - r).max(0)..(cx + r).min(600) {
                let idx = (y as usize * 600 + x as usize) * 4;
                if struck_frame[idx..idx + 3] != clean_frame[idx..idx + 3] {
                    differs_near_planet = true;
                }
            }
        }
        assert!(
            differs_near_planet,
            "a struck moon's parent planet must show a visible ripple"
        );
    }

    #[test]
    fn an_asteroid_approaches_its_target_and_a_comet_crosses_the_frame() {
        let nodes = [node(None, BodyKind::Sun), node(Some(0), BodyKind::Planet)];
        let layout = build_layout(&nodes, 300, 300);
        let mid_flight = SceneEffects {
            asteroids: vec![AsteroidInFlight {
                target: 1,
                severity: Severity::Serious,
                progress: 0.5,
                approach_angle: 0.0,
            }],
            ..Default::default()
        };
        let no_effects = SceneEffects::default();
        assert_ne!(
            effects_frame(&layout, &mid_flight, 0.0),
            effects_frame(&layout, &no_effects, 0.0)
        );

        let mid_comet = SceneEffects {
            comets: vec![Comet {
                start: (0.0, 0.0),
                end: (1.0, 1.0),
                target: None,
                magnitude: 0.8,
                progress: 0.5,
            }],
            ..Default::default()
        };
        assert_ne!(
            effects_frame(&layout, &mid_comet, 0.0),
            effects_frame(&layout, &no_effects, 0.0)
        );
    }

    #[test]
    fn deep_nesting_stays_inside_the_canvas() {
        // Sun -> planet -> moon -> moon's own worker: four levels, the third orbit past the cap.
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
            node(Some(2), BodyKind::Moon),
        ];
        let layout = build_layout(&nodes, 1000, 1000);
        for idx in 0..nodes.len() {
            let (x, y) = layout.position(idx, 1.0);
            assert!((0.0..=1000.0).contains(&x), "x={x} out of canvas");
            assert!((0.0..=1000.0).contains(&y), "y={y} out of canvas");
        }
    }

    #[test]
    fn empty_layout_produces_an_empty_frame_without_panicking() {
        let layout = build_layout(&[], 10, 10);
        assert!(layout.is_empty());
        assert_eq!(frame(&layout, 0.0).len(), 10 * 10 * 4);
        assert_eq!(
            effects_frame(&layout, &SceneEffects::default(), 0.0).len(),
            10 * 10 * 4
        );
    }

    #[test]
    fn sample_cell_backgrounds_reads_a_flat_ambient_colour_per_cell() {
        let width = 8u32;
        let height = 4u32;
        let mut ambient = vec![0u8; (width * height * 4) as usize];
        for px in ambient.chunks_mut(4) {
            px.copy_from_slice(&[40, 60, 80, 255]);
        }
        let effects = vec![0u8; (width * height * 4) as usize]; // fully transparent: no overlay
        let cols = 4;
        let rows = 2;
        let samples = sample_cell_backgrounds(&ambient, &effects, width, height, 2, 2, cols, rows);
        assert_eq!(samples.len(), (cols * rows) as usize);
        for sample in samples {
            assert_eq!(sample, (40, 60, 80));
        }
    }

    #[test]
    fn sample_cell_backgrounds_blends_a_fully_opaque_effect_over_the_ambient_layer() {
        let width = 4u32;
        let height = 4u32;
        let ambient = [10u8, 10, 10, 255].repeat((width * height) as usize);
        // One fully-opaque bright pixel in the top-left cell, transparent everywhere else.
        let mut effects = vec![0u8; (width * height * 4) as usize];
        effects[0..4].copy_from_slice(&[255, 255, 255, 255]);
        let samples = sample_cell_backgrounds(&ambient, &effects, width, height, 2, 2, 2, 2);
        // The top-left cell's 4 pixels average 3 dark ambient pixels and 1 bright effect pixel.
        assert_eq!(samples[0], (71, 71, 71));
        // Every other cell is untouched ambient.
        assert_eq!(samples[1], (10, 10, 10));
        assert_eq!(samples[2], (10, 10, 10));
        assert_eq!(samples[3], (10, 10, 10));
    }

    /// A representative mid-size fleet: one sun, four planets, three moons each.
    /// A moon with a planet parent, in a scene big enough that a moon is several pixels across —
    /// the shared fixture for the effect-pixel tests below.
    fn struck_moon_scene() -> (SceneLayout, usize, usize) {
        let nodes = [
            node(None, BodyKind::Sun),
            node(Some(0), BodyKind::Planet),
            node(Some(1), BodyKind::Moon),
        ];
        (build_layout(&nodes, 1600, 900), 2, 1)
    }

    /// Highest effects-layer alpha anywhere inside a disk of `radius` about `centre`. Reading the
    /// real rendered buffer rather than re-deriving the formula is the point: these assert what
    /// actually lands on screen.
    fn peak_alpha(rgba: &[u8], width: u32, centre: (f32, f32), radius: f32) -> f32 {
        let height = rgba.len() / 4 / width as usize;
        let mut peak = 0.0f32;
        for y in 0..height {
            for x in 0..width as usize {
                let dx = x as f32 + 0.5 - centre.0;
                let dy = y as f32 + 0.5 - centre.1;
                if (dx * dx + dy * dy).sqrt() > radius {
                    continue;
                }
                peak = peak.max(f32::from(rgba[(y * width as usize + x) * 4 + 3]) / 255.0);
            }
        }
        peak
    }

    fn crater_only(body: usize, age: f32) -> SceneEffects {
        SceneEffects {
            craters: vec![Crater {
                body,
                angle_on_surface: 0.9,
                severity: Severity::Critical,
                age,
                is_ripple: false,
            }],
            ..Default::default()
        }
    }

    /// Q2's fade curve: `(1 - age)^0.42` is an ease-out, so a scar at the halfway point of its
    /// 45s budget still carries most of the opacity it landed with — where a linear fade would be
    /// down to exactly half. Measured on the rendered buffer, not on the expression.
    #[test]
    fn the_crater_fade_holds_the_scar_longer_than_linear() {
        let (layout, moon, _) = struck_moon_scene();
        let phase = 0.8;
        let pos = layout.position(moon, phase);
        let radius = layout.bodies[moon].body_radius_px * 1.5;

        let fresh = peak_alpha(
            &effects_frame(&layout, &crater_only(moon, 0.0), phase),
            layout.width,
            pos,
            radius,
        );
        let halfway = peak_alpha(
            &effects_frame(&layout, &crater_only(moon, 0.5), phase),
            layout.width,
            pos,
            radius,
        );
        assert!(fresh > 0.0, "a fresh crater draws nothing at all");

        let retained = halfway / fresh;
        // Linear would be 0.50 here; the approved curve is 0.5^0.42 ≈ 0.75.
        assert!(
            retained > 0.65,
            "a half-aged crater retained only {retained:.2} of its opacity — that is the linear \
             fade this curve replaced"
        );
        assert!(
            retained < 0.85,
            "a half-aged crater retained {retained:.2} — it is barely fading at all"
        );

        // The budget itself is unchanged: the scar still clears completely at the end.
        assert_eq!(
            peak_alpha(
                &effects_frame(&layout, &crater_only(moon, 1.0), phase),
                layout.width,
                pos,
                radius
            ),
            0.0
        );
    }

    /// F3: the echo on a struck moon's parent has to be visible, not merely present in the
    /// buffer. It still has to read as the *fainter* half of the pair.
    #[test]
    fn a_parent_planet_ripple_reads_as_more_than_a_trace() {
        let (layout, moon, planet) = struck_moon_scene();
        let phase = 0.8;
        let rgba = effects_frame(&layout, &crater_only(moon, 0.05), phase);

        let on_planet = peak_alpha(
            &rgba,
            layout.width,
            layout.position(planet, phase),
            layout.bodies[planet].body_radius_px * 1.2,
        );

        // Measured: 0.27 at the current strength, against 0.15 at the round-1 0.35 this replaced.
        // The threshold sits between the two, so a regression back toward the invisible value
        // fails here rather than passing quietly.
        assert!(
            on_planet > 0.21,
            "the parent ripple peaks at {on_planet:.2} alpha — that is the barely-visible 0.35 \
             strength this raised"
        );
        // ...and it is still the *fainter* half of the pair. That comparison has to be made at
        // each mark's own core, not over each body's whole disk: both marks are noise-modulated,
        // so a peak taken over a wide area finds a higher noise sample than one taken over a
        // narrow area, and the parent's patch covers far more pixels than a worker moon's does at
        // the radius F16's bound leaves it (see `moon_radius_ceil`). Sampling the same small disk
        // about each mark's own centre measures which mark is darker rather than which one had
        // more pixels to draw a lucky sample from.
        let mark_core = |body: usize| {
            let pos = layout.position(body, phase);
            let radius = layout.bodies[body].body_radius_px;
            let centre = (
                pos.0 + radius * 0.35 * 0.9f32.cos(),
                pos.1 + radius * 0.35 * 0.9f32.sin(),
            );
            peak_alpha(&rgba, layout.width, centre, CRATER_MIN_FALLOFF_PX)
        };
        let (strike_core, echo_core) = (mark_core(moon), mark_core(planet));
        assert!(
            echo_core < strike_core,
            "the ripple ({echo_core:.2}) is not fainter than the strike itself ({strike_core:.2})"
        );
    }

    /// Q2's ejecta: the whole reason the storyboard picked rays over a crater alone is that a moon
    /// is barely a dozen pixels across at the real target resolution, so the readable part of an
    /// impact has to happen *outside* the struck body's own disk.
    #[test]
    fn an_impact_throws_rays_clear_of_the_struck_body() {
        let (layout, moon, _) = struck_moon_scene();
        let phase = 0.8;
        let pos = layout.position(moon, phase);
        let body_radius = layout.bodies[moon].body_radius_px;

        let burst = SceneEffects {
            ejecta: vec![Ejecta {
                body: moon,
                angle_on_surface: 0.9,
                severity: Severity::Critical,
                age: 0.0,
            }],
            ..Default::default()
        };
        let rgba = effects_frame(&layout, &burst, phase);

        let mut past_limb = 0usize;
        let mut furthest = 0.0f32;
        for y in 0..layout.height as usize {
            for x in 0..layout.width as usize {
                if rgba[(y * layout.width as usize + x) * 4 + 3] < 8 {
                    continue;
                }
                let dx = x as f32 + 0.5 - pos.0;
                let dy = y as f32 + 0.5 - pos.1;
                let dist = (dx * dx + dy * dy).sqrt() / body_radius;
                if dist > 1.0 {
                    past_limb += 1;
                }
                furthest = furthest.max(dist);
            }
        }
        assert!(
            past_limb > 0,
            "the burst drew nothing outside the moon's own disk — it adds no reach at all"
        );
        assert!(
            furthest > 1.5,
            "the burst reached only {furthest:.2}x the body radius"
        );

        // And it is a flash: fully gone by the end of its own short life.
        let spent = SceneEffects {
            ejecta: vec![Ejecta {
                body: moon,
                angle_on_surface: 0.9,
                severity: Severity::Critical,
                age: 1.0,
            }],
            ..Default::default()
        };
        assert!(effects_frame(&layout, &spent, phase)
            .chunks_exact(4)
            .all(|px| px[3] == 0));
    }

    /// Q3's landing tier: an arrival comet ends on the body the work landed on — and that body is
    /// orbiting, so the endpoint has to follow it rather than be frozen at spawn time.
    #[test]
    fn an_arrival_comet_ends_on_its_target_wherever_that_body_has_orbited_to() {
        let (layout, _, planet) = struck_moon_scene();
        let arriving = SceneEffects {
            comets: vec![Comet {
                start: (0.0, 0.0),
                // A crossing endpoint deliberately nowhere near the target, so anything landing on
                // the body can only have come from `target`.
                end: (1.0, 1.0),
                target: Some(planet),
                magnitude: 0.8,
                progress: 1.0,
            }],
            ..Default::default()
        };

        for phase in [0.0f32, 2.0, 4.0] {
            let pos = layout.position(planet, phase);
            let rgba = effects_frame(&layout, &arriving, phase);
            let on_target = peak_alpha(
                &rgba,
                layout.width,
                pos,
                layout.bodies[planet].body_radius_px,
            );
            assert!(
                on_target > 0.5,
                "at phase {phase} the arrival peaked at {on_target:.2} on its target — it is not \
                 tracking the body's orbit"
            );
        }

        // The same comet without a target is the round-1 crossing, unchanged: it ends at `end`.
        let crossing = SceneEffects {
            comets: vec![Comet {
                target: None,
                ..arriving.comets[0]
            }],
            ..Default::default()
        };
        let rgba = effects_frame(&layout, &crossing, 0.0);
        assert_eq!(
            peak_alpha(
                &rgba,
                layout.width,
                layout.position(planet, 0.0),
                layout.bodies[planet].body_radius_px
            ),
            0.0
        );
    }

    /// A fleet shaped like a real one, for the benchmark to measure against.
    ///
    /// The mates carry real file counts and real streaks rather than one flat size, because both
    /// registers now cost pixels: a mate's rank decides whether it draws a ring at all, and its
    /// streak decides how wide that ring is and how far a gas giant swells. A fixture that left
    /// them at the floor would benchmark the one fleet shape that is cheapest to draw.
    ///
    /// The file counts are the fleet orrery's own `MATES` table — the real checkouts on this box.
    fn representative_fleet() -> Vec<TreeNode> {
        const MATES: [(u32, f32); 4] = [(2470, 1.0), (860, 0.6), (430, 0.3), (99, 0.0)];
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for (files, streak) in MATES {
            nodes.push(TreeNode {
                streak,
                ..sized(Some(0), BodyKind::Planet, BodySize::Files(files))
            });
            let planet_idx = nodes.len() - 1;
            for _ in 0..3 {
                nodes.push(node(Some(planet_idx), BodyKind::Moon));
            }
        }
        nodes
    }

    /// Real cost measurement at the captain's confirmed 1440p target, requested explicitly by
    /// round 1 of the visual-execution decision
    /// (`data/decisions/2026-08-07-terminal-background-visual-execution-round1.md`, firstmate
    /// home: "worth a fresh cost check once the rendering approach is chosen, not assumed free
    /// because the particle numbers were fine"). Run with `cargo test --release --bin herdr
    /// solar_system::tests::bench_1440p -- --ignored --nocapture`.
    #[test]
    #[ignore = "benchmark: prints ms/frame, run explicitly with --release --ignored --nocapture"]
    fn bench_1440p() {
        let nodes = representative_fleet();
        let (w, h) = (2560u32, 1440u32);
        let layout = build_layout(&nodes, w, h);
        let effects = SceneEffects {
            asteroids: vec![AsteroidInFlight {
                target: 1,
                severity: Severity::Serious,
                progress: 0.5,
                approach_angle: 0.3,
            }],
            craters: vec![Crater {
                body: 2,
                angle_on_surface: 1.0,
                severity: Severity::Critical,
                age: 0.2,
                is_ripple: false,
            }],
            // A live ejecta burst is part of the worst case this benchmark exists to bound: it
            // is the only effect that draws outside a body's own disk on the struck-moon path.
            ejecta: vec![Ejecta {
                body: 2,
                angle_on_surface: 1.0,
                severity: Severity::Critical,
                age: 0.2,
            }],
            comets: vec![Comet {
                start: (0.0, 0.2),
                end: (1.0, 0.8),
                target: None,
                magnitude: 0.9,
                progress: 0.4,
            }],
        };

        fn median_ms(mut run: impl FnMut(f32) -> Vec<u8>) -> f64 {
            let _ = run(0.0); // warm up (first call pays one-time allocator/cache costs)
            let mut samples = Vec::new();
            for i in 0..15 {
                let phase = i as f32 * 0.1;
                let started = std::time::Instant::now();
                let _ = run(phase);
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            samples[samples.len() / 2]
        }

        // The ambient scene: full-frame starfield plus every shaded body. Expensive, but baked
        // once into a cached loop and only regenerated on a fleet/resize change — see
        // `App::observe_background_scene`.
        let ambient_median = median_ms(|phase| frame(&layout, phase));
        // The effects overlay: transparent, bounding-box-limited draws only. This is the number
        // that matters for steady-state cost, since it regenerates every tick while anything is
        // live — see `App::observe_background_effects`.
        let effects_median = median_ms(|phase| effects_frame(&layout, &effects, phase));

        let cores = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1);
        println!(
            "\n# solar_system 1440p bench ({cores} logical cores on this box, {} bodies)\n\
             ambient frame():         {ambient_median:.2} ms/frame ({:.1} fps) — cached, regenerated only on fleet/resize change\n\
             effects_frame() overlay: {effects_median:.2} ms/frame ({:.1} fps) — regenerated every tick while something is live",
            nodes.len(),
            1000.0 / ambient_median,
            1000.0 / effects_median,
        );

        // AGENTS.md's multiplicative-performance rule: a change that widens work in a pane-scaled
        // loop has to report its *scaling* delta, not only one absolute number. Body count is this
        // generator's cardinality axis — one mate against a fleet of fifteen, each with workers —
        // and it is the axis body types moved, since a mate's rank decides whether it draws a ring
        // at all and its streak decides how wide that ring is.
        //
        // What this exists to catch is a body-local cost that has quietly become full-frame. Every
        // draw here is bounded to its own bounding box, so the per-body slope should stay small
        // against the one genuinely full-frame pass — the gradient and starfield — which every run
        // pays once whatever the fleet looks like.
        let one_mate = scaling_fleet(1);
        let fifteen_mates = scaling_fleet(15);
        let one_median = median_ms(|phase| frame(&build_layout(&one_mate, w, h), phase));
        let fifteen_median = median_ms(|phase| frame(&build_layout(&fifteen_mates, w, h), phase));
        println!(
            "  scaling: {} bodies {one_median:.2} ms -> {} bodies {fifteen_median:.2} ms \
             ({:+.2} ms total, {:.3} ms per extra body)",
            one_mate.len(),
            fifteen_mates.len(),
            fifteen_median - one_median,
            (fifteen_median - one_median) / (fifteen_mates.len() - one_mate.len()) as f64,
        );
    }

    // ---------------------------------------------------------------------------
    // The ladder seats eight, and the overflow is a reading
    // ---------------------------------------------------------------------------

    /// A fleet whose mates carry the given file counts, each with one worker, so the "a dropped
    /// mate takes its workers with it" rule has something to drop.
    fn fleet_of(files: &[Option<u32>]) -> Vec<TreeNode> {
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for size in files {
            let size = size.map_or(BodySize::Unmeasured, BodySize::Files);
            nodes.push(sized(Some(0), BodyKind::Planet, size));
            let mate = nodes.len() - 1;
            nodes.push(node(Some(mate), BodyKind::Moon));
        }
        nodes
    }

    #[test]
    fn the_ring_seats_the_heaviest_eight_and_counts_the_rest() {
        // The real case rather than a corner: seventeen mates, five of them unmeasured — the
        // roster A42's own CHECK is driven by. The nine smallest have to be the nine dropped.
        let mut files: Vec<Option<u32>> = (0..12).map(|i| Some((i + 1) * 200)).collect();
        files.extend(std::iter::repeat_n(None, 5));
        let nodes = fleet_of(&files);
        let layout = build_layout(&nodes, 1_000, 1_000);

        let (seated, beyond) = layout.ladder_occupancy();
        assert_eq!(
            (seated, beyond),
            (ORBIT_LADDER_SLOTS, 17 - ORBIT_LADDER_SLOTS)
        );

        // And they are the heaviest eight — 2400 down to 1000 — by tracked files, not by roster
        // order and not by arrival.
        let seated_files: Vec<Option<u32>> = nodes
            .iter()
            .enumerate()
            .filter(|(idx, n)| n.kind == BodyKind::Planet && layout.bodies[*idx].seated)
            .map(|(_, n)| match n.size {
                BodySize::Files(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(
            seated_files,
            vec![
                Some(1_000),
                Some(1_200),
                Some(1_400),
                Some(1_600),
                Some(1_800),
                Some(2_000),
                Some(2_200),
                Some(2_400)
            ]
        );
    }

    #[test]
    fn an_unmeasured_mate_ranks_at_the_floor_rather_than_below_zero() {
        // A42(b), and the reason the key is `register_fraction` rather than the raw file count:
        // an unmeasured project is not a zero-file project. Ranked on the raw field it would sort
        // below every measured mate and could never win a slot at all — and five of the mates on
        // this box are unmeasured, so this is the case that occurs rather than a corner.
        //
        // Nine mates: eight measured *below* the floor's equivalent, and one unmeasured. The
        // unmeasured one has to beat the small ones, because that is where it is drawn.
        let files: Vec<Option<u32>> = std::iter::once(None)
            .chain((0..ORBIT_LADDER_SLOTS).map(|_| Some(0)))
            .collect();
        let nodes = fleet_of(&files);
        let layout = build_layout(&nodes, 1_000, 1_000);
        assert!(
            layout.bodies[1].seated,
            "the unmeasured mate lost its slot to a mate drawn exactly its own size"
        );

        // ...and it ties with an empty project rather than beating or losing to it, so the tie
        // has to be broken by roster order — otherwise two identical snapshots seat different
        // bodies (A42(c)).
        let repeated = build_layout(&nodes, 1_000, 1_000);
        let seats: Vec<bool> = layout.bodies.iter().map(|b| b.seated).collect();
        let again: Vec<bool> = repeated.bodies.iter().map(|b| b.seated).collect();
        assert_eq!(seats, again, "the seating is not deterministic");
    }

    #[test]
    fn a_dropped_mate_takes_its_own_workers_with_it() {
        // A worker orbits its mate, and a mate that is not in the picture has nothing for its
        // workers to orbit. Leaving them behind would draw moons circling empty space.
        let files: Vec<Option<u32>> = (0..ORBIT_LADDER_SLOTS + 3)
            .map(|i| Some((i as u32 + 1) * 100))
            .collect();
        let nodes = fleet_of(&files);
        let layout = build_layout(&nodes, 1_000, 1_000);

        for (idx, n) in nodes.iter().enumerate() {
            if n.kind != BodyKind::Moon {
                continue;
            }
            let parent = n.parent.expect("a worker has a mate");
            assert_eq!(
                layout.bodies[idx].seated, layout.bodies[parent].seated,
                "worker {idx} and its mate {parent} disagree about being in the picture"
            );
        }
    }

    #[test]
    fn a_fleet_that_fits_loses_nothing_and_discloses_nothing() {
        for count in 0..=ORBIT_LADDER_SLOTS {
            let files: Vec<Option<u32>> = (0..count).map(|i| Some(i as u32 * 37 + 1)).collect();
            let layout = build_layout(&fleet_of(&files), 1_000, 1_000);
            assert_eq!(layout.ladder_occupancy(), (count, 0));
            assert!(
                layout.bodies.iter().all(|b| b.seated),
                "{count} mates fit the ring and something was still dropped"
            );
        }
    }

    #[test]
    fn the_overflow_is_marked_in_the_frame_and_absent_when_there_is_none() {
        // A41(b) and (c): counted and dropped, never a silent vanish — and absent at zero,
        // because a disclosure of nothing is noise rather than population.
        let fits = build_layout(&fleet_of(&[Some(1), Some(2)]), 900, 900);
        let overflowing: Vec<Option<u32>> = (0..ORBIT_LADDER_SLOTS + 6)
            .map(|i| Some(i as u32 + 1))
            .collect();
        let overflowing = build_layout(&fleet_of(&overflowing), 900, 900);

        // The mark lives outside every orbit, so the two frames can be differenced in a band no
        // body ever reaches — and the starfield out there is identical between them, being a pure
        // function of the frame size. Whatever differs is the disclosure and nothing else.
        let centre = (450.0f32, 450.0f32);
        let inner = OVERFLOW_ORBIT_FRACTION * 900.0 - 8.0;
        let quiet = frame(&fits, 0.0);
        let disclosed = frame(&overflowing, 0.0);
        let marked = quiet
            .chunks_exact(4)
            .zip(disclosed.chunks_exact(4))
            .enumerate()
            .filter(|(i, (a, b))| {
                let (x, y) = ((i % 900) as f32, (i / 900) as f32);
                let d = ((x - centre.0).powi(2) + (y - centre.1).powi(2)).sqrt();
                d >= inner && (0..3).any(|c| a[c].abs_diff(b[c]) > 1)
            })
            .count();
        assert!(
            marked > 0,
            "a fleet with {} mates beyond the ring marked nothing outside it",
            overflowing.ladder_occupancy().1
        );
        // ...and the mark is small: it is a disclosure, not a second fleet. Well under the area
        // one seated mate's own disk covers.
        assert!(
            marked < 2_000,
            "the overflow mark is drawing {marked} pixels — that is a body, not a mark"
        );
        // A fleet that fits marks nothing at all out there, which is the "absent at zero" half.
        let space = frame(&build_layout(&[], 900, 900), 0.0);
        let when_it_fits = quiet
            .chunks_exact(4)
            .zip(space.chunks_exact(4))
            .enumerate()
            .filter(|(i, (a, b))| {
                let (x, y) = ((i % 900) as f32, (i / 900) as f32);
                let d = ((x - centre.0).powi(2) + (y - centre.1).powi(2)).sqrt();
                d >= inner && (0..3).any(|c| a[c].abs_diff(b[c]) > 1)
            })
            .count();
        assert_eq!(
            when_it_fits, 0,
            "a fleet the ring seats whole still drew a disclosure"
        );
    }

    #[test]
    fn an_effect_on_an_unseated_mate_draws_nothing() {
        // Effects name their body by index, and the indices are preserved across seating so a
        // pane's identity still resolves. That makes "this body is not in the picture" a thing
        // every effect path has to check, or a crater lands on empty space.
        let files: Vec<Option<u32>> = (0..ORBIT_LADDER_SLOTS + 2)
            .map(|i| Some((i as u32 + 1) * 100))
            .collect();
        let nodes = fleet_of(&files);
        let layout = build_layout(&nodes, 900, 900);
        let dropped = layout
            .bodies
            .iter()
            .position(|b| !b.seated)
            .expect("this fleet overflows the ring");

        let effects = SceneEffects {
            craters: vec![Crater {
                body: dropped,
                angle_on_surface: 0.4,
                severity: Severity::Critical,
                age: 0.0,
                is_ripple: false,
            }],
            ejecta: vec![Ejecta {
                body: dropped,
                angle_on_surface: 0.4,
                severity: Severity::Critical,
                age: 0.0,
            }],
            asteroids: vec![AsteroidInFlight {
                target: dropped,
                severity: Severity::Critical,
                progress: 0.5,
                approach_angle: 0.4,
            }],
            comets: vec![Comet {
                start: (0.1, 0.1),
                end: (0.9, 0.9),
                target: Some(dropped),
                magnitude: 1.0,
                progress: 0.5,
            }],
        };
        let drawn = effects_frame(&layout, &effects, 0.0);
        let clean = effects_frame(&layout, &SceneEffects::default(), 0.0);
        // The comet still crosses — it is a scene-wide event, and degrading to its own crossing
        // path is the existing behaviour for a target that cannot be resolved. Everything that
        // draws *on* the body draws nothing.
        let on_body = drawn
            .chunks_exact(4)
            .zip(clean.chunks_exact(4))
            .enumerate()
            .filter(|(i, (a, b))| {
                let (x, y) = ((i % 900) as f32, (i / 900) as f32);
                let pos = layout.position(dropped, 0.0);
                let near = ((x - pos.0).powi(2) + (y - pos.1).powi(2)).sqrt() < 60.0;
                near && a != b
            })
            .count();
        assert_eq!(
            on_body, 0,
            "an effect drew on a mate the ring had no slot for"
        );
    }

    // ---------------------------------------------------------------------------
    // Orbital period reads mass
    // ---------------------------------------------------------------------------

    fn revolutions(kind: BodyKind, size: BodySize) -> f32 {
        let nodes = [node(None, BodyKind::Sun), sized(Some(0), kind, size)];
        build_layout(&nodes, 1_000, 1_000).bodies[1].revolutions_per_loop
    }

    #[test]
    fn the_heaviest_project_takes_the_longest_to_come_round() {
        // `T = k · a^1.5 · m^0.5` — Kepler's third with a mass term. Period *rises* with mass, so
        // the heaviest mate in the fleet is the slowest thing on the ring. That is the reading:
        // a mate that has grown into the biggest checkout comes round with a weight the small ones
        // do not have.
        let ceiling = revolutions(BodyKind::Planet, BodySize::Files(FILES_CEIL));
        let floor = revolutions(BodyKind::Planet, BodySize::Unmeasured);
        assert!(
            ceiling < floor,
            "the heaviest mate ({ceiling} rev/loop) is not slower than the lightest ({floor})"
        );

        // Monotonic all the way along, not merely ordered at its two ends.
        let mut previous = f32::INFINITY;
        for files in [0, 100, 400, 900, 1_600, 2_500, 3_600, FILES_CEIL] {
            let rate = revolutions(BodyKind::Planet, BodySize::Files(files));
            assert!(
                rate <= previous,
                "{files} files revolves faster than a lighter project: {rate} after {previous}"
            );
            previous = rate;
        }

        // And the spread is real rather than a rounding artefact — the fleet's own range of
        // checkouts has to land on more than one rate or the register is not being read.
        let real_fleet: Vec<f32> = [2_470, 860, 430, 314, 99, 2]
            .into_iter()
            .map(|files| revolutions(BodyKind::Planet, BodySize::Files(files)))
            .collect();
        let distinct: std::collections::BTreeSet<u32> =
            real_fleet.iter().map(|r| *r as u32).collect();
        assert!(
            distinct.len() >= 3,
            "the real fleet lands on only {} distinct periods: {real_fleet:?}",
            distinct.len()
        );
    }

    #[test]
    fn every_body_completes_a_whole_number_of_orbits_per_loop() {
        // The seamless-loop contract, and the reason the mass-driven period is *quantized* rather
        // than continuous: the ambient loop is baked once into [`FRAME_COUNT`] frames and played
        // forever, so a body on a fractional period jumps at every repeat.
        for kind in [BodyKind::Sun, BodyKind::Planet, BodyKind::Moon] {
            for size in [
                BodySize::Fixed,
                BodySize::Unmeasured,
                BodySize::Files(0),
                BodySize::Files(1),
                BodySize::Files(431),
                BodySize::Files(2_470),
                BodySize::Files(FILES_CEIL),
                BodySize::Files(u32::MAX),
            ] {
                let rate = revolutions(kind, size);
                assert_eq!(
                    rate,
                    rate.round(),
                    "{kind:?} at {size:?} draws {rate} revolutions per loop"
                );
                assert!((0.0..=8.0).contains(&rate), "{kind:?} at {size:?}: {rate}");
            }
        }
    }

    #[test]
    fn a_mass_driven_orbit_still_lands_back_where_it_started() {
        // The whole point of quantizing: whatever the register does to a body's rate, the loop
        // still closes. A fleet spanning the register, every body checked.
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for files in [2_470u32, 860, 430, 99, 2] {
            nodes.push(sized(Some(0), BodyKind::Planet, BodySize::Files(files)));
            let planet = nodes.len() - 1;
            nodes.push(node(Some(planet), BodyKind::Moon));
            nodes.push(sized(
                Some(planet),
                BodyKind::Moon,
                BodySize::Files(files / 2),
            ));
        }
        let layout = build_layout(&nodes, 800, 800);
        for idx in 0..nodes.len() {
            let start = layout.position(idx, 0.0);
            let looped = layout.position(idx, 2.0 * PI);
            assert!(
                (start.0 - looped.0).abs() < 0.01 && (start.1 - looped.1).abs() < 0.01,
                "body {idx} does not close its loop: {start:?} -> {looped:?}"
            );
        }
    }

    #[test]
    fn a_body_outside_the_register_keeps_its_tiers_own_rate() {
        // A pane is not a checkout, so a worker has no mass to read — exactly the reason it keeps
        // its tier's fixed *radius*. Its rate is the one every moon drew before period read mass,
        // so nothing about the composition moved for the bodies the register does not cover.
        assert_eq!(revolutions(BodyKind::Moon, BodySize::Fixed), 4.0);
        assert_eq!(revolutions(BodyKind::Planet, BodySize::Fixed), 1.0);
        // The sun does not orbit anything, whatever size a caller hands it.
        for size in [BodySize::Fixed, BodySize::Unmeasured, BodySize::Files(9)] {
            assert_eq!(revolutions(BodyKind::Sun, size), 0.0);
        }
        // ...and a worker's own rate sits inside the band the register would have put it in, so a
        // fleet of workers and nested projects reads as one population rather than two.
        let (slowest, fastest) = BodyKind::Moon.revolution_band();
        let worker = revolutions(BodyKind::Moon, BodySize::Fixed);
        assert!((slowest..=fastest).contains(&worker), "{worker}");
    }

    #[test]
    fn the_fastest_body_is_still_sampled_often_enough_to_read_as_a_circle() {
        // The band's fast end is set by [`FRAME_COUNT`], not by taste: a body doing `R`
        // revolutions per loop is sampled `36/R` times per revolution, and below about seven
        // samples an orbit stops reading as a circle and starts reading as a polygon. This is the
        // constraint that makes the *quantized* period a narrow band rather than a wide one.
        let fastest = [BodyKind::Planet, BodyKind::Moon]
            .into_iter()
            .map(|kind| kind.revolution_band().1)
            .fold(0.0f32, f32::max);
        let samples_per_revolution = FRAME_COUNT as f32 / fastest;
        assert!(
            samples_per_revolution >= 7.0,
            "the fastest body gets {samples_per_revolution:.1} samples per revolution"
        );
    }

    #[test]
    fn two_mates_on_different_periods_actually_separate_on_screen() {
        // The reading only exists if it is visible: two mates that start together have to be
        // meaningfully apart by the middle of the loop, or the register is driving a number
        // nobody can see. Measured as the angle between them about the sun.
        let nodes = [
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(FILES_CEIL)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(0)),
        ];
        let layout = build_layout(&nodes, 1_000, 1_000);
        let sun = layout.position(0, 0.0);
        let angle_to = |idx: usize, phase: f32| {
            let p = layout.position(idx, phase);
            (p.1 - sun.1).atan2(p.0 - sun.0)
        };
        let separation = |phase: f32| {
            let mut d = (angle_to(1, phase) - angle_to(2, phase)).abs();
            if d > PI {
                d = 2.0 * PI - d;
            }
            d
        };
        // Their starting separation is the sibling spread, which is composition rather than
        // motion; what has to grow is the *change* in it.
        let start = separation(0.0);
        let drifted = (0..=10)
            .map(|i| (separation(i as f32 / 10.0 * 2.0 * PI) - start).abs())
            .fold(0.0f32, f32::max);
        assert!(
            drifted > 1.0,
            "the heaviest and lightest mates never drift more than {drifted:.2} rad apart"
        );
    }

    // ---------------------------------------------------------------------------
    // Body types: rings and gas giants
    // ---------------------------------------------------------------------------

    /// A fleet of `mates` second mates with distinct, descending file counts, so every rank is
    /// unambiguous and the assignment can be read off directly.
    fn ranked_fleet(mates: usize) -> Vec<TreeNode> {
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for i in 0..mates {
            let files = (mates - i) as u32 * 100;
            nodes.push(sized(Some(0), BodyKind::Planet, BodySize::Files(files)));
        }
        nodes
    }

    fn types_of(nodes: &[TreeNode]) -> Vec<BodyType> {
        build_layout(nodes, 1_000, 1_000)
            .bodies
            .iter()
            .map(|body| body.body_type)
            .collect()
    }

    #[test]
    fn two_second_mates_in_three_are_gas_giants_evenly_spaced() {
        // A54's rule: a mate is a gas giant *unless* its rank is 2 mod 3. That is "even
        // distribution" read as even spacing rather than as 50/50 — the captain asked for more gas
        // planets in the same sentence, and a 50/50 split adds none.
        // A full ladder, so this is about the modulus alone rather than about the ladder's cap.
        let types = types_of(&ranked_fleet(ORBIT_LADDER_SLOTS));
        let mates = &types[1..];
        assert_eq!(
            mates,
            [
                BodyType::Gas,
                BodyType::Gas,
                BodyType::Ringed,
                BodyType::Gas,
                BodyType::Gas,
                BodyType::Ringed,
                BodyType::Gas,
                BodyType::Gas,
            ],
            "the ringed mates are not every third one, heaviest first"
        );

        // The proportion is the point, and it has to hold for a fleet that is not a multiple of
        // three as well as one that is — and, past the ladder, for the mates that got a seat.
        for count in 1..=20 {
            let types = types_of(&ranked_fleet(count));
            let ringed = types.iter().filter(|t| **t == BodyType::Ringed).count();
            let gas = types.iter().filter(|t| **t == BodyType::Gas).count();
            assert_eq!(
                ringed + gas,
                count.min(ORBIT_LADDER_SLOTS),
                "every *seated* second mate needs a type, and nothing else may have one"
            );
            assert!(
                gas >= ringed,
                "{count} mates split {gas} gas / {ringed} ringed — that is not two in three"
            );
        }
    }

    #[test]
    fn the_sun_and_the_workers_carry_no_body_type_at_all() {
        // Body type is a second mate's fact and binding — a worker is a moon and the firstmate is
        // a star, so neither has one. `Plain` is the absence of the question, not a third planet.
        let nodes = [
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(500)),
            node(Some(1), BodyKind::Moon),
            node(Some(2), BodyKind::Moon),
        ];
        let types = types_of(&nodes);
        assert_eq!(types[0], BodyType::Plain, "the sun is not a planet");
        assert_ne!(types[1], BodyType::Plain, "a second mate always has a type");
        assert_eq!(types[2], BodyType::Plain, "a worker is not a planet");
        assert_eq!(types[3], BodyType::Plain, "nor is a worker's own worker");
    }

    #[test]
    fn a_roster_change_reseats_the_types_rather_than_keeping_them() {
        // A54(b): recomputed, never stored. A rule that is even only for the fleet that happened
        // to exist at bake time is not a rule, and this fleet's roster changes under it.
        let before = types_of(&[
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(300)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(200)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(100)),
        ]);
        assert_eq!(
            before[3],
            BodyType::Ringed,
            "the lightest of three is rank 2"
        );

        // The lightest mate grows past both of the others. It is now rank 0, and the type has to
        // follow the rank — not the body.
        let after = types_of(&[
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(300)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(200)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(4_000)),
        ]);
        assert_eq!(
            after[3],
            BodyType::Gas,
            "the mate that grew kept the type its old rank gave it"
        );
        assert_eq!(after[2], BodyType::Ringed, "...and rank 2 moved with it");

        // A mate joining does the same thing, which is the case a stored type gets wrong most
        // often: nothing about the existing bodies changed, and two of them still have to move.
        let joined = types_of(&[
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(300)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(200)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(100)),
            sized(Some(0), BodyKind::Planet, BodySize::Files(4_000)),
        ]);
        assert_eq!(
            &joined[1..],
            [
                BodyType::Gas,
                BodyType::Ringed,
                BodyType::Gas,
                BodyType::Gas
            ],
            "the new heaviest mate took rank 0, which pushed rank 2 up the roster"
        );
    }

    #[test]
    fn an_unmeasured_mate_ranks_where_it_is_drawn_rather_than_below_zero() {
        // Same reasoning the size band already uses: a project nobody has measured is drawn at the
        // register floor, so it ranks at the floor too — alongside an empty project, not beneath
        // every project that exists.
        let types = types_of(&[
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Unmeasured),
            sized(Some(0), BodyKind::Planet, BodySize::Files(0)),
        ]);
        assert_eq!(&types[1..], [BodyType::Gas, BodyType::Gas]);

        // ...and ties fall back on roster order, so an all-unmeasured fleet is stable rather than
        // shuffling its rings around on every rebuild.
        let all_unmeasured: Vec<TreeNode> = std::iter::once(node(None, BodyKind::Sun))
            .chain((0..6).map(|_| sized(Some(0), BodyKind::Planet, BodySize::Unmeasured)))
            .collect();
        assert_eq!(types_of(&all_unmeasured), types_of(&all_unmeasured));
        assert_eq!(
            types_of(&all_unmeasured)[3],
            BodyType::Ringed,
            "roster order should still seat rank 2"
        );
    }

    /// The one second mate under test sits at index [`MATE`], in a fleet arranged so it lands on
    /// `body_type`.
    ///
    /// Only the *filler* mates differ between the two arrangements: the mate under test keeps its
    /// index, its size, its streak and therefore its seed, position and radius. That is what makes
    /// differencing a ringed frame against a gas one an exact isolate of the ring — every other
    /// pixel of the body is bit-identical between them, so anything that moved is ring material
    /// and nothing has to be inferred from colour.
    const MATE: usize = 3;

    fn one_mate_scene(body_type: BodyType, streak: f32) -> SceneLayout {
        let filler = |files| sized(Some(0), BodyKind::Planet, BodySize::Files(files));
        let mate = TreeNode {
            streak,
            ..sized(Some(0), BodyKind::Planet, BodySize::Files(1_000))
        };
        // Rank 2 is the ringed one: heavier fillers push the mate down to rank 2, lighter fillers
        // lift it to rank 0, which is a gas giant.
        let nodes = match body_type {
            BodyType::Ringed => vec![
                node(None, BodyKind::Sun),
                filler(4_000),
                filler(3_000),
                mate,
            ],
            _ => vec![node(None, BodyKind::Sun), filler(100), filler(50), mate],
        };
        let layout = build_layout(&nodes, 1_400, 1_400);
        assert_eq!(
            layout.bodies[MATE].body_type, body_type,
            "fixture built the wrong type"
        );
        layout
    }

    /// The same ringed mate rendered with and without its ring, plus its on-screen geometry.
    ///
    /// Same layout, same phase, same seed — so every pixel that differs between the two is ring
    /// material, and that stays true *inside* the body's own disk, which is where occlusion has to
    /// be measured.
    fn ringed_against_plain(streak: f32, phase: f32) -> (Vec<u8>, Vec<u8>, u32, (i32, i32), f32) {
        let layout = one_mate_scene(BodyType::Ringed, streak);
        let pos = layout.position(MATE, phase);
        (
            frame(&layout, phase),
            frame_without_rings(&layout, phase),
            layout.width,
            (pos.0 as i32, pos.1 as i32),
            layout.bodies[MATE].body_radius_px,
        )
    }

    /// Count pixels in a box where the ringed frame differs from the unringed one — which, given
    /// the two fixtures differ only in which mate is ranked where, is exactly the ring.
    ///
    /// Read off the real rendered buffers rather than re-deriving `draw_ring`'s own geometry, so
    /// these assert what actually lands on screen. A colour test would not do the job: a ring
    /// particle blended over a lit planet at partial alpha carries mostly the planet's own colour.
    fn ring_pixels(
        ringed: &[u8],
        plain: &[u8],
        width: u32,
        (x0, y0): (i32, i32),
        (x1, y1): (i32, i32),
    ) -> usize {
        let mut found = 0;
        for y in y0.max(0)..y1 {
            for x in x0.max(0)..x1 {
                let i = (y as usize * width as usize + x as usize) * 4;
                if i + 3 >= ringed.len() {
                    continue;
                }
                // More than one step on some channel, so the count is material rather than
                // rounding at the edge of a blend.
                if (0..3).any(|c| ringed[i + c].abs_diff(plain[i + c]) > 1) {
                    found += 1;
                }
            }
        }
        found
    }

    #[test]
    fn a_ringed_mate_draws_a_ring_and_a_gas_giant_does_not() {
        let (ringed, plain, width, (cx, cy), radius) = ringed_against_plain(0.5, 0.0);

        // The band outside the body's own disk on both sides — where only a ring can be. The glow
        // fringe reaches 1.4x, so this starts past it.
        let outer = (radius * (RING_OUTER + 0.3)) as i32;
        let inner = (radius * 1.45) as i32;
        let left = ring_pixels(
            &ringed,
            &plain,
            width,
            (cx - outer, cy - 4),
            (cx - inner, cy + 4),
        );
        let right = ring_pixels(
            &ringed,
            &plain,
            width,
            (cx + inner, cy - 4),
            (cx + outer, cy + 4),
        );
        assert!(
            left > 0 && right > 0,
            "a ringed mate has no ring material beside it: {left} left, {right} right"
        );

        // ...and a gas giant draws no ring at all — not "a fainter one", none: suppressing rings
        // changes not one pixel anywhere a ring of its own could have reached. (Scoped to that
        // mate's own box rather than the whole frame, because the fleet it is ranked inside still
        // has to contain a ringed mate for it to be ranked *against*.)
        let gas = one_mate_scene(BodyType::Gas, 0.5);
        let gas_pos = gas.position(MATE, 0.0);
        let reach = (gas.bodies[MATE].body_radius_px * (RING_OUTER + RING_OUTER_PER_STREAK)) as i32;
        let (gx, gy) = (gas_pos.0 as i32, gas_pos.1 as i32);
        assert_eq!(
            ring_pixels(
                &frame(&gas, 0.0),
                &frame_without_rings(&gas, 0.0),
                gas.width,
                (gx - reach, gy - reach),
                (gx + reach, gy + reach),
            ),
            0,
            "a gas giant drew ring material"
        );
    }

    #[test]
    fn a_planet_occludes_the_back_of_its_own_ring_and_not_the_front() {
        // The one thing a ring needs that nothing else in this scene does: the body sits *inside*
        // it. `RING_SQUASH` puts the ring's near and far crossings of the body's own vertical at
        // ~0.48 of its radius, so both land on the disk — the far one behind it, the near one in
        // front. A ring drawn in one pass shows both, which is a planet with a bracelet painted
        // over it.
        let (ringed, plain, width, (cx, cy), radius) = ringed_against_plain(0.5, 0.0);
        let reach = (radius * 0.75) as i32;
        let half = (radius * 0.55) as i32;

        let behind = ring_pixels(
            &ringed,
            &plain,
            width,
            (cx - reach, cy - half),
            (cx + reach, cy - 1),
        );
        let in_front = ring_pixels(
            &ringed,
            &plain,
            width,
            (cx - reach, cy + 1),
            (cx + reach, cy + half),
        );

        assert!(
            in_front > 0,
            "the near arc of the ring is not drawn over its own planet"
        );
        assert_eq!(
            behind, 0,
            "{behind} ring pixels are showing through the planet that should hide them"
        );
    }

    #[test]
    fn a_streak_widens_and_brightens_a_mates_ring() {
        // *"Streak brightens and thickens the ring"* — the captain's binding correction, measured
        // on the rendered buffer rather than on the two constants that produce it.
        let phase = 0.0;
        let measure = |streak: f32| {
            let (ringed, plain, width, (cx, cy), radius) = ringed_against_plain(streak, phase);
            let span = (radius * 3.0) as i32;
            (
                ring_pixels(
                    &ringed,
                    &plain,
                    width,
                    (cx - span, cy - span),
                    (cx + span, cy + span),
                ),
                one_mate_scene(BodyType::Ringed, streak).bodies[MATE]
                    .ring_radii_px()
                    .map(|(_, outer)| outer)
                    .unwrap_or(0.0),
                radius,
            )
        };
        let (cold_px, cold_outer, cold_radius) = measure(0.0);
        let (hot_px, hot_outer, hot_radius) = measure(1.0);

        // Thicker: the ring reaches further out. The body itself must not have moved — a ringed
        // mate does not swell, only a gas giant does — so this is the ring and nothing else.
        assert_eq!(cold_radius, hot_radius, "a ringed mate must not swell");
        assert!(
            hot_outer > cold_outer * 1.3,
            "a full streak barely widened the ring: {cold_outer:.1} -> {hot_outer:.1}"
        );
        // ...and brighter and denser, which is what "brightens" reads as on a particle ring.
        assert!(
            hot_px > cold_px,
            "a full streak drew no more ring material: {cold_px} -> {hot_px}"
        );
    }

    #[test]
    fn a_streak_swells_a_gas_giant_but_never_past_the_size_bound() {
        let cold = one_mate_scene(BodyType::Gas, 0.0).bodies[MATE].body_radius_px;
        let hot = one_mate_scene(BodyType::Gas, 1.0).bodies[MATE].body_radius_px;
        assert!(
            hot > cold * 1.2,
            "a full streak barely swelled the gas: {cold:.2} -> {hot:.2}"
        );

        // F16 is explicit that the swell is *inside* the bound rather than outside it: it is
        // clamped, and the clamp is the stated price. The heaviest possible mate on the longest
        // possible streak still cannot rival the sun.
        let maxed = build_layout(
            &[
                node(None, BodyKind::Sun),
                TreeNode {
                    streak: 1.0,
                    ..sized(Some(0), BodyKind::Planet, BodySize::Files(u32::MAX))
                },
            ],
            1_000,
            1_000,
        );
        let planet = maxed.bodies[1].body_radius_px;
        let sun = maxed.bodies[0].body_radius_px;
        assert!(
            planet <= sun / 2.0,
            "a swollen gas giant ({planet}) broke F16's half-the-sun bound ({sun})"
        );
        // And it really is the clamp doing that rather than the swell being too small to reach it.
        assert_eq!(planet, MATE_RADIUS_CEIL * 1_000.0);
    }

    /// How the shaded surface varies down a body's own meridian *because of banding alone*.
    ///
    /// Sampled as a ratio against the identical surface with `bands: None`, so everything the two
    /// share — the Lambert term, limb darkening, the mottle — divides out exactly. Measuring the
    /// banded surface on its own instead measures the sphere's geometry, which swamps the band
    /// term by an order of magnitude and makes the assertion meaningless.
    ///
    /// Returns `(spread, crossings)`: how far the ratio departs from 1, and how many times it
    /// crosses back through it — which is the band *count* rather than the band depth.
    fn meridian_banding(bands: Option<f32>) -> (f32, usize) {
        const RADIUS: f32 = 120.0;
        let surface = |bands| Surface {
            base: (0.8, 0.8, 0.8),
            seed: 17,
            self_luminous: false,
            bands,
            mottle_scale: 1.0,
        };
        // Straight down the lit centre of the disk, so the only thing varying is latitude.
        let sample = |bands, ny: f32| {
            luminance(shade_surface(
                0.0,
                ny * RADIUS,
                RADIUS,
                (0.0, 0.0, 1.0),
                surface(bands),
            ))
        };
        let ratios: Vec<f32> = (0..256)
            .map(|i| {
                // Well inside the limb: the rim and planetshine terms are added *after* the
                // banded texture, so they do not divide out, and near the edge they are the
                // larger signal. Sampling the face rather than the limb is what keeps this
                // measuring bands.
                let ny = (i as f32 / 255.0 - 0.5) * 1.2;
                let plain = sample(None, ny);
                if plain <= 0.0 {
                    1.0
                } else {
                    sample(bands, ny) / plain
                }
            })
            .collect();
        let spread = ratios
            .iter()
            .map(|r| (r - 1.0).abs())
            .fold(0.0f32, f32::max);
        let crossings = ratios
            .windows(2)
            .filter(|w| (w[0] - 1.0).signum() != (w[1] - 1.0).signum())
            .count();
        (spread, crossings)
    }

    #[test]
    fn a_second_mate_is_banded_and_a_worker_moon_is_not() {
        // Latitudinal cloud banding is most of what separates a second mate from a worker at a
        // glance, and it is why a gas giant reads as a gas giant rather than as a big moon.
        let (plain, plain_crossings) = meridian_banding(None);
        let (ringed, ringed_crossings) = meridian_banding(BodyType::Ringed.band_count());
        let (gas, gas_crossings) = meridian_banding(BodyType::Gas.band_count());

        // A worker moon carries no bands at all — not "fewer", none.
        assert_eq!((plain, plain_crossings), (0.0, 0));
        assert!(
            ringed > 0.10,
            "a ringed mate's banding is too shallow to see: {ringed:.4}"
        );
        assert!(
            gas > 0.10,
            "a gas giant's banding is too shallow to see: {gas:.4}"
        );

        // A gas giant carries more bands than a ringed planet — 7 against 5 — so its meridian
        // crosses more of them. This is the half that a depth-only assertion misses: the two
        // types have the same band *amplitude* and differ in band *count*.
        assert!(
            gas_crossings > ringed_crossings,
            "a gas giant crosses {gas_crossings} bands against a ringed mate's {ringed_crossings}"
        );
        // ...and a cloud deck is smoother than rock, so a gas giant keeps less of the rocky
        // mottle. Both facts together are what make the two types tell apart at a crop.
        assert!(BodyType::Gas.mottle_scale() < BodyType::Ringed.mottle_scale());
    }

    #[test]
    fn a_rings_particles_land_back_where_they_started_after_one_loop() {
        // Same seamless-loop contract every orbit here obeys: the ambient loop is baked once and
        // played forever, so anything that moves in it has to close. A fractional revolution count
        // shows a seam on every repeat.
        assert_eq!(
            RING_REVOLUTIONS_PER_LOOP,
            RING_REVOLUTIONS_PER_LOOP.round(),
            "the ring's spin must be a whole number of turns per loop"
        );
        let layout = one_mate_scene(BodyType::Ringed, 0.5);
        let start = frame(&layout, 0.0);
        let looped = frame(&layout, 2.0 * PI);
        // Not bit equality: `phase` and `phase + 2*PI` reach the same angle through different
        // floats, so a particle can land a fraction of a pixel over and quantize one step away. A
        // seam is a whole ring's worth of pixels in the wrong place, three orders of magnitude
        // above that.
        let moved = start
            .chunks_exact(4)
            .zip(looped.chunks_exact(4))
            .filter(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) > 1))
            .count();
        assert!(
            moved * 2_000 < start.len() / 4,
            "{moved} pixels moved across the loop seam — the ring does not close"
        );
    }

    /// A fleet of `mates` second mates, each with three workers, spread across the whole
    /// project-size register and the whole streak range — so both the ring and the gas path are
    /// exercised at every count rather than one flat body repeated.
    fn scaling_fleet(mates: usize) -> Vec<TreeNode> {
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for i in 0..mates {
            let files = (i as u32 * 337) % FILES_CEIL;
            let streak = (i % 5) as f32 / 4.0;
            nodes.push(TreeNode {
                streak,
                ..sized(Some(0), BodyKind::Planet, BodySize::Files(files))
            });
            let planet_idx = nodes.len() - 1;
            for _ in 0..3 {
                nodes.push(node(Some(planet_idx), BodyKind::Moon));
            }
        }
        nodes
    }
}
