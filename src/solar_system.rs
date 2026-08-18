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
/// The fleet orrery's own void constant `#03060b`, ported exactly. Rec.709 luminance **5.72**,
/// which is the *median pixel* of the reference frame: the median pixel of that picture is bare
/// void, and that is what makes the few bright things in it read as bright.
///
/// **Flat, and that is the whole point.** This used to be `(6, 8, 16)` under a vertical gradient
/// that lifted it by up to `0.06` — measured, the frame's median came out at 16.30 against the
/// reference's 5.72, with **not one pixel** as dark as the reference's median. A gradient across the
/// void is a light budget spent on a wash, and the reference spends the same budget on objects: it
/// puts more pixels above L64 and above L128 than herdr did while sitting three times darker
/// overall.
const SPACE_SURFACE: (u8, u8, u8) = (3, 6, 11);

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

    /// How far this body orbits from its parent, as a fraction of `min(width, height)` — the
    /// **semi-major axis**, not a radius, since every mate's orbit is an ellipse. Unused for
    /// [`Self::Sun`], which never orbits anything.
    ///
    /// A second mate reads its own rung of [`ORBIT_LADDER`]; a mate with no rung (nothing seated it,
    /// which can only be a caller drawing an unseated body) falls back to the innermost so it is
    /// never placed on top of the sun. A worker keeps its single small ring around its own mate —
    /// the ladder is the *mate* tier's composition, and a worker's distance from its mate carries
    /// nothing.
    fn orbit_radius_fraction(self, slot: Option<usize>) -> f32 {
        match self {
            Self::Sun => 0.0,
            Self::Planet => {
                ORBIT_LADDER[slot.unwrap_or(0).min(ORBIT_LADDER_SLOTS - 1)] * LADDER_UNIT_FRACTION
            }
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
    /// **`a` stays out of the normalisation, and the ladder does not change that — but for a
    /// different reason than before.** It used to cancel because herdr drew one ring per tier and
    /// every mate sat at the same distance. Now the rungs span `6.13x` and `a` genuinely has
    /// something to say, and the band cannot say it: `a^1.5` alone spans `15x` across the ladder,
    /// and [`FRAME_COUNT`] caps the fast end of the band at three whole revolutions per loop. A
    /// three-integer band cannot carry a fifteen-fold spread, and folding it in anyway collapses the
    /// *mass* reading — measured, the real six-mate fleet fell from three distinct periods to two.
    ///
    /// So the mass register keeps the band, and the ladder's separation reads where it now actually
    /// is: on screen, as linear speed, because a body on the outer rung sweeps six times the arc per
    /// revolution. Expressing `a` in the *period* as well needs sampling headroom this loop does not
    /// have; it is a real open item and not this pass's.
    pub(crate) fn revolutions_per_loop(self, size: BodySize) -> f32 {
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

/// The fleet orrery's own `ORBIT_LADDER`, in its scene units (`hone-bodies.html:5747`).
///
/// **The spread is the composition.** Eight seated mates on *one* radius is a clock face, not a
/// solar system — every effect already built on top of it (the wear grooves, the mass-driven
/// period, the ring cap) reads as one ring at eight angles because there was only ever one ring.
/// The ladder is what those things were built to sit on: innermost to outermost is `1140/186 =
/// 6.13x`, so the eye reads distance from the sun as a register before it reads anything else.
const ORBIT_LADDER: [f32; ORBIT_LADDER_SLOTS] =
    [186.0, 292.0, 400.0, 512.0, 672.0, 826.0, 980.0, 1140.0];

/// One ladder unit as a fraction of `min(width, height)`.
///
/// Solved from the artifact's own composed frame rather than chosen: at 1920x1080 its
/// `SCALE_BASE = max(0.62, min((W - PANEL_W)/1580, H/1080))` is `0.99494`, so the outermost rung
/// draws at `1140 * 0.99494 = 1134px` against a 1080px short side. The outermost orbit is
/// therefore **wider than the frame is tall** — the system overfills its picture instead of being
/// inscribed in it, which is the single most visible difference between an orrery and a diagram of
/// one.
const LADDER_UNIT_FRACTION: f32 = 0.994_94 / 1080.0;

/// How far the orbital plane is foreshortened vertically. The artifact's `YS` (`:1097`).
///
/// This one number is what makes the scene a three-quarter view of a system rather than a top-down
/// map of one. Applied to every orbit — a mate's around the sun and a worker's around its mate —
/// because they are the same plane.
const ORBIT_PLANE_SQUASH: f32 = 0.72;

/// How much nearer/farther the plane's front and back edges draw, as a fraction of a body's own
/// radius. The artifact's `DEPTH_K` (`:1414`), and the reason its `MATE_R_MAX` carries a
/// perspective factor at all.
///
/// `1 +/- 0.27` is a **1.74x** spread between a body at the near edge and the same body at the far
/// edge, which clears H4's stated `>= 1.5x` bound. Occlusion comes with it: bodies are drawn back
/// to front, so a near mate passing a far one covers it.
const DEPTH_K: f32 = 0.27;

/// The full width of the per-mate inclination band, in the same units as [`ORBIT_PLANE_SQUASH`]'s
/// vertical offset. The artifact's bridge-side seeding, `((seed%17)/17 - 0.5) * 0.34` (`:5910`) —
/// so a mate's own plane is tilted up to `+/-0.17` out of the shared one.
///
/// Without it eight ellipses share one centre and one tilt and read as concentric contour lines.
/// With it they cross, which is what a system of independent orbits looks like.
const INCLINATION_SPAN: f32 = 0.34;

/// A mate's orbital eccentricity: the floor every orbit carries, and the divisor that turns its own
/// file count into the rest. The artifact's `e = 0.085 + ((files*13)%9)/58` (`:1001`) — a
/// `0.085..=0.223` band, read off the same register the radius and the period already read.
///
/// A circle is the one orbit shape that carries no information. This is the register showing up in
/// the *shape* of the path as well as its size, and it is why the grooves are not nested rings.
const ECCENTRICITY_FLOOR: f32 = 0.085;
const ECCENTRICITY_DIVISOR: f32 = 58.0;

/// Where the sun sits horizontally, as a fraction of the main area — the frame less the panel the
/// worker tree occupies on the left. The artifact's `SUN.x = PANEL_W + (W-PANEL_W)*0.51` (`:1076`),
/// with its own measured note that 51% is the largest shift that still leaves the outermost track
/// running behind the panel.
const SUN_X_OF_MAIN_AREA: f32 = 0.51;
const SUN_Y_OF_FRAME: f32 = 0.487;

/// How wide the left panel is, as a fraction of frame width, and the ceiling that fraction is held
/// under. The artifact's `PANEL_W = clamp(302, 348, round(W*0.195))` (`:1074`).
///
/// Derived from the frame rather than read off the client's sidebar on purpose: this generator is
/// server-owned and pure, and a scene whose composition depended on a UI surface's current width
/// would be a presentation fact reaching into runtime state. The artifact's own panel is a fixed
/// share of the frame, so the share is the portable half of the rule.
///
/// **The artifact's 302px *floor* is deliberately not ported.** It is a minimum readable width for a
/// panel of real DOM in a browser window, not a composition rule — and carried over literally it
/// stops being a fifth of the frame and starts being a third of it: at a 900px-wide terminal the
/// floor would reserve 33% of the picture and push the sun out to 67% of the width. The ceiling is
/// composition and is kept, so the reserve stops growing on a wall display. At every width from
/// about 1,550px up the fraction is above the artifact's floor anyway, so this is the artifact's own
/// number wherever the artifact had one.
const PANEL_WIDTH_FRACTION: f32 = 0.195;
const PANEL_WIDTH_CEIL_PX: f32 = 348.0;

/// Where the scene's own centre of mass sits, in pixels — the sun's position, and the origin every
/// orbit is measured from.
fn scene_origin(width: u32, height: u32) -> (f32, f32) {
    let panel = panel_width(width);
    (
        panel + (width as f32 - panel) * SUN_X_OF_MAIN_AREA,
        height as f32 * SUN_Y_OF_FRAME,
    )
}

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
/// Which rung of [`ORBIT_LADDER`] each node sits on, whether it is drawn at all, and the seated and
/// overflow counts.
pub(crate) struct Ladder {
    /// Whether this node is drawn — `false` for a mate the ring had no slot for, and everything
    /// under it.
    pub(crate) seated: Vec<bool>,
    /// The ladder rung a seated second mate sits on, `None` for everything that is not one.
    ///
    /// A42(d): the selected set is seated **in roster order**, never sorted by the register that
    /// selected it. The artifact costed both alternatives and both break something — largest
    /// outermost stretches the slowest orbit past its published period, largest innermost collapses
    /// the period spread from 8.8x to 2.9x and takes the mass reading with it.
    slot: Vec<Option<usize>>,
    seated_count: usize,
    beyond: usize,
}

/// Returns one flag per node — whether it is drawn — alongside the seated and overflow counts.
pub(crate) fn seat_the_ladder(nodes: &[TreeNode]) -> Ladder {
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

    // The ladder's rungs go to the seated set in the roster's own order — A42(d) — so the ring's
    // spacing states composition and the register states size, rather than one number saying both.
    let mut slot = vec![None; nodes.len()];
    let mut next = 0usize;
    for (idx, node) in nodes.iter().enumerate() {
        if node.kind == BodyKind::Planet && seated[idx] && next < ORBIT_LADDER_SLOTS {
            slot[idx] = Some(next);
            next += 1;
        }
    }

    let shown = total.min(ORBIT_LADDER_SLOTS);
    Ladder {
        seated,
        slot,
        seated_count: shown,
        beyond: total.saturating_sub(ORBIT_LADDER_SLOTS),
    }
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
pub(crate) fn assign_body_types(nodes: &[TreeNode], seated: &[bool]) -> Vec<BodyType> {
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

/// The longest caption a body carries in the sky. A project name past this is cut on a character
/// boundary rather than wrapped: a sky label is an identifier, and a two-line name beside a body is
/// a paragraph in the middle of a picture.
pub(crate) const SCENE_LABEL_CAP: usize = 40;

/// One body's own name, as it is captioned in the sky.
///
/// Carried inline rather than as a `String` so [`TreeNode`] stays `Copy` — every consumer of the
/// node slice copies freely, and one heap-allocated field would make the whole owner tree a
/// different kind of value for the sake of forty bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SceneLabel {
    bytes: [u8; SCENE_LABEL_CAP],
    len: u8,
}

impl Default for SceneLabel {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl SceneLabel {
    /// A body with nothing to say — the caption is simply not drawn.
    pub(crate) const EMPTY: Self = Self {
        bytes: [0; SCENE_LABEL_CAP],
        len: 0,
    };

    /// The caption for `name`, cut at [`SCENE_LABEL_CAP`] on a character boundary.
    pub(crate) fn new(name: &str) -> Self {
        let mut end = name.len().min(SCENE_LABEL_CAP);
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0u8; SCENE_LABEL_CAP];
        bytes[..end].copy_from_slice(&name.as_bytes()[..end]);
        Self {
            bytes,
            len: end as u8,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        // Only ever written from a `&str` prefix cut on a character boundary, so this cannot fail;
        // an unexpected byte draws nothing rather than panicking a render.
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// One node of the fleet's owner tree, exactly as `src/app/background_scene.rs` derived it from
/// `crate::ui::sidebar::workspace_list_entries_whole_fleet` — this module knows nothing about
/// panes, workspaces or tokens, only shape and already-resolved colour, size and streak facts.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TreeNode {
    /// What this body is called, for its caption in the sky. Empty for anything that should not
    /// carry one — see [`draw_sky_label`].
    pub(crate) label: SceneLabel,
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
    /// How worn this body's own orbit is, `0.0..=1.0` — see [`OrbitWear`]. Already quantized by
    /// `src/app/background_scene.rs`, which owns the accumulation and the clock.
    pub(crate) wear: f32,
    /// How many ambient motes this body has earned, and how bright each of them draws.
    ///
    /// One mote per unit of work the body's own agent actually did, and a share resolved through
    /// the attribution transform in `src/app/background_scene.rs` — this module draws them and
    /// counts nothing.
    pub(crate) motes: u32,
    pub(crate) mote_share: f32,
}

/// How deep a body's orbit track is worn, in whole steps.
///
/// **Density is revolutions completed** — a groove in this scene means "how much has passed here",
/// and an orbit that has been travelled a thousand times has a deeper one than an orbit that has
/// been travelled twice.
///
/// Quantized to a small number of steps rather than carried as a continuous number, and that is
/// the load-bearing decision rather than a tidy one: the ambient scene is *baked* into a cached
/// loop and regenerated only when its key moves, so a continuously-varying wear value would
/// re-bake all [`FRAME_COUNT`] frames on every tick. In steps, a re-bake happens exactly when a
/// track visibly deepens — which at real orbital rates is a few times an hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct OrbitWear(pub(crate) u8);

impl OrbitWear {
    /// How many steps of wear a track has, above bare. Four, matching the track material's own
    /// 1–4px width: the material is what the steps are steps *of*.
    pub(crate) const STEPS: u8 = 4;

    /// The wear a body's own revolution count has earned, as a step.
    ///
    /// Saturating rather than running away: a groove that keeps deepening forever would end up the
    /// loudest thing in the frame, and a readout that shouts over its own subject is a composition
    /// defect rather than a data one.
    pub(crate) fn of(revolutions: f32) -> Self {
        let fraction = clamp01(revolutions / REVOLUTIONS_TO_FULL_WEAR);
        // `sqrt` rather than linear: the first hundred revolutions of a fresh orbit are the ones
        // that say "this body has been here a while", and the ten-thousandth says very little the
        // nine-thousandth did not.
        Self((fraction.sqrt() * Self::STEPS as f32).round() as u8)
    }

    /// This step as a `0.0..=1.0` fraction, for the renderer.
    pub(crate) fn fraction(self) -> f32 {
        f32::from(self.0.min(Self::STEPS)) / f32::from(Self::STEPS)
    }
}

/// Revolutions at which a track is fully worn.
///
/// A second mate completes one to three per animation loop, and a loop is about five seconds — so
/// full wear is a few hours of a body sitting in the fleet. Long enough that the deepest groove
/// means something, short enough that a working day reaches it.
const REVOLUTIONS_TO_FULL_WEAR: f32 = 2_400.0;

/// How wide a track is drawn at bare and at full wear, in pixels. The orrery's own 1–4px.
const TRACK_WIDTH_PX: (f32, f32) = (1.0, 4.0);

/// How bright a track is at bare and at full wear.
///
/// The top is deliberately low. The grooves are a real readout, and they are also the loudest
/// thing in the field after the bodies themselves — A33's single stated gain, applied at the one
/// place marks are written, and held above the level where a groove stops reading as a path.
const TRACK_ALPHA: (f32, f32) = (0.045, 0.30);

/// One body's static placement facts, resolved once per topology change (mirrors
/// `App::observe_sidebar_particle_field`'s "regenerate on resize, not per tick" cadence — a body
/// added or removed is the equivalent event here).
#[derive(Debug, Clone, Copy)]
struct BodyLayout {
    parent: Option<usize>,
    kind: BodyKind,
    /// This body's caption in the sky, and the register reading it prints under it — see
    /// [`draw_sky_label`].
    label: SceneLabel,
    size: BodySize,
    body_type: BodyType,
    hue: f32,
    severity: Severity,
    /// This mate's already-resolved streak expression, `0.0..=1.0` — see [`TreeNode::streak`].
    streak: f32,
    /// How worn this body's own orbit is, `0.0..=1.0` — see [`OrbitWear`].
    wear: f32,
    motes: u32,
    mote_share: f32,
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
    /// The **semi-major axis** of this body's own orbit, in pixels. Named for what it was when
    /// every orbit was a circle; it is the `a` of an ellipse now — see [`BodyLayout::eccentricity`].
    orbit_radius_px: f32,
    /// How far from circular this body's orbit is, `0.0..=0.223` — see [`ECCENTRICITY_FLOOR`].
    eccentricity: f32,
    /// Which way this body's ellipse points: the angle of its closest approach to its parent.
    periapsis: f32,
    /// How far this body's own orbital plane is tilted out of the shared one, `+/-0.17` — see
    /// [`INCLINATION_SPAN`]. Zero for a worker, whose ring sits in its mate's own plane.
    inclination: f32,
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

/// How far the sun's corona reaches, as a multiple of its own radius.
///
/// Far further than a planet's atmospheric fringe, because a corona *is* far further — and it is
/// what separates the one self-luminous body in the scene from a bright disc with a halo drawn
/// round it.
/// The artifact's own `COR_R = 2.55` (`hone-bodies.html:4026`), not a number of herdr's own.
const CORONA_REACH: f32 = 2.55;

/// The corona's peak alpha at the limb, before the streamer structure cuts into it. The artifact's
/// own `COR_A`, and it is a **swept and settled** number rather than a chosen one.
///
/// Its build notes record the sweep verbatim: made visible, the corona *"instantly became the
/// loudest perpetual thing in the frame: 6,799 units in one coarse cell, against a ceremonial
/// event's 2,367"*, and it was swept `0.165 -> 6,799 | 0.080 -> 1,354 | 0.040 -> 207 | 0.020 -> 5`
/// and settled here, with the cost stated: *"at a level F23 permits the corona is FAINT — a
/// structured, asymmetric, streamered presence at the limb rather than the thing a photograph of an
/// eclipse shows. A corona big enough to look like the reference and a frame that keeps its
/// ceremonial tier legible are in tension, and the bar wins."*
///
/// herdr's was `0.55` — over seven times this — and it measured: the annulus at 1.02–1.60 solar
/// radii sat **+73 luminance units** over the frame's own void, where the reference's sits at
/// **+0.00**. Structure is what survives a light budget; a wash is what does not.
const CORONA_PEAK: f32 = 0.075;

/// How many streamers the corona carries around the disc. The artifact's own `COR_LOBES = 5`.
///
/// Structure rather than an even halo: a real corona is streamers at stated angular widths, and an
/// evenly falling glow is exactly the "light source" default a scene of real bodies is refusing.
///
/// **And the structure is what makes the light budget affordable.** The previous implementation
/// sampled one noise field around the disc with a floor of `1 - 0.55`, so *every* angle carried at
/// least 45% of the peak — a wash with ripples in it. The artifact's model is five lobes at stated
/// angular half-widths covering roughly a quarter of the disc's circumference and **nothing** in
/// between, which is why the median pixel of its own annulus reads as bare void while the streamers
/// themselves are still clearly there.
const CORONA_STREAMERS: usize = 5;

/// Where the corona's outer taper begins, as a fraction of [`CORONA_REACH`] measured back from the
/// edge. The artifact's `v *= clamp01((COR_R - rho) / (COR_R * 0.30))` — *"it must actually END
/// rather than being clipped by the canvas edge"*.
const CORONA_TAPER: f32 = 0.30;

/// The fine radial striation across a streamer: its floor and its span. The artifact's
/// `0.55 + 0.90 * frac(...)`, widened there from `0.80..1.20` deliberately — *"the same mean, twice
/// the range — because this is the term that decides whether a streamer reads as a bundle of threads
/// or as a smear, and it costs nothing in the sum."*
const CORONA_STRIATION: (f32, f32) = (0.55, 0.90);

/// One streamer of the corona: where it points, how wide it is, how bright, and how fast it falls
/// off with distance.
///
/// Solved per sun from one hash rather than authored, so the asymmetry is a reproducible property of
/// the star rather than of the frame it happened to be drawn in — the artifact's own reasoning for
/// deriving all four numbers from the same place.
#[derive(Debug, Clone, Copy, Default)]
struct CoronaLobe {
    angle: f32,
    half_width: f32,
    amplitude: f32,
    radial_power: f32,
}

/// The artifact's own hash — `frac(sin(x) * k)`. Ported rather than replaced by
/// [`value_noise`] because these five lobes are the shape of a specific published picture, and a
/// different hash is a different corona.
fn hash_frac(x: f32, k: f32) -> f32 {
    let v = (x.sin() * k).fract();
    if v < 0.0 {
        v + 1.0
    } else {
        v
    }
}

/// The corona's five streamers, exactly as `bakeCorona` solves them.
///
/// Equatorial streamers are long and narrow, polar plumes short and stubby — `eq` is `|cos(angle)|`,
/// `1` at the equator and `0` at the poles, and it is what makes a corona read as a body with an
/// axis rather than a starburst.
fn corona_lobes() -> [CoronaLobe; CORONA_STREAMERS] {
    let mut lobes = [CoronaLobe::default(); CORONA_STREAMERS];
    for (i, lobe) in lobes.iter_mut().enumerate() {
        let i = i as f32;
        let h1 = hash_frac(i * 12.9898 + 3.7, 43_758.545);
        let h2 = hash_frac(i * 78.233 + 1.1, 24_634.635);
        let h3 = hash_frac(i * 5.113 + 9.4, 1_237.77);
        let angle = (i / CORONA_STREAMERS as f32) * 2.0 * PI + (h1 - 0.5) * 0.42;
        let eq = angle.cos().abs();
        *lobe = CoronaLobe {
            angle,
            half_width: 0.038 + 0.055 * h2 + 0.030 * eq,
            amplitude: (0.55 + 0.45 * h3) * (0.42 + 0.58 * eq * eq),
            radial_power: 1.15 + 0.70 * (1.0 - eq) + 0.35 * h1,
        };
    }
    lobes
}

/// How much of the corona this pixel carries, `0.0` over the three quarters of the disc no streamer
/// points at — before [`CORONA_PEAK`] scales it.
fn corona_at(lobes: &[CoronaLobe; CORONA_STREAMERS], angle: f32, rho: f32, seed: u32) -> f32 {
    let mut v = 0.0f32;
    for lobe in lobes {
        // A raised cosine in angle, cut off at `1.9` half-widths — past that a streamer has ended.
        let da = (angle - lobe.angle + PI).rem_euclid(2.0 * PI) - PI;
        let u = da.abs() / lobe.half_width;
        if u > 1.9 {
            continue;
        }
        let ang_k = 0.5 * (1.0 + (u * PI / 1.9).min(PI).cos());
        // ...and a power law in radius, so a streamer leaves the star and keeps going rather than
        // stopping at a stated ring.
        v += lobe.amplitude * ang_k * (1.0 / rho).powf(lobe.radial_power);
    }
    if v <= 0.0 {
        return 0.0;
    }
    // Threads rather than a smear: banded in `rho` so the striation runs *along* the streamer.
    v *= CORONA_STRIATION.0
        + CORONA_STRIATION.1 * value_noise(angle * 37.1, (rho * 9.0).floor() * 4.7, seed);
    v * clamp01((CORONA_REACH - rho) / (CORONA_REACH * CORONA_TAPER))
}

/// How far a prominence reaches past the limb, as a fraction of the corona's own reach, how many
/// there are around the disc, and how bright they get.
///
/// Short, so they read as arcs *off the edge* rather than as a second, lumpier corona — and now
/// short by the artifact's own numbers rather than by intent. Its `drawProminences` gives five
/// (`PROM_N`) arcs reaching `0.055..0.16` of the sun's radius at alpha `0.30`; over a corona
/// reaching `2.55 R` that top is `0.16 / 1.55 = 0.103` of the reach. The previous `0.28` at gain
/// `0.45` over nine of them put a second corona at the limb, which is exactly what the corona sweep
/// had just finished taking out.
const PROMINENCE_REACH: f32 = 0.103;
const PROMINENCE_COUNT: f32 = 5.0;
const PROMINENCE_GAIN: f32 = 0.30;

/// Where the angular gate opens for a prominence, and over how much noise it opens fully.
///
/// A prominence is an *arc*, a few hundredths of a radian wide in the artifact. The old gate
/// (`noise * 1.8 - 0.9`) opened over half of every angle around the disc, which is a lumpy ring
/// rather than five arcs.
const PROMINENCE_GATE: (f32, f32) = (0.74, 0.86);

/// How far a ring shadow's edge is feathered, in units of the planet's radius.
const RING_SHADOW_FEATHER: f32 = 0.06;

/// How much of the direct light a ring shadow removes at its darkest. Not all of it: a ring is
/// translucent, and a black band across a planet reads as a hole rather than a shadow.
const RING_SHADOW_DEPTH: f32 = 0.82;

/// How far the *planet's* own shadow reaches across its rings, as a multiple of its radius, and
/// how deep it goes. The other half of the pair, and the one the ring's particle stream makes
/// available for the cost of one dot product.
const PLANET_SHADOW_REACH: f32 = 1.10;
const PLANET_SHADOW_DEPTH: f32 = 0.88;

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
    /// **Measured against the radius the body is actually drawn at**, not its resting one: a mate on
    /// the near side of the plane draws `1 + DEPTH_K` larger, and a ring solved from the resting
    /// radius would sit inside its own planet there and float off it on the far side. Every geometry
    /// that answers to the body's size takes the drawn radius for the same reason — the ring's own
    /// shadow on the planet, and the planet's on the ring, are both solved in these units.
    fn ring_radii_px(&self, drawn_radius_px: f32) -> Option<(f32, f32)> {
        if self.body_type != BodyType::Ringed {
            return None;
        }
        Some((
            drawn_radius_px * RING_INNER,
            drawn_radius_px * (RING_OUTER + RING_OUTER_PER_STREAK * clamp01(self.streak)),
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
    let Ladder {
        seated,
        slot,
        seated_count: mates_seated,
        beyond: mates_beyond_ladder,
    } = seat_the_ladder(nodes);
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
        let ladder_slot = slot.get(idx).copied().flatten();
        // A mate's own orbital elements are seeded off its rung, exactly as the artifact seeds a
        // mate that arrives over the bridge (`hone-bodies.html:5910`): angle, eccentricity,
        // periapsis and inclination all fall out of one number, so two identical snapshots place
        // the same body identically and no two mates share a plane.
        //
        // Spread-by-sibling-index is what a *worker* still gets: workers share one ring around
        // their mate, so the even spread is the only thing keeping them apart.
        let base_angle = match ladder_slot {
            Some(rung) => (ORBIT_LADDER[rung] * 0.37).rem_euclid(2.0 * PI),
            None => parent_angle + (sibling_index as f32 / sibling_count as f32) * 2.0 * PI,
        };
        let (eccentricity, periapsis, inclination) = match ladder_slot {
            Some(rung) => {
                // Eccentricity is read off the mate's *own* file count rather than off its rung —
                // the artifact's `e = 0.085 + ((files*13)%9)/58` — so the register shows up in the
                // shape of the path as well as in its size. An unmeasured project has no count to
                // read and draws the floor's circle.
                let files = match node.size {
                    BodySize::Files(files) => files.min(FILES_CEIL),
                    BodySize::Unmeasured | BodySize::Fixed => 0,
                };
                let e = ECCENTRICITY_FLOOR
                    + ((files.wrapping_mul(13)) % 9) as f32 / ECCENTRICITY_DIVISOR;
                let rung_units = ORBIT_LADDER[rung];
                let incl = ((rung_units as u32 % 17) as f32 / 17.0 - 0.5) * INCLINATION_SPAN;
                (e, (rung_units * 0.021).rem_euclid(2.0 * PI), incl)
            }
            // A worker's ring sits in its mate's own plane, and is round: the register it would
            // read has nothing in it, and a tilted ring around a body a few dozen pixels across is
            // noise rather than depth.
            None => (0.0, 0.0, 0.0),
        };

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
            // Only a second mate is captioned. The reference labels its mates and its sun and leaves
            // the workers bare, and at a worker's size a caption is longer than the thing it names.
            label: match node.kind {
                BodyKind::Moon => SceneLabel::EMPTY,
                _ => node.label,
            },
            size: node.size,
            body_type,
            hue: node.hue,
            severity: node.severity,
            streak,
            wear: clamp01(node.wear),
            motes: node.motes,
            mote_share: clamp01(node.mote_share),
            seated: seated.get(idx).copied().unwrap_or(true),
            revolutions_per_loop: node.kind.revolutions_per_loop(node.size),
            base_angle,
            orbit_radius_px: node.kind.orbit_radius_fraction(ladder_slot) * scale * nesting,
            eccentricity,
            periapsis,
            inclination,
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

    /// Where one body is drawn at `phase`, for a caller outside this module checking that the
    /// scene's own composition reached the picture.
    #[cfg(test)]
    pub(crate) fn body_position(&self, idx: usize, phase: f32) -> (f32, f32) {
        self.position(idx, phase)
    }

    /// The radius one body is **actually drawn at** at `phase`, depth included.
    ///
    /// Everything that answers to a body's size on screen — its ring's inner and outer radii, the
    /// shadow it throws across them — is solved in these units, so a measurement taken against the
    /// resting radius is measuring a body that is not the one in the frame.
    #[cfg(test)]
    fn drawn_radius_px(&self, idx: usize, phase: f32) -> f32 {
        self.bodies
            .get(idx)
            .map(|body| body.body_radius_px * self.place(idx, phase).depth_scale())
            .unwrap_or(0.0)
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// One seated body's current position in the normalised coordinates transient effects store.
    pub(crate) fn body_position_normalized(&self, idx: usize, phase: f32) -> Option<(f32, f32)> {
        self.is_seated(idx).then(|| {
            let position = self.position(idx, phase);
            (
                position.0 / self.width.max(1) as f32,
                position.1 / self.height.max(1) as f32,
            )
        })
    }

    /// This body's on-screen centre at `phase` (`0.0..=2*PI` covering one full animation loop).
    fn position(&self, idx: usize, phase: f32) -> (f32, f32) {
        let placed = self.place(idx, phase);
        (placed.x, placed.y)
    }

    /// This body's full placement at `phase`: where it is on screen, and where it is in the plane's
    /// own depth.
    ///
    /// One walk up the parent chain produces both, because the second falls out of the first —
    /// splitting them would either walk the chain twice per body per frame or carry a second copy
    /// of the projection.
    fn place(&self, idx: usize, phase: f32) -> Placed {
        let centre = scene_origin(self.width, self.height);
        let Some(body) = self.bodies.get(idx) else {
            return Placed::at(centre);
        };
        let parent = body
            .parent
            .map(|parent| self.place(parent, phase))
            .unwrap_or_else(|| Placed::at(centre));
        if body.orbit_radius_px <= 0.0 {
            return parent;
        }
        let angle = body.base_angle + phase * body.revolutions_per_loop;
        let (sin_a, cos_a) = angle.sin_cos();
        // The orbit equation, straight off the artifact's `orbRadius`: an ellipse with its parent at
        // one focus, so a body genuinely comes closer and swings out again rather than tracing a
        // circle drawn off-centre.
        let e = body.eccentricity;
        let r = body.orbit_radius_px * (1.0 - e * e) / (1.0 + e * (angle - body.periapsis).cos());
        Placed {
            x: parent.x + cos_a * r,
            // `sin(th)*r*YS` is the shared plane seen at a slant; `cos(th)*r*incl` is this body's own
            // plane tilted out of it. The artifact writes the second as `sin(th + PI/2)`, which is
            // the same number one identity later.
            y: parent.y + sin_a * r * ORBIT_PLANE_SQUASH + cos_a * r * body.inclination,
            // +1 at the near edge of the plane, -1 at the far edge. Carried as the raw cue for
            // [`Self::depth_scale`] and in real pixels for draw order, because "which of these two
            // is in front" is a question about distance and not about phase.
            z: sin_a,
            depth: parent.depth + sin_a * r,
        }
    }

    /// Every seated body, farthest first.
    ///
    /// Painter's algorithm, and the cheapest possible occlusion: a near mate passing a far one
    /// simply covers it, which is the whole difference between a plane the bodies are *in* and a set
    /// of discs laid on top of one picture. Ties keep roster order so a rebuild never reshuffles two
    /// bodies at the same depth.
    fn draw_order(&self, phase: f32) -> Vec<usize> {
        let mut order: Vec<(usize, f32)> = self
            .bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| body.seated)
            .map(|(idx, _)| (idx, self.place(idx, phase).depth))
            .collect();
        order.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        order.into_iter().map(|(idx, _)| idx).collect()
    }
}

/// One body's placement in the projected plane: where it is drawn, and how far into the picture it
/// is.
#[derive(Debug, Clone, Copy)]
struct Placed {
    x: f32,
    y: f32,
    /// The depth cue for this body's *own* orbit, `-1.0..=1.0` — what [`SceneLayout::depth_scale`]
    /// reads.
    z: f32,
    /// Depth in pixels, summed up the whole parent chain — what draw order sorts on. A moon on the
    /// far side of its mate is behind that mate, whichever side of the sun the pair is on.
    depth: f32,
}

impl Placed {
    /// How much larger or smaller a body draws for being on the near or far side of its orbit —
    /// `1 +/- DEPTH_K`, the artifact's `depthScale`. The other half of what makes this a plane the
    /// bodies are *in* rather than a set of discs laid on one picture.
    fn depth_scale(&self) -> f32 {
        1.0 + self.z * DEPTH_K
    }

    fn at(centre: (f32, f32)) -> Self {
        Self {
            x: centre.0,
            y: centre.1,
            z: 0.0,
            depth: 0.0,
        }
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

/// The three magnitudes of a win. The producer names the tier; this renderer owns its visual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WinTier {
    Ask,
    Ci,
    Merge,
}

impl WinTier {
    pub(crate) fn head_scale(self) -> f32 {
        match self {
            Self::Ask => 1.0,
            Self::Ci => 2.0,
            Self::Merge => 4.0,
        }
    }

    pub(crate) fn trail_scale(self) -> f32 {
        match self {
            Self::Ask => 1.0,
            Self::Ci => 230.0 / 150.0,
            Self::Merge => 330.0 / 150.0,
        }
    }

    /// Ballast: larger wins leave more slowly, matching the captain's reference artifact.
    pub(crate) fn ejection_scale(self) -> f32 {
        match self {
            Self::Ask => 1.0,
            Self::Ci => 0.81,
            Self::Merge => 0.62,
        }
    }

    pub(crate) fn life_scale(self) -> f32 {
        match self {
            Self::Ask => 1.0,
            Self::Ci => 21.0 / 13.0,
            Self::Merge => 34.0 / 13.0,
        }
    }
}

/// A comet crossing the whole scene. `start`/`end` are normalised `0.0..=1.0` scene coordinates;
/// `magnitude` is the already-resolved work-size intensity (`0.0..=1.0`, quiet green-test tier at
/// the bottom, a landed large task at the top) driving both brightness and tail length.
#[derive(Debug, Clone)]
pub(crate) struct Comet {
    pub(crate) start: (f32, f32),
    pub(crate) end: (f32, f32),
    /// The body this comet is flying *into*, if it is an arrival rather than a crossing — a
    /// landing comet ends on the body the work landed on, which moves along its own orbit, so the
    /// endpoint has to be resolved per-frame here rather than frozen into `end` at spawn time.
    /// `None` (a crossing) uses `end` exactly as given, which is the round-1 behaviour unchanged.
    pub(crate) target: Option<usize>,
    pub(crate) magnitude: f32,
    pub(crate) tier: WinTier,
    /// Actual earlier normalised positions observed while this comet was live, oldest first.
    pub(crate) trail: std::sync::Arc<Vec<(f32, f32)>>,
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

/// A body falling toward something accelerates into it.
///
/// Every travelling thing in this scene moved at a constant speed: an asteroid crossed the last
/// tenth of its approach as slowly as the first, and a comet arriving at a mate coasted in.
/// Nothing falls like that. A quadratic ease-in is the standard accelerate — position goes as
/// `t^2`, so speed goes as `2t` and rises linearly through the travel, which is what constant
/// acceleration actually is rather than a curve chosen because it looked right.
///
/// Both endpoints are exact: `ease_in(0) == 0` and `ease_in(1) == 1`, so an effect still starts
/// where it started and lands where it landed, and the caller's own `progress` stays the thing
/// that says how far through it is.
fn ease_in(t: f32) -> f32 {
    let t = clamp01(t);
    t * t
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

/// Whole turns per animation loop at the equator and at the pole.
///
/// Differential rotation: the zonal rate falls toward the poles, so belts travel *past* each other
/// rather than together — the difference between a banded body and a striped one. Whole numbers
/// because the loop is baked once and played forever, and a rate that is not a whole number of
/// turns leaves the belts somewhere else at the end of the loop than at the start.
const BAND_TURNS: (f32, f32) = (4.0, 2.0);

/// How far the domain warp displaces the band coordinate, at two octaves. The orrery's own
/// `0.085` / `0.034`.
const BAND_WARP: (f32, f32) = (0.085, 0.034);

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
    /// This body's own rotation, in radians. Bands are advected by it, so a gas giant's belts
    /// travel past each other instead of being a still image slid around the frame.
    spin: f32,
    /// This body's ring, inner and outer radius in units of its own radius, or `None`. Only used
    /// to cast the ring's shadow back onto the surface.
    ring: Option<(f32, f32)>,
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
        spin,
        ring,
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

            // Two harmonics plus a darker belt is a still image being slid around the frame. What
            // a gas giant actually has is turbulence sheared by **differential rotation**: the
            // zonal rate falls with latitude, so belts travel past each other and the boundaries
            // between them curl.
            //
            // The longitude of the visible hemisphere is recovered from `nx` and the latitude's
            // own cosine; the zonal rate is a function of latitude alone; and the band coordinate
            // is the latitude *warped* by noise in (advected longitude, latitude) — a domain warp,
            // so the band edges wander instead of tracing a clean sine. Two octaves: one reads as
            // a wobble and three cost more than they show at these radii.
            let cos_lat = (1.0 - latitude * latitude).max(1e-3).sqrt();
            let longitude = (nx / cos_lat).clamp(-1.0, 1.0).asin();
            // **Whole turns per loop, per latitude.** The zonal rate falls toward the poles, which
            // is what differential rotation is — but the loop is baked once and played forever, so
            // a rate that is not a whole number of turns leaves the belts somewhere else at the
            // end of the loop than at the start. Quantizing the rate is the same answer the
            // orbital periods needed, for the same reason, and it costs nothing here: what makes a
            // belt read as sheared is that its neighbour moves at a *different* rate, not that the
            // difference is smooth.
            let turns = mix(BAND_TURNS.0, BAND_TURNS.1, latitude * latitude).round();
            let advected = longitude + seed_phase + spin * turns;
            // The warp is sampled on the *circle* of the advected longitude rather than on its raw
            // value, because value noise is a hash lattice and is not periodic: fed the raw
            // longitude, a belt would land on a different lattice cell after a whole turn and the
            // seam would show. `cos`/`sin` make the sample exactly periodic in it.
            let (adv_s, adv_c) = advected.sin_cos();
            let warp = value_noise(adv_c * 2.3, adv_s * 2.3 + latitude * 5.1, seed) - 0.5;
            let warp2 = value_noise(
                adv_c * 5.7,
                adv_s * 5.7 + latitude * 11.3,
                seed.wrapping_add(31),
            ) - 0.5;
            let warped = latitude + BAND_WARP.0 * warp + BAND_WARP.1 * warp2;

            let mut band = 1.0
                + BAND_PRIMARY * (warped * count * PI + seed_phase).sin()
                + BAND_SECONDARY * (warped * count * 2.7 * PI + seed_phase * 1.7).sin();
            if (warped.abs() - BAND_BELT_LATITUDE).abs() < BAND_BELT_HALF_WIDTH {
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

    // The rings cast on the planet. Project this surface point back along the light vector to the
    // ring's own plane and ask whether it lands between the inner and outer radii — that is the
    // geometry, not a drawn band: the shadow narrows, widens and crosses the disc as the body goes
    // round, because the light vector does.
    //
    // **The ring plane is not `y = 0`.** The ring is drawn as a horizontal ellipse squashed by
    // [`RING_SQUASH`], so its plane is tilted about the horizontal axis and its normal is
    // `(0, sqrt(1 - k^2), k)` rather than `(0, 1, 0)`. Solving against the wrong plane puts every
    // intersection inside the ring's inner radius and the shadow never fires at all.
    let ring_shadow = match ring {
        Some((inner, outer)) => {
            let k = RING_SQUASH;
            let normal = (0.0, (1.0 - k * k).max(0.0).sqrt(), k);
            let l_dot_n = light_dir.0 * normal.0 + light_dir.1 * normal.1 + light_dir.2 * normal.2;
            if l_dot_n.abs() < 0.05 {
                0.0
            } else {
                let p_dot_n = nx * normal.0 + ny * normal.1 + nz * normal.2;
                let t = -p_dot_n / l_dot_n;
                if t <= 0.0 {
                    0.0
                } else {
                    let hit = (
                        nx + t * light_dir.0,
                        ny + t * light_dir.1,
                        nz + t * light_dir.2,
                    );
                    let rho = (hit.0 * hit.0 + hit.1 * hit.1 + hit.2 * hit.2).sqrt();
                    // A soft edge, because a hard-edged shadow crossing a disc a few dozen pixels
                    // across stutters as the body turns.
                    let edge_in = clamp01(
                        (rho - (inner - RING_SHADOW_FEATHER)) / (RING_SHADOW_FEATHER * 2.0),
                    );
                    let edge_out = clamp01(
                        ((outer + RING_SHADOW_FEATHER) - rho) / (RING_SHADOW_FEATHER * 2.0),
                    );
                    edge_in.min(edge_out) * RING_SHADOW_DEPTH
                }
            }
        }
        None => 0.0,
    };

    let ambient = 0.10;
    let lit = (ambient + diffuse * (1.0 - ambient)) * (1.0 - ring_shadow);
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

/// How much frame area one star gets, in square pixels. The fleet orrery's own density —
/// `Ntot = round(w*h / 1150)`, which is 1,803 candidates at 1920x1080 and about a thousand drawn.
///
/// **A count, not a density, was the defect.** herdr drew a flat 260 stars at every size, so the sky
/// thinned as the frame grew — 3.8x fewer than the artifact at 1920x1080 and 6.8x fewer at 2560x1440
/// — while each of the few it drew was measurably *brighter* (median point-source peak L 64–143
/// against the reference's 24–31). That is M2's banned "few bright dots" with the sign flipped, and
/// it is the sky H6 exists to forbid.
const STAR_AREA_PER_STAR: f32 = 1150.0;

/// Bounds on the star pool, so a one-cell scene does not draw a single star and a wall-sized one
/// does not spend the whole frame budget on the backdrop.
const STAR_COUNT_BOUNDS: (usize, usize) = (48, 6_000);

/// How many stars a frame of this size carries — the density above, resolved.
///
/// Positioned and dimmed from a fixed seed rather than from `width`/`height`, so the stars a frame
/// already had stay exactly where they were when it grows: a resize adds stars to the end of the
/// field rather than re-shuffling it.
fn star_count(width: u32, height: u32) -> usize {
    ((width as f32 * height as f32 / STAR_AREA_PER_STAR).round() as usize)
        .clamp(STAR_COUNT_BOUNDS.0, STAR_COUNT_BOUNDS.1)
}

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

/// The galactic band: its slope across the frame, where its centre line crosses the middle column,
/// and how wide it is — all as fractions of the frame's own height. The artifact's `bandA`, `bandC`
/// and its `h*0.15` sigma.
const STAR_BAND_SLOPE: f32 = -0.42;
const STAR_BAND_CENTRE: f32 = 0.60;
const STAR_BAND_SIGMA: f32 = 0.15;

/// How much of the candidate pool survives off the band, and how much more the band keeps.
///
/// **The cull is what makes the sky a sky.** A uniform scatter at the artifact's density is a
/// texture; a band with a genuine falloff either side of it is a place. It is also where the
/// artifact's own "1,803 candidates, roughly a thousand drawn" comes from — the pool is
/// deliberately larger than the field.
const STAR_KEEP: (f32, f32) = (0.30, 0.62);

/// How far the band lifts a star's drawn alpha: everything outside it draws at `0.55` of its
/// magnitude's alpha.
///
/// This is the term herdr was missing, and it is the whole of the "individually brighter" finding:
/// with the floor at a bare `STAR_ALPHA.0`, the *faintest possible* star measured at peak luminance
/// 42 against the reference's whole-field median of 24–31. Every star herdr drew was brighter than
/// the reference's typical one.
const STAR_BAND_GAIN: (f32, f32) = (0.55, 0.45);

/// How many candidates are drawn from per star the field is sized for — the artifact's `N*3`.
const STAR_POOL_OVERSAMPLE: usize = 3;

/// The magnitude above which a star is one of "the brightest few" that scintillate. A sky where
/// every star twinkles is a screensaver, so this names a handful and leaves the rest steady.
///
/// **Re-derived with the count, not left where it was.** `0.80` was solved for a field of 260 —
/// its own note said so: *"the orrery cuts at `0.93` over roughly a thousand stars; `STAR_COUNT` is
/// a quarter of that, so the same count needs a lower cut."* The pool is now the artifact's own
/// density, so the artifact's own cut is the one that names the same handful; leaving `0.80` under a
/// field seven times larger would have set most of the sky twinkling, which is the screensaver this
/// constant exists to prevent.
const STAR_SCINTILLATION_MAGNITUDE: f32 = 0.93;

/// How many depths the starfield is split across.
///
/// Three, the orrery's own count. Stars are assigned to a layer by index, so each layer inherits
/// the same magnitude distribution and colour-temperature spread the whole field has — the depths
/// are a motion fact, not a second population.
const STAR_LAYERS: usize = 3;

/// How far each layer sways across the frame, as a fraction of the width.
///
/// A **sway** rather than a pan, and that is forced rather than chosen: the ambient loop is baked
/// once and played forever, so a layer panning steadily would be somewhere else at the end of the
/// loop than at the start and the seam would show on every repeat. A sinusoid in the loop phase
/// closes exactly, and it is still the observer's own slow motion — integrated and never constant,
/// which is what the drift is supposed to be. The rates differ by depth, so the near layer swims
/// against the far one.
const STAR_LAYER_DRIFT: [f32; STAR_LAYERS] = [0.030, 0.018, 0.008];

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
    // A star's corona reaches far further than a planet's atmospheric fringe, and it is not a
    // round glow: it has structure. See [`corona_at`].
    let glow = radius * if self_luminous { CORONA_REACH } else { 1.4 };
    // Solved once per body rather than per pixel: five lobes off one hash, and the same five for
    // every pixel of the same star.
    let lobes = self_luminous.then(corona_lobes);

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
                if let Some(lobes) = lobes.as_ref() {
                    // The corona: five streamers at real angular widths rather than an even halo,
                    // and prominences arching off the limb. A star with a round glow around it is
                    // the one body in this scene drawn as a light source rather than as a thing.
                    let angle = dy.atan2(dx);
                    let out = (dist - radius) / (glow - radius).max(0.001);
                    let corona = CORONA_PEAK
                        * corona_at(lobes, angle, dist / radius.max(0.001), surface.seed);
                    // Prominences live at the limb and reach only a little way out, so they read
                    // as arcs off the edge rather than as a second, lumpier corona.
                    let limbward = clamp01(1.0 - out / PROMINENCE_REACH);
                    let prominence = PROMINENCE_GAIN
                        * limbward
                        * limbward
                        * clamp01(
                            (value_noise(angle * PROMINENCE_COUNT, 3.0, surface.seed ^ 0x9E37)
                                - PROMINENCE_GATE.0)
                                / (PROMINENCE_GATE.1 - PROMINENCE_GATE.0),
                        );
                    blend(&mut buf[idx], base, (corona + prominence).min(1.0));
                } else {
                    blend(&mut buf[idx], base, t * t * 0.22);
                }
            }
        }
    }
}

/// A 5x7 monospace bitmap face, ASCII `0x20..=0x7E`, five columns per glyph with bit 0 the top row.
///
/// **This module used to have no font by construction**, and said so: *"painting a private bitmap
/// font into a wash that sits under real glyphs is exactly the thing this scene does not do."* The
/// captain's decision of 2026-08-13 (`data/decisions/2026-08-13-orrery-color-labels-material-tabbar.md`,
/// firstmate home) overrides it directly — *"labels should follow the planets... I expect my
/// background to literally be that HTML"* — and the reference's captions are how its mass register is
/// read at all. The old principle's real concern, which is that a private font could fight the real
/// glyphs over it, is answered the way the reference answers it: see [`label_clear`].
const FONT_5X7: [[u8; 5]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00], // space
    [0x00, 0x00, 0x5F, 0x00, 0x00], // !
    [0x00, 0x07, 0x00, 0x07, 0x00], // "
    [0x14, 0x7F, 0x14, 0x7F, 0x14], // #
    [0x24, 0x2A, 0x7F, 0x2A, 0x12], // $
    [0x23, 0x13, 0x08, 0x64, 0x62], // %
    [0x36, 0x49, 0x55, 0x22, 0x50], // &
    [0x00, 0x05, 0x03, 0x00, 0x00], // '
    [0x00, 0x1C, 0x22, 0x41, 0x00], // (
    [0x00, 0x41, 0x22, 0x1C, 0x00], // )
    [0x14, 0x08, 0x3E, 0x08, 0x14], // *
    [0x08, 0x08, 0x3E, 0x08, 0x08], // +
    [0x00, 0x50, 0x30, 0x00, 0x00], // ,
    [0x08, 0x08, 0x08, 0x08, 0x08], // -
    [0x00, 0x60, 0x60, 0x00, 0x00], // .
    [0x20, 0x10, 0x08, 0x04, 0x02], // /
    [0x3E, 0x51, 0x49, 0x45, 0x3E], // 0
    [0x00, 0x42, 0x7F, 0x40, 0x00], // 1
    [0x42, 0x61, 0x51, 0x49, 0x46], // 2
    [0x21, 0x41, 0x45, 0x4B, 0x31], // 3
    [0x18, 0x14, 0x12, 0x7F, 0x10], // 4
    [0x27, 0x45, 0x45, 0x45, 0x39], // 5
    [0x3C, 0x4A, 0x49, 0x49, 0x30], // 6
    [0x01, 0x71, 0x09, 0x05, 0x03], // 7
    [0x36, 0x49, 0x49, 0x49, 0x36], // 8
    [0x06, 0x49, 0x49, 0x29, 0x1E], // 9
    [0x00, 0x36, 0x36, 0x00, 0x00], // :
    [0x00, 0x56, 0x36, 0x00, 0x00], // ;
    [0x08, 0x14, 0x22, 0x41, 0x00], // <
    [0x14, 0x14, 0x14, 0x14, 0x14], // =
    [0x00, 0x41, 0x22, 0x14, 0x08], // >
    [0x02, 0x01, 0x51, 0x09, 0x06], // ?
    [0x32, 0x49, 0x79, 0x41, 0x3E], // @
    [0x7E, 0x11, 0x11, 0x11, 0x7E], // A
    [0x7F, 0x49, 0x49, 0x49, 0x36], // B
    [0x3E, 0x41, 0x41, 0x41, 0x22], // C
    [0x7F, 0x41, 0x41, 0x22, 0x1C], // D
    [0x7F, 0x49, 0x49, 0x49, 0x41], // E
    [0x7F, 0x09, 0x09, 0x09, 0x01], // F
    [0x3E, 0x41, 0x49, 0x49, 0x7A], // G
    [0x7F, 0x08, 0x08, 0x08, 0x7F], // H
    [0x00, 0x41, 0x7F, 0x41, 0x00], // I
    [0x20, 0x40, 0x41, 0x3F, 0x01], // J
    [0x7F, 0x08, 0x14, 0x22, 0x41], // K
    [0x7F, 0x40, 0x40, 0x40, 0x40], // L
    [0x7F, 0x02, 0x0C, 0x02, 0x7F], // M
    [0x7F, 0x04, 0x08, 0x10, 0x7F], // N
    [0x3E, 0x41, 0x41, 0x41, 0x3E], // O
    [0x7F, 0x09, 0x09, 0x09, 0x06], // P
    [0x3E, 0x41, 0x51, 0x21, 0x5E], // Q
    [0x7F, 0x09, 0x19, 0x29, 0x46], // R
    [0x46, 0x49, 0x49, 0x49, 0x31], // S
    [0x01, 0x01, 0x7F, 0x01, 0x01], // T
    [0x3F, 0x40, 0x40, 0x40, 0x3F], // U
    [0x1F, 0x20, 0x40, 0x20, 0x1F], // V
    [0x3F, 0x40, 0x38, 0x40, 0x3F], // W
    [0x63, 0x14, 0x08, 0x14, 0x63], // X
    [0x07, 0x08, 0x70, 0x08, 0x07], // Y
    [0x61, 0x51, 0x49, 0x45, 0x43], // Z
    [0x00, 0x7F, 0x41, 0x41, 0x00], // [
    [0x02, 0x04, 0x08, 0x10, 0x20], // \
    [0x00, 0x41, 0x41, 0x7F, 0x00], // ]
    [0x04, 0x02, 0x01, 0x02, 0x04], // ^
    [0x40, 0x40, 0x40, 0x40, 0x40], // _
    [0x00, 0x01, 0x02, 0x04, 0x00], // `
    [0x20, 0x54, 0x54, 0x54, 0x78], // a
    [0x7F, 0x48, 0x44, 0x44, 0x38], // b
    [0x38, 0x44, 0x44, 0x44, 0x20], // c
    [0x38, 0x44, 0x44, 0x48, 0x7F], // d
    [0x38, 0x54, 0x54, 0x54, 0x18], // e
    [0x08, 0x7E, 0x09, 0x01, 0x02], // f
    [0x0C, 0x52, 0x52, 0x52, 0x3E], // g
    [0x7F, 0x08, 0x04, 0x04, 0x78], // h
    [0x00, 0x44, 0x7D, 0x40, 0x00], // i
    [0x20, 0x40, 0x44, 0x3D, 0x00], // j
    [0x7F, 0x10, 0x28, 0x44, 0x00], // k
    [0x00, 0x41, 0x7F, 0x40, 0x00], // l
    [0x7C, 0x04, 0x18, 0x04, 0x78], // m
    [0x7C, 0x08, 0x04, 0x04, 0x78], // n
    [0x38, 0x44, 0x44, 0x44, 0x38], // o
    [0x7C, 0x14, 0x14, 0x14, 0x08], // p
    [0x08, 0x14, 0x14, 0x18, 0x7C], // q
    [0x7C, 0x08, 0x04, 0x04, 0x08], // r
    [0x48, 0x54, 0x54, 0x54, 0x20], // s
    [0x04, 0x3F, 0x44, 0x40, 0x20], // t
    [0x3C, 0x40, 0x40, 0x20, 0x7C], // u
    [0x1C, 0x20, 0x40, 0x20, 0x1C], // v
    [0x3C, 0x40, 0x30, 0x40, 0x3C], // w
    [0x44, 0x28, 0x10, 0x28, 0x44], // x
    [0x0C, 0x50, 0x50, 0x50, 0x3C], // y
    [0x44, 0x64, 0x54, 0x4C, 0x44], // z
    [0x00, 0x08, 0x36, 0x41, 0x00], // {
    [0x00, 0x00, 0x7F, 0x00, 0x00], // |
    [0x00, 0x41, 0x36, 0x08, 0x00], // }
    [0x08, 0x04, 0x08, 0x10, 0x08], // ~
];

/// The glyph for one character, or `None` for anything the face does not carry.
///
/// A character with no glyph draws **nothing**, never a substitute box: a caption is an identifier,
/// and a wrong glyph in one is a wrong identifier.
fn glyph(c: char) -> Option<&'static [u8; 5]> {
    match c {
        // The one non-ASCII character the reference's own captions use as a separator.
        '\u{b7}' => Some(&[0x00, 0x00, 0x08, 0x00, 0x00]),
        c if (0x20..0x7F).contains(&(c as u32)) => FONT_5X7.get(c as usize - 0x20),
        _ => None,
    }
}

/// One glyph cell's width and height in face units, and the gap between glyphs.
const GLYPH_SIZE: (usize, usize) = (5, 7);
const GLYPH_ADVANCE: usize = 6;

/// The caption's two colours — a cold cyan for the name, dimmer for the readout under it — and
/// their alphas. The reference's `#7fb0c4` at `0.88` and `#4d7d90` at `0.55`.
///
/// Cold on purpose: every body in this scene is warm, so the one thing that is *not* a celestial
/// object reads as an instrument rather than as more sky.
const LABEL_NAME_RGB01: (f32, f32, f32) = (127.0 / 255.0, 176.0 / 255.0, 196.0 / 255.0);
const LABEL_READOUT_RGB01: (f32, f32, f32) = (77.0 / 255.0, 125.0 / 255.0, 144.0 / 255.0);
const LABEL_NAME_ALPHA: f32 = 0.88;
const LABEL_READOUT_ALPHA: f32 = 0.55;

/// How far a caption sits from its body, and how far the readout sits under the name — the
/// reference's `+R+7 / -R-3` offset and its `+12` line step, in face units so they scale together.
const LABEL_GAP_PX: (f32, f32) = (7.0, 3.0);
const LABEL_LINE_STEP: f32 = 12.0;

/// How far a caption fades over as it enters something that covers it — the reference's `LBL_FADE`.
const LABEL_FADE_PX: f32 = 30.0;

/// The keep-out around the sun's own caption, which never moves while everything else drifts across
/// it. The reference's `sunRadius() + SUN_LBL_GAP + 18`.
const LABEL_SUN_KEEP_OUT_PX: f32 = 13.0 + 18.0;

/// How far under its own limb the sun's caption sits. The reference's `SUN_LBL_GAP`.
const LABEL_SUN_GAP_PX: f32 = 13.0;

/// How readable a caption at `(tx, ty)` is, `0.0..=1.0`.
///
/// **A label is drawn only where it can be read.** The reference fades a caption out as it enters
/// anything that covers it *"rather than a hard cut that would pop"*, and this is the same rule
/// against the two regions a pure generator can actually know about: the panel strip the worker tree
/// occupies on the left (the same fraction [`scene_origin`] composes against) and the sun's own
/// caption box.
fn label_clear(tx: f32, ty: f32, width: u32, sun: (f32, f32), sun_radius: f32) -> f32 {
    let panel = panel_width(width);
    let from_panel = tx - (panel + 4.0);
    let from_sun =
        ((tx - sun.0).powi(2) + (ty - sun.1).powi(2)).sqrt() - (sun_radius + LABEL_SUN_KEEP_OUT_PX);
    clamp01(from_panel.min(from_sun) / LABEL_FADE_PX)
}

/// Draw one line of the face into `buf`, top-left at `(x, y)` in pixels.
fn draw_text(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    text: &str,
    rgb: (f32, f32, f32),
    alpha: f32,
    scale: usize,
) {
    if alpha <= 0.004 || scale == 0 {
        return;
    }
    let mut pen = x;
    for c in text.chars() {
        let Some(glyph) = glyph(c) else {
            pen += (GLYPH_ADVANCE * scale) as f32;
            continue;
        };
        for (col, bits) in glyph.iter().enumerate() {
            for row in 0..GLYPH_SIZE.1 {
                if bits & (1 << row) == 0 {
                    continue;
                }
                let px0 = pen + (col * scale) as f32;
                let py0 = y + (row * scale) as f32;
                for sy in 0..scale {
                    let py = py0 as i32 + sy as i32;
                    if py < 0 || py >= height as i32 {
                        continue;
                    }
                    for sx in 0..scale {
                        let px = px0 as i32 + sx as i32;
                        if px < 0 || px >= width as i32 {
                            continue;
                        }
                        let idx = py as usize * width as usize + px as usize;
                        blend(&mut buf[idx], rgb, alpha);
                    }
                }
            }
        }
        pen += (GLYPH_ADVANCE * scale) as f32;
    }
    let _ = GLYPH_SIZE.0;
}

/// The readout line under a body's name: what the register says about it, in the register's own
/// units.
///
/// The reference prints `fileCount(m) + '  streak ' + m.streak`, and A38(e) is why the unit is in
/// there at all — *"a readout that hides its register is that defect one layer down"*. herdr's own
/// streak register is a decayed score in named bands rather than a count, so it is expressed in the
/// artifact's own [`STREAK_UNITS`], which is how every other ported streak rate in this module is
/// scaled.
fn label_readout(size: BodySize, streak: f32) -> Option<String> {
    let files = match size {
        // The sun is out of the register by decision: it routes to projects rather than being one,
        // so it carries a name and nothing else.
        BodySize::Fixed => return None,
        BodySize::Unmeasured => "size unmeasured".to_string(),
        BodySize::Files(files) => {
            let mut grouped = String::new();
            let digits = files.to_string();
            for (i, d) in digits.chars().enumerate() {
                if i > 0 && (digits.len() - i) % 3 == 0 {
                    grouped.push(',');
                }
                grouped.push(d);
            }
            format!("{grouped} {}", if files == 1 { "file" } else { "files" })
        }
    };
    Some(format!(
        "{files}  streak {}",
        (streak * STREAK_UNITS).round() as u32
    ))
}

/// Draw one body's caption in the sky: its name, and the register reading under it.
///
/// Bare text — no border, no box, no plate behind it. The captain's own words on this
/// (`data/decisions/2026-08-13-orrery-color-labels-material-tabbar.md`, firstmate home): *"labels
/// should follow the planets, but need to be smooth. No obvious border that clashes with the
/// background. And, clean."* It follows the body because it is positioned off the body's own drawn
/// centre and radius every frame, so it moves exactly as the body moves and there is nothing to
/// recompute or snap.
#[allow(clippy::too_many_arguments)]
fn draw_sky_label(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    body: &BodyLayout,
    pos: (f32, f32),
    body_radius_px: f32,
    sun: (f32, f32),
    sun_radius: f32,
    scale: usize,
) {
    if body.label.is_empty() {
        return;
    }
    // The sun's caption is the one that never moves while everything else drifts across it, so it
    // sits centred under the disc — its own reserved box — rather than beside it, and it is exempt
    // from the keep-out that box *is*.
    let is_sun = body.kind == BodyKind::Sun;
    let (tx, ty) = if is_sun {
        let text_px = (body.label.as_str().chars().count() * GLYPH_ADVANCE * scale) as f32;
        (
            pos.0 - text_px / 2.0,
            pos.1 + body_radius_px + LABEL_SUN_GAP_PX * scale as f32,
        )
    } else {
        (
            pos.0 + body_radius_px + LABEL_GAP_PX.0 * scale as f32,
            pos.1 - body_radius_px - LABEL_GAP_PX.1 * scale as f32,
        )
    };
    let clear = if is_sun {
        clamp01((tx - (panel_width(width) + 4.0)) / LABEL_FADE_PX)
    } else {
        label_clear(tx, ty, width, sun, sun_radius)
    };
    if clear <= 0.01 {
        return;
    }
    draw_text(
        buf,
        width,
        height,
        tx,
        ty,
        body.label.as_str(),
        LABEL_NAME_RGB01,
        LABEL_NAME_ALPHA * clear,
        scale,
    );
    if let Some(readout) = label_readout(body.size, body.streak) {
        draw_text(
            buf,
            width,
            height,
            tx,
            ty + LABEL_LINE_STEP * scale as f32,
            &readout,
            LABEL_READOUT_RGB01,
            LABEL_READOUT_ALPHA * clear,
            scale,
        );
    }
}

/// How large the caption face is drawn, in whole face units per pixel.
///
/// Whole rather than fractional because a bitmap face scaled by a fraction is a blurred bitmap face:
/// the whole reason to carry one is that its stems land on pixel boundaries. One unit reproduces the
/// reference's own `10.5px` caption at its own composed size.
fn label_scale(width: u32, height: u32) -> usize {
    ((width.min(height) as f32 / 1080.0).round() as usize).max(1)
}

/// The panel strip the worker tree occupies on the left, in pixels — see
/// [`PANEL_WIDTH_FRACTION`].
fn panel_width(width: u32) -> f32 {
    (width as f32 * PANEL_WIDTH_FRACTION).min(PANEL_WIDTH_CEIL_PX)
}

/// Draw one body's orbit track: the ring it has worn into the scene, about its own parent.
///
/// Centred on the parent's *current* position rather than a fixed point, because a worker's orbit
/// is around a mate that is itself moving — the track travels with the body it belongs to, which
/// is what makes it that body's own path rather than a decoration at a fixed radius.
fn draw_orbit_track(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    path: &OrbitTrack,
    wear: f32,
    seed: u32,
) {
    if wear <= 0.0 || path.semi_major_px <= 0.0 {
        return;
    }
    let half = mix(TRACK_WIDTH_PX.0, TRACK_WIDTH_PX.1, wear) * 0.5;
    let alpha = mix(TRACK_ALPHA.0, TRACK_ALPHA.1, wear);

    // Walked along the path rather than tested over the annulus it used to enclose. An ellipse
    // tilted out of the plane has no closed-form distance field worth the arithmetic, and stepping
    // the parameter costs a stamp per pixel of arc — the same order the annulus scan cost, and
    // bounded by circumference rather than by the square of the radius.
    let apoapsis = path.semi_major_px * (1.0 + path.eccentricity);
    let steps = ((apoapsis * 2.0 * PI / TRACK_STEP_PX) as usize).clamp(256, 6_144);
    let reach = half + 1.0;

    for step in 0..steps {
        let th = (step as f32 / steps as f32) * 2.0 * PI;
        let (x, y) = path.point(th);
        // Wear varies along the groove, so it reads as a worn path rather than a drawn ellipse.
        let variation = mix(
            0.62,
            1.0,
            value_noise(th * 3.1, 0.0, seed) * 0.6 + value_noise(th * 11.7, 4.0, seed) * 0.4,
        );
        let x0 = (x - reach).floor().max(0.0) as i32;
        let x1 = (x + reach).ceil().min(width as f32) as i32;
        let y0 = (y - reach).floor().max(0.0) as i32;
        let y1 = (y + reach).ceil().min(height as f32) as i32;
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - x;
                let dy = py as f32 + 0.5 - y;
                let offset = (dx * dx + dy * dy).sqrt();
                if offset > half {
                    continue;
                }
                let idx_px = py as usize * width as usize + px as usize;
                // Maximum rather than accumulation along the walk: consecutive stamps overlap by
                // construction, and summing them would make a groove's alpha a function of how
                // finely it was sampled.
                let a = alpha * variation * clamp01(1.0 - offset / half.max(0.5));
                let cell = &mut buf[idx_px];
                let before = cell[3];
                blend(cell, TRACK_RGB01, a);
                if cell[3] < before {
                    cell[3] = before;
                }
            }
        }
    }
}

/// How far apart consecutive stamps along a groove are, in pixels of arc. Under a pixel, so a track
/// is continuous at every radius the ladder reaches without being sampled finely enough to cost
/// more than the pixels it covers.
const TRACK_STEP_PX: f32 = 0.7;

/// One orbit as a path in screen space — everything [`SceneLayout::place`] uses to put a body
/// somewhere, without the body.
///
/// Held as its own value because two different things need the same curve: the body walking it, and
/// the groove worn into it. A groove drawn from a second copy of the projection is a groove that can
/// drift off the path it is supposed to be a record of.
#[derive(Debug, Clone, Copy)]
struct OrbitTrack {
    centre: (f32, f32),
    semi_major_px: f32,
    eccentricity: f32,
    periapsis: f32,
    inclination: f32,
}

impl OrbitTrack {
    /// Where on the path the parameter `th` lands, in screen pixels. The same three lines
    /// [`SceneLayout::place`] runs.
    fn point(&self, th: f32) -> (f32, f32) {
        let (sin_t, cos_t) = th.sin_cos();
        let e = self.eccentricity;
        let r = self.semi_major_px * (1.0 - e * e) / (1.0 + e * (th - self.periapsis).cos());
        (
            self.centre.0 + cos_t * r,
            self.centre.1 + sin_t * r * ORBIT_PLANE_SQUASH + cos_t * r * self.inclination,
        )
    }
}

/// How dark the centre of a transiting worker's shadow is, as a multiple of the light it removes.
///
/// Not opaque: a worker is small against its mate, so the umbra it casts is partial by geometry
/// rather than by choice, and a black disc would read as a hole rather than a shadow.
const TRANSIT_UMBRA: f32 = 0.62;

/// How much wider the penumbra is than the umbra. A real shadow has a soft edge, and a hard-edged
/// one on a body a few dozen pixels across stutters as it crosses.
const TRANSIT_PENUMBRA: f32 = 1.75;

/// Draw the shadow of every worker currently passing between its mate and the sun.
///
/// **The umbral geometry, from positions the frame already computes in order to draw.** A worker
/// is in transit when it lies on the sunward side of its mate and its offset perpendicular to the
/// mate–sun line is inside the mate's own disc; where it lands is that same perpendicular offset,
/// projected onto the face. It is free, it is true, and it is the one mechanism here that makes a
/// worker legible by making the *planet* change — which is why it works at sizes where the worker
/// itself is a handful of pixels.
#[allow(clippy::too_many_arguments)]
fn draw_shadow_transits(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    layout: &SceneLayout,
    parent_idx: usize,
    parent: &BodyLayout,
    parent_pos: (f32, f32),
    sun_pos: (f32, f32),
    phase: f32,
) {
    if parent.kind != BodyKind::Planet || parent.body_radius_px <= 0.0 {
        return;
    }
    // The direction the sun is in, from this mate.
    let to_sun = (sun_pos.0 - parent_pos.0, sun_pos.1 - parent_pos.1);
    let len = (to_sun.0 * to_sun.0 + to_sun.1 * to_sun.1).sqrt();
    if len <= 0.001 {
        return;
    }
    let (sx, sy) = (to_sun.0 / len, to_sun.1 / len);

    for (idx, moon) in layout.bodies.iter().enumerate() {
        if moon.parent != Some(parent_idx) || !moon.seated || moon.kind != BodyKind::Moon {
            continue;
        }
        let moon_pos = layout.position(idx, phase);
        let (dx, dy) = (moon_pos.0 - parent_pos.0, moon_pos.1 - parent_pos.1);
        // Sunward of its mate, or it is behind the planet and casting nothing onto it.
        let along = dx * sx + dy * sy;
        if along <= 0.0 {
            continue;
        }
        // Perpendicular offset from the mate–sun line: where on the face the shadow lands, and
        // whether it lands on the face at all.
        let perp = dx * -sy + dy * sx;
        let umbra = moon.body_radius_px;
        let reach = umbra * TRANSIT_PENUMBRA;
        if perp.abs() > parent.body_radius_px + reach {
            continue;
        }
        // Projected back onto the disc along the same perpendicular, which is what an umbra
        // landing on a sphere from a distant light actually does.
        let cx = parent_pos.0 + -sy * perp;
        let cy = parent_pos.1 + sx * perp;

        let x0 = (cx - reach).floor().max(0.0) as i32;
        let x1 = (cx + reach).ceil().min(width as f32) as i32;
        let y0 = (cy - reach).floor().max(0.0) as i32;
        let y1 = (cy + reach).ceil().min(height as f32) as i32;
        for py in y0..y1 {
            for px in x0..x1 {
                let ox = px as f32 + 0.5 - cx;
                let oy = py as f32 + 0.5 - cy;
                let dist = (ox * ox + oy * oy).sqrt();
                if dist > reach {
                    continue;
                }
                // Only on the mate's own face — a shadow does not hang in the space beside it.
                let fx = px as f32 + 0.5 - parent_pos.0;
                let fy = py as f32 + 0.5 - parent_pos.1;
                if (fx * fx + fy * fy).sqrt() > parent.body_radius_px {
                    continue;
                }
                // Full umbra in the middle, softening through the penumbra to nothing.
                let strength = if dist <= umbra {
                    1.0
                } else {
                    clamp01(1.0 - (dist - umbra) / (reach - umbra).max(0.001))
                };
                let idx_px = py as usize * width as usize + px as usize;
                let under = buf[idx_px];
                let shade = 1.0 - TRANSIT_UMBRA * strength;
                buf[idx_px] = [
                    under[0] * shade,
                    under[1] * shade,
                    under[2] * shade,
                    under[3],
                ];
            }
        }
    }
}

/// The most motes one body ever draws.
///
/// A cap on the *drawn* count, not on the counted one: the accounting is exact and unbounded, and
/// this is only how many marks fit round an orbit before they stop being marks and become a
/// second groove. A body past the cap draws the cap.
const MOTE_DRAW_CAP: u32 = 64;

/// A mote's drawn radius, and the alpha range its attribution share maps onto.
///
/// The top is low by design. The ambient tier is the more frequent thing, and the failure mode a
/// tier beneath an existing one has is that it quietly becomes the loudest thing in the frame and
/// the ceremony stops reading as ceremony.
const MOTE_PX: f32 = 1.15;
const MOTE_ALPHA: (f32, f32) = (0.10, 0.34);

/// A mote is light, not material: the same warm neutral a comet's core carries, because both are
/// work arriving rather than a thing in the sky.
const MOTE_RGB01: (f32, f32, f32) = (1.0, 0.97, 0.86);

/// How far a mote's own amplitude swings over one loop, and how many whole cycles it swings in.
///
/// **This is the tier's identity, and it was missing.** A10's load-bearing claim is that the two
/// event registers are told apart by *kind of motion*, not by size — *"two effects that differ only
/// in size read as one effect at two strengths, which is the failure this amendment exists to
/// avoid"* — and the artifact's own self-report names each kind: the ceremonial tier is *"a discrete
/// object that TRAVELS, arrives and is gone"*; the ambient tier is *"continuous — a standing wave;
/// amplitude varies, nothing travels"*.
///
/// herdr drew static marks at fixed alpha, which is neither: it read as more groove. A mote now
/// *breathes* — its amplitude varies continuously, and it stays exactly where the event that emitted
/// it happened. Nothing travels, so it can never be mistaken for the ceremonial tier, and nothing is
/// static, so it can never be mistaken for the wear layer underneath it.
///
/// A whole number of cycles per loop, like everything else that moves in this scene, so the bake's
/// seam still closes.
const MOTE_BREATH: (f32, f32) = (0.42, 1.0);
const MOTE_BREATH_CYCLES: f32 = 1.0;

/// Draw one body's ambient motes: one small mark on its own orbit track per unit of work its agent
/// actually did.
///
/// Placed at angles derived from the mote's own index rather than from a clock, so a mote sits
/// permanently where its event happened and the same fleet always draws the same picture. What
/// varies is its amplitude — see [`MOTE_BREATH`].
fn draw_motes(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    layout: &SceneLayout,
    idx: usize,
    body: &BodyLayout,
    phase: f32,
) {
    if body.motes == 0 || body.orbit_radius_px <= 0.0 {
        return;
    }
    let Some(parent) = body.parent.filter(|p| layout.is_seated(*p)) else {
        return;
    };
    // The same path the groove is worn into, so a mote is always somewhere the body has been rather
    // than somewhere a circle through its aphelion would have taken it.
    let track = OrbitTrack {
        centre: layout.position(parent, phase),
        semi_major_px: body.orbit_radius_px,
        eccentricity: body.eccentricity,
        periapsis: body.periapsis,
        inclination: body.inclination,
    };
    let alpha = mix(MOTE_ALPHA.0, MOTE_ALPHA.1, body.mote_share);
    let seed = body_seed(idx).wrapping_add(7_717);
    let drawn = body.motes.min(MOTE_DRAW_CAP);

    for i in 0..drawn {
        // Spread round the orbit by index, with a per-body jitter so two bodies with the same
        // count do not stud their orbits identically.
        let angle = (i as f32 / MOTE_DRAW_CAP as f32) * 2.0 * PI
            + value_noise(i as f32 * 1.9, 0.0, seed) * 0.4
            + body.base_angle;
        let (px, py) = track.point(angle);
        // Each mote breathes on its own offset, so the field varies continuously without pulsing in
        // unison — a standing wave rather than a blinking row.
        let offset = value_noise(i as f32 * 3.7, 12.0, seed) * 2.0 * PI;
        let breath = mix(
            MOTE_BREATH.0,
            MOTE_BREATH.1,
            0.5 + 0.5 * (phase * MOTE_BREATH_CYCLES + offset).sin(),
        );
        let alpha = alpha * breath;

        let x0 = (px - MOTE_PX).floor().max(0.0) as i32;
        let x1 = (px + MOTE_PX).ceil().min(width as f32) as i32;
        let y0 = (py - MOTE_PX).floor().max(0.0) as i32;
        let y1 = (py + MOTE_PX).ceil().min(height as f32) as i32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let dx = xx as f32 + 0.5 - px;
                let dy = yy as f32 + 0.5 - py;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > MOTE_PX {
                    continue;
                }
                let idx_px = yy as usize * width as usize + xx as usize;
                blend(
                    &mut buf[idx_px],
                    MOTE_RGB01,
                    alpha * clamp01(1.0 - dist / MOTE_PX),
                );
            }
        }
    }
}

/// How far back a body's trail reaches, as a fraction of one full animation loop.
///
/// Short: a trail is *where the body was a moment ago* and reads speed, which is a different
/// reading from the groove's permanent wear underneath it. Two readings, two lifetimes, one path —
/// so a trail that reached far enough to overlap its own groove's meaning would collapse them.
const TRAIL_LOOKBACK: f32 = 0.055;

/// Bounds on how many samples one trail is drawn from.
///
/// The count itself is derived from the arc the body actually sweeps, not fixed: a worker doing
/// four revolutions a loop covers several times the ground a heavy mate doing one does, so a fixed
/// count draws the mate a smooth line and the worker a dotted one — which is precisely backwards,
/// since the fast body is the one whose motion is worth stating. Solved against the drawn dot
/// size, so consecutive samples always overlap.
const TRAIL_SAMPLE_BOUNDS: (usize, usize) = (10, 240);

/// How far apart consecutive trail samples may be, as a fraction of the drawn dot's own radius, and
/// the floor under that spacing.
///
/// **Derived from the dot, not fixed at `1.4px`.** The old constant was solved against a dot of
/// `0.595` of the body's radius — a dozen pixels across on a real mate — where 1.4px steps overlap
/// several times over. At the artifact's `0.10` hairline the same dot is barely a pixel wide and
/// 1.4px steps do not touch: measured, a mate's wake came out as a dotted line with gaps three times
/// the dot's own width, which is a *different effect*, not a fainter one. The spacing follows the
/// width so a trail stays continuous at either end of the register.
const TRAIL_SAMPLE_SPACING_OF_WIDTH: f32 = 0.8;
const TRAIL_SAMPLE_SPACING_FLOOR_PX: f32 = 0.6;

/// Alpha at the trail's root, where it leaves the body. It fades to nothing at the far end. The
/// artifact's own `TRAIL_A`.
const TRAIL_ALPHA: f32 = 0.22;

/// A trail's width at its root, as a multiple of the body's own radius.
///
/// **`0.10` is the third value of a constant the artifact tuned down twice** — its own comment
/// records `0.42 -> 0.16 -> 0.10` — and the last cut was made against the *worst* observed
/// ceremonial bar rather than the average one, because a scene sized against the average bar fails
/// half the time by construction. herdr's was `0.7 * 0.85 = 0.595` at alpha `0.34`: width times
/// alpha of `0.202` against the artifact's `0.022`, **about nine times the drawn light per unit of
/// trail length**. They read as comets rather than as wakes.
///
/// **The moon keeps what it had, and that is the artifact's own split rather than an omission.** Its
/// note is explicit: *"THE MOON TRAIL IS NOT CUT WITH IT - H13(iii) leans on it, and it is a fifth
/// of the width to begin with - so the whole reduction is taken by the planets, where the light
/// actually was."* A worker moon is a few pixels across and its trail is one of the two things that
/// make it findable without hunting; the light this pass is spending down was never on the moons.
const TRAIL_WIDTH_MATE: f32 = 0.10;
const TRAIL_WIDTH_MOON: f32 = 0.595;

/// How much of its root width a trail still has at the far end, and the floor under a drawn sample.
/// The artifact's `sz = max(0.7, w0 * (0.35 + 0.65 * u))`, in herdr's radius units.
const TRAIL_TAPER: (f32, f32) = (0.35, 0.65);
const TRAIL_MIN_PX: f32 = 0.35;

/// Draw one body's trail: a short fading wake of where it has just been.
///
/// The cheapest possible statement that the frame is alive, and — for a worker moon — one of the
/// two mechanisms that make it findable without hunting, because motion is the strongest pop-out
/// cue available and this one costs almost no static light.
fn draw_trail(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    layout: &SceneLayout,
    idx: usize,
    body: &BodyLayout,
    phase: f32,
) {
    if body.orbit_radius_px <= 0.0 || body.revolutions_per_loop <= 0.0 {
        return;
    }
    let root = body.body_radius_px
        * match body.kind {
            BodyKind::Moon => TRAIL_WIDTH_MOON,
            _ => TRAIL_WIDTH_MATE,
        };
    let base = severity_rgb01(body.hue, body.severity);
    let span = TRAIL_LOOKBACK * 2.0 * PI;

    // The arc this body sweeps over the lookback, in pixels — the whole point of deriving the
    // sample count rather than fixing it.
    let arc = body.orbit_radius_px * span * body.revolutions_per_loop;
    // Solved against the drawn dot's own narrow end, so consecutive samples still overlap where the
    // trail has thinned rather than only where it leaves the body.
    let spacing =
        (root * TRAIL_TAPER.0 * TRAIL_SAMPLE_SPACING_OF_WIDTH).max(TRAIL_SAMPLE_SPACING_FLOOR_PX);
    let samples = ((arc / spacing) as usize).clamp(TRAIL_SAMPLE_BOUNDS.0, TRAIL_SAMPLE_BOUNDS.1);

    for step in 1..=samples {
        let back = step as f32 / samples as f32;
        let pos = layout.position(idx, phase - span * back);
        // Thinning and fading as it recedes, so the wake has a direction without needing an arrow.
        let u = 1.0 - back;
        let width_here = (root * (TRAIL_TAPER.0 + TRAIL_TAPER.1 * u)).max(TRAIL_MIN_PX);
        let alpha = TRAIL_ALPHA * u * u;
        if alpha <= 0.004 {
            continue;
        }
        let x0 = (pos.0 - width_here).floor().max(0.0) as i32;
        let x1 = (pos.0 + width_here).ceil().min(width as f32) as i32;
        let y0 = (pos.1 - width_here).floor().max(0.0) as i32;
        let y1 = (pos.1 + width_here).ceil().min(height as f32) as i32;
        for py in y0..y1 {
            for px in x0..x1 {
                let dx = px as f32 + 0.5 - pos.0;
                let dy = py as f32 + 0.5 - pos.1;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > width_here {
                    continue;
                }
                let idx_px = py as usize * width as usize + px as usize;
                blend(
                    &mut buf[idx_px],
                    base,
                    alpha * clamp01(1.0 - dist / width_here.max(0.5)),
                );
            }
        }
    }
}

/// Where the debris belt sits, as a fraction of `min(width, height)`.
///
/// A stated band **between two of the scene's own orbits**, not a scatter: outside the sun's own
/// glow, which reaches `0.13`, and inside the nearest a worker moon ever comes to the middle,
/// which is the mate ring at `0.34` less a worker's orbit and radius — about `0.28`. Sitting the
/// belt in that gap is what keeps it a belt rather than debris tangled through the bodies, and it
/// is where a real one sits too.
const DEBRIS_BAND: (f32, f32) = (0.200, 0.240);

/// How many fragments the belt carries.
const DEBRIS_COUNT: usize = 340;

/// Whole revolutions a fragment makes per loop. Integer, so the belt closes with everything else.
/// Slower than the mates it sits outside, which is what a wider orbit means.
const DEBRIS_REVOLUTIONS: f32 = 1.0;

/// A fragment's drawn radius and alpha. Tiny and dim: the belt is texture, not population.
const DEBRIS_PX: f32 = 0.9;
const DEBRIS_ALPHA: (f32, f32) = (0.06, 0.17);

/// Draw the debris belt: many tiny dim fragments, permanently in motion, on real orbits.
///
/// **It is not fleet data and carries none.** Nothing here is keyed to a project, a pane or an
/// event — it is scene furniture, on the same footing as the starfield, and reading it as a
/// register would be reading something that was never written.
fn draw_debris_belt(buf: &mut [[f32; 4]], width: u32, height: u32, phase: f32) {
    if width == 0 || height == 0 {
        return;
    }
    let scale = width.min(height) as f32;
    let centre = scene_origin(width, height);

    for i in 0..DEBRIS_COUNT {
        let lattice = i as f32 * 0.311;
        // Its own orbit inside the band, and its own starting angle — fixed per fragment, so the
        // belt is one object in motion rather than a fresh scatter each frame.
        let band = value_noise(lattice, 7.0, 23);
        let radius = mix(DEBRIS_BAND.0, DEBRIS_BAND.1, band) * scale;
        let start = value_noise(lattice, 19.0, 23) * 2.0 * PI;
        // A fragment further out goes round more slowly, on the same law the bodies obey.
        let rate = DEBRIS_REVOLUTIONS;
        let angle = start + phase * rate;
        let (sin_a, cos_a) = angle.sin_cos();
        let px = centre.0 + cos_a * radius;
        // In the plane, like everything else that orbits: a belt drawn round rather than
        // foreshortened is the one thing in the frame viewed from a different angle.
        let py = centre.1 + sin_a * radius * ORBIT_PLANE_SQUASH;

        let alpha = mix(
            DEBRIS_ALPHA.0,
            DEBRIS_ALPHA.1,
            value_noise(lattice, 31.0, 23),
        );
        let x0 = (px - DEBRIS_PX).floor().max(0.0) as i32;
        let x1 = (px + DEBRIS_PX).ceil().min(width as f32) as i32;
        let y0 = (py - DEBRIS_PX).floor().max(0.0) as i32;
        let y1 = (py + DEBRIS_PX).ceil().min(height as f32) as i32;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let dx = xx as f32 + 0.5 - px;
                let dy = yy as f32 + 0.5 - py;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > DEBRIS_PX {
                    continue;
                }
                let idx = yy as usize * width as usize + xx as usize;
                blend(
                    &mut buf[idx],
                    DEBRIS_RGB01,
                    alpha * clamp01(1.0 - dist / DEBRIS_PX),
                );
            }
        }
    }
}

/// Debris is rock, so it is the asteroid's own neutral tone rather than anything severity-coded.
const DEBRIS_RGB01: (f32, f32, f32) = (0.55, 0.51, 0.46);

/// Every distinct orbit in the scene: `(parent, its elements, deepest wear on it, seed)`.
///
/// Distinct by parent and by the elements themselves rather than by body: a whole tier of workers
/// under one mate shares a single ring, so drawing one groove per body composites several copies of
/// the same path and saturates it long before any of them is worn. Since the ladder arrived, two
/// second mates never share one — each rung is its own curve, which is what makes the track layer a
/// field of nested grooves instead of one line. The seed is taken from the parent and the rung, so a
/// shared groove has one shape rather than a different scatter depending on which body drew it last.
fn distinct_orbits(layout: &SceneLayout) -> Vec<(usize, OrbitTrack, f32, u32)> {
    let mut orbits: Vec<(usize, OrbitTrack, f32, u32)> = Vec::new();
    for body in layout.bodies.iter() {
        if !body.seated || body.wear <= 0.0 || body.orbit_radius_px <= 0.0 {
            continue;
        }
        let Some(parent) = body.parent.filter(|p| layout.is_seated(*p)) else {
            continue;
        };
        // Centre is filled in per frame by the caller, which is the only part of the path that
        // moves: a worker's ring travels with its mate.
        let track = OrbitTrack {
            centre: (0.0, 0.0),
            semi_major_px: body.orbit_radius_px,
            eccentricity: body.eccentricity,
            periapsis: body.periapsis,
            inclination: body.inclination,
        };
        match orbits.iter_mut().find(|(existing_parent, existing, _, _)| {
            *existing_parent == parent
                && (existing.semi_major_px - track.semi_major_px).abs() < 0.5
                && (existing.eccentricity - track.eccentricity).abs() < 1e-3
                && (existing.inclination - track.inclination).abs() < 1e-3
        }) {
            // "How much has passed here" is the deepest wear on the path, not the sum of every
            // body's separately: two bodies each half-worn have not worn one groove fully.
            Some((_, _, wear, _)) => *wear = wear.max(body.wear),
            None => orbits.push((
                parent,
                track,
                body.wear,
                body_seed(parent).wrapping_add(4_099).wrapping_add(
                    // Each rung gets its own scatter; without this every groove in the field wears
                    // in the same places, which reads as one texture stretched over eight curves.
                    (body.orbit_radius_px as u32).wrapping_mul(2_654_435_761),
                ),
            )),
        }
    }
    orbits
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
/// One mote per mate, **and the key named in words beside them**.
///
/// A41(c)/A42(e) require the disclosure to say what was dropped and by what rule, not only that
/// something was: the reference prints `8 of 16 mates dropped - smallest by files at HEAD`. This
/// module could not, having no font by construction, and a fan of countable dots is an exact count
/// of an unstated quantity — a viewer can see that eight are missing and not which eight, or why.
/// The captain's sky-label ruling put a face in this module, so the sentence the marks were standing
/// in for is now simply written.
fn draw_overflow_mark(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    seated: usize,
    beyond: usize,
    phase: f32,
) {
    if beyond == 0 || width == 0 || height == 0 {
        return;
    }
    let scale = width.min(height) as f32;
    let centre = scene_origin(width, height);
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
        let py = centre.1 + sin_a * radius * ORBIT_PLANE_SQUASH;
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

    // The key, under the fan. Not a label on a body — nothing here *is* a body — so it is placed
    // against the fan's own arc rather than against a radius, and it fades against the panel like
    // every other caption.
    let text_scale = label_scale(width, height);
    let (sin_a, cos_a) = OVERFLOW_ANGLE.sin_cos();
    let anchor = (
        centre.0 + cos_a * radius,
        centre.1 + sin_a * radius * ORBIT_PLANE_SQUASH,
    );
    let key = format!(
        "{beyond} of {} mates dropped \u{b7} smallest by files at HEAD",
        seated + beyond
    );
    let tx = anchor.0 - (key.chars().count() * GLYPH_ADVANCE * text_scale) as f32 / 2.0;
    let ty = anchor.1 + (OVERFLOW_MARK_PX + LABEL_GAP_PX.0 * text_scale as f32).ceil();
    let clear = clamp01((tx - (panel_width(width) + 4.0)) / LABEL_FADE_PX);
    draw_text(
        buf,
        width,
        height,
        tx,
        ty,
        &key,
        LABEL_READOUT_RGB01,
        LABEL_READOUT_ALPHA * clear,
        text_scale,
    );
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
#[allow(clippy::too_many_arguments)]
fn draw_ring(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    body: &BodyLayout,
    position: (f32, f32),
    // The radius the planet is actually drawn at this frame, depth included — the ring is solved
    // in these units so it never sits inside its own planet on the near side of the plane.
    drawn_radius_px: f32,
    half: RingHalf,
    phase: f32,
    seed: u32,
    sun_pos: (f32, f32),
) {
    let Some((inner, outer)) = body.ring_radii_px(drawn_radius_px) else {
        return;
    };
    if outer <= inner || outer <= 0.0 {
        return;
    }

    let count = ((outer * 2.0 * PI * RING_PARTICLES_PER_PX) as usize)
        .clamp(RING_PARTICLE_BOUNDS.0, RING_PARTICLE_BOUNDS.1);
    let bright = RING_BRIGHT + RING_BRIGHT_PER_STREAK * body.streak;
    let spin = phase * RING_REVOLUTIONS_PER_LOOP + body.base_angle;

    // The planet casts on its rings — the other half of the pair, and the one the particle stream
    // makes available for the cost of a dot product. The test is the shadow cylinder, done in the
    // *ring's own plane*: a particle is in shadow if it lies anti-sunward of the body and its
    // perpendicular offset from the body–sun line is inside the body's radius. The ring is drawn
    // squashed, so the sun direction is un-squashed into the same frame first, or the shadow sits
    // at the wrong angle by exactly that factor.
    let (mut sun_x, mut sun_y) = (
        sun_pos.0 - position.0,
        (sun_pos.1 - position.1) / RING_SQUASH,
    );
    let sun_len = (sun_x * sun_x + sun_y * sun_y).sqrt().max(0.0001);
    sun_x /= sun_len;
    sun_y /= sun_len;
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
        // In the planet's shadow? Ring-plane offsets, not screen ones.
        let (qx, qy) = (cos_a * radius, sin_a * radius);
        let shadow = if qx * sun_x + qy * sun_y < 0.0 {
            let perp = (qx * sun_y - qy * sun_x).abs();
            let reach = drawn_radius_px * PLANET_SHADOW_REACH;
            if perp < reach {
                // A real umbra has a penumbra, and a hard-edged shadow on a 3px particle stream
                // stutters as the ring turns.
                clamp01((reach - perp) / (drawn_radius_px * 0.22)) * PLANET_SHADOW_DEPTH
            } else {
                0.0
            }
        } else {
            0.0
        };

        let alpha = (bright * (0.55 + 0.75 * spread)).min(0.92) * (1.0 - shadow);
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
fn comet_travel(tier: WinTier, progress: f32) -> f32 {
    // Ask is the pre-tier p² motion exactly. Heavier wins eject more slowly but still arrive at
    // precisely the endpoint when their longer life closes.
    clamp01(progress).powf(1.0 + 1.0 / tier.ejection_scale())
}

pub(crate) fn comet_position_normalized(
    tier: WinTier,
    start: (f32, f32),
    end: (f32, f32),
    progress: f32,
) -> (f32, f32) {
    let travel = comet_travel(tier, progress);
    (mix(start.0, end.0, travel), mix(start.1, end.1, travel))
}

fn comet_position(
    comet: &Comet,
    end: (f32, f32),
    progress: f32,
    width: u32,
    height: u32,
) -> (f32, f32) {
    let point = comet_position_normalized(comet.tier, comet.start, end, progress);
    (point.0 * width as f32, point.1 * height as f32)
}

fn draw_comet_dot(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    center: (f32, f32),
    radius: f32,
    alpha: f32,
) {
    const COLOR: (f32, f32, f32) = (184.0 / 255.0, 216.0 / 255.0, 230.0 / 255.0);
    let x0 = (center.0 - radius).floor().max(0.0) as i32;
    let x1 = (center.0 + radius).ceil().min(width as f32) as i32;
    let y0 = (center.1 - radius).floor().max(0.0) as i32;
    let y1 = (center.1 + radius).ceil().min(height as f32) as i32;
    for yy in y0..y1 {
        for xx in x0..x1 {
            let dx = xx as f32 + 0.5 - center.0;
            let dy = yy as f32 + 0.5 - center.1;
            if dx * dx + dy * dy <= radius * radius {
                blend(
                    &mut buf[yy as usize * width as usize + xx as usize],
                    COLOR,
                    alpha,
                );
            }
        }
    }
}

fn draw_comet(buf: &mut [[f32; 4]], width: u32, height: u32, comet: &Comet, end: (f32, f32)) {
    let pos = comet_position(comet, end, comet.progress, width, height);

    let core_radius = mix(1.4, 4.0, comet.magnitude) * comet.tier.head_scale();
    let tail_cap = mix(18.0, 140.0, comet.magnitude)
        * comet.tier.trail_scale()
        * (width.min(height) as f32 / 1440.0);

    // A tail is a series of actual earlier trajectory samples. Every mark has the same solid
    // #b8d8e6 colour; there is no line fill and no synthetic gradient between head and tail.
    let mut previous = pos;
    let mut trail_length = 0.0;
    for normalised in comet.trail.iter().rev().skip(1) {
        if trail_length >= tail_cap {
            break;
        }
        let point = (normalised.0 * width as f32, normalised.1 * height as f32);
        trail_length += ((point.0 - previous.0).powi(2) + (point.1 - previous.1).powi(2)).sqrt();
        draw_comet_dot(
            buf,
            width,
            height,
            point,
            (core_radius * 0.28).max(0.6),
            0.58,
        );
        previous = point;
    }
    draw_comet_dot(buf, width, height, pos, core_radius, 1.0);
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
    frame_inner(layout, phase, Parts::ALL)
}

/// Which of the ambient scene's own layers a pass draws.
///
/// A `#[cfg(test)]`-only lever, and it earns its keep: several of these layers are faint by design
/// — a trail is a tenth of its body's brightness, a shadow transit is a fraction off an already
/// shaded face — so a threshold test against "is this pixel bright enough" measures the *scene*
/// rather than the layer, and a comparison against a differently-shaped fleet measures the fleet.
/// Differenced against the same frame with one layer withheld, the answer is that layer and
/// provably nothing else.
///
/// Free in production: every field is a constant at the one call site that is not a test.
#[derive(Debug, Clone, Copy)]
struct Parts {
    rings: bool,
    trails: bool,
    transits: bool,
    debris: bool,
    motes: bool,
    labels: bool,
    overflow: bool,
    /// The shadow a ring casts on its own planet's face — held apart from the ring's particles so a
    /// caller can withhold either one alone. They are two mechanisms sharing one cause, and a test
    /// that can only take both at once cannot tell which of them it is measuring.
    ring_shadow: bool,
}

impl Parts {
    const ALL: Self = Self {
        rings: true,
        trails: true,
        transits: true,
        debris: true,
        motes: true,
        labels: true,
        overflow: true,
        ring_shadow: true,
    };
}

/// [`frame`] with the named layers withheld and nothing else changed.
#[cfg(test)]
fn frame_without(layout: &SceneLayout, phase: f32, parts: Parts) -> Vec<u8> {
    frame_inner(layout, phase, parts)
}

/// [`frame`] with every ring's **particles** suppressed and nothing else changed — the shadow the
/// ring casts on its own planet stays, so a difference against this is ring material and provably
/// nothing else.
///
/// A test-only seam, and it earns its keep: it is the only way to isolate ring pixels *exactly*.
/// Differencing a ringed mate against a gas one instead looks like an isolate and is not — the two
/// types also band differently and a gas giant swells on a streak, so the difference carries three
/// mechanisms and the test would pass on any of them. Against this the difference is the ring and
/// provably nothing else, including inside the body's own disk, which is where the interesting
/// half of the question lives (does the near arc draw over the planet, and does the far arc not).
///
#[cfg(test)]
fn frame_without_rings(layout: &SceneLayout, phase: f32) -> Vec<u8> {
    frame_inner(
        layout,
        phase,
        Parts {
            rings: false,
            ..Parts::ALL
        },
    )
}

fn frame_inner(layout: &SceneLayout, phase: f32, parts: Parts) -> Vec<u8> {
    let rings = parts.rings;
    let (width, height) = (layout.width, layout.height);
    let pixels = width as usize * height as usize;
    let mut buf = vec![[0.0f32; 4]; pixels];

    if width > 0 && height > 0 {
        render_bands(&mut buf, width, height, phase);
    }

    // Under every body and every effect: a groove is wear in the ground, not something laid over
    // what stands on it.
    //
    // One track per *orbit*, not per body. herdr's ladder is a single ring per tier, so every
    // second mate shares one path — five bodies each drawing the whole circle composites five
    // copies of it and saturates the groove long before any of them is actually worn, which loses
    // exactly the reading the layer exists for. The shared groove carries the deepest wear on it,
    // because that is what "how much has passed here" means when several things pass here.
    for (parent, mut track, wear, seed) in distinct_orbits(layout) {
        track.centre = layout.position(parent, phase);
        draw_orbit_track(&mut buf, width, height, &track, wear, seed);
    }

    // Above the grooves and below the bodies: a trail is where a body *was*, so it passes over
    // the permanent wear and under the thing casting it.
    if parts.trails {
        for (idx, body) in layout.bodies.iter().enumerate() {
            if body.seated {
                draw_trail(&mut buf, width, height, layout, idx, body, phase);
            }
        }
    }

    if parts.debris {
        draw_debris_belt(&mut buf, width, height, phase);
    }
    // Studding the grooves, and only the grooves: the ambient tier rides the track layer rather
    // than floating beside it, so a mote is always somewhere the body has actually been.
    if parts.motes {
        for (idx, body) in layout.bodies.iter().enumerate() {
            if body.seated {
                draw_motes(&mut buf, width, height, layout, idx, body, phase);
            }
        }
    }

    if parts.overflow {
        draw_overflow_mark(
            &mut buf,
            width,
            height,
            layout.mates_seated,
            layout.mates_beyond_ladder,
            phase,
        );
    }

    let sun_pos = sun_position(layout, phase);
    // Farthest first. A mate the ring had no slot for is not in the picture, and neither is anything
    // under it — `draw_order` filters those out. It is not gone, though: see [`draw_overflow_mark`].
    for idx in layout.draw_order(phase) {
        let Some(body) = layout.bodies.get(idx) else {
            continue;
        };
        let placed = layout.place(idx, phase);
        let pos = (placed.x, placed.y);
        let body_radius_px = body.body_radius_px * placed.depth_scale();
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
                body_radius_px,
                RingHalf::Back,
                phase,
                seed,
                sun_pos,
            );
        }
        draw_body(
            &mut buf,
            width,
            height,
            pos,
            body_radius_px,
            surface_of(body, seed, phase, parts.ring_shadow, body_radius_px),
            normalize3(light_dir_toward(sun_pos, pos, body.kind)),
        );
        // ...and any worker currently between this mate and the sun drops a real shadow on the
        // face just drawn. After the body, because it lands *on* the surface.
        if parts.transits {
            draw_shadow_transits(
                &mut buf, width, height, layout, idx, body, pos, sun_pos, phase,
            );
        }
        if rings {
            draw_ring(
                &mut buf,
                width,
                height,
                body,
                pos,
                body_radius_px,
                RingHalf::Front,
                phase,
                seed,
                sun_pos,
            );
        }
    }

    // Captions last, over every body: a label is an instrument reading laid on the picture, not
    // something in the picture that another body could pass in front of. Same order as the bodies,
    // so where two captions do meet the nearer body's is the one on top.
    if parts.labels {
        let sun_radius = layout
            .bodies
            .iter()
            .find(|body| body.kind == BodyKind::Sun)
            .map(|body| body.body_radius_px)
            .unwrap_or(0.0);
        let scale = label_scale(width, height);
        for idx in layout.draw_order(phase) {
            let Some(body) = layout.bodies.get(idx) else {
                continue;
            };
            let placed = layout.place(idx, phase);
            draw_sky_label(
                &mut buf,
                width,
                height,
                body,
                (placed.x, placed.y),
                body.body_radius_px * placed.depth_scale(),
                sun_pos,
                sun_radius,
                scale,
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
fn surface_of(
    body: &BodyLayout,
    seed: u32,
    phase: f32,
    rings: bool,
    drawn_radius_px: f32,
) -> Surface {
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
        // The loop phase itself. Each latitude turns a whole number of times within it — see
        // [`BAND_TURNS`] — so a gas giant's day is far shorter than its year and the belts still
        // land back where they started.
        spin: phase,
        // In units of the body's own radius, which is the frame `shade_surface` solves in.
        // Withheld along with the ring itself, so a caller that suppresses rings suppresses the
        // shadow they cast as well — a ring that is not drawn casting a shadow that is would be
        // a picture of nothing.
        ring: body
            .ring_radii_px(drawn_radius_px)
            .filter(|_| rings && drawn_radius_px > 0.0)
            .map(|(inner, outer)| (inner / drawn_radius_px, outer / drawn_radius_px)),
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
            // A rock falling onto a body accelerates into it — see [`ease_in`].
            let travel = ease_in(asteroid.progress);
            let pos = (
                mix(start.0, target_pos.0, travel),
                mix(start.1, target_pos.1, travel),
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
        .unwrap_or_else(|| scene_origin(layout.width, layout.height))
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
pub(crate) fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    encode_png(width, height, rgba)
}

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
    // Flat. There was a vertical gradient here — up to `0.06` of lift toward the top and bottom
    // edges, plus a `0.01` blue bump — meant as a depth cue. It cost the whole light budget: it
    // tripled the frame's median luminance and put a floor under every pixel in the picture, so
    // nothing in the scene could read as bright against it. Depth is what the plane's own
    // foreshortening and [`DEPTH_K`] are for; the void's job is to be void.
    let _ = height;
    for y in y0..y1 {
        for x in 0..width as usize {
            let local_idx = (y - y0) * width as usize + x;
            buf[local_idx] = [base.0, base.1, base.2, 1.0];
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
    let band_sigma = (height as f32 * STAR_BAND_SIGMA).max(1.0);
    for i in 0..star_count(width, height) * STAR_POOL_OVERSAMPLE {
        let lattice = i as f32 * 0.123;
        let sx = value_noise(lattice, 0.0, 11);
        let sy = value_noise(lattice, 100.0, 11);

        // The galactic band, and the cull that draws it: a candidate off the band survives only
        // three times in ten, and draws at a little over half the light when it does.
        let band_distance = (sy * height as f32
            - (height as f32 * STAR_BAND_CENTRE
                + (sx * width as f32 - width as f32 / 2.0) * STAR_BAND_SLOPE))
            .abs();
        let band_boost = (-(band_distance * band_distance) / (2.0 * band_sigma * band_sigma)).exp();
        if value_noise(lattice, 500.0, 11) > STAR_KEEP.0 + STAR_KEEP.1 * band_boost {
            continue;
        }

        // Which depth this star sits at, and therefore how fast it drifts. The field used to be
        // baked once and completely static — a photograph behind a moving system. The drift is the
        // observer's own slow motion, so the near layer swims against the far one and the sky
        // stops being a backdrop. Whole cycles per loop, like everything else that moves here, so
        // the seam still closes.
        let drift = STAR_LAYER_DRIFT[i % STAR_LAYERS] * phase.sin();
        let sx = (sx + drift).rem_euclid(1.0);

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
        let alpha = (STAR_ALPHA.0 + STAR_ALPHA.1 * magnitude)
            * (STAR_BAND_GAIN.0 + STAR_BAND_GAIN.1 * band_boost)
            * twinkle;

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
/// A layer drawn over part of the scene rather than all of it, for
/// [`sample_cell_backgrounds`] to composite in — the machine corner.
///
/// Carries its own origin **in cells** because that is what it is placed by, and its own pixel
/// size because it is its own surface rather than a crop of the scene's.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CornerLayer<'a> {
    pub(crate) rgba: &'a [u8],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) col: u32,
    pub(crate) row: u32,
}

pub(crate) fn sample_cell_backgrounds(
    ambient: &[u8],
    effects: &[u8],
    corner: Option<CornerLayer<'_>>,
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

    // The corner's own pixel box in scene coordinates. Everything outside it is untouched, which
    // is the whole shape of the rule: the reservation is the panel's own box and nothing else. A
    // full-width band across the frame — the obvious implementation — would move the legibility
    // decision for every cell on those rows, most of which have nothing over them at all.
    let corner_box = corner.map(|layer| {
        let x0 = layer.col * cell_width_px;
        let y0 = layer.row * cell_height_px;
        (layer, x0, y0, x0 + layer.width, y0 + layer.height)
    });

    for y in 0..height as usize {
        let row = ((y as u32 / cell_height_px).min(rows - 1)) as usize;
        for x in 0..width as usize {
            let col = ((x as u32 / cell_width_px).min(cols - 1)) as usize;
            let px_idx = (y * width as usize + x) * 4;

            let effects_alpha = f32::from(effects[px_idx + 3]) / 255.0;
            // The machine corner is a third surface placed over the same cells, so text sitting on
            // it has to be measured against what is actually behind it. Sampling only the scene
            // would read the void under a lit groove, commit the wrong foreground, and be wrong
            // exactly where a readout the reader is looking at happens to be.
            let over_corner = corner_box.and_then(|(layer, x0, y0, x1, y1)| {
                let (px, py) = (x as u32, y as u32);
                if px < x0 || px >= x1 || py < y0 || py >= y1 {
                    return None;
                }
                let idx = ((py - y0) as usize * layer.width as usize + (px - x0) as usize) * 4;
                (idx + 3 < layer.rgba.len()).then_some((layer.rgba, idx))
            });
            let corner_alpha = over_corner
                .map(|(rgba, idx)| f32::from(rgba[idx + 3]) / 255.0)
                .unwrap_or(0.0);

            let composite = |channel: usize| {
                let ambient_c = f32::from(ambient[px_idx + channel]);
                let effects_c = f32::from(effects[px_idx + channel]);
                let scene = effects_c * effects_alpha + ambient_c * (1.0 - effects_alpha);
                match over_corner {
                    Some((rgba, idx)) => {
                        f32::from(rgba[idx + channel]) * corner_alpha + scene * (1.0 - corner_alpha)
                    }
                    None => scene,
                }
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

// ---------------------------------------------------------------------------
// The machine corner
// ---------------------------------------------------------------------------

/// The host machine's own state, in the shape this renderer draws it — one already-resolved
/// history per quantity plus one current load per logical CPU.
///
/// Plain numbers, because this module reads no clock, no `/proc` and no `AppState`. Everything
/// about *where these came from* and *whether they are current* is settled by
/// `crate::machine_register` before it gets here; a corner that had to decide for itself whether a
/// sample was stale would be a second copy of that rule.
#[derive(Debug, Clone, Default)]
pub(crate) struct MachineCorner {
    /// Each quantity's history, oldest sample first, in `crate::machine_register::Quantity::ALL`
    /// order. An empty history draws no groove at all rather than a flat one — a groove with no
    /// wear in it is a claim that the machine was idle, which is not the same statement as "not
    /// measured".
    pub(crate) grooves: Vec<Vec<f32>>,
    /// Each logical CPU's current load, in the OS's own core order. A core that reported nothing
    /// is `None` and is drawn absent rather than at zero.
    pub(crate) cores: Vec<Option<f32>>,
}

impl MachineCorner {
    /// Whether there is anything at all to draw. An empty corner draws nothing — never a seeded
    /// history, never an idle trace, never a plausible number invented from nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.grooves.iter().all(|g| g.is_empty()) && self.cores.iter().all(Option::is_none)
    }
}

/// The orbit track's own colour, `#2b6d84` — a worn groove.
///
/// A46(c): the corner introduces **no new material**. A metric's history *is* the orbit track,
/// laid straight and read left to right in time, with the wear at each point the value measured at
/// that point. That is not an analogy borrowed to justify a sparkline: a groove in this scene has
/// always meant "how much has passed here", and a history trace is that sentence with time on the
/// x axis.
pub(crate) const TRACK_RGB01: (f32, f32, f32) = (43.0 / 255.0, 109.0 / 255.0, 132.0 / 255.0);

/// How thick a groove is drawn where nothing has passed, and where the most has. The orrery's own
/// 1–4px.
const GROOVE_WIDTH_PX: (f32, f32) = (1.0, 4.0);

/// How bright a groove is at no wear and at full. A groove at zero is still a groove — the track
/// exists whether or not anything has worn it — but it is faint enough to read as unworn.
const GROOVE_ALPHA: (f32, f32) = (0.22, 0.95);

/// The largest a core body is drawn, as a fraction of **the corner surface's** own
/// `min(width, height)`.
///
/// A46(c): *a core is a body* — shaded by the same [`shade_surface`] every planet is, sized at the
/// cube root of its own load on the same law. A busy core is a heavier body.
///
/// Measured against the corner rather than against the scene, and that distinction is worth
/// keeping straight: this is its own small surface placed over the scene, so the scene's own
/// radius fractions are in a different coordinate space entirely and comparing the two would be a
/// units error wearing the clothes of an invariant. What keeps a core from reading as a stray
/// worker moon is [`CORE_RGB01`] — cores are the substrate, not the work, so they sit outside the
/// lifecycle hue channel every fleet body resolves through.
const CORE_RADIUS_FRACTION: f32 = 0.022;

/// The smallest a core body is drawn, as a share of [`CORE_RADIUS_FRACTION`]. An idle core is
/// still a core, and a core that draws at nothing is indistinguishable from one that reported
/// nothing — which is a distinction H12 requires this corner to keep.
const CORE_RADIUS_FLOOR: f32 = 0.34;

/// Space between core bodies, as a multiple of the largest one's diameter.
const CORE_GAP: f32 = 1.35;

/// The hue a core body carries. Cores are the substrate rather than the work, so they are
/// deliberately outside the lifecycle hue channel every fleet body resolves through — the same
/// exemption the sun and the rings already hold, and for the same reason.
const CORE_RGB01: (f32, f32, f32) = (0.62, 0.72, 0.80);

/// Render the machine corner: one groove per quantity, and one shaded body per logical CPU.
///
/// Returns RGBA8 sized `width` x `height`, transparent everywhere nothing is drawn — this is its
/// own small surface placed at the corner, not a pass over the whole scene. That is what makes it
/// affordable: the register moves every two seconds, and re-baking the whole 36-frame ambient loop
/// at that cadence would cost more CPU than it has.
///
/// Draws nothing at all when there is nothing measured. F21: no fabricated machine number, ever,
/// and no decorative history — not a seeded trace, not an animated idle, not a zero standing in
/// for an unknown.
pub(crate) fn machine_corner_frame(corner: &MachineCorner, width: u32, height: u32) -> Vec<u8> {
    let pixels = width as usize * height as usize;
    let mut buf = vec![[0.0f32; 4]; pixels];
    if width == 0 || height == 0 || corner.is_empty() {
        return pack_rgba8(&buf, false);
    }

    let scale = width.min(height) as f32;
    let pad = GROOVE_WIDTH_PX.1;
    let inner_width = (width as f32 - pad * 2.0).max(1.0);

    // A core body is sized to fit the row it is in, not to a fraction of the frame: this surface
    // is a corner box a couple of dozen cells across, and a 64-core host has to fit the same width
    // a 4-core one does. The cap the other way keeps a 2-core machine from drawing two moons.
    let cores = corner.cores.len().max(1) as f32;
    let core_radius_max = (CORE_RADIUS_FRACTION * scale)
        .min(inner_width / (cores * 2.0 * CORE_GAP))
        .max(1.2);

    // The core row sits at the top, and the grooves fill the rest evenly. The cores identify the
    // row below them by construction: the CPU groove is the one the cores belong to, which is what
    // lets the four grooves be read in a fixed order without this generator needing a font. The
    // labelled numbers go out over the session API, where they can be read as text — herdr's text
    // surface is the terminal itself, and painting a private bitmap font into a wash that sits
    // *under* real glyphs is exactly the thing this scene does not do.
    let core_row_height = core_radius_max * 2.0 + pad * 2.0;
    draw_core_row(
        &mut buf,
        width,
        height,
        &corner.cores,
        (pad, pad + core_radius_max),
        core_radius_max,
    );

    // Evenly over what is left, so the readout fills its box at any cell size rather than huddling
    // at the top of one.
    let rows = corner.grooves.len().max(1) as f32;
    let band = ((height as f32 - core_row_height - pad) / rows).max(GROOVE_WIDTH_PX.1 * 1.5);
    for (row, history) in corner.grooves.iter().enumerate() {
        if history.is_empty() {
            // No groove at all rather than a flat one: an unworn track and an unmeasured one are
            // different statements, and only one of them is true here.
            continue;
        }
        let y = core_row_height + band * (row as f32 + 0.5);
        draw_groove(&mut buf, width, height, history, y, pad, width as f32 - pad);
    }

    pack_rgba8(&buf, false)
}

/// One quantity's history as a worn groove: a straight horizontal track whose thickness and
/// brightness at each point is the value measured at that point, oldest at the left.
///
/// Deliberately *not* a sparkline — no y axis, no line rising and falling. A sparkline would be
/// the widget-strip default this scene is refusing, and it would also introduce a second way of
/// encoding a magnitude into a frame that already has one. Wear is the encoding this scene already
/// uses for "how much has passed here".
fn draw_groove(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    history: &[f32],
    centre_y: f32,
    x0: f32,
    x1: f32,
) {
    if history.is_empty() || x1 <= x0 {
        return;
    }
    let span = x1 - x0;
    let px0 = x0.floor().max(0.0) as i32;
    let px1 = x1.ceil().min(width as f32) as i32;

    for px in px0..px1 {
        // Time runs left to right, with the newest sample at the right-hand end. A partly-filled
        // history draws only as far as it actually reaches rather than stretching to fill the
        // groove: a corner that has been watching for ten seconds must not look like one that has
        // been watching for two minutes.
        let t = (px as f32 + 0.5 - x0) / span;
        let position = t * HISTORY_GROOVE_SAMPLES as f32;
        let filled_from = HISTORY_GROOVE_SAMPLES.saturating_sub(history.len()) as f32;
        if position < filled_from {
            continue;
        }
        let idx = ((position - filled_from) as usize).min(history.len() - 1);
        let wear = clamp01(history[idx]);

        let half = mix(GROOVE_WIDTH_PX.0, GROOVE_WIDTH_PX.1, wear) * 0.5;
        let alpha = mix(GROOVE_ALPHA.0, GROOVE_ALPHA.1, wear);
        let py0 = (centre_y - half).floor().max(0.0) as i32;
        let py1 = (centre_y + half).ceil().min(height as f32) as i32;
        for py in py0..py1 {
            let dy = (py as f32 + 0.5 - centre_y).abs();
            if dy > half {
                continue;
            }
            let idx = py as usize * width as usize + px as usize;
            // Softer at the groove's edges, so a 1px track and a 4px one are the same object at
            // two depths rather than two different bars.
            blend(
                &mut buf[idx],
                TRACK_RGB01,
                alpha * clamp01(1.0 - (dy / half.max(0.5)) * 0.55),
            );
        }
    }
}

/// How many samples a full groove is drawn as. Mirrors `crate::machine_register::HISTORY_SAMPLES`
/// — kept as this module's own constant rather than imported, so the pure generator stays
/// independent of the register's storage decisions and a caller handing it a shorter history gets
/// a partly-drawn groove rather than a stretched one.
pub(crate) const HISTORY_GROOVE_SAMPLES: usize = 60;

/// The per-core row: one shaded sphere per logical CPU, sized by the cube root of its own load.
///
/// A core reporting nothing is drawn *absent* — no body, and no gap closed up behind it either, so
/// core 5 does not silently become core 4. Twelve small bodies are exactly where an average would
/// be invisible, which is why the row exists at all rather than the aggregate alone.
fn draw_core_row(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    cores: &[Option<f32>],
    origin: (f32, f32),
    radius_max: f32,
) {
    if cores.is_empty() {
        return;
    }
    let step = radius_max * 2.0 * CORE_GAP;
    for (idx, core) in cores.iter().enumerate() {
        let cx = origin.0 + radius_max + idx as f32 * step;
        if cx - radius_max > width as f32 {
            break;
        }
        let Some(load) = core else {
            // Absent, and its slot is still spent — the row's positions are the OS's core order.
            continue;
        };
        // Volume ~ mass, the same cube root every planet in this scene obeys: a busy core is a
        // heavier body. Floored so an idle core is still a core.
        let radius = radius_max * mix(CORE_RADIUS_FLOOR, 1.0, clamp01(*load).cbrt());
        draw_body(
            buf,
            width,
            height,
            (cx, origin.1),
            radius,
            Surface {
                base: CORE_RGB01,
                seed: body_seed(idx).wrapping_add(31),
                self_luminous: false,
                bands: None,
                mottle_scale: 1.0,
                // A core is a body, not a world: nothing about it turns, and it carries no ring.
                spin: 0.0,
                ring: None,
            },
            // Lit from the upper left, in the corner's own frame. The scene's real sun is off this
            // surface entirely, so the alternative is an unlit disk — and an unshaded circle is a
            // picture of an instrument rather than a body, which is the gauge-cluster default this
            // corner is refusing.
            normalize3((-0.55, -0.45, 0.70)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawn_win_heads_read_back_at_one_two_four() {
        let layout = build_layout(&[], 256, 256);
        let drawn_width = |tier| {
            let rgba = effects_frame(
                &layout,
                &SceneEffects {
                    comets: vec![Comet {
                        start: (0.5, 0.5),
                        end: (1.0, 0.5),
                        target: None,
                        magnitude: 1.0,
                        tier,
                        trail: std::sync::Arc::new(Vec::new()),
                        progress: 0.0,
                    }],
                    ..Default::default()
                },
                0.0,
            );
            let xs: Vec<_> = rgba
                .chunks_exact(4)
                .enumerate()
                .filter(|(_, pixel)| pixel[3] > 0)
                .map(|(index, _)| index % 256)
                .collect();
            let width =
                xs.iter().max().expect("drawn head") - xs.iter().min().expect("drawn head") + 1;
            let center = &rgba[(128 * 256 + 128) * 4..(128 * 256 + 128) * 4 + 4];
            assert_eq!(
                &center[..3],
                &[184, 216, 230],
                "comet must be solid #b8d8e6"
            );
            width
        };

        let widths = [
            drawn_width(WinTier::Ask),
            drawn_width(WinTier::Ci),
            drawn_width(WinTier::Merge),
        ];
        eprintln!("MEASURE drawn comet head widths: ask/ci/merge = {widths:?} px");
        assert_eq!(widths, [8, 16, 32]);
    }

    #[test]
    fn every_visual_dimension_changes_with_the_win_tier() {
        assert!(WinTier::Ask.trail_scale() < WinTier::Ci.trail_scale());
        assert!(WinTier::Ci.trail_scale() < WinTier::Merge.trail_scale());
        assert!(WinTier::Ask.ejection_scale() > WinTier::Ci.ejection_scale());
        assert!(WinTier::Ci.ejection_scale() > WinTier::Merge.ejection_scale());
        assert!(WinTier::Ask.life_scale() < WinTier::Ci.life_scale());
        assert!(WinTier::Ci.life_scale() < WinTier::Merge.life_scale());
        assert!(comet_travel(WinTier::Ask, 0.5) > comet_travel(WinTier::Ci, 0.5));
        assert!(comet_travel(WinTier::Ci, 0.5) > comet_travel(WinTier::Merge, 0.5));
    }

    #[test]
    fn comet_tail_draws_the_supplied_past_positions() {
        let layout = build_layout(&[], 256, 256);
        let rgba = effects_frame(
            &layout,
            &SceneEffects {
                comets: vec![Comet {
                    start: (0.5, 0.5),
                    end: (1.0, 0.5),
                    target: None,
                    magnitude: 1.0,
                    tier: WinTier::Ask,
                    // A deliberately kinked earlier position. Reconstructing a straight gradient
                    // behind the head cannot put a mark here.
                    trail: std::sync::Arc::new(vec![(0.2, 0.8), (0.5, 0.5)]),
                    progress: 0.0,
                }],
                ..Default::default()
            },
            0.0,
        );
        let past = &rgba[(205 * 256 + 51) * 4..(205 * 256 + 51) * 4 + 4];
        assert!(past[3] > 0, "the recorded past position was not drawn");
        assert_eq!(&past[..3], &[184, 216, 230]);
    }

    fn node(parent: Option<usize>, kind: BodyKind) -> TreeNode {
        TreeNode {
            label: SceneLabel::EMPTY,
            parent,
            kind,
            hue: 41.0,
            severity: Severity::Clear,
            size: BodySize::Fixed,
            streak: 0.0,
            wear: 0.0,
            motes: 0,
            mote_share: 0.0,
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
            label: SceneLabel::EMPTY,
            parent,
            kind,
            hue,
            severity,
            size: BodySize::Fixed,
            streak: 0.0,
            wear: 0.0,
            motes: 0,
            mote_share: 0.0,
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
        let orbit = BodyKind::Moon.orbit_radius_fraction(None) * 1440.0;
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

    /// The sun holds the scene's own origin and never moves off it, whatever the phase.
    ///
    /// **Not the frame's centre.** The composition places it at 51% of the *main area* — the frame
    /// less the panel the worker tree occupies — because the one immovable emitter sitting behind a
    /// prose pane is the thing that move exists to prevent. See [`scene_origin`].
    #[test]
    fn a_lone_sun_sits_at_the_scenes_own_origin() {
        let nodes = [node(None, BodyKind::Sun)];
        let layout = build_layout(&nodes, 1920, 1080);
        let origin = scene_origin(1920, 1080);
        assert_eq!(layout.position(0, 0.0), origin);
        assert_eq!(layout.position(0, 3.0), origin);
        // Right of centre, and by a stated amount rather than by whatever fell out.
        assert!(
            origin.0 > 1920.0 * 0.55,
            "the sun sits at {}, which is not off-centre at all",
            origin.0
        );
    }

    /// A planet orbits the sun on **its own rung** of the ladder, as an ellipse in a foreshortened
    /// plane — not on one shared circle.
    ///
    /// The old shape of this test asserted a *fixed radius* from the sun, which is exactly the
    /// property that made eight seated mates a clock face. What is fixed now is the rung, and what
    /// varies inside it is the orbit's own shape.
    #[test]
    fn a_planet_orbits_the_sun_on_its_own_rung() {
        let nodes = [node(None, BodyKind::Sun), node(Some(0), BodyKind::Planet)];
        let layout = build_layout(&nodes, 1000, 1000);
        let sun = layout.position(0, 0.7);
        let axis = BodyKind::Planet.orbit_radius_fraction(Some(0)) * 1000.0;

        // Over a whole revolution the body's distance from the sun sweeps a real range — periapsis
        // to apoapsis — and the vertical extent is the horizontal one foreshortened.
        let mut min_dist = f32::MAX;
        let mut max_dist = 0.0f32;
        let (mut min_y, mut max_y, mut min_x, mut max_x) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for step in 0..720 {
            let phase = step as f32 / 720.0 * 2.0 * PI;
            let p = layout.position(1, phase);
            let d = ((p.0 - sun.0).powi(2) + (p.1 - sun.1).powi(2)).sqrt();
            min_dist = min_dist.min(d);
            max_dist = max_dist.max(d);
            min_x = min_x.min(p.0);
            max_x = max_x.max(p.0);
            min_y = min_y.min(p.1);
            max_y = max_y.max(p.1);
        }
        assert!(
            (max_dist / min_dist) > 1.15,
            "the orbit is a circle: {min_dist:.1}..{max_dist:.1}"
        );
        // The semi-major axis is the rung it was seated on. Read across the *horizontal* extent,
        // which the plane's foreshortening leaves alone — screen distance from the sun is the
        // squashed quantity and is not `a` in any direction but one.
        let half_width = (max_x - min_x) / 2.0;
        assert!(
            (half_width - axis).abs() < axis * 0.06,
            "the orbit's horizontal half-extent is {half_width}, not the rung at {axis}"
        );
        // And the plane is seen at a slant rather than from above.
        let squash = (max_y - min_y) / (max_x - min_x);
        assert!(
            squash < 0.9 && squash > 0.5,
            "the plane is not foreshortened: vertical extent is {squash:.2} of horizontal"
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
        assert!(dist < BodyKind::Planet.orbit_radius_fraction(Some(0)) * 1000.0);
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

    /// The field one composed frame draws, as the count the density resolves to.
    #[cfg(test)]
    fn reference_star_count() -> usize {
        star_count(1920, 1080)
    }

    #[test]
    fn the_starfield_is_many_faint_stars_and_a_few_bright_ones() {
        let count = reference_star_count();
        let magnitudes: Vec<f32> = (0..count).map(star_magnitude).collect();
        let faint = magnitudes.iter().filter(|m| **m < 0.25).count();
        let bright = magnitudes.iter().filter(|m| **m > 0.8).count();

        // A real magnitude distribution, not an even scatter: the great majority of the field is
        // faint and only a handful stand out. An even scatter would put ~75% above 0.25.
        assert!(
            faint > count / 2,
            "{faint} of {count} stars are faint — the field is an even scatter, not a distribution"
        );
        assert!(
            (1..count / 20).contains(&bright),
            "{bright} of {count} stars are bright — 'very few' has to be some, and few"
        );
    }

    #[test]
    fn only_the_brightest_few_stars_scintillate() {
        let count = reference_star_count();
        let mut magnitudes: Vec<f32> = (0..count).map(star_magnitude).collect();
        let scintillating = magnitudes
            .iter()
            .filter(|m| **m > STAR_SCINTILLATION_MAGNITUDE)
            .count();
        magnitudes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // The bar is the *few*, not the constant: a threshold that names none loses the twinkle
        // entirely, and one that names most of the sky is a shimmer over everything.
        // The bar scales with the field: "a handful" of a thousand is not the same integer as a
        // handful of two hundred and sixty, and pinning it to one would make this test a check on
        // the frame size rather than on the cut.
        assert!(
            (3..=count / 40).contains(&scintillating),
            "{scintillating} of {count} stars scintillate"
        );
        // And they really are the brightest ones — nothing below the top decile qualifies.
        let top_decile = magnitudes[count - count / 10];
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
                    spin: 0.0,
                    ring: None,
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
                tier: WinTier::Ask,
                trail: std::sync::Arc::new(Vec::new()),
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
        let samples =
            sample_cell_backgrounds(&ambient, &effects, None, width, height, 2, 2, cols, rows);
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
        let samples = sample_cell_backgrounds(&ambient, &effects, None, width, height, 2, 2, 2, 2);
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
                tier: WinTier::Merge,
                trail: std::sync::Arc::new(Vec::new()),
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
                ..arriving.comets[0].clone()
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
                tier: WinTier::Ask,
                trail: std::sync::Arc::new(Vec::new()),
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
    // The ambient tier
    // ---------------------------------------------------------------------------

    /// A fleet where every body carries motes, at shares spread across the transform's range.
    fn moted_fleet() -> Vec<TreeNode> {
        let counts = [64u32, 24, 8, 2, 0];
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for (i, motes) in counts.iter().enumerate() {
            nodes.push(TreeNode {
                wear: 0.6,
                motes: *motes,
                mote_share: 1.0 - i as f32 * 0.18,
                ..sized(
                    Some(0),
                    BodyKind::Planet,
                    BodySize::Files((i as u32 + 1) * 500),
                )
            });
        }
        nodes
    }

    /// Total light one layer puts into the composited frame, by render difference — the measure
    /// with no free parameter in it. Asking "how bright is one element" instead depends entirely
    /// on what gets called one element, which is the trap this measure exists to avoid.
    fn light_added(layout: &SceneLayout, phase: f32, without: Parts) -> u64 {
        let with = frame(layout, phase);
        let bare = frame_without(layout, phase, without);
        with.chunks_exact(4)
            .zip(bare.chunks_exact(4))
            .map(|(a, b)| {
                (0..3)
                    .map(|c| u64::from(a[c].saturating_sub(b[c])))
                    .sum::<u64>()
            })
            .sum()
    }

    #[test]
    fn a_busier_body_draws_a_brighter_ambient_tier() {
        // The transform reaches the picture: the share is not just computed, it is what the motes
        // are drawn at. Same count both times, so the only thing differing is the share.
        let ink = |share: f32| {
            let nodes = [
                node(None, BodyKind::Sun),
                TreeNode {
                    wear: 0.5,
                    motes: 40,
                    mote_share: share,
                    ..sized(Some(0), BodyKind::Planet, BodySize::Files(900))
                },
            ];
            let layout = build_layout(&nodes, 900, 900);
            light_added(
                &layout,
                0.0,
                Parts {
                    motes: false,
                    ..Parts::ALL
                },
            )
        };
        assert!(
            ink(1.0) > ink(0.1),
            "the attribution share never reached the picture"
        );
        // A body with no motes draws none — the tier is events, not decoration.
        let none = [
            node(None, BodyKind::Sun),
            TreeNode {
                wear: 0.5,
                motes: 0,
                mote_share: 1.0,
                ..sized(Some(0), BodyKind::Planet, BodySize::Files(900))
            },
        ];
        let layout = build_layout(&none, 900, 900);
        assert_eq!(
            light_added(
                &layout,
                0.0,
                Parts {
                    motes: false,
                    ..Parts::ALL
                }
            ),
            0
        );
    }

    #[test]
    fn the_ambient_tier_never_outranks_the_ceremonial_one() {
        // The refusal that matters when a tier is added *beneath* an existing one: the new, more
        // frequent thing quietly becomes the loudest thing in the frame and the ceremony stops
        // reading as ceremony.
        //
        // Measured as **light added to the composited frame by render difference**, which has no
        // free parameter in it — "peak drawn presence of one element" cannot be applied honestly
        // when one tier is small-and-bright and the other is many-and-faint, because the answer
        // depends entirely on what gets called one element.
        let nodes = moted_fleet();
        let layout = build_layout(&nodes, 1_200, 800);
        let phase = 0.4;

        // Everything the ambient tier puts in the frame, at its fullest: every body moted.
        let ambient = light_added(
            &layout,
            phase,
            Parts {
                motes: false,
                ..Parts::ALL
            },
        );

        // One ceremonial event at its peak — a landed large task, the loudest single thing the
        // scene draws.
        let ceremony = SceneEffects {
            comets: vec![Comet {
                start: (0.0, 0.15),
                end: (1.0, 0.85),
                target: None,
                magnitude: 1.0,
                tier: WinTier::Merge,
                trail: std::sync::Arc::new(Vec::new()),
                progress: 0.5,
            }],
            ..Default::default()
        };
        let lit = effects_frame(&layout, &ceremony, phase);
        let dark = effects_frame(&layout, &SceneEffects::default(), phase);
        let ceremonial: u64 = lit
            .chunks_exact(4)
            .zip(dark.chunks_exact(4))
            .map(|(a, b)| {
                // Weighted by the overlay's own alpha, because that is the light it actually adds
                // to the composite rather than the colour it would add at full opacity.
                let alpha = u64::from(a[3]);
                (0..3)
                    .map(|c| u64::from(a[c].saturating_sub(b[c])) * alpha / 255)
                    .sum::<u64>()
            })
            .sum();

        assert!(
            ceremonial > 0,
            "the ceremonial event drew nothing to compare against"
        );
        assert!(
            ambient * 4 <= ceremonial,
            "the ambient tier puts {ambient} into the frame against one ceremonial event's \
             {ceremonial} — the bar is a quarter, so it may put at most {}",
            ceremonial / 4
        );
    }

    // ---------------------------------------------------------------------------
    // Corona, band shear, and ring shadows
    // ---------------------------------------------------------------------------

    #[test]
    fn the_sun_carries_a_structured_corona_rather_than_a_round_glow() {
        // A star with an evenly-falling halo round it is drawn as a *light source*; a star with
        // streamers and prominences is drawn as a thing. This is the difference.
        let layout = build_layout(&[node(None, BodyKind::Sun)], 900, 900);
        let rgba = frame(&layout, 0.0);
        let (cx, cy) = scene_origin(900, 900);
        let radius = layout.bodies[0].body_radius_px;

        // Sampled all the way round at a fixed distance outside the disc, so the only thing that
        // can vary is the corona's own structure.
        // The identical frame with no body in it at all. The starfield is a pure function of the
        // frame size, so every sample outside the sun's own reach is the *same pixel* in both — and
        // differencing them isolates the corona exactly, at every radius, with no threshold guessed
        // against a sky that has stars in it. That matters more than it used to: at the swept
        // `COR_A = 0.075` the corona is a few luminance steps above the void where it used to be
        // dozens, and a single star on the sampling circle is brighter than all of it.
        let void_frame = frame(&build_layout(&[], 900, 900), 0.0);
        let corona_ring = |out: f32| {
            let at = radius * out;
            (0..180)
                .map(|i| {
                    let a = i as f32 / 180.0 * 2.0 * PI;
                    let (x, y) = ((cx + a.cos() * at) as i32, (cy + a.sin() * at) as i32);
                    luminance8(&rgba, layout.width, x, y)
                        - luminance8(&void_frame, layout.width, x, y)
                })
                .collect::<Vec<f32>>()
        };

        // Sampled out where the streamers have separated rather than right at the limb, where
        // every corona is nearly even by construction. **An even halo scores exactly 1.0** — every
        // angle carrying the same light — so a peak at twice the ring's own mean is a wide margin
        // over the thing being ruled out rather than a narrow one under a nicer number.
        let ring = corona_ring(1.5);
        let peak = ring.iter().copied().fold(0.0f32, f32::max);
        let mean = ring.iter().sum::<f32>() / ring.len() as f32;
        assert!(
            mean > 0.0 && peak > mean * 2.0,
            "the corona is an even halo: peak {peak:.1} against a ring mean of {mean:.1}"
        );

        // And the structure is *radial* — streamers rather than blobs — so the whole angular
        // profile at one distance still resembles the profile further out. Correlated rather than
        // compared at their peaks: with several streamers of nearly equal brightness, which one
        // happens to be highest can swap between radii without the structure having moved at all.
        //
        // **Read as angular sectors rather than sample by sample.** The corona carries a fine radial
        // striation whose noise is re-drawn every ninth of a radius — the artifact's own
        // `frac(sin(th*37.1 + floor(rho*9)*4.7))`, and the term it deliberately *widened* because it
        // is what makes a streamer read as a bundle of threads rather than a smear. Sample by
        // sample that striation is most of the variance and it decorrelates completely between
        // bands, so a raw profile measures the threads and not the streamer they are in. A sector is
        // three striation periods wide and a good deal narrower than a lobe, so summing within it
        // cancels the one and keeps the other.
        const SECTORS: usize = 12;
        const PER_SECTOR: usize = 15;
        let profile = |out: f32| {
            let ring = corona_ring(out);
            let mut sectors = [0.0f32; SECTORS];
            for (sector, slot) in sectors.iter_mut().enumerate() {
                *slot = ring[sector * PER_SECTOR..(sector + 1) * PER_SECTOR]
                    .iter()
                    .sum::<f32>();
            }
            let mean = sectors.iter().sum::<f32>() / SECTORS as f32;
            sectors.into_iter().map(|v| v - mean).collect::<Vec<f32>>()
        };
        let near = profile(1.12);
        let far = profile(1.42);
        let dot: f32 = near.iter().zip(&far).map(|(a, b)| a * b).sum();
        let norm = (near.iter().map(|a| a * a).sum::<f32>()
            * far.iter().map(|b| b * b).sum::<f32>())
        .sqrt()
        .max(1e-6);
        let correlation = dot / norm;
        assert!(
            correlation > 0.5,
            "the corona's angular structure correlates only {correlation:.2} between radii — \
             those are blobs, not streamers"
        );

        // ...and it reaches far further than a planet's atmospheric fringe, which is what a corona
        // actually does and what the fringe constant does not — and then it *ends*, rather than
        // being clipped by whatever bound the loop happened to run to.
        const { assert!(CORONA_REACH > 2.5) };
        let light_at = |out: f32| corona_ring(out).iter().sum::<f32>();
        assert!(
            light_at(2.0) > 0.0,
            "the corona adds nothing past twice the disc's radius"
        );
        assert_eq!(
            light_at(2.7),
            0.0,
            "the corona is still adding light past its own stated reach"
        );
    }

    #[test]
    fn a_gas_giants_belts_are_sheared_rather_than_striped() {
        // Two harmonics plus a darker belt is a still image slid around the frame. What a gas
        // giant has is turbulence sheared by differential rotation: the zonal rate falls with
        // latitude, so belts travel past each other and their boundaries curl.
        let surface = |spin: f32| Surface {
            base: (0.8, 0.8, 0.8),
            seed: 17,
            self_luminous: false,
            bands: BodyType::Gas.band_count(),
            mottle_scale: 0.0,
            spin,
            ring: None,
        };
        const R: f32 = 120.0;
        let at = |nx: f32, ny: f32, spin: f32| {
            luminance(shade_surface(
                nx * R,
                ny * R,
                R,
                (0.0, 0.0, 1.0),
                surface(spin),
            ))
        };

        // A stripe is constant along a line of latitude. A sheared, domain-warped band is not:
        // walking round the visible hemisphere at one latitude has to change what is there.
        let along: Vec<f32> = (0..24)
            .map(|i| at((i as f32 / 23.0 - 0.5) * 1.3, 0.22, 0.0))
            .collect();
        let peak = along.iter().copied().fold(0.0f32, f32::max);
        let floor = along.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(
            peak - floor > 0.02,
            "the belts are flat along a latitude — they are stripes, not turbulence \
             ({floor:.4}..{peak:.4})"
        );

        // Differential rotation: the belts move *past each other*, so the same rotation does not
        // shift every latitude by the same amount. Two latitudes, one turn.
        let shift_at = |ny: f32| {
            let before = at(0.0, ny, 0.0);
            let after = at(0.0, ny, 0.7);
            (after - before).abs()
        };
        let equator = shift_at(0.02);
        let midlatitude = shift_at(0.62);
        assert!(equator > 0.0 && midlatitude > 0.0, "nothing rotated at all");
        assert!(
            (equator - midlatitude).abs() > 1e-4,
            "every latitude moved by the same amount — that is rotation, not shear"
        );
        // ...and every latitude turns a whole number of times per loop, which is what lets the
        // shear exist at all inside a baked loop.
        assert_ne!(BAND_TURNS.0, BAND_TURNS.1);
        assert_eq!(BAND_TURNS.0, BAND_TURNS.0.round());
        assert_eq!(BAND_TURNS.1, BAND_TURNS.1.round());
    }

    #[test]
    fn a_ring_casts_a_shadow_on_its_own_planet() {
        // The geometry, not a drawn band: the shadow narrows, widens and crosses the disc as the
        // body goes round, because the light vector does. Measured *inside* the planet's own disc,
        // which is the half of the pair that has nothing to do with the ring's own pixels.
        let ringed = one_mate_scene(BodyType::Ringed, 0.5);
        let radius = ringed.bodies[MATE].body_radius_px;

        let mut ever_shadowed = false;
        for step in 0..16 {
            let phase = step as f32 / 16.0 * 2.0 * PI;
            let pos = ringed.position(MATE, phase);
            let with = frame(&ringed, phase);
            let without = frame_without(
                &ringed,
                phase,
                Parts {
                    ring_shadow: false,
                    ..Parts::ALL
                },
            );
            // The shadow alone, named directly: the ring's own particles are drawn in both frames
            // and difference away, so nothing here can be a near-arc particle rather than a shadow.
            let reach = (radius * 0.55) as i32;
            for dy in -reach..=reach {
                for dx in -reach..=reach {
                    if ((dx * dx + dy * dy) as f32).sqrt() > radius * 0.55 {
                        continue;
                    }
                    let (x, y) = (pos.0 as i32 + dx, pos.1 as i32 + dy);
                    let lit = luminance8(&without, ringed.width, x, y);
                    let shaded = luminance8(&with, ringed.width, x, y);
                    if lit > 8.0 && shaded < lit * 0.85 {
                        ever_shadowed = true;
                    }
                }
            }
        }
        assert!(
            ever_shadowed,
            "the rings never darkened their own planet at any phase of its orbit"
        );
        // ...and it is a shadow rather than a band painted black.
        const { assert!(RING_SHADOW_DEPTH < 1.0) };
    }

    #[test]
    fn a_planet_casts_a_shadow_on_its_own_ring() {
        // The other half of the pair, and the one the ring's particle stream makes available for
        // the cost of a dot product: a particle anti-sunward of the body, inside the body's own
        // radius perpendicular to the body–sun line, is in shadow.
        let phase = 0.0;
        let layout = one_mate_scene(BodyType::Ringed, 1.0);
        let with = frame(&layout, phase);
        let without = frame_without(
            &layout,
            phase,
            Parts {
                rings: false,
                ..Parts::ALL
            },
        );
        let pos = layout.position(MATE, phase);
        let sun = sun_position(&layout, phase);
        let (sx, sy) = {
            let (dx, dy) = (sun.0 - pos.0, (sun.1 - pos.1) / RING_SQUASH);
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            (dx / len, dy / len)
        };

        // Ring **light** on each side, in the ring plane rather than on screen.
        //
        // Counting *pixels* is what this used to do and it stopped being an instrument when the
        // void went flat and dark: a shadowed particle still lands somewhere, and against a darker
        // ground its residual delta clears any "did anything draw here" threshold just as an
        // unshadowed one does. What a shadow removes is light, so light is what is summed.
        let (mut sunward, mut anti) = (0u64, 0u64);
        let radius = layout.drawn_radius_px(MATE, phase);
        let reach = (radius * 3.0) as i32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let (x, y) = (pos.0 as i32 + dx, pos.1 as i32 + dy);
                let i = (y.max(0) as usize * layout.width as usize + x.max(0) as usize) * 4;
                if i + 2 >= with.len() {
                    continue;
                }
                if !(0..3).any(|c| with[i + c].abs_diff(without[i + c]) > 1) {
                    continue;
                }
                // Outside the planet's own disc, so this is ring and not surface.
                if ((dx * dx + dy * dy) as f32).sqrt() < radius * 1.2 {
                    continue;
                }
                let light: u64 = (0..3)
                    .map(|c| u64::from(with[i + c].abs_diff(without[i + c])))
                    .sum();
                let (qx, qy) = (dx as f32, dy as f32 / RING_SQUASH);
                if qx * sx + qy * sy < 0.0 {
                    anti += light;
                } else {
                    sunward += light;
                }
            }
        }
        assert!(
            sunward > 0 && anti < sunward,
            "the ring is unshadowed: {sunward} units of light sunward against {anti} anti-sunward"
        );
        const { assert!(PLANET_SHADOW_DEPTH < 1.0) };
    }

    // ---------------------------------------------------------------------------
    // The rest of the sky
    // ---------------------------------------------------------------------------

    /// Rec.709 luminance of one pixel of an RGBA8 frame — the measure every published number about
    /// the reference artifact is quoted in.
    fn luminance709(rgba: &[u8], i: usize) -> f32 {
        0.2126 * f32::from(rgba[i * 4])
            + 0.7152 * f32::from(rgba[i * 4 + 1])
            + 0.0722 * f32::from(rgba[i * 4 + 2])
    }

    fn median_of(values: &mut [f32]) -> f32 {
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        values.get(values.len() / 2).copied().unwrap_or(0.0)
    }

    /// **The light budget, measured on a rendered frame rather than argued about.**
    ///
    /// This exists because of a specific failure: two build rounds shipped a scene that did not
    /// look like the artifact it was built from, and every item in both was checked against a
    /// description of the gap rather than against a picture. Measured afterwards, herdr's sky was
    /// `1.69x` brighter overall than the reference while containing *fewer* bright things, and not
    /// one pixel in the frame was as dark as the reference's median pixel. Those are numbers, so
    /// they belong in a test — the corrective process this build ran under says the numeric check is
    /// part of "done" and not a judgment call.
    ///
    /// Every bar below is the reference's own measured value with a margin, not a target invented
    /// here. See `data/herdr-orrery-full-vision-spec-20260813/report.md` section 2.4 (firstmate
    /// home) for the full table.
    #[test]
    fn the_scenes_light_budget_is_spent_on_objects_rather_than_on_a_wash() {
        let (w, h) = (1_280u32, 720u32);
        let layout = build_layout(&sky_fleet(), w, h);
        let rgba = frame(&layout, 0.0);
        let pixels = (w * h) as usize;
        let panel = panel_width(w);
        let (cx, cy) = scene_origin(w, h);
        let sun_radius = SUN_RADIUS_FRACTION * w.min(h) as f32;

        let mut main_area = Vec::with_capacity(pixels);
        let mut limb_annulus = Vec::new();
        let mut void_ring = Vec::new();
        let mut at_or_below_8 = 0usize;
        for i in 0..pixels {
            let x = (i as u32 % w) as f32 + 0.5;
            let y = (i as u32 / w) as f32 + 0.5;
            let l = luminance709(&rgba, i);
            if l <= 8.0 {
                at_or_below_8 += 1;
            }
            if x >= panel {
                main_area.push(l);
            }
            let radii = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() / sun_radius;
            if (1.02..1.60).contains(&radii) {
                limb_annulus.push(l);
            }
            // Far enough out to be bare sky at every rung of the ladder, and the frame's own answer
            // to "what is void here" rather than a constant restated.
            if (6.0..8.0).contains(&radii) {
                void_ring.push(l);
            }
        }

        // The median pixel of the main area *is* bare void, as the reference's is — measured 5.72
        // there, which is `#03060b` exactly.
        let void = median_of(&mut void_ring);
        let median = median_of(&mut main_area);
        assert!(
            (median - void).abs() <= 1.0,
            "the median pixel of the frame is {median:.2} against a void of {void:.2} — the scene \
             is sitting on a wash rather than on space"
        );

        // ...and almost all of it is that dark. The reference measures 93.4%; this build measures
        // a little more, having no prose across it.
        let dark = at_or_below_8 as f32 / pixels as f32;
        assert!(
            dark >= 0.90,
            "only {:.1}% of the frame is at or below luminance 8",
            dark * 100.0
        );

        // The corona is invisible at the median, which is what the artifact's own swept `COR_A`
        // buys and what its published capture shows: the annulus just outside the limb reads the
        // void, while the streamers in it are still plainly there.
        let annulus_mean = limb_annulus.iter().sum::<f32>() / limb_annulus.len().max(1) as f32;
        let limb = median_of(&mut limb_annulus);
        assert!(
            (limb - void).abs() <= 2.0,
            "the annulus at 1.02-1.60 solar radii reads {limb:.2} against a void of {void:.2} — \
             the corona is a wash again"
        );
        // **And the median alone does not settle it.** Five lobes leave three quarters of that
        // annulus at exactly the void whatever their peak is, so the median above is a check on the
        // corona having *structure* and is blind to how bright the structure is — a `COR_A` seven
        // times the artifact's passes it unchanged. The mean is what carries the amplitude. It is
        // the reading the reference capture could not be measured by, because that frame has prose
        // crossing the disc by design and a mean there measures text; herdr's scene has no prose in
        // it, so here it is the honest number and it is the one the sweep was run against.
        assert!(
            annulus_mean - void < 6.0,
            "the annulus at 1.02-1.60 solar radii means {annulus_mean:.2} against a void of \
             {void:.2} — the corona is the loudest perpetual thing in the frame again"
        );

        // And the sky is many-faint rather than few-bright. Measured on the starfield alone, which
        // is a pure function of the frame size and carries no fleet data at all, and **in a patch
        // clear of the galactic band** — which is the method the reference numbers were taken by,
        // and it matters: on the band a star draws at its full magnitude alpha and off it at a
        // little over half, so a whole-frame median measures the mix of the two rather than the
        // typical star either published figure is about.
        let sky = frame(&build_layout(&[], w, h), 0.0);
        let (patch_x, patch_y, patch_w, patch_h) = (300u32, 20u32, 300u32, 200u32);
        let mut peaks = Vec::new();
        for y in patch_y..patch_y + patch_h {
            for x in patch_x..patch_x + patch_w {
                let i = (y * w + x) as usize;
                let l = luminance709(&sky, i);
                let is_peak = (-1i32..=1).all(|dy| {
                    (-1i32..=1).all(|dx| {
                        (dx == 0 && dy == 0)
                            || l - luminance709(
                                &sky,
                                ((y as i32 + dy) as u32 * w + (x as i32 + dx) as u32) as usize,
                            ) >= 2.0
                    })
                });
                if is_peak {
                    peaks.push(l);
                }
            }
        }
        // The artifact's own density is 1,736-1,972 point sources per megapixel; this patch is
        // 0.06 of one, and off the band, so a real field puts several dozen in it and a field that
        // stopped scaling with area puts a handful.
        assert!(
            peaks.len() > 40,
            "only {} point sources in a 300x200 patch of empty sky — the field is not a field",
            peaks.len()
        );
        let typical = median_of(&mut peaks);
        assert!(
            typical < 40.0,
            "the typical star peaks at luminance {typical:.1} — the reference's peak at 24-31, so \
             this is M2's few-bright-dots with the sign flipped"
        );
    }

    /// Write one rendered frame to a PNG, so "look at it" is one command rather than a scratch
    /// crate somebody has to build first.
    ///
    /// `#[ignore]`d because it writes a file and asserts nothing. This is the other half of the
    /// corrective process the test above exists for: the numbers catch a light budget going wrong,
    /// and this catches everything a number cannot — which is what the two build rounds this
    /// replaces were missing.
    ///
    /// ```text
    /// HERDR_SCENE_PNG=/tmp/scene.png cargo test -- --ignored write_the_scene_to_a_png
    /// ```
    #[test]
    #[ignore = "writes a PNG for a human to look at; asserts nothing"]
    fn write_the_scene_to_a_png() {
        let path = std::env::var("HERDR_SCENE_PNG").unwrap_or_else(|_| "scene.png".into());
        let (w, h) = (1_920u32, 1_080u32);
        let layout = build_layout(&sky_fleet(), w, h);
        std::fs::write(&path, encode_rgba_png(w, h, &frame(&layout, 0.0))).expect("write the png");
        eprintln!("wrote {path} at {w}x{h}");
    }

    /// A fleet with workers, at a size worth measuring on.
    fn sky_fleet() -> Vec<TreeNode> {
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for files in [2_470u32, 860, 430] {
            nodes.push(sized(Some(0), BodyKind::Planet, BodySize::Files(files)));
            let planet = nodes.len() - 1;
            for _ in 0..4 {
                nodes.push(node(Some(planet), BodyKind::Moon));
            }
        }
        nodes
    }

    fn luminance8(rgba: &[u8], width: u32, x: i32, y: i32) -> f32 {
        if x < 0 || y < 0 {
            return 0.0;
        }
        let i = (y as usize * width as usize + x as usize) * 4;
        if i + 2 >= rgba.len() {
            return 0.0;
        }
        f32::from(rgba[i]) * 0.299 + f32::from(rgba[i + 1]) * 0.587 + f32::from(rgba[i + 2]) * 0.114
    }

    #[test]
    fn the_starfield_drifts_at_three_depths_and_still_closes_its_loop() {
        // The field used to be baked once and completely static — a photograph behind a moving
        // system. The drift is the observer's own slow motion, and the near layer has to swim
        // against the far one or the "three depths" is a comment rather than a mechanism.
        assert_eq!(STAR_LAYER_DRIFT.len(), STAR_LAYERS);
        let distinct: std::collections::BTreeSet<u32> =
            STAR_LAYER_DRIFT.iter().map(|d| d.to_bits()).collect();
        assert_eq!(
            distinct.len(),
            STAR_LAYERS,
            "two layers drift at the same rate"
        );

        // The field really moves, on a frame with nothing in it but sky.
        let empty = build_layout(&[], 600, 400);
        assert_ne!(frame(&empty, 0.0), frame(&empty, PI));
        // ...and it still closes: every drift is a whole fraction of the width per loop.
        let start = frame(&empty, 0.0);
        let looped = frame(&empty, 2.0 * PI);
        let moved = start
            .chunks_exact(4)
            .zip(looped.chunks_exact(4))
            .filter(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) > 1))
            .count();
        assert!(
            moved * 500 < start.len() / 4,
            "{moved} pixels moved across the loop seam"
        );
    }

    #[test]
    fn every_body_leaves_a_trail_behind_where_it_has_just_been() {
        // The cheapest possible statement that the frame is alive, and distinct from the groove
        // underneath it: a groove is permanent wear and reads revolutions completed, a trail is
        // where the body was a moment ago and reads speed.
        let nodes = sky_fleet();
        let layout = build_layout(&nodes, 1_200, 800);
        let phase = 0.9;
        let rgba = frame(&layout, phase);
        let bare = frame_without(
            &layout,
            phase,
            Parts {
                trails: false,
                ..Parts::ALL
            },
        );

        for (idx, body) in layout.bodies.iter().enumerate() {
            if body.kind == BodyKind::Sun {
                continue;
            }
            // A little way back along this body's own path — inside the trail's reach, and clear
            // of the body itself.
            // The nearest point on this body's own past path that is clear of the body itself —
            // walked outward rather than picked, because how far back that is depends on how fast
            // the body moves, which is exactly what varies across a fleet. The fade is quadratic,
            // so the far end of a trail is nearly gone by design and asserting on it would be
            // asserting that the fade does not work.
            let here = layout.position(idx, phase);
            let mut behind = here;
            for step in 1..=40 {
                let back = step as f32 / 40.0 * TRAIL_LOOKBACK * 2.0 * PI;
                let at = layout.position(idx, phase - back);
                let gap = ((at.0 - here.0).powi(2) + (at.1 - here.1).powi(2)).sqrt();
                if gap > body.body_radius_px * 1.6 {
                    behind = at;
                    break;
                }
            }
            assert_ne!(behind, here, "body {idx} barely moved");

            // Differenced against the identical frame with trails withheld, so what is measured
            // is the trail rather than whatever the sky happens to be doing there.
            let with = luminance8(&rgba, layout.width, behind.0 as i32, behind.1 as i32);
            let without = luminance8(&bare, layout.width, behind.0 as i32, behind.1 as i32);
            // **The bar came down with the trail, deliberately.** Root width times alpha went from
            // `0.595 x 0.34` to the artifact's own `0.10 x 0.22` — about a ninth of the drawn light
            // per unit of trail length — because at the old value they read as comets rather than
            // as wakes. A wake that still cleared the old margin would be one this pass failed to
            // cut. What still has to hold is that the body left something behind it at all.
            assert!(
                with > without + 1.0,
                "body {idx} left nothing behind it at {behind:?}: {with:.1} against {without:.1}"
            );
        }
        // And a trail is *short* — it is a different reading from the groove, so it must not reach
        // far enough round the orbit to become one.
        const { assert!(TRAIL_LOOKBACK < 0.15) };
        // ...and a *hairline*: the artifact's own published product of width and alpha, which it
        // tuned down three times to get to. A build that quietly widened either would pass every
        // other assertion in this test.
        const { assert!(TRAIL_WIDTH_MATE * TRAIL_ALPHA <= 0.023) };
    }

    #[test]
    fn the_debris_belt_is_on_real_orbits_and_carries_no_fleet_data() {
        // Many, tiny, dim, permanently in motion — and explicitly *not* a register: nothing here is
        // keyed to a project, a pane or an event, so it must be identical for any fleet at all.
        // **The belt is drawn into a bare buffer, not differenced out of a composed frame.** The
        // difference was exact while every mate shared one orbit at `0.34` and the belt sat inside
        // it, untouched by anything; the ladder now spans the whole frame and its rungs, grooves,
        // captions and bodies all cross the belt's band, so a difference over a composed frame
        // measures what is drawn *over* the belt as well as the belt. The layer itself takes no
        // fleet input at all — `draw_debris_belt(buf, width, height, phase)` — which is the
        // strongest available form of "carries no fleet data", and drawing it alone is how that is
        // read rather than asserted.
        let belt_ink = |phase: f32| {
            let mut buf = vec![[0.0f32; 4]; 900 * 900];
            draw_debris_belt(&mut buf, 900, 900, phase);
            pack_rgba8(&buf, false)
                .chunks_exact(4)
                .map(|p| u64::from(p[3]))
                .sum::<u64>()
        };
        // Same belt whatever the fleet is doing, because the fleet never reaches it: the same call
        // with the same frame size and phase is the same picture, and there is no third argument it
        // could have read a fleet from.
        assert_eq!(belt_ink(0.0), belt_ink(0.0));
        assert!(belt_ink(0.0) > 0, "the belt drew nothing at all");
        // It is permanently in motion, on real orbits rather than a fixed scatter...
        assert_ne!(belt_ink(0.0), belt_ink(PI * 0.5));
        // ...and it closes, like everything else that moves here.
        assert_eq!(DEBRIS_REVOLUTIONS, DEBRIS_REVOLUTIONS.round());
        // ...and it is dim: the belt is texture, not population.
        assert!(DEBRIS_ALPHA.1 < 0.2);
    }

    #[test]
    fn a_worker_between_its_mate_and_the_sun_lands_a_real_shadow_on_it() {
        // The one mechanism here that makes a worker legible by making the *planet* change, which
        // is why it works at sizes where the worker itself is a handful of pixels.
        let nodes = sky_fleet();
        let layout = build_layout(&nodes, 1_400, 900);

        // Find a phase where some worker is genuinely in transit, using the same geometry the
        // renderer uses rather than a hand-picked frame.
        let mut found = None;
        for step in 0..240 {
            let phase = step as f32 / 240.0 * 2.0 * PI;
            let sun = sun_position(&layout, phase);
            for (idx, moon) in layout.bodies.iter().enumerate() {
                let Some(parent_idx) = moon.parent else {
                    continue;
                };
                if moon.kind != BodyKind::Moon {
                    continue;
                }
                let parent = &layout.bodies[parent_idx];
                let ppos = layout.position(parent_idx, phase);
                let mpos = layout.position(idx, phase);
                let to_sun = (sun.0 - ppos.0, sun.1 - ppos.1);
                let len = (to_sun.0 * to_sun.0 + to_sun.1 * to_sun.1).sqrt();
                let (sx, sy) = (to_sun.0 / len, to_sun.1 / len);
                let (dx, dy) = (mpos.0 - ppos.0, mpos.1 - ppos.1);
                let along = dx * sx + dy * sy;
                let perp = dx * -sy + dy * sx;
                if along > 0.0 && perp.abs() < parent.body_radius_px * 0.4 {
                    found = Some((phase, parent_idx, ppos, (-sy * perp, sx * perp)));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (phase, parent_idx, ppos, offset) = found.expect("some worker transits its mate");
        let rgba = frame(&layout, phase);
        // The identical frame with transits withheld. Differencing is the only exact instrument
        // here: a transit lands on an already-shaded face, so "is this pixel dark" measures the
        // terminator, and the mirror point across the mate's centre coincides with the shadow
        // itself whenever the worker is dead on the sun line — which is exactly the case this
        // search is looking for.
        let unshadowed = frame_without(
            &layout,
            phase,
            Parts {
                transits: false,
                ..Parts::ALL
            },
        );

        let shadow = (ppos.0 + offset.0, ppos.1 + offset.1);
        let in_shadow = luminance8(&rgba, layout.width, shadow.0 as i32, shadow.1 as i32);
        let unshaded = luminance8(&unshadowed, layout.width, shadow.0 as i32, shadow.1 as i32);
        assert!(
            in_shadow < unshaded * 0.85,
            "the transit left no mark: {in_shadow:.1} against an unshadowed {unshaded:.1}"
        );
        // ...and it is a shadow rather than a hole punched in the planet.
        assert!(in_shadow > 0.5, "the shadow is opaque black");
        // The rest of the mate's face is untouched: a transit is a mark where the worker is, not a
        // dimming of the whole body.
        let far = (
            ppos.0 - offset.0 * 3.0 - layout.bodies[parent_idx].body_radius_px * 0.6,
            ppos.1 - offset.1 * 3.0,
        );
        assert_eq!(
            luminance8(&rgba, layout.width, far.0 as i32, far.1 as i32),
            luminance8(&unshadowed, layout.width, far.0 as i32, far.1 as i32),
            "the transit dimmed the whole face rather than a spot on it"
        );
    }

    #[test]
    fn a_worker_moon_is_found_without_hunting_for_it() {
        // H13, end to end. Weber |Lt - Lb| / max(Lb, Lt, 8) >= 0.55 on the *peak* pixel of the
        // moon's disc — a five-pixel body is found by its brightest pixel, not by its average —
        // against whatever is actually behind it in the composited frame, at the worst backdrop
        // each moon actually sees rather than a flattering one.
        //
        // The mechanism is four things and none of them is a glow: the body's own albedo, the
        // atmospheric rim and terminator pair (#148), the trail, and the shadow transit. The last
        // two are what this PR adds, so this is the first point at which the whole bar can be
        // asked end to end.
        let nodes = sky_fleet();
        let layout = build_layout(&nodes, 1_400, 900);

        let mut worst = f32::INFINITY;
        let mut worst_at = (0usize, 0.0f32);
        for step in 0..24 {
            let phase = step as f32 / 24.0 * 2.0 * PI;
            let rgba = frame(&layout, phase);
            let backdrop_frame = frame_without(
                &layout,
                phase,
                Parts {
                    trails: false,
                    ..Parts::ALL
                },
            );
            for (idx, body) in layout.bodies.iter().enumerate() {
                if body.kind != BodyKind::Moon {
                    continue;
                }
                let pos = layout.position(idx, phase);
                let radius = body.body_radius_px;

                // The moon's own peak, and the backdrop it actually sits on — sampled in a ring
                // just outside its disc, which is what "actually behind it" means for a body this
                // renderer never draws over anything.
                let mut peak = 0.0f32;
                let mut backdrop = 0.0f32;
                let mut backdrop_n = 0.0f32;
                let reach = (radius * 2.6).ceil() as i32;
                for dy in -reach..=reach {
                    for dx in -reach..=reach {
                        let d = ((dx * dx + dy * dy) as f32).sqrt();
                        let (x, y) = (pos.0 as i32 + dx, pos.1 as i32 + dy);
                        if d <= radius {
                            peak = peak.max(luminance8(&rgba, layout.width, x, y));
                        } else if d > radius * 1.8 && d <= radius * 2.6 {
                            backdrop += luminance8(&backdrop_frame, layout.width, x, y);
                            backdrop_n += 1.0;
                        }
                    }
                }
                if backdrop_n == 0.0 {
                    continue;
                }
                let back = backdrop / backdrop_n;
                let weber = (peak - back).abs() / peak.max(back).max(8.0);
                if weber < worst {
                    worst = weber;
                    worst_at = (idx, phase);
                }
            }
        }

        assert!(
            worst >= 0.55,
            "the hardest worker moon to find holds only Weber {worst:.3} \
             (body {}, phase {:.2}) against a bar of 0.55",
            worst_at.0,
            worst_at.1
        );
    }

    // ---------------------------------------------------------------------------
    // Orbit tracks
    // ---------------------------------------------------------------------------

    fn worn_fleet(wear: &[f32]) -> Vec<TreeNode> {
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for (i, w) in wear.iter().enumerate() {
            nodes.push(TreeNode {
                wear: *w,
                ..sized(
                    Some(0),
                    BodyKind::Planet,
                    BodySize::Files((i as u32 + 1) * 400),
                )
            });
        }
        nodes
    }

    /// Total ink in a rendered frame. Only ever compared between two fleets of the **same shape**
    /// — same bodies, same sizes, same positions — with nothing differing but the wear, so what is
    /// left in the difference is the groove. Comparing fleets of different sizes would measure
    /// body count instead, which is the mistake this comment exists to stop.
    fn frame_ink(nodes: &[TreeNode]) -> u64 {
        let layout = build_layout(nodes, 900, 900);
        frame(&layout, 0.0)
            .chunks_exact(4)
            .map(|px| u64::from(px[0]) + u64::from(px[1]) + u64::from(px[2]))
            .sum()
    }

    #[test]
    fn a_worn_orbit_draws_a_deeper_groove_than_a_fresh_one() {
        // Density is revolutions completed, measured on the rendered frame against the identical
        // fleet at a different wear — same bodies in the same places, so the difference is the
        // groove and nothing else.
        let bare = frame_ink(&worn_fleet(&[0.0]));
        let half = frame_ink(&worn_fleet(&[0.5]));
        let full = frame_ink(&worn_fleet(&[1.0]));
        assert!(
            half > bare,
            "a half-worn orbit drew no more than a bare one"
        );
        assert!(full > half, "wear stopped deepening the groove");

        // An untravelled path is not a path yet: at zero wear there is no groove at all, rather
        // than a faint ring waiting to be deepened.
        assert!(distinct_orbits(&build_layout(&worn_fleet(&[0.0]), 900, 900)).is_empty());
        assert_eq!(bare, frame_ink(&worn_fleet(&[0.0])));
    }

    /// A tier of bodies that genuinely shares one path wears **one** groove, at the deepest wear on
    /// it — not one copy per body.
    ///
    /// **The shared tier is the workers now, not the mates.** Before the ladder every second mate
    /// sat on one ring, and this test was written against that; each mate has its own rung today, so
    /// the case the rule exists for is several workers around one mate. The rule itself is unchanged
    /// and so is why it matters: five bodies each drawing the whole path composites five copies and
    /// saturates the groove long before any of them is worn, which loses exactly the reading the
    /// layer is for.
    #[test]
    fn a_shared_orbit_is_one_groove_carrying_the_deepest_wear_on_it() {
        // Same bodies both times: in one, all five workers are quarter-worn; in the other, only one
        // is and the rest are bare. If the shared orbit is drawn once at the deepest wear on it,
        // those are the same picture. If it is drawn per body, the first is four copies deeper.
        let worker_fleet = |wear: &[f32]| {
            let mut nodes = vec![
                node(None, BodyKind::Sun),
                sized(Some(0), BodyKind::Planet, BodySize::Files(900)),
            ];
            for w in wear {
                nodes.push(TreeNode {
                    wear: *w,
                    ..node(Some(1), BodyKind::Moon)
                });
            }
            nodes
        };
        let all_worn = worker_fleet(&[0.25, 0.25, 0.25, 0.25, 0.25]);
        let one_worn = worker_fleet(&[0.25, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            frame(&build_layout(&all_worn, 900, 900), 0.0),
            frame(&build_layout(&one_worn, 900, 900), 0.0),
            "the shared orbit is being composited once per body"
        );

        // ...and it carries the deepest wear on it rather than the first or the last body's.
        let layout = build_layout(&worker_fleet(&[0.25, 1.0, 0.25]), 900, 900);
        let orbits = distinct_orbits(&layout);
        assert_eq!(
            orbits.len(),
            1,
            "three workers on one ring drew {} grooves",
            orbits.len()
        );
        assert_eq!(orbits[0].2, 1.0);
    }

    #[test]
    fn a_track_belongs_to_the_body_it_is_under() {
        // A worker's orbit is around a mate that is itself moving, so its groove has to travel
        // with it — a track drawn at a fixed point would be a decoration at a radius rather than
        // that body's own path.
        let nodes = [
            node(None, BodyKind::Sun),
            TreeNode {
                wear: 1.0,
                ..sized(Some(0), BodyKind::Planet, BodySize::Files(2_000))
            },
            TreeNode {
                wear: 1.0,
                ..node(Some(1), BodyKind::Moon)
            },
        ];
        let layout = build_layout(&nodes, 900, 900);
        let moon_orbit = distinct_orbits(&layout)
            .into_iter()
            .find(|(parent, _, _, _)| *parent == 1)
            .expect("the worker's own orbit");
        for phase in [0.0f32, 1.0, 2.5] {
            let planet = layout.position(1, phase);
            let centre = layout.position(moon_orbit.0, phase);
            assert_eq!(
                centre, planet,
                "the worker's groove is not centred on its mate"
            );
        }
    }

    #[test]
    fn an_unseated_bodys_orbit_is_not_drawn() {
        // A mate the ring had no slot for is not in the picture, and neither is the path it wore.
        let mut wear = vec![1.0f32; ORBIT_LADDER_SLOTS + 4];
        wear[0] = 1.0;
        let nodes = worn_fleet(&wear);
        let layout = build_layout(&nodes, 900, 900);
        for (parent, _, _, _) in distinct_orbits(&layout) {
            assert!(layout.bodies[parent].seated);
        }
        // Each seated mate has its own rung, so the field carries at most one groove per rung —
        // never one per *mate on the roster*, which is what an unseated body wearing a path would
        // make it.
        assert!(
            distinct_orbits(&layout).len() <= ORBIT_LADDER_SLOTS,
            "{} grooves for {ORBIT_LADDER_SLOTS} rungs — an unseated body is wearing a path",
            distinct_orbits(&layout).len()
        );
    }

    // ---------------------------------------------------------------------------
    // Travel accelerates
    // ---------------------------------------------------------------------------

    #[test]
    fn a_falling_thing_speeds_up_instead_of_coasting() {
        // Both ends exact, so nothing starts or lands anywhere new.
        assert_eq!(ease_in(0.0), 0.0);
        assert_eq!(ease_in(1.0), 1.0);
        // Behind linear the whole way through — it is accelerating *into* the target, so it is
        // still short of halfway when half the time has gone.
        assert!(ease_in(0.5) < 0.5 - 0.2, "{}", ease_in(0.5));
        for i in 1..10 {
            let t = i as f32 / 10.0;
            assert!(ease_in(t) < t, "ease_in({t}) is not behind linear");
        }
        // Monotonic, and its *speed* rises: each successive tenth of the travel covers more ground
        // than the one before it, which is what constant acceleration means.
        let step = |t: f32| ease_in(t + 0.1) - ease_in(t);
        let mut previous = 0.0;
        for i in 0..9 {
            let covered = step(i as f32 / 10.0);
            assert!(
                covered > previous,
                "the {i}th tenth was not faster than the one before"
            );
            previous = covered;
        }
        // Clamped, so a caller's progress running past its own bounds cannot overshoot the target.
        assert_eq!(ease_in(1.4), 1.0);
        assert_eq!(ease_in(-0.3), 0.0);
    }

    #[test]
    fn an_asteroid_covers_less_ground_early_and_still_lands_on_its_target() {
        // A second mate rather than a worker: the approach distance scales off the struck body's
        // own radius, and on a worker moon it is only a few times the rock's own size — too close
        // together for "where a linear one would be" and "where an eased one is" to be separate
        // places on screen at all. The eased travel is the same either way; this fixture is what
        // lets the difference be *measured* rather than only computed.
        let nodes = [
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(FILES_CEIL)),
        ];
        let layout = build_layout(&nodes, 1600, 900);
        let moon = 1;
        let phase = 0.4;
        let target = layout.position(moon, phase);
        let approach = layout.bodies[moon].body_radius_px * 6.0;
        let angle = 0.7f32;
        let start = (
            target.0 + approach * angle.cos(),
            target.1 + approach * angle.sin(),
        );

        let at = |progress: f32| {
            let travel = ease_in(progress);
            (
                mix(start.0, target.0, travel),
                mix(start.1, target.1, travel),
            )
        };
        let distance = |p: (f32, f32)| ((p.0 - target.0).powi(2) + (p.1 - target.1).powi(2)).sqrt();

        // Still most of the way out at the halfway point, where a linear approach would be exactly
        // half way in.
        assert!(
            distance(at(0.5)) > approach * 0.7,
            "halfway through the flight it is already {:.0}px from a {approach:.0}px approach",
            distance(at(0.5))
        );
        // And it still starts where it started and lands where it lands — the acceleration changes
        // the pacing, never the geometry.
        assert!((distance(at(0.0)) - approach).abs() < 0.01);
        assert!(distance(at(1.0)) < 0.01);

        // Measured on the rendered buffer rather than only on the formula: the rock really is
        // further out at the halfway point than a linear one would be.
        let rendered = |progress: f32| {
            let effects = SceneEffects {
                asteroids: vec![AsteroidInFlight {
                    target: moon,
                    severity: Severity::Critical,
                    progress,
                    approach_angle: angle,
                }],
                ..Default::default()
            };
            effects_frame(&layout, &effects, phase)
        };
        let half = rendered(0.5);
        // Where a *linear* asteroid would be at half-way: the midpoint of the approach.
        let midpoint = (mix(start.0, target.0, 0.5), mix(start.1, target.1, 0.5));
        assert_eq!(
            peak_alpha(&half, layout.width, midpoint, 2.0),
            0.0,
            "the rock is sitting where a linear one would be"
        );
        assert!(
            peak_alpha(&half, layout.width, at(0.5), 4.0) > 0.5,
            "the rock is not where the eased travel puts it"
        );
    }

    #[test]
    fn an_arriving_comet_accelerates_into_the_body_it_lands_on() {
        let nodes = [
            node(None, BodyKind::Sun),
            sized(Some(0), BodyKind::Planet, BodySize::Files(900)),
        ];
        let layout = build_layout(&nodes, 800, 600);
        let phase = 0.3;
        let comet = Comet {
            start: (0.05, 0.05),
            end: (0.5, 0.5),
            target: Some(1),
            magnitude: 1.0,
            tier: WinTier::Ask,
            trail: std::sync::Arc::new(Vec::new()),
            progress: 0.5,
        };
        let target = layout.position(1, phase);
        let start_px = (comet.start.0 * 800.0, comet.start.1 * 600.0);
        let travelled =
            |p: (f32, f32)| ((p.0 - start_px.0).powi(2) + (p.1 - start_px.1).powi(2)).sqrt();

        let rgba = effects_frame(
            &layout,
            &SceneEffects {
                comets: vec![comet],
                ..Default::default()
            },
            phase,
        );
        // The comet's core is the brightest thing it draws, so the furthest-along lit pixel along
        // its own line is where its head is. Read off the buffer rather than recomputed.
        let mut head = start_px;
        for step in 0..=200 {
            let t = step as f32 / 200.0;
            let p = (mix(start_px.0, target.0, t), mix(start_px.1, target.1, t));
            if peak_alpha(&rgba, layout.width, p, 1.5) > 0.5 {
                head = p;
            }
        }
        let whole = travelled(target);
        assert!(
            travelled(head) < whole * 0.45,
            "halfway through its flight the comet has covered {:.0}px of {whole:.0}px",
            travelled(head)
        );
        assert!(travelled(head) > 0.0, "the comet has not moved at all");
    }

    // ---------------------------------------------------------------------------
    // What sits over the scene, and where
    // ---------------------------------------------------------------------------

    /// A corner-sized surface filled with one opaque colour, for the sampler to composite.
    fn opaque_corner(width: u32, height: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for px in rgba.chunks_exact_mut(4) {
            px[0] = rgb.0;
            px[1] = rgb.1;
            px[2] = rgb.2;
            px[3] = 255;
        }
        rgba
    }

    #[test]
    fn a_surface_over_the_scene_is_sampled_where_it_is_and_nowhere_else() {
        // The whole of it: a third surface placed over the same cells has to reach the legibility
        // decision for the cells it covers — text sitting on a lit groove read against the void
        // behind it commits the wrong foreground, and it does so exactly where a readout somebody
        // is looking at happens to be — while every cell outside its box samples precisely what it
        // sampled before. A full-width band across those rows, which is the obvious
        // implementation, fails the second half.
        let (cols, rows) = (8u32, 4u32);
        let (cell_w, cell_h) = (4u32, 4u32);
        let (width, height) = (cols * cell_w, rows * cell_h);
        let layout = build_layout(&[], width, height);
        let ambient = frame(&layout, 0.0);
        let effects = effects_frame(&layout, &SceneEffects::default(), 0.0);

        let without = sample_cell_backgrounds(
            &ambient, &effects, None, width, height, cell_w, cell_h, cols, rows,
        );

        // Two cells wide, one tall, at the top right — the corner's own shape in miniature.
        let (corner_cols, corner_rows) = (2u32, 1u32);
        let corner_rgba = opaque_corner(corner_cols * cell_w, corner_rows * cell_h, (255, 0, 0));
        let with = sample_cell_backgrounds(
            &ambient,
            &effects,
            Some(CornerLayer {
                rgba: &corner_rgba,
                width: corner_cols * cell_w,
                height: corner_rows * cell_h,
                col: cols - corner_cols,
                row: 0,
            }),
            width,
            height,
            cell_w,
            cell_h,
            cols,
            rows,
        );

        for row in 0..rows {
            for col in 0..cols {
                let idx = (row * cols + col) as usize;
                let covered = row < corner_rows && col >= cols - corner_cols;
                if covered {
                    assert_eq!(
                        with[idx],
                        (255, 0, 0),
                        "cell ({col}, {row}) is under the surface and did not sample it"
                    );
                } else {
                    assert_eq!(
                        with[idx], without[idx],
                        "cell ({col}, {row}) is outside the surface's box and moved anyway"
                    );
                }
            }
        }
    }

    #[test]
    fn a_transparent_part_of_that_surface_shows_the_scene_through_it() {
        // The corner is mostly transparent — it is a few grooves and a row of small bodies on
        // nothing — so a cell it nominally covers is usually still reading the sky. Compositing it
        // as an opaque box would darken text decisions across its whole rectangle for no reason.
        let (cols, rows) = (4u32, 2u32);
        let (cell_w, cell_h) = (4u32, 4u32);
        let (width, height) = (cols * cell_w, rows * cell_h);
        let layout = build_layout(&[node(None, BodyKind::Sun)], width, height);
        let ambient = frame(&layout, 0.0);
        let effects = effects_frame(&layout, &SceneEffects::default(), 0.0);

        let without = sample_cell_backgrounds(
            &ambient, &effects, None, width, height, cell_w, cell_h, cols, rows,
        );
        // Fully transparent: alpha zero everywhere.
        let clear = vec![0u8; (width * height * 4) as usize];
        let with = sample_cell_backgrounds(
            &ambient,
            &effects,
            Some(CornerLayer {
                rgba: &clear,
                width,
                height,
                col: 0,
                row: 0,
            }),
            width,
            height,
            cell_w,
            cell_h,
            cols,
            rows,
        );
        assert_eq!(
            with, without,
            "a fully transparent overlay changed what is behind it"
        );
    }

    // ---------------------------------------------------------------------------
    // The machine corner
    // ---------------------------------------------------------------------------

    fn corner_of(grooves: &[&[f32]], cores: &[Option<f32>]) -> MachineCorner {
        MachineCorner {
            grooves: grooves.iter().map(|g| g.to_vec()).collect(),
            cores: cores.to_vec(),
        }
    }

    /// How many pixels of the corner carry anything at all.
    fn drawn_pixels(rgba: &[u8]) -> usize {
        rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
    }

    #[test]
    fn an_unmeasured_machine_draws_nothing_at_all() {
        // F21, the whole of it: no fabricated machine number, ever, and no decorative history.
        // Not an idle trace, not a flat groove at zero, not a row of dark cores — nothing. A
        // plausible number invented from nothing is worse than an empty corner.
        let empty = MachineCorner::default();
        assert!(empty.is_empty());
        assert_eq!(drawn_pixels(&machine_corner_frame(&empty, 260, 168)), 0);

        // A register that has cores but has measured none of them is the same case, and it is the
        // one a "draw what you have" implementation gets wrong: the cores exist, so a naive draw
        // puts twelve bodies on screen claiming twelve idle CPUs.
        let all_absent = corner_of(&[&[], &[], &[], &[]], &[None; 12]);
        assert!(all_absent.is_empty());
        assert_eq!(
            drawn_pixels(&machine_corner_frame(&all_absent, 260, 168)),
            0
        );
    }

    #[test]
    fn a_quantity_with_no_history_draws_no_groove_rather_than_a_flat_one() {
        // An unworn track and an unmeasured one are different statements about a machine, and only
        // one of them is true. Drawing a groove at zero says "nothing has happened here", which is
        // a claim; drawing nothing says "this was not measured", which is the truth.
        let measured = corner_of(&[&[0.5; 60], &[0.5; 60], &[0.5; 60], &[0.5; 60]], &[]);
        let one_missing = corner_of(&[&[0.5; 60], &[], &[0.5; 60], &[0.5; 60]], &[]);
        let all_missing = corner_of(&[&[], &[], &[], &[]], &[Some(0.5)]);

        let four = drawn_pixels(&machine_corner_frame(&measured, 260, 168));
        let three = drawn_pixels(&machine_corner_frame(&one_missing, 260, 168));
        assert!(three < four, "a missing quantity still drew a groove");
        // ...and it is a whole groove's worth that went, not a rounding difference.
        assert!(four - three > 200, "only {} pixels went", four - three);

        // A corner with no history at all but one live core still draws the core: the rules are
        // per quantity, not per corner.
        assert!(drawn_pixels(&machine_corner_frame(&all_missing, 260, 168)) > 0);
    }

    #[test]
    fn a_busier_quantity_wears_its_groove_harder() {
        // A46(c): the wear at each point is the value measured at that point. That is the whole
        // encoding — no y axis, no line rising and falling, because a sparkline would be a second
        // way of encoding a magnitude into a frame that already has one.
        let wear = |value: f32| {
            let corner = corner_of(&[&vec![value; 60]], &[]);
            let rgba = machine_corner_frame(&corner, 260, 60);
            // Total ink, which is thickness and brightness together — both are the encoding.
            rgba.chunks_exact(4).map(|px| u32::from(px[3])).sum::<u32>()
        };
        let quiet = wear(0.05);
        let middling = wear(0.5);
        let hammered = wear(1.0);
        assert!(
            quiet < middling && middling < hammered,
            "wear is not monotonic in the value: {quiet} / {middling} / {hammered}"
        );
        // A busy machine has to be *obviously* different from a quiet one, not measurably so.
        assert!(
            hammered > quiet * 3,
            "a fully loaded groove ({hammered}) barely outdraws an idle one ({quiet})"
        );
    }

    #[test]
    fn a_short_history_draws_short_rather_than_stretched() {
        // A corner that has been watching for ten seconds must not look like one that has been
        // watching for two minutes. Stretching a partial history to fill the groove is the
        // specific lie: it claims a past the register does not have.
        let young = corner_of(&[&[1.0; 6]], &[]);
        let old = corner_of(&[&[1.0; HISTORY_GROOVE_SAMPLES]], &[]);
        let young_ink = drawn_pixels(&machine_corner_frame(&young, 600, 60));
        let old_ink = drawn_pixels(&machine_corner_frame(&old, 600, 60));
        assert!(
            (young_ink as f32) < old_ink as f32 * 0.3,
            "six samples drew {young_ink} against a full history's {old_ink}"
        );
        assert!(young_ink > 0, "six samples drew nothing");
    }

    #[test]
    fn a_busier_core_is_a_heavier_body_and_an_absent_one_is_not_drawn() {
        // H12's hard case: twelve small bodies are exactly where an average would be invisible.
        // A core is sized on the same cube root every planet obeys, and a core that reported
        // nothing is drawn *absent* rather than at zero — which is the distinction that makes the
        // row worth having.
        let ink = |cores: &[Option<f32>]| {
            drawn_pixels(&machine_corner_frame(&corner_of(&[], cores), 260, 60))
        };
        let idle = ink(&[Some(0.0); 4]);
        let busy = ink(&[Some(1.0); 4]);
        assert!(
            busy > idle,
            "a busy core is not a heavier body: {busy} vs {idle}"
        );
        // An idle core is still a core: a body that draws at nothing is indistinguishable from a
        // core that reported nothing, and those are different machine states.
        assert!(idle > 0, "an idle core drew nothing at all");

        // Absent is absent, and its slot is still spent — the row's positions are the OS's core
        // order, so closing a gap would silently renumber every core after it.
        let one_absent = ink(&[Some(1.0), None, Some(1.0), Some(1.0)]);
        assert!(
            one_absent < busy,
            "an absent core still drew a body: {one_absent} vs {busy}"
        );
        let mixed = machine_corner_frame(&corner_of(&[], &[Some(1.0), None, Some(1.0)]), 260, 60);
        let shifted = machine_corner_frame(&corner_of(&[], &[Some(1.0), Some(1.0)]), 260, 60);
        assert_ne!(
            mixed, shifted,
            "an absent core's slot was closed up, renumbering every core after it"
        );
    }

    #[test]
    fn the_corner_fits_however_many_cores_the_host_has() {
        // A corner box is a couple of dozen cells across, and a 64-core host has to fit the same
        // width a 4-core one does. Nothing may be drawn outside the surface it was given.
        for count in [1usize, 4, 12, 32, 64, 256] {
            let cores: Vec<Option<f32>> =
                (0..count).map(|i| Some(i as f32 / count as f32)).collect();
            let corner = corner_of(&[&[0.5; 60], &[0.5; 60], &[0.5; 60], &[0.5; 60]], &cores);
            let rgba = machine_corner_frame(&corner, 260, 168);
            assert_eq!(
                rgba.len(),
                260 * 168 * 4,
                "{count} cores changed the surface size"
            );
            assert!(drawn_pixels(&rgba) > 0, "{count} cores drew nothing");
        }
    }

    #[test]
    fn the_corner_is_a_transparent_overlay_not_a_second_backdrop() {
        // It is placed over the scene, so everywhere it does not draw has to show the sky through
        // it. An opaque corner box would punch a rectangle out of the picture.
        let corner = corner_of(&[&[1.0; 60]], &[Some(1.0)]);
        let rgba = machine_corner_frame(&corner, 260, 168);
        let transparent = rgba.chunks_exact(4).filter(|px| px[3] == 0).count();
        assert!(
            transparent > 260 * 168 / 2,
            "the corner is opaque over {} of its own box",
            260 * 168 - transparent
        );
    }

    #[test]
    fn the_corner_introduces_no_material_the_sky_does_not_already_have() {
        // A46(c): a groove is the orbit track's own colour and a core is a body drawn by the same
        // shader every planet uses. Held structurally rather than by eye — the corner cannot drift
        // into its own palette without this failing.
        assert_eq!(TRACK_RGB01, (43.0 / 255.0, 109.0 / 255.0, 132.0 / 255.0));

        // ...and what keeps a core from reading as a worker moon that wandered into the corner is
        // its colour rather than its size: the substrate sits outside the lifecycle hue channel
        // every fleet body resolves through. Held against the real channel, at both ends of it,
        // rather than against a remembered pair of numbers.
        let cool = |rgb: (f32, f32, f32)| rgb.2 > rgb.0;
        assert!(cool(CORE_RGB01), "a core has drifted warm");
        assert!(cool(TRACK_RGB01), "the track has drifted warm");
        for hue in [IDLE_HUE, FAILED_HUE, 41.0] {
            for severity in [Severity::Clear, Severity::Critical] {
                assert!(
                    !cool(severity_rgb01(hue, severity)),
                    "a fleet body at hue {hue} reads as cool as the substrate does"
                );
            }
        }
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

        // **Differenced against the same frame with the mark withheld**, not against a smaller
        // fleet's frame. The mark used to live outside every orbit — every mate shared one ring at
        // `0.34` and the mark sat at `0.44` — so a radius band cleanly separated it. The ladder now
        // reaches past it in both directions, and two fleets of different sizes differ everywhere.
        // Withholding the one layer is exact where a band no longer is.
        let mark_pixels = |layout: &SceneLayout| {
            let with = frame(layout, 0.0);
            let without = frame_without(
                layout,
                0.0,
                Parts {
                    overflow: false,
                    ..Parts::ALL
                },
            );
            with.chunks_exact(4)
                .zip(without.chunks_exact(4))
                .filter(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) > 1))
                .count()
        };
        let marked = mark_pixels(&overflowing);
        assert!(
            marked > 0,
            "a fleet with {} mates beyond the ring marked nothing at all",
            overflowing.ladder_occupancy().1
        );
        // ...and the mark is small: it is a disclosure, not a second fleet. Well under the area
        // one seated mate's own disk covers.
        assert!(
            marked < 2_000,
            "the overflow mark is drawing {marked} pixels — that is a body, not a mark"
        );
        // A fleet that fits marks nothing at all, which is the "absent at zero" half.
        let when_it_fits = mark_pixels(&fits);
        assert_eq!(
            when_it_fits, 0,
            "a fleet the ring seats whole still drew a disclosure"
        );

        // **And it names its key.** A41(c)/A42(e): a countable fan of dots is an exact count of an
        // unstated quantity — a viewer can see that nine are missing and not which nine, or why. The
        // sentence is `N of M mates dropped - smallest by files at HEAD`, so the disclosure spans a
        // horizontal run far wider than the fan's own arc, which is what tells the two apart without
        // reading pixels back as glyphs.
        let with = frame(&overflowing, 0.0);
        let without = frame_without(
            &overflowing,
            0.0,
            Parts {
                overflow: false,
                ..Parts::ALL
            },
        );
        let columns: Vec<usize> = with
            .chunks_exact(4)
            .zip(without.chunks_exact(4))
            .enumerate()
            .filter(|(_, (a, b))| (0..3).any(|c| a[c].abs_diff(b[c]) > 1))
            .map(|(i, _)| i % 900)
            .collect();
        let span = columns.iter().max().unwrap_or(&0) - columns.iter().min().unwrap_or(&0);
        let fan_arc = (OVERFLOW_ORBIT_FRACTION * 900.0 * OVERFLOW_FAN) as usize;
        assert!(
            span > fan_arc + 120,
            "the disclosure spans {span}px against a fan of {fan_arc}px — it is dots and no key"
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
                tier: WinTier::Ask,
                trail: std::sync::Arc::new(Vec::new()),
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
            layout.drawn_radius_px(MATE, phase),
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
        // Kept clear of the disc's own antialiased limb. The body composites at `aa.max(0.85)`, so
        // the outermost pixel of the disc lets ~15% of whatever is under it through — including the
        // back arc — and against a void that is now genuinely black that residue is a visible
        // difference where it used to round away. The box corners used to graze that band; they no
        // longer do, and the region that actually answers the question (the ring's near and far
        // crossings of the body's own vertical, at ~0.48 of its radius) is still inside it.
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
                {
                    let scene = one_mate_scene(BodyType::Ringed, streak);
                    scene.bodies[MATE]
                        .ring_radii_px(scene.drawn_radius_px(MATE, phase))
                        .map(|(_, outer)| outer)
                        .unwrap_or(0.0)
                },
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
            spin: 0.0,
            ring: None,
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
