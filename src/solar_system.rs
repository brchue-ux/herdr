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
/// resolved per-node from lifecycle/severity and [`Self::base_radius_fraction`] — because two
/// bodies of the same kind can be in wildly different states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyKind {
    Sun,
    Planet,
    Moon,
}

impl BodyKind {
    /// Body pixel radius, as a fraction of `min(width, height)`.
    fn base_radius_fraction(self) -> f32 {
        match self {
            Self::Sun => 0.050,
            Self::Planet => 0.020,
            Self::Moon => 0.009,
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

    /// Full orbits completed per animation loop. Planets complete exactly one; moons complete
    /// several more so the loop reads as motion rather than a slow crawl, while both land back on
    /// their starting angle after [`FRAME_COUNT`] samples — the same seamless-loop contract
    /// `src/particle_field.rs::loop_frames` already relies on.
    fn revolutions_per_loop(self) -> f32 {
        match self {
            Self::Sun => 0.0,
            Self::Planet => 1.0,
            Self::Moon => 4.0,
        }
    }
}

/// One node of the fleet's owner tree, exactly as `src/app/background_scene.rs` derived it from
/// `crate::ui::sidebar::workspace_list_entries_whole_fleet` — this module knows nothing about
/// panes, workspaces or tokens, only shape and two already-resolved colour facts.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TreeNode {
    /// Index into the same slice, or `None` for a root (the sun tier).
    pub(crate) parent: Option<usize>,
    pub(crate) kind: BodyKind,
    /// Lifecycle-stage hue in degrees, from `crate::app::lifecycle::stage(...).hue(...)`.
    pub(crate) hue: f32,
    pub(crate) severity: Severity,
}

/// One body's static placement facts, resolved once per topology change (mirrors
/// `App::observe_sidebar_particle_field`'s "regenerate on resize, not per tick" cadence — a body
/// added or removed is the equivalent event here).
#[derive(Debug, Clone, Copy)]
struct BodyLayout {
    parent: Option<usize>,
    kind: BodyKind,
    hue: f32,
    severity: Severity,
    /// Angle this body sits at within its parent's ring at `phase == 0`.
    base_angle: f32,
    orbit_radius_px: f32,
    body_radius_px: f32,
}

/// Every body's static placement for one frame size, ready to be evaluated at any phase.
#[derive(Debug, Clone)]
pub(crate) struct SceneLayout {
    bodies: Vec<BodyLayout>,
    width: u32,
    height: u32,
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

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (idx, node) in nodes.iter().enumerate() {
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

        bodies.push(BodyLayout {
            parent: node.parent,
            kind: node.kind,
            hue: node.hue,
            severity: node.severity,
            base_angle,
            orbit_radius_px: node.kind.orbit_radius_fraction() * scale * nesting,
            body_radius_px: node.kind.base_radius_fraction() * scale * nesting.max(0.35),
        });
    }

    SceneLayout {
        bodies,
        width,
        height,
    }
}

impl SceneLayout {
    pub(crate) fn is_empty(&self) -> bool {
        self.bodies.is_empty()
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
        let angle = body.base_angle + phase * body.kind.revolutions_per_loop();
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

/// Lambertian shading with limb darkening and a mottled texture, for one pixel `(dx, dy)` offset
/// from a body's centre, `dist` away, inside a body of `radius` pixels.
///
/// `light_dir` is normalised and points *from the surface toward the light* (i.e. toward the
/// sun's on-screen position for a planet/moon; straight out of the screen for the sun itself,
/// which is self-luminous rather than lit).
fn shade_surface(
    dx: f32,
    dy: f32,
    radius: f32,
    light_dir: (f32, f32, f32),
    base: (f32, f32, f32),
    seed: u32,
    self_luminous: bool,
) -> (f32, f32, f32) {
    let nx = dx / radius;
    let ny = dy / radius;
    let nz = (1.0 - (nx * nx + ny * ny)).max(0.0).sqrt();

    let texture = value_noise(nx * 4.0 + 7.0, ny * 4.0 + 3.0, seed) * 0.5
        + value_noise(nx * 11.0, ny * 11.0, seed.wrapping_add(97)) * 0.5;
    let mottle = mix(0.86, 1.0, texture);

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

    let diffuse = (nx * light_dir.0 + ny * light_dir.1 + nz * light_dir.2).max(0.0);
    let ambient = 0.10;
    let lit = ambient + diffuse * (1.0 - ambient);
    let limb_darkening = mix(0.55, 1.0, nz);
    let lit = lit * limb_darkening * mottle;

    (
        clamp01(base.0 * lit),
        clamp01(base.1 * lit),
        clamp01(base.2 * lit),
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

/// Draw one body (disk, shading, and soft glow fringe) into `buf`, restricted to its own
/// bounding box rather than the whole frame — the cost lever that keeps a handful of shaded
/// spheres cheap even at 1440p, unlike `src/particle_field.rs`'s necessarily full-frame passes.
#[allow(clippy::too_many_arguments)]
fn draw_body(
    buf: &mut [[f32; 4]],
    width: u32,
    height: u32,
    center: (f32, f32),
    radius: f32,
    hue: f32,
    severity: Severity,
    seed: u32,
    self_luminous: bool,
    light_dir: (f32, f32, f32),
) {
    let glow = radius * if self_luminous { 2.6 } else { 1.4 };
    // The sun is a star, not a severity-coded body: it holds one fixed warm colour regardless of
    // fleet state, while every other body still resolves through the shared hue/severity channel.
    // See [`SUN_STAR_RGB01`].
    let base = if self_luminous {
        SUN_STAR_RGB01
    } else {
        severity_rgb01(hue, severity)
    };

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
                let color = shade_surface(dx, dy, radius, light_dir, base, seed, self_luminous);
                blend(&mut buf[idx], color, aa.max(0.85));
            } else if dist <= glow {
                let t = 1.0 - (dist - radius) / (glow - radius).max(0.001);
                let alpha = t * t * if self_luminous { 0.55 } else { 0.22 };
                blend(&mut buf[idx], base, alpha);
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
            let edge = clamp01(1.0 - dist / patch_radius);
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
    let (width, height) = (layout.width, layout.height);
    let pixels = width as usize * height as usize;
    let mut buf = vec![[0.0f32; 4]; pixels];

    if width > 0 && height > 0 {
        render_bands(&mut buf, width, height, phase);
    }

    let sun_pos = sun_position(layout, phase);
    for (idx, body) in layout.bodies.iter().enumerate() {
        let pos = layout.position(idx, phase);
        draw_body(
            &mut buf,
            width,
            height,
            pos,
            body.body_radius_px,
            body.hue,
            body.severity,
            body_seed(idx),
            body.kind == BodyKind::Sun,
            normalize3(light_dir_toward(sun_pos, pos, body.kind)),
        );
    }

    pack_rgba8(&buf, true)
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
        let Some(body) = layout.bodies.get(crater.body) else {
            continue;
        };
        let pos = layout.position(crater.body, phase);
        draw_crater(&mut buf, width, height, crater, body, pos);
        if let Some(parent_idx) = body.parent {
            if let Some(parent) = layout.bodies.get(parent_idx) {
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
        let Some(body) = layout.bodies.get(ejecta.body) else {
            continue;
        };
        let pos = layout.position(ejecta.body, phase);
        draw_ejecta(&mut buf, width, height, ejecta, body, pos);
    }

    for asteroid in &effects.asteroids {
        if let Some(target) = layout.bodies.get(asteroid.target) {
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
            .filter(|idx| *idx < layout.bodies.len())
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
        let seed = i as u32;
        let sx = value_noise(seed as f32 * 0.123, 0.0, 11);
        let sy = value_noise(seed as f32 * 0.123, 100.0, 11);
        let py = (sy * height as f32) as usize;
        if py < y0 || py >= y1 {
            continue;
        }
        let brightness = 0.25 + 0.75 * value_noise(seed as f32 * 0.123, 200.0, 11);
        let twinkle_offset = value_noise(seed as f32 * 0.123, 300.0, 11) * 2.0 * PI;
        let twinkle = 0.75 + 0.25 * (phase + twinkle_offset).sin();
        let px = (sx * width as f32) as usize;
        if px >= width as usize {
            continue;
        }
        let local_idx = (py - y0) * width as usize + px;
        let value = brightness * twinkle;
        buf[local_idx] = [value, value, value, 1.0];
    }
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
        }
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

        let on_moon = peak_alpha(
            &rgba,
            layout.width,
            layout.position(moon, phase),
            layout.bodies[moon].body_radius_px * 1.2,
        );
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
        assert!(
            on_planet < on_moon,
            "the ripple ({on_planet:.2}) is not fainter than the strike itself ({on_moon:.2})"
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

    fn representative_fleet() -> Vec<TreeNode> {
        let mut nodes = vec![node(None, BodyKind::Sun)];
        for planet in 0..4 {
            nodes.push(node(Some(0), BodyKind::Planet));
            let planet_idx = nodes.len() - 1;
            for _ in 0..3 {
                nodes.push(node(Some(planet_idx), BodyKind::Moon));
            }
            let _ = planet;
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
    }
}
