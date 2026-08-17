//! Per-cell adaptive text-foreground legibility against the persistent whole-terminal
//! solar-system background (`src/solar_system.rs`, `src/app/background_scene.rs`).
//!
//! Captain-picked technique (`data/decisions/2026-08-07-text-legibility-technique-choice.md`,
//! firstmate home): adapt each cell's own foreground colour toward black or white from the
//! background pixels sampled under it, falling back to an opaque scrim only on the rare cell
//! where neither clears the contrast floor. The flicker failure mode this design exists to avoid
//! — a moving bright/dark object under static text flipping the foreground every frame — is
//! addressed by three things, all specified by
//! `data/herdr-text-legibility-prior-art/report.md` section 3 (firstmate home) and composed here
//! exactly as that report lays out:
//!
//! - the sampling cadence is decoupled from the effects layer's ~16ms regeneration cadence
//!   ([`SAMPLE_INTERVAL`], an order of magnitude coarser);
//! - the sampled colour is EMA-smoothed before it ever reaches a contrast decision
//!   ([`EMA_ALPHA`]);
//! - the black/white *target* a cell is corrected toward is "committed": once picked, it only
//!   flips when the smoothed sample clears a hysteresis margin *and* a minimum dwell time has
//!   passed since the last flip ([`HYSTERESIS_MARGIN`], [`MIN_DWELL`]).
//!
//! `crate::ui::color::ensure_contrast`/`relative_luminance`/`contrast_ratio` are reused
//! unmodified — this module only decides *which* black/white target to feed
//! `crate::ui::color::ensure_contrast_toward`, never re-derives the WCAG maths itself. The
//! candidate-#2 scrim fallback is likewise keyed off this same committed decision
//! ([`CellLegibilityState::needs_scrim`]), not a separate raw per-frame check, so the fallback
//! cannot flap on its own uncontrolled cadence even when the adaptive-colour decision is stable.

use std::time::{Duration, Instant};

use crate::solar_system::{self, SceneEffects, SceneLayout};
use crate::ui::color::{contrast_ratio, ensure_contrast, ensure_contrast_toward, mix_rgb};
use crate::ui::color::{Rgb, BLACK, WHITE};

/// How often the background is resampled and the smoothed/committed decision updated — far
/// coarser than the effects layer's own ~16ms regeneration cadence
/// (`App::observe_background_effects`), since nothing needs the legibility decision to react
/// sharper than roughly this often to read as smooth. Deliberately close to
/// `background_scene::FRAME_GAP_MS` (140ms): the legibility layer never claims sharper time
/// resolution than the scene it's reading is actually presenting.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// EMA weight given to each new sample, `0.0..=1.0`. Small enough that one comet's
/// `background_scene`'s `COMET_FLIGHT` (2200ms, roughly 11 samples at [`SAMPLE_INTERVAL`]) only
/// partially moves the smoothed value — the transient's *magnitude* is damped before it ever
/// reaches the hysteresis decision below, independent of that decision's own damping.
const EMA_ALPHA: f32 = 0.25;

/// How far the other target's contrast ratio has to exceed the committed target's before a flip
/// is even considered — a Schmitt-trigger dead-band around the plain crossover
/// `crate::ui::color::ensure_contrast` uses, so a smoothed value hovering near the crossover does
/// not still flip on every sample.
const HYSTERESIS_MARGIN: f32 = 1.0;

/// Minimum time between two flips of the same cell's committed target, regardless of how far the
/// hysteresis band has been cleared — the hard backstop on worst-case flip count per comet
/// transit.
const MIN_DWELL: Duration = Duration::from_millis(600);

/// The WCAG contrast ratio every committed target is held to. A cell that cannot clear this even
/// at a full black/white commit falls back to [`CellLegibilityState::needs_scrim`].
const CONTRAST_FLOOR: f32 = 4.5;

/// Opaque backdrop for the rare cell whose sampled background is genuinely mid-grey: near-black,
/// close to `src/solar_system.rs`'s own deep-space canvas colour, so a scrimmed cell reads as
/// "part of the scene's shadow" rather than a jarring UI patch.
const SCRIM_BG: Rgb = (8, 9, 14);

/// One cell's remembered legibility decision: the EMA-smoothed background colour, the
/// black/white target currently committed to, when that target last flipped, and whether even
/// that commit fails the contrast floor (the candidate-#2 scrim fallback).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CellLegibilityState {
    smoothed_bg: Rgb,
    committed_target: Rgb,
    last_flip_at: Instant,
    needs_scrim: bool,
}

impl CellLegibilityState {
    fn bootstrap(sampled: Rgb, now: Instant) -> Self {
        let committed_target = if contrast_ratio(WHITE, sampled) >= contrast_ratio(BLACK, sampled) {
            WHITE
        } else {
            BLACK
        };
        let needs_scrim = contrast_ratio(committed_target, sampled) < CONTRAST_FLOOR;
        Self {
            smoothed_bg: sampled,
            committed_target,
            last_flip_at: now,
            needs_scrim,
        }
    }

    fn update(&mut self, sampled: Rgb, now: Instant) {
        self.smoothed_bg = mix_rgb(self.smoothed_bg, sampled, EMA_ALPHA);

        let contrast_white = contrast_ratio(WHITE, self.smoothed_bg);
        let contrast_black = contrast_ratio(BLACK, self.smoothed_bg);
        let dwell_elapsed = now.saturating_duration_since(self.last_flip_at) >= MIN_DWELL;

        if dwell_elapsed {
            let flip_to = if self.committed_target == WHITE {
                (contrast_black > contrast_white + HYSTERESIS_MARGIN).then_some(BLACK)
            } else {
                (contrast_white > contrast_black + HYSTERESIS_MARGIN).then_some(WHITE)
            };
            if let Some(target) = flip_to {
                self.committed_target = target;
                self.last_flip_at = now;
            }
        }

        self.needs_scrim = contrast_ratio(self.committed_target, self.smoothed_bg) < CONTRAST_FLOOR;
    }

    /// The colour to render `fg` as against this cell's sampled background, and — only on the
    /// rare cell where even the committed target fails [`CONTRAST_FLOOR`] — the opaque scrim
    /// colour to paint behind it instead of leaving the cell's background transparent.
    ///
    /// The scrim path measures contrast against the fixed [`SCRIM_BG`] rather than the sampled
    /// background: a fixed, never-resampled background carries no flicker risk, so plain
    /// `ensure_contrast` (not `_toward`) is the right primitive there, same as everywhere else in
    /// this codebase that floors against a static colour.
    pub(crate) fn render(&self, fg: Rgb) -> (Rgb, Option<Rgb>) {
        if self.needs_scrim {
            (
                ensure_contrast(fg, SCRIM_BG, CONTRAST_FLOOR),
                Some(SCRIM_BG),
            )
        } else {
            (
                ensure_contrast_toward(fg, self.smoothed_bg, self.committed_target, CONTRAST_FLOOR),
                None,
            )
        }
    }
}

/// One legibility decision per visible terminal cell, sized to the background scene's own
/// screen-rect grid. Presentation state, safe to forget wholesale whenever nobody is watching or
/// the scene resizes — mirrors `crate::app::background_scene::BackgroundEffectsState` in spirit.
#[derive(Debug, Clone)]
pub(crate) struct LegibilityGrid {
    cols: u32,
    rows: u32,
    cells: Vec<CellLegibilityState>,
    last_sampled_at: Option<Instant>,
}

impl LegibilityGrid {
    fn bootstrap(cols: u32, rows: u32, sampled: &[Rgb], now: Instant) -> Self {
        Self {
            cols,
            rows,
            cells: sampled
                .iter()
                .map(|&rgb| CellLegibilityState::bootstrap(rgb, now))
                .collect(),
            last_sampled_at: Some(now),
        }
    }

    /// This cell's current legibility decision, or `None` if `(row, col)` is outside the grid —
    /// a resize the sampler has not caught up with yet, or a client whose own area is larger than
    /// the background scene's. Callers should leave such a cell untouched rather than guess.
    pub(crate) fn cell(&self, row: u16, col: u16) -> Option<&CellLegibilityState> {
        let (row, col) = (u32::from(row), u32::from(col));
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells.get((row * self.cols + col) as usize)
    }
}

/// (Re-)sample the background under every cell and advance each cell's smoothed/committed
/// legibility decision, if [`SAMPLE_INTERVAL`] has elapsed since the last sample, the grid does
/// not yet exist, or the scene's own cell grid has resized.
///
/// Called from `App::observe_background_effects` once per tick — the same site that already
/// computes `layout`, `phase`, and `effects` for the transient overlay each pass, rather than
/// introducing a second call site. This is exactly `report.md` section 3c's "natural call site
/// for 'resample this tick,' gated per 3c": this function gates its own heavier work internally
/// instead of the once-per-tick caller doing so. Returns whether a fresh sample actually ran, so
/// the caller's own `changed` bookkeeping schedules a repaint that reflects the new colours.
///
/// `corner` is the machine register's own readout, which is a *third* surface placed over the same
/// cells (`crate::machine_register`). It has to be composited in here or the decision for the
/// cells it covers is made against a background that is not what is behind them — and it is
/// exactly the cells carrying a readout somebody is reading that would be wrong. Only its own box
/// is affected: every cell outside it samples precisely what it sampled before.
pub(crate) fn observe(
    grid: &mut Option<LegibilityGrid>,
    layout: &SceneLayout,
    phase: f32,
    effects: &SceneEffects,
    corner: Option<solar_system::CornerLayer<'_>>,
    cell_width_px: u32,
    cell_height_px: u32,
    now: Instant,
) -> bool {
    if cell_width_px == 0 || cell_height_px == 0 {
        return grid.take().is_some();
    }

    let cols = layout.width() / cell_width_px;
    let rows = layout.height() / cell_height_px;
    if cols == 0 || rows == 0 {
        return grid.take().is_some();
    }

    let stale = match grid {
        Some(existing) if existing.cols == cols && existing.rows == rows => existing
            .last_sampled_at
            .is_none_or(|last| now.saturating_duration_since(last) >= SAMPLE_INTERVAL),
        _ => true,
    };
    if !stale {
        return false;
    }

    let ambient = solar_system::frame(layout, phase);
    let effects_rgba = solar_system::effects_frame(layout, effects, phase);
    let sampled = solar_system::sample_cell_backgrounds(
        &ambient,
        &effects_rgba,
        corner,
        layout.width(),
        layout.height(),
        cell_width_px,
        cell_height_px,
        cols,
        rows,
    );

    match grid {
        Some(existing) if existing.cols == cols && existing.rows == rows => {
            for (cell, &sample) in existing.cells.iter_mut().zip(sampled.iter()) {
                cell.update(sample, now);
            }
            existing.last_sampled_at = Some(now);
        }
        _ => *grid = Some(LegibilityGrid::bootstrap(cols, rows, &sampled, now)),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_cell_commits_to_whichever_target_has_more_headroom() {
        let bright = CellLegibilityState::bootstrap((240, 240, 240), Instant::now());
        assert_eq!(bright.committed_target, BLACK);
        let dark = CellLegibilityState::bootstrap((10, 10, 10), Instant::now());
        assert_eq!(dark.committed_target, WHITE);
    }

    #[test]
    fn ema_smoothing_damps_a_single_transient_bright_sample() {
        let now = Instant::now();
        let mut cell = CellLegibilityState::bootstrap((10, 10, 10), now);
        // One frame under a bright comet core, then back to the dark ambient sample.
        cell.update((250, 250, 250), now + MIN_DWELL * 10);
        // The smoothed background moved toward the bright sample, but not all the way to it.
        assert!(cell.smoothed_bg.0 > 10);
        assert!(cell.smoothed_bg.0 < 250);
    }

    #[test]
    fn hysteresis_and_dwell_prevent_a_single_frame_from_flipping_the_committed_target() {
        let now = Instant::now();
        let mut cell = CellLegibilityState::bootstrap((10, 10, 10), now);
        assert_eq!(cell.committed_target, WHITE);
        // A single very bright sample, immediately after bootstrap (well inside MIN_DWELL) —
        // even though it would cross the crossover, the dwell timer must hold the target.
        cell.update((250, 250, 250), now + Duration::from_millis(50));
        assert_eq!(
            cell.committed_target, WHITE,
            "a single sample inside the dwell window must not flip the target"
        );
    }

    #[test]
    fn a_sustained_bright_background_eventually_flips_the_committed_target() {
        let mut now = Instant::now();
        let mut cell = CellLegibilityState::bootstrap((10, 10, 10), now);
        assert_eq!(cell.committed_target, WHITE);
        // Many sustained bright samples, spaced past the dwell window each time — a real,
        // persistent change (not a transient sweep) must still be able to flip eventually.
        for _ in 0..30 {
            now += MIN_DWELL + Duration::from_millis(10);
            cell.update((250, 250, 250), now);
        }
        assert_eq!(
            cell.committed_target, BLACK,
            "a sustained bright background must eventually flip the committed target"
        );
    }

    #[test]
    fn needs_scrim_is_keyed_off_the_committed_decision_not_a_fresh_raw_check() {
        let now = Instant::now();
        // Mid-grey background: the crossover luminance where neither black nor white comfortably
        // clears a high floor.
        let mut cell = CellLegibilityState::bootstrap((120, 120, 120), now);
        let scrim_before = cell.needs_scrim;
        // A single later sample nudges the smoothed value only slightly (EMA damping); the scrim
        // decision must track the *committed*/smoothed state, so it should not thrash on this one
        // sample alone.
        cell.update((121, 121, 121), now + Duration::from_millis(50));
        assert_eq!(cell.needs_scrim, scrim_before);
    }

    #[test]
    fn render_uses_the_scrim_background_only_when_flagged() {
        let now = Instant::now();
        let mut adaptive = CellLegibilityState::bootstrap((10, 10, 10), now);
        adaptive.needs_scrim = false;
        let (_, scrim) = adaptive.render((200, 200, 200));
        assert_eq!(scrim, None);

        let mut scrimmed = CellLegibilityState::bootstrap((10, 10, 10), now);
        scrimmed.needs_scrim = true;
        let (fg, scrim) = scrimmed.render((200, 200, 200));
        assert_eq!(scrim, Some(SCRIM_BG));
        assert!(contrast_ratio(fg, SCRIM_BG) >= CONTRAST_FLOOR);
    }

    #[test]
    fn observe_bootstraps_then_holds_off_until_the_sample_interval_elapses() {
        let nodes = [solar_system_test_node()];
        let layout = solar_system::build_layout(&nodes, 32, 16);
        let effects = SceneEffects::default();
        let mut grid = None;
        let now = Instant::now();

        let changed = observe(&mut grid, &layout, 0.0, &effects, None, 8, 8, now);
        assert!(changed, "the first observe call must bootstrap the grid");
        assert!(grid.is_some());

        let changed_again = observe(
            &mut grid,
            &layout,
            0.0,
            &effects,
            None,
            8,
            8,
            now + Duration::from_millis(10),
        );
        assert!(
            !changed_again,
            "a resample inside SAMPLE_INTERVAL must be a no-op"
        );

        let changed_later = observe(
            &mut grid,
            &layout,
            0.0,
            &effects,
            None,
            8,
            8,
            now + SAMPLE_INTERVAL + Duration::from_millis(1),
        );
        assert!(
            changed_later,
            "a resample past SAMPLE_INTERVAL must actually resample"
        );
    }

    #[test]
    fn observe_resizes_the_grid_when_the_cell_grid_changes() {
        let nodes = [solar_system_test_node()];
        let layout = solar_system::build_layout(&nodes, 32, 16);
        let effects = SceneEffects::default();
        let mut grid = None;
        let now = Instant::now();
        observe(&mut grid, &layout, 0.0, &effects, None, 8, 8, now);
        assert_eq!(grid.as_ref().unwrap().cols, 4);
        assert_eq!(grid.as_ref().unwrap().rows, 2);

        // A finer host cell size resizes the grid even though SAMPLE_INTERVAL has not elapsed.
        let changed = observe(&mut grid, &layout, 0.0, &effects, None, 4, 4, now);
        assert!(changed);
        assert_eq!(grid.as_ref().unwrap().cols, 8);
        assert_eq!(grid.as_ref().unwrap().rows, 4);
    }

    fn solar_system_test_node() -> solar_system::TreeNode {
        solar_system::TreeNode {
            label: solar_system::SceneLabel::EMPTY,
            parent: None,
            kind: solar_system::BodyKind::Sun,
            stage: crate::anim::cell::LifecycleStage::Running,
            severity: crate::anim::cell::Severity::Clear,
            size: solar_system::BodySize::Fixed,
            streak: 0.0,
            wear: 0.0,
            motes: 0,
            mote_share: 0.0,
        }
    }
}
