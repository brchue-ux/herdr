//! The diff pane — herdr's "Changes" zone: a third zone, always to the right
//! of the sidebar and terminal zones, showing the active Space's uncommitted
//! `git diff`, plus a popup-overlay fallback for when the zone is folded —
//! see `crate::app::AppState::diff_zone_width_threshold` for the fold rule
//! and `crate::app::AppState::diff_popup_open` for the fallback's toggle
//! state. Its own width is a percentage of the remaining space
//! (`crate::ui::DIFF_ZONE_PERCENT`), and it scrolls independently of the
//! sidebar and terminal zones via `crate::app::AppState::diff_pane_scroll`.
//! This is the ONLY place unified-diff content — hunks, `+`/`-` lines, file
//! headers — ever renders; the triview log zone (`crate::ui::panes`) renders
//! plain command text through an unrelated model
//! (`crate::app::pane_command_log`) and never calls into this module.
//!
//! Styled to echo the same rail/card-edge material the sidebar tree and
//! panel borders already use (`crate::ui::navigator`'s `│`/`├──`/`└──`
//! branch glyphs, `render_panel_shell`'s box-drawing border): a two-column
//! line-number gutter, a rail glyph standing in for the `+`/`-` marker, a
//! tinted wash behind added/removed rows, and a ruled file-header card
//! rather than plain recolored diff text. Real syntax highlighting is a
//! separate, larger capability this project does not have yet — out of
//! scope here.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{AppState, Palette};
use crate::workspace::{GitDiffLine, GitDiffLineKind, GitDiffText};

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

    let scroll = normalized_diff_scroll(app, area, app.diff_pane_scroll);
    render_diff_lines(app, frame, area, diff, scroll);
}

/// Clamps `requested` to how far the active diff can actually scroll for
/// `area`, mirroring `crate::ui::sidebar::normalized_workspace_scroll`. Both
/// the per-frame render clamp (`compute_view_internal`) and the mouse wheel
/// handler (`AppState::scroll_diff_pane`) call through this so the two can
/// never disagree.
pub(crate) fn normalized_diff_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let total = app
        .active
        .and_then(|idx| app.workspaces.get(idx))
        .and_then(|ws| ws.git_diff())
        .map(|diff| diff.lines.len())
        .unwrap_or(0);
    requested.min(total.saturating_sub(area.height as usize))
}

fn render_message(frame: &mut Frame, area: Rect, text: &str, color: Color) {
    let paragraph = Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color))));
    frame.render_widget(paragraph, area);
}

/// One row of parsed diff content, ahead of gutter-width sizing and
/// terminal-row layout, so the two-column line-number gutter lines up
/// consistently across every row instead of being sized per-line.
enum DiffRow {
    /// The `diff --git a/x b/y` line — rendered as a small ruled card
    /// header (bold path, then a full-width rule) rather than shown
    /// verbatim.
    FileHeader { path: String },
    /// Any other `FileHeader`-kind line worth keeping (rename/mode/binary
    /// notices) — `index`/`--- `/`+++ ` lines are dropped as redundant with
    /// the ruled header above them.
    FileInfo { text: String },
    /// A `@@ -a,b +c,d @@` hunk line, parsed for its starting line numbers
    /// so the gutter can count forward from here.
    Hunk { label: String },
    Content {
        kind: GitDiffLineKind,
        old_ln: Option<u32>,
        new_ln: Option<u32>,
        text: String,
    },
}

/// Parses a unified-diff hunk header down to its two starting line numbers.
/// `git diff` always emits this exact `@@ -<old>[,<len>] +<new>[,<len>] @@`
/// shape, so a plain whitespace/prefix walk is enough — no need for a regex
/// dependency over one fixed grammar.
fn parse_hunk_header(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split_whitespace();
    if parts.next()? != "@@" {
        return None;
    }
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse().ok()?;
    let new_start = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// File path a `diff --git a/<old> b/<new>` line names — the `b/` side,
/// since that is the path that exists after the change (or the only side,
/// for a delete).
fn file_header_path(text: &str) -> &str {
    text.rsplit_once(" b/")
        .map(|(_, path)| path)
        .unwrap_or(text)
}

fn render_diff_lines(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    diff: &GitDiffText,
    scroll: usize,
) {
    let visible_rows = area.height as usize;
    let width = area.width as usize;

    // `scroll` is a count of source diff lines (not terminal rows) to skip
    // before the same bounded forward walk this pane always did from line 0
    // — stopping once enough terminal rows exist to know whether the
    // remainder still overflows. Header/hunk rows can spend more than one
    // terminal row per diff line, so the walk keeps going a little past
    // `visible_rows` before it can be sure it has enough. Line-number
    // continuity (`old_ln`/`new_ln`) still walks from the true top even when
    // scrolled, so a scrolled-in hunk's gutter numbers are correct rather
    // than restarting from 0.
    let mut rows: Vec<DiffRow> = Vec::new();
    let mut terminal_rows = 0usize;
    let mut consumed = 0usize;
    let mut old_ln: u32 = 0;
    let mut new_ln: u32 = 0;

    let diff_lines: &Vec<GitDiffLine> = &diff.lines;
    for (idx, line) in diff_lines.iter().enumerate() {
        if idx >= scroll && terminal_rows > visible_rows {
            break;
        }
        if idx < scroll {
            // Still walked (for line-number continuity) but not rendered.
            match line.kind {
                GitDiffLineKind::Hunk => {
                    if let Some((old_start, new_start)) = parse_hunk_header(&line.text) {
                        old_ln = old_start;
                        new_ln = new_start;
                    }
                }
                GitDiffLineKind::Added => new_ln = new_ln.saturating_add(1),
                GitDiffLineKind::Removed => old_ln = old_ln.saturating_add(1),
                GitDiffLineKind::Context => {
                    old_ln = old_ln.saturating_add(1);
                    new_ln = new_ln.saturating_add(1);
                }
                GitDiffLineKind::FileHeader => {}
            }
            continue;
        }
        consumed += 1;
        match line.kind {
            GitDiffLineKind::FileHeader if line.text.starts_with("diff --git ") => {
                rows.push(DiffRow::FileHeader {
                    path: file_header_path(&line.text).to_string(),
                });
                terminal_rows += 2;
            }
            GitDiffLineKind::FileHeader => {
                if line.text.starts_with("index ")
                    || line.text.starts_with("--- ")
                    || line.text.starts_with("+++ ")
                {
                    continue;
                }
                rows.push(DiffRow::FileInfo {
                    text: line.text.clone(),
                });
                terminal_rows += 1;
            }
            GitDiffLineKind::Hunk => {
                if let Some((old_start, new_start)) = parse_hunk_header(&line.text) {
                    old_ln = old_start;
                    new_ln = new_start;
                }
                rows.push(DiffRow::Hunk {
                    label: line.text.clone(),
                });
                terminal_rows += 1;
            }
            GitDiffLineKind::Added => {
                rows.push(DiffRow::Content {
                    kind: line.kind,
                    old_ln: None,
                    new_ln: Some(new_ln),
                    text: line.text.clone(),
                });
                new_ln = new_ln.saturating_add(1);
                terminal_rows += 1;
            }
            GitDiffLineKind::Removed => {
                rows.push(DiffRow::Content {
                    kind: line.kind,
                    old_ln: Some(old_ln),
                    new_ln: None,
                    text: line.text.clone(),
                });
                old_ln = old_ln.saturating_add(1);
                terminal_rows += 1;
            }
            GitDiffLineKind::Context => {
                rows.push(DiffRow::Content {
                    kind: line.kind,
                    old_ln: Some(old_ln),
                    new_ln: Some(new_ln),
                    text: line.text.clone(),
                });
                old_ln = old_ln.saturating_add(1);
                new_ln = new_ln.saturating_add(1);
                terminal_rows += 1;
            }
        }
    }

    let gutter_w = rows
        .iter()
        .filter_map(|row| match row {
            DiffRow::Content { old_ln, new_ln, .. } => {
                Some(old_ln.unwrap_or(0).max(new_ln.unwrap_or(0)))
            }
            _ => None,
        })
        .max()
        .map(|max| max.to_string().len())
        .unwrap_or(2)
        .max(2);

    let mut lines: Vec<Line> = Vec::with_capacity(terminal_rows.min(visible_rows + 2));
    if scroll > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "↑ {scroll} line{} above",
                if scroll == 1 { "" } else { "s" }
            ),
            Style::default().fg(app.palette.subtext0),
        )));
    }
    for row in &rows {
        push_row(app, &mut lines, row, width, gutter_w);
    }

    let shown = scroll + consumed;
    let overflowed = shown < diff.lines.len() || diff.truncated;
    if overflowed {
        lines.truncate(visible_rows.saturating_sub(1));
        let hidden = diff.lines.len().saturating_sub(shown);
        let suffix = if diff.truncated { "+" } else { "" };
        lines.push(Line::from(Span::styled(
            format!("… {hidden}{suffix} more lines"),
            Style::default().fg(app.palette.subtext0),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

/// Background tint for a wash behind added/removed rows and the hunk band —
/// a low-strength blend toward `fg` over `bg`, standing in for the
/// mockup's translucent `rgba(...)` row washes, which a terminal cell has
/// no alpha channel to express directly.
fn tint(fg: Color, bg: Color, amount: f32) -> Color {
    match (fg, bg) {
        (Color::Rgb(fr, fgr, fb), Color::Rgb(br, bgr, bb)) => Color::Rgb(
            lerp(br, fr, amount),
            lerp(bgr, fgr, amount),
            lerp(bb, fb, amount),
        ),
        _ => bg,
    }
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Right-pads `text` with spaces to `width` display columns, so a row's
/// background tint reads as a full-width band instead of stopping wherever
/// the code text happened to end.
fn pad_to_width(text: &str, width: usize) -> String {
    let w = super::text::display_width(text);
    if w >= width {
        return text.to_string();
    }
    format!("{text}{}", " ".repeat(width - w))
}

fn push_row(
    app: &AppState,
    lines: &mut Vec<Line<'static>>,
    row: &DiffRow,
    width: usize,
    gutter_w: usize,
) {
    let p = &app.palette;
    match row {
        DiffRow::FileHeader { path } => {
            lines.push(Line::from(Span::styled(
                truncate_end(path, width),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(p.surface_dim),
            )));
        }
        DiffRow::FileInfo { text } => {
            lines.push(Line::from(Span::styled(
                truncate_end(text, width),
                Style::default().fg(p.subtext0),
            )));
        }
        DiffRow::Hunk { label } => {
            lines.push(hunk_rule(p, label, width));
        }
        DiffRow::Content {
            kind,
            old_ln,
            new_ln,
            text,
        } => {
            lines.push(content_row(
                p, *kind, *old_ln, *new_ln, text, width, gutter_w,
            ));
        }
    }
}

/// Draws the hunk header as a ruled, tinted band cutting across the pane —
/// the same box-drawing rule the panel border and file-header card already
/// use — rather than a plain dim-colored line of `@@ ... @@` text.
fn hunk_rule(p: &Palette, label: &str, width: usize) -> Line<'static> {
    let range = label
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split_once(" @@"))
        .map(|(range, _)| range)
        .unwrap_or(label);
    let bg = tint(p.accent, p.panel_bg, 0.10);
    let lead = "── ";
    let body = truncate_end(range, width.saturating_sub(lead.len() + 1));
    let used = lead.len() + super::text::display_width(&body) + 1;
    let fill = "─".repeat(width.saturating_sub(used));
    Line::from(vec![
        Span::styled(lead, Style::default().fg(p.surface_dim).bg(bg)),
        Span::styled(body, Style::default().fg(p.accent).bg(bg)),
        Span::styled(
            format!(" {fill}"),
            Style::default().fg(p.surface_dim).bg(bg),
        ),
    ])
}

fn content_row(
    p: &Palette,
    kind: GitDiffLineKind,
    old_ln: Option<u32>,
    new_ln: Option<u32>,
    text: &str,
    width: usize,
    gutter_w: usize,
) -> Line<'static> {
    let old_cell = old_ln.map(|n| n.to_string()).unwrap_or_default();
    let new_cell = new_ln.map(|n| n.to_string()).unwrap_or_default();

    let (marker, marker_color, bg, code_style) = match kind {
        GitDiffLineKind::Added => (
            "+",
            p.green,
            tint(p.green, p.panel_bg, 0.14),
            Style::default().fg(p.text),
        ),
        GitDiffLineKind::Removed => (
            "-",
            p.red,
            tint(p.red, p.panel_bg, 0.12),
            Style::default().fg(p.text).add_modifier(Modifier::DIM),
        ),
        _ => ("│", p.surface1, p.panel_bg, Style::default().fg(p.subtext0)),
    };

    let gutter = format!("{old_cell:>gutter_w$} {new_cell:>gutter_w$} ");
    let prefix_w = super::text::display_width(&gutter) + 2; // marker + trailing space
    let code_w = width.saturating_sub(prefix_w);
    let code = pad_to_width(
        &truncate_end(text.strip_prefix(['+', '-']).unwrap_or(text), code_w),
        code_w,
    );

    Line::from(vec![
        Span::styled(gutter, Style::default().fg(p.overlay0).bg(bg)),
        Span::styled(
            format!("{marker} "),
            Style::default()
                .fg(marker_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(code, code_style.bg(bg)),
    ])
}
