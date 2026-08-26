//! The Changes zone's pixel overlay — mechanics 3 and 4 of the "Rio Window,
//! Assembled" mockup gap analysis
//! (`data/herdr-mockup-stack-gap-20260824/report.md`), built per the
//! captain's decision
//! (`data/herdr-mockup-stack-gap-20260824/captain-decision-20260825.txt`) to
//! give the Changes zone the full pixel-canvas treatment rather than a
//! text-only approximation:
//!
//! - **Mechanic 3**, the traveling diff-rail light (`.diff-rail-light`): a
//!   glowing bar that slides once along a rail row between two files, when
//!   the active Space's diff just changed.
//! - **Mechanic 4**, the arriving diff file (`.diff-file.arriving`): a
//!   translateY + opacity pop-in over a file's header row, played once when
//!   that file first appears in the diff.
//!
//! Both are one-shot, not the mockup's demo-only infinite CSS loop —
//! `report.md`'s "Anything else" section is explicit that new animated
//! chrome adds to Herdr's existing Kitty-graphics upload cadence
//! (`.github/workflows/windows-gpu-probe.yml`), and a rail light or a pop-in
//! that never stopped animating would cost that cadence on every frame
//! forever rather than for the ~1 s it takes to notice a diff arrived. This
//! is the same "transient, not a constant loop" shape
//! `crate::anim::behaviour`'s `CARD_LIVE`/`CARD_REST` tiering already
//! prescribes for animated card chrome, applied here as "animate for a fixed
//! window, then draw nothing at all and cost nothing" rather than a second
//! frame-interval tier — there is no *resting* state of this overlay to
//! tier, because at rest it draws no pixels.
//!
//! [`DiffOverlayState`] is the state; [`observe`] advances it and reports
//! whether anything is still animating; [`frame`] is the pure function that
//! turns a state and this frame's row anchors
//! ([`crate::ui::diff_pane::DiffOverlayAnchors`]) into RGBA8 pixels. The
//! surface itself joins the same TUI-drawn-graphics family `MachineCorner`
//! and the sidebar's particle field are in — see
//! `crate::kitty_graphics::HostSurfaceId::DiffOverlay` — rather than being
//! negotiable with a client that rasterises its own scenes: it is small,
//! transient chrome, not a surface worth a delegation protocol of its own.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::kitty_graphics::HostCellSize;
use crate::ui::diff_pane::DiffOverlayAnchors;
use crate::workspace::GitDiffText;

/// How long the rail light takes to cross a rail row, once.
const RAIL_TRAVEL: Duration = Duration::from_millis(900);
/// How long a file's arrival glow plays, once.
const ARRIVAL_POP: Duration = Duration::from_millis(550);

/// One-shot animation state for the Changes zone's pixel overlay.
///
/// Keyed to the workspace it was last observed for, so switching Spaces
/// starts clean instead of replaying every file in the new diff as newly
/// arrived — a Space switch is not a file arriving, it is looking at a
/// different, already-settled picture.
#[derive(Debug, Clone, Default)]
pub(crate) struct DiffOverlayState {
    workspace_id: Option<String>,
    known_files: HashSet<String>,
    diff_signature: u64,
    rail_started: Option<Instant>,
    arrivals: Vec<(String, Instant)>,
}

impl DiffOverlayState {
    fn reset_to(&mut self, workspace_id: &str, files: HashSet<String>, signature: u64) {
        self.workspace_id = Some(workspace_id.to_string());
        self.known_files = files;
        self.diff_signature = signature;
        self.rail_started = None;
        self.arrivals.clear();
    }

    /// Whether [`frame`] would draw anything right now.
    pub(crate) fn is_animating(&self, now: Instant) -> bool {
        self.rail_started
            .is_some_and(|started| now.saturating_duration_since(started) < RAIL_TRAVEL)
            || self
                .arrivals
                .iter()
                .any(|(_, started)| now.saturating_duration_since(*started) < ARRIVAL_POP)
    }
}

/// The file paths a diff touches, in the order `git diff` names them —
/// [`crate::workspace::GitDiffLineKind::FileHeader`]'s `diff --git a/x b/y`
/// lines, the same source [`crate::ui::diff_pane`] reads a file boundary off.
fn file_paths(diff: &GitDiffText) -> Vec<String> {
    diff.lines
        .iter()
        .filter(|line| {
            line.kind == crate::workspace::GitDiffLineKind::FileHeader
                && line.text.starts_with("diff --git ")
        })
        .map(|line| {
            line.text
                .rsplit_once(" b/")
                .map(|(_, path)| path)
                .unwrap_or(line.text.as_str())
                .to_string()
        })
        .collect()
}

/// A cheap signature of a diff's actual content, so a re-fetch that came back
/// byte-identical is not mistaken for a change.
///
/// The lines are the whole of the content: `GitDiffText::truncated` is always
/// false for everything that reaches this zone — see
/// [`crate::ui::diff_pane::focused_pane_diff`] — so hashing it could only ever
/// mix a constant into every signature.
fn diff_signature(diff: &GitDiffText) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for line in &diff.lines {
        (line.kind as u8).hash(&mut hasher);
        line.text.hash(&mut hasher);
    }
    hasher.finish()
}

/// Advances `state` against the active Space's current diff, arming the
/// rail-light and arrival animations exactly once per real change, and
/// reports whether anything is still animating (so the caller knows whether
/// to keep the graphics layer around at all).
///
/// `now` is threaded in rather than read here, the same as every other
/// clock-driven module in this codebase (`crate::quota`, `crate::quality_streak`):
/// every branch below is then a pure function of its inputs.
pub(crate) fn observe(
    state: &mut DiffOverlayState,
    workspace_id: &str,
    diff: Option<&GitDiffText>,
    now: Instant,
) -> bool {
    let Some(diff) = diff else {
        *state = DiffOverlayState::default();
        return false;
    };

    let files: HashSet<String> = file_paths(diff).into_iter().collect();
    let signature = diff_signature(diff);

    if state.workspace_id.as_deref() != Some(workspace_id) {
        state.reset_to(workspace_id, files, signature);
        return state.is_animating(now);
    }

    if signature != state.diff_signature {
        let new_files: Vec<String> = files.difference(&state.known_files).cloned().collect();
        for path in new_files {
            if !state.arrivals.iter().any(|(existing, _)| existing == &path) {
                state.arrivals.push((path, now));
            }
        }
        // The rail only ever stands between two files, so nothing changed
        // travels it unless there is more than one file to stand between.
        if files.len() > 1 {
            state.rail_started = Some(now);
        }
        state.known_files = files;
        state.diff_signature = signature;
    }

    state
        .arrivals
        .retain(|(_, started)| now.saturating_duration_since(*started) < ARRIVAL_POP);

    state.is_animating(now)
}

/// `t` eased with a symmetric rise-then-fall bell, `0` at both ends and `1`
/// at the middle — the rail light's own opacity curve
/// (`0%,34%: opacity 0; 40%: opacity 1; 50%,100%: opacity 0`
/// in the mockup's `@keyframes diff-travel`), continuous rather than the
/// mockup's stepped keyframe percentages.
fn bell(t: f32) -> f32 {
    (1.0 - (t * 2.0 - 1.0).abs()).clamp(0.0, 1.0)
}

/// `t` eased out — matches the mockup's `cubic-bezier(.2,1.3,.35,1)`
/// `diff-pop` closely enough for a highlight band: a fast rise with a slight
/// overshoot past `1.0` before settling.
fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.70158;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

fn blend(px: &mut [u8], color: (u8, u8, u8), alpha: f32) {
    let alpha = alpha.clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let dst_a = f32::from(px[3]) / 255.0;
    let out_a = alpha + dst_a * (1.0 - alpha);
    if out_a <= 0.0 {
        return;
    }
    for (channel, src) in [color.0, color.1, color.2].into_iter().enumerate() {
        let src = f32::from(src);
        let dst = f32::from(px[channel]);
        px[channel] = ((src * alpha + dst * dst_a * (1.0 - alpha)) / out_a)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    px[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Renders `state`'s current animation frame as straight-alpha RGBA8, sized
/// `area.width * cell.width_px` by `area.height * cell.height_px` — the
/// pixel span of the diff pane's inner content rect, so the layer's own
/// placement can be the same cell rect the text renders into.
///
/// Returns `None` when nothing is animating: an idle overlay costs nothing to
/// draw and nothing to upload, matching every other mechanism this project's
/// frame-budget guidance already applies to transient chrome.
pub(crate) fn frame(
    state: &DiffOverlayState,
    anchors: &DiffOverlayAnchors,
    area: ratatui::layout::Rect,
    cell: HostCellSize,
    ink: (u8, u8, u8),
    now: Instant,
) -> Option<(u32, u32, Vec<u8>)> {
    if !state.is_animating(now) || area.width == 0 || area.height == 0 {
        return None;
    }
    let width = area.width as u32 * cell.width_px;
    let height = area.height as u32 * cell.height_px;
    if width == 0 || height == 0 {
        return None;
    }
    let mut px = vec![0u8; (width as usize) * (height as usize) * 4];
    let row_px = |row: u16| (row.saturating_sub(area.y)) as u32 * cell.height_px;

    if let Some(started) = state.rail_started {
        let elapsed = now.saturating_duration_since(started);
        if elapsed < RAIL_TRAVEL {
            let t = elapsed.as_secs_f32() / RAIL_TRAVEL.as_secs_f32();
            let alpha = bell(t);
            let bar_w = (width as f32 * 0.12).max(cell.width_px as f32);
            let margin = cell.width_px as f32;
            let travel = (width as f32 - margin * 2.0 - bar_w).max(0.0);
            let bar_x = margin + travel * t;
            let bar_h = (cell.height_px as f32 * 0.3).max(1.0);
            for &row in &anchors.rail_rows {
                let top = row_px(row) as f32 + (cell.height_px as f32 - bar_h) / 2.0;
                fill_glow_bar(&mut px, width, height, bar_x, top, bar_w, bar_h, ink, alpha);
            }
        }
    }

    for (path, started) in &state.arrivals {
        let elapsed = now.saturating_duration_since(*started);
        if elapsed >= ARRIVAL_POP {
            continue;
        }
        let Some(&(_, row)) = anchors.file_rows.iter().find(|(p, _)| p == path) else {
            continue;
        };
        let t = elapsed.as_secs_f32() / ARRIVAL_POP.as_secs_f32();
        let eased = ease_out_back(t);
        let alpha = t.clamp(0.0, 1.0);
        // translateY: slides down into place from a half-cell above, exactly
        // the sub-cell motion `report.md` names as the reason ratatui text
        // alone cannot draw this mechanic.
        let slide_px = (1.0 - eased) * (cell.height_px as f32 * 0.5);
        let top = (row_px(row) as f32 - slide_px).max(0.0);
        fill_glow_bar(
            &mut px,
            width,
            height,
            0.0,
            top,
            width as f32,
            cell.height_px as f32 * 2.0,
            ink,
            alpha * 0.35,
        );
    }

    Some((width, height, px))
}

/// A soft horizontal band, opaque at its own vertical centre and falling off
/// toward its top and bottom edge — the rail light's glow and the arrival
/// highlight are both this same shape, just sized and positioned
/// differently.
#[allow(clippy::too_many_arguments)]
fn fill_glow_bar(
    px: &mut [u8],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: (u8, u8, u8),
    peak_alpha: f32,
) {
    if peak_alpha <= 0.0 || w <= 0.0 || h <= 0.0 {
        return;
    }
    let x0 = x.floor().max(0.0) as u32;
    let x1 = ((x + w).ceil().max(0.0) as u32).min(width);
    let y0 = y.floor().max(0.0) as u32;
    let y1 = ((y + h).ceil().max(0.0) as u32).min(height);
    let cy = y + h / 2.0;
    for py in y0..y1 {
        // Coverage falls off toward the band's own top/bottom edge, giving
        // the glow a soft rather than a hard-edged rectangle.
        let dy = ((py as f32 + 0.5) - cy).abs() / (h / 2.0).max(1.0);
        let vertical = (1.0 - dy).clamp(0.0, 1.0);
        if vertical <= 0.0 {
            continue;
        }
        for px_x in x0..x1 {
            let i = ((py as usize) * (width as usize) + px_x as usize) * 4;
            blend(&mut px[i..i + 4], color, peak_alpha * vertical);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::{GitDiffLine, GitDiffLineKind};

    fn diff_with_files(paths: &[&str]) -> GitDiffText {
        let mut lines = Vec::new();
        for path in paths {
            lines.push(GitDiffLine {
                kind: GitDiffLineKind::FileHeader,
                text: format!("diff --git a/{path} b/{path}"),
            });
            lines.push(GitDiffLine {
                kind: GitDiffLineKind::Hunk,
                text: "@@ -1,1 +1,1 @@".to_string(),
            });
            lines.push(GitDiffLine {
                kind: GitDiffLineKind::Added,
                text: "+x".to_string(),
            });
        }
        GitDiffText {
            lines,
            truncated: false,
        }
    }

    /// The first diff a workspace is ever observed against settles quietly:
    /// nothing "arrives", because there was nothing before it to arrive
    /// relative to.
    #[test]
    fn the_first_diff_seen_for_a_space_animates_nothing() {
        let mut state = DiffOverlayState::default();
        let diff = diff_with_files(&["a.rs", "b.rs"]);
        let now = Instant::now();
        let animating = observe(&mut state, "ws-1", Some(&diff), now);
        assert!(!animating, "a first observation is not an arrival");
        assert!(state.arrivals.is_empty());
        assert!(state.rail_started.is_none());
    }

    /// A file that shows up in a later diff of the *same* Space plays the
    /// arrival animation, and the rail travels because there is now more
    /// than one file for it to stand between.
    #[test]
    fn a_new_file_in_the_same_spaces_diff_arrives() {
        let mut state = DiffOverlayState::default();
        let now = Instant::now();
        observe(&mut state, "ws-1", Some(&diff_with_files(&["a.rs"])), now);

        let animating = observe(
            &mut state,
            "ws-1",
            Some(&diff_with_files(&["a.rs", "b.rs"])),
            now,
        );
        assert!(animating);
        assert_eq!(state.arrivals.len(), 1);
        assert_eq!(state.arrivals[0].0, "b.rs");
        assert!(state.rail_started.is_some());
    }

    /// Switching Spaces is not a file arriving — every file in the new
    /// Space's diff is already-settled, not newly appeared.
    #[test]
    fn switching_spaces_starts_clean_instead_of_replaying_every_file() {
        let mut state = DiffOverlayState::default();
        let now = Instant::now();
        observe(&mut state, "ws-1", Some(&diff_with_files(&["a.rs"])), now);
        let animating = observe(
            &mut state,
            "ws-2",
            Some(&diff_with_files(&["x.rs", "y.rs"])),
            now,
        );
        assert!(!animating);
        assert!(state.arrivals.is_empty());
    }

    /// A diff re-fetched with identical content is not a change: refetching
    /// on the same tick this project already polls at must not replay the
    /// rail light every time.
    #[test]
    fn an_identical_diff_refetch_does_not_retrigger_the_rail() {
        let mut state = DiffOverlayState::default();
        let now = Instant::now();
        let diff = diff_with_files(&["a.rs", "b.rs"]);
        observe(&mut state, "ws-1", Some(&diff), now);
        let later = now + Duration::from_secs(5);
        let animating = observe(&mut state, "ws-1", Some(&diff), later);
        assert!(
            !animating,
            "an unchanged diff must not restart the animation"
        );
    }

    /// Once its window has fully elapsed, an arrival stops animating and
    /// [`DiffOverlayState::is_animating`] reports it — this is the "costs
    /// nothing at rest" half of the transient design, not just a rendering
    /// detail.
    #[test]
    fn an_arrival_stops_animating_once_its_window_elapses() {
        let mut state = DiffOverlayState::default();
        let now = Instant::now();
        observe(&mut state, "ws-1", Some(&diff_with_files(&["a.rs"])), now);
        observe(
            &mut state,
            "ws-1",
            Some(&diff_with_files(&["a.rs", "b.rs"])),
            now,
        );
        assert!(state.is_animating(now));
        let later = now + ARRIVAL_POP + RAIL_TRAVEL + Duration::from_millis(1);
        assert!(!state.is_animating(later));
    }

    /// No diff at all (zone folded, not a checkout, ...) clears any
    /// in-flight animation rather than leaving a stale one to redraw.
    #[test]
    fn no_diff_clears_the_state() {
        let mut state = DiffOverlayState::default();
        let now = Instant::now();
        observe(&mut state, "ws-1", Some(&diff_with_files(&["a.rs"])), now);
        observe(
            &mut state,
            "ws-1",
            Some(&diff_with_files(&["a.rs", "b.rs"])),
            now,
        );
        assert!(!observe(&mut state, "ws-1", None, now));
        assert!(state.arrivals.is_empty());
        assert!(state.workspace_id.is_none());
    }

    /// [`frame`] draws nothing, and says so, once nothing is animating —
    /// this is the actual cost guarantee: an idle overlay is not just a
    /// transparent image, it is no image at all.
    #[test]
    fn frame_returns_none_when_idle() {
        let state = DiffOverlayState::default();
        let anchors = DiffOverlayAnchors {
            rail_rows: vec![5],
            file_rows: vec![("a.rs".to_string(), 6)],
        };
        let result = frame(
            &state,
            &anchors,
            ratatui::layout::Rect::new(0, 0, 40, 20),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            (90, 209, 255),
            Instant::now(),
        );
        assert!(result.is_none());
    }

    /// A rail mid-travel actually paints something over its anchored row.
    #[test]
    fn frame_paints_the_travelling_rail_light() {
        let state = DiffOverlayState {
            rail_started: Some(Instant::now() - RAIL_TRAVEL / 2),
            ..Default::default()
        };
        let anchors = DiffOverlayAnchors {
            rail_rows: vec![3],
            file_rows: Vec::new(),
        };
        let (width, height, px) = frame(
            &state,
            &anchors,
            ratatui::layout::Rect::new(0, 0, 40, 10),
            HostCellSize {
                width_px: 8,
                height_px: 16,
            },
            (90, 209, 255),
            Instant::now(),
        )
        .expect("mid-travel rail must draw something");
        assert_eq!((width, height), (40 * 8, 10 * 16));
        assert!(px.chunks_exact(4).any(|p| p[3] > 0), "no pixel was painted");
    }
}
