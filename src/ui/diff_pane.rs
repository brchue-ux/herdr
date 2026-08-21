//! The diff pane: a fixed third zone (sibling to the sidebar and terminal
//! zones) showing the active Space's uncommitted `git diff`, plus a
//! popup-overlay fallback for when the zone is folded — see
//! `crate::app::AppState::diff_zone_width_threshold` for the fold rule and
//! `crate::app::AppState::diff_popup_open` for the fallback's toggle state.
//!
//! v1 coloring is deliberately simple: green for `+` lines, red for `-`
//! lines. Real syntax highlighting is a separate, larger capability this
//! project does not have yet — out of scope here.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::app::state::AppState;
use crate::workspace::{GitDiffLineKind, GitDiffText};

use super::text::truncate_end;
use super::widgets::render_panel_shell;

/// Renders the diff zone in place, as the third of the fixed three zones.
pub(super) fn render_diff_zone(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) =
        render_panel_shell(frame, area, app.palette.surface_dim, app.palette.panel_bg)
    else {
        return;
    };
    render_diff_content(app, frame, inner);
}

/// Renders the diff pane as a popup overlay over `area` (the terminal zone),
/// for when the fixed zone is folded but the user reached for it anyway via
/// `toggle_diff_pane` — see `AppState::diff_popup_open`.
pub(super) fn render_diff_popup_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(inner) = render_panel_shell(frame, area, app.palette.accent, app.palette.panel_bg)
    else {
        return;
    };
    render_diff_content(app, frame, inner);
}

fn render_diff_content(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let active_workspace = app.active.and_then(|idx| app.workspaces.get(idx));

    let Some(ws) = active_workspace else {
        render_message(frame, area, "no active space", app.palette.subtext0);
        return;
    };

    if ws.git_space().is_none() {
        render_message(frame, area, "not a git checkout", app.palette.subtext0);
        return;
    }

    let Some(diff) = ws.git_diff() else {
        render_message(frame, area, "loading diff…", app.palette.subtext0);
        return;
    };

    if diff.lines.is_empty() {
        render_message(frame, area, "no changes", app.palette.subtext0);
        return;
    }

    render_diff_lines(app, frame, area, diff);
}

fn render_message(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    let paragraph = Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color))));
    frame.render_widget(paragraph, area);
}

fn render_diff_lines(app: &AppState, frame: &mut Frame, area: Rect, diff: &GitDiffText) {
    let visible_rows = area.height as usize;
    // No scroll state in v1 (Stage 3 polish per the scoping report) — show the
    // top of the diff and say plainly when more was cut, rather than either
    // silently truncating or growing unbounded past the pane's own height.
    let fits = diff.lines.len() <= visible_rows && !diff.truncated;
    let content_rows = if fits {
        visible_rows
    } else {
        visible_rows.saturating_sub(1)
    };

    let width = area.width as usize;
    let mut lines: Vec<Line> = diff
        .lines
        .iter()
        .take(content_rows)
        .map(|line| diff_line_span(app, line, width))
        .collect();

    if !fits && content_rows > 0 {
        let hidden = diff.lines.len().saturating_sub(content_rows);
        let suffix = if diff.truncated { "+" } else { "" };
        lines.push(Line::from(Span::styled(
            format!("… {hidden}{suffix} more lines"),
            Style::default().fg(app.palette.subtext0),
        )));
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn diff_line_span(
    app: &AppState,
    line: &crate::workspace::GitDiffLine,
    width: usize,
) -> Line<'static> {
    let color = match line.kind {
        GitDiffLineKind::Added => app.palette.green,
        GitDiffLineKind::Removed => app.palette.red,
        GitDiffLineKind::Hunk => app.palette.accent,
        GitDiffLineKind::FileHeader => app.palette.subtext0,
        GitDiffLineKind::Context => app.palette.text,
    };
    Line::from(Span::styled(
        truncate_end(&line.text, width),
        Style::default().fg(color),
    ))
}
