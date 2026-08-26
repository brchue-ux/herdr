//! The diff pane — herdr's "Changes" zone: a third zone, always to the right
//! of the sidebar and terminal zones, showing the running edits the coding
//! agent in the focused pane has made this session — that pane's
//! [`crate::agent_edit_log::AgentEditLog`], reported in over
//! `pane.report_edit_diff` and read here through
//! `crate::app::AppState::focused_pane_agent_edit_lines`, not the Space's
//! `git diff` — plus a popup-overlay fallback for when the zone is folded —
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

/// The Changes zone's content: the active Space's focused pane's agent edit
/// log, as the one [`GitDiffText`] every part of this zone reads — the drawn
/// text, the scroll clamp, and the pixel overlay's anchors and animation
/// state. One source, so none of them can disagree about what is on screen.
///
/// `None` means there is nothing to show a log *for* (no active Space, no
/// focused pane, no terminal behind it); `Some` with no lines means the pane
/// simply has not reported an edit yet.
///
/// `truncated: false` is a fact about this pipeline, not a placeholder: no
/// file's entry can ever carry a set flag to aggregate. `pane.report_edit_diff`
/// caps a report at `GIT_DIFF_MAX_LINES` — the parser's own cap, derived from
/// the same constant — and applies that cap to the *same* post-synthesis text
/// the parser is then handed, so the parser's truncation branch cannot fire on
/// anything that reaches this log. Nothing else writes to it. That is why the
/// renderer and the overlay's signature carry no truncation branch either;
/// loosening either half of the cap rule would have to put both back.
///
/// [`AgentEditLog::flatten`] would drop a per-file flag anyway — the
/// aggregate stream has nowhere to say "one of these files was cut short" —
/// which is only harmless because there is never one to drop.
///
/// [`AgentEditLog::flatten`]: crate::agent_edit_log::AgentEditLog::flatten
pub(crate) fn focused_pane_diff(app: &AppState) -> Option<GitDiffText> {
    let lines = app.focused_pane_agent_edit_lines(app.active?)?;
    Some(GitDiffText {
        lines,
        truncated: false,
    })
}

fn render_diff_content(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let Some(diff) = focused_pane_diff(app) else {
        render_message(frame, area, "no active space", app.palette.subtext0);
        return;
    };

    if diff.lines.is_empty() {
        render_message(frame, area, "no edits yet", app.palette.subtext0);
        return;
    }

    let scroll = normalized_diff_scroll(app, area, app.diff_pane_scroll);
    render_diff_lines(app, frame, area, &diff, scroll);
}

/// Clamps `requested` to how far the active diff can actually scroll for
/// `area`, mirroring `crate::ui::sidebar::normalized_workspace_scroll`. Both
/// the per-frame render clamp (`compute_view_internal`) and the mouse wheel
/// handler (`AppState::scroll_diff_pane`) call through this so the two can
/// never disagree.
///
/// Counts through [`AppState::focused_pane_agent_edit_line_count`] rather
/// than [`focused_pane_diff`]: this runs twice per drawn frame — once from
/// [`render_diff_content`], once from [`diff_overlay_anchors`] — and the
/// clamp needs a total, not the lines. Going through `focused_pane_diff`
/// cloned the whole session's edit log on each of those calls to read
/// `.len()` off it.
pub(crate) fn normalized_diff_scroll(app: &AppState, area: Rect, requested: usize) -> usize {
    let total = app
        .active
        .and_then(|workspace_idx| app.focused_pane_agent_edit_line_count(workspace_idx))
        .unwrap_or(0);
    requested.min(total.saturating_sub(area.height as usize))
}

/// The diff pane's content rect, inside its border — the same rect
/// `render_panel_shell` hands [`render_diff_content`] for the fixed Changes
/// zone, recomputed here from `outer` (`AppState::view::diff_area`) without a
/// `Frame` to draw into, so [`diff_overlay_anchors`] can be called from the
/// scene-observing tick, which has none.
pub(crate) fn diff_inner_rect(outer: Rect) -> Option<Rect> {
    (outer.width >= 2 && outer.height >= 2)
        .then(|| Rect::new(outer.x + 1, outer.y + 1, outer.width - 2, outer.height - 2))
}

/// Terminal-row anchors for the diff pane's pixel overlay (mechanics 3/4 —
/// the traveling rail light and the arriving-file glow), in absolute screen
/// rows.
pub(crate) struct DiffOverlayAnchors {
    /// Every currently-visible [`DiffRow::Rail`] row, top to bottom.
    pub(crate) rail_rows: Vec<u16>,
    /// Every currently-visible file's header row, keyed by the same path
    /// [`crate::ui::diff_overlay::DiffOverlayState`] tracks arrivals under.
    pub(crate) file_rows: Vec<(String, u16)>,
}

/// Resolves [`DiffOverlayAnchors`] for the fixed Changes zone at `outer`
/// (`AppState::view::diff_area`) — `None` when the zone is not showing a
/// diff at all, the same set of early-outs [`render_diff_content`] takes.
///
/// Walks the *same* [`build_diff_rows`] pass `render_diff_lines` draws from,
/// rather than a second derivation of where each row landed — the failure
/// mode that leaves a drawn position and a computed one free to disagree.
/// This is still a separate call, from a separate tick (the graphics
/// observer, not the render pass), so the two can still see a different
/// `scroll`/diff snapshot a frame apart; that is the same tolerance every
/// other TUI-drawn overlay in this codebase (`machine_corner`, the sidebar's
/// particle field) already accepts.
pub(crate) fn diff_overlay_anchors(app: &AppState, outer: Rect) -> Option<DiffOverlayAnchors> {
    let area = diff_inner_rect(outer)?;
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let diff = focused_pane_diff(app)?;
    if diff.lines.is_empty() {
        return None;
    }
    let scroll = normalized_diff_scroll(app, area, app.diff_pane_scroll);
    let built = build_diff_rows(&diff, scroll, area.height as usize);

    let mut rail_rows = Vec::new();
    let mut file_rows = Vec::new();
    let mut offset: u16 = if scroll > 0 { 1 } else { 0 };
    for row in &built.rows {
        if offset >= area.height {
            break;
        }
        match row {
            DiffRow::Rail => rail_rows.push(area.y + offset),
            DiffRow::FileHeader { path } => file_rows.push((path.clone(), area.y + offset)),
            _ => {}
        }
        offset += match row {
            DiffRow::FileHeader { .. } => 2,
            _ => 1,
        };
    }
    Some(DiffOverlayAnchors {
        rail_rows,
        file_rows,
    })
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
    /// The mockup's `.diff-rail` — a divider between one file's diff content
    /// and the next file's header. Inserted only *between* files (never
    /// before the first or after the last), so a single-file diff draws none
    /// and the row exists at all only where the mockup's own DOM puts one.
    /// Its static track is drawn here as plain text; the traveling light that
    /// rides it (mechanic 3) is a pixel overlay — see
    /// [`super::diff_overlay`] — anchored to this row by
    /// [`diff_overlay_anchors`].
    Rail,
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

/// Walks `diff.lines` into terminal-ready [`DiffRow`]s, stopping once enough
/// have been produced to know whether the remainder still overflows `area`.
///
/// The one row-layout pass, shared by [`render_diff_lines`] and
/// [`diff_overlay_anchors`] rather than run twice with two chances to
/// disagree — the same anchoring failure this project has hit before when a
/// drawn position and a computed one came from two different walks.
fn build_diff_rows(diff: &GitDiffText, scroll: usize, visible_rows: usize) -> BuiltDiffRows {
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
    // Whether any row from an earlier file has already been pushed — a
    // `Rail` divider goes in front of every `FileHeader` after the first one
    // rendered, and only there, so a single-file diff (or a diff scrolled to
    // start mid-file) draws no rail at all.
    let mut saw_a_file = false;

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
                GitDiffLineKind::FileHeader => {
                    if line.text.starts_with("diff --git ") {
                        saw_a_file = true;
                    }
                }
            }
            continue;
        }
        consumed += 1;
        match line.kind {
            GitDiffLineKind::FileHeader if line.text.starts_with("diff --git ") => {
                if saw_a_file {
                    rows.push(DiffRow::Rail);
                    terminal_rows += 1;
                }
                saw_a_file = true;
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

    BuiltDiffRows {
        rows,
        gutter_w,
        consumed,
        terminal_rows,
    }
}

struct BuiltDiffRows {
    rows: Vec<DiffRow>,
    gutter_w: usize,
    consumed: usize,
    terminal_rows: usize,
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

    let BuiltDiffRows {
        rows,
        gutter_w,
        consumed,
        terminal_rows,
    } = build_diff_rows(diff, scroll, visible_rows);

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
    if shown < diff.lines.len() {
        lines.truncate(visible_rows.saturating_sub(1));
        let hidden = diff.lines.len().saturating_sub(shown);
        lines.push(Line::from(Span::styled(
            format!("… {hidden} more lines"),
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
        // The static track only — mirrors the mockup's always-visible
        // `.diff-rail-line`. The traveling `.diff-rail-light` bar is not text
        // at all; see `diff_overlay`.
        DiffRow::Rail => {
            lines.push(Line::from(Span::styled(
                "─".repeat(width),
                Style::default().fg(p.surface_dim),
            )));
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

#[cfg(test)]
mod the_diff_rail_stands_only_between_files {
    use super::*;

    fn file(path: &str) -> Vec<GitDiffLine> {
        vec![
            GitDiffLine {
                kind: GitDiffLineKind::FileHeader,
                text: format!("diff --git a/{path} b/{path}"),
            },
            GitDiffLine {
                kind: GitDiffLineKind::Hunk,
                text: "@@ -1,1 +1,1 @@".to_string(),
            },
            GitDiffLine {
                kind: GitDiffLineKind::Added,
                text: "+x".to_string(),
            },
        ]
    }

    fn diff(paths: &[&str]) -> GitDiffText {
        GitDiffText {
            lines: paths.iter().flat_map(|p| file(p)).collect(),
            truncated: false,
        }
    }

    /// A single-file diff has no boundary to stand a rail on — the mockup's
    /// own DOM only ever shows one, between two files.
    #[test]
    fn a_single_file_diff_draws_no_rail() {
        let built = build_diff_rows(&diff(&["a.rs"]), 0, 20);
        assert!(
            !built.rows.iter().any(|row| matches!(row, DiffRow::Rail)),
            "one file has no boundary to stand a rail on"
        );
    }

    /// Two files draw exactly one rail, between them and nowhere else — not
    /// before the first file and not after the last.
    #[test]
    fn two_files_draw_exactly_one_rail_between_them() {
        let built = build_diff_rows(&diff(&["a.rs", "b.rs"]), 0, 20);
        let rail_count = built
            .rows
            .iter()
            .filter(|row| matches!(row, DiffRow::Rail))
            .count();
        assert_eq!(rail_count, 1);
        let rail_idx = built
            .rows
            .iter()
            .position(|row| matches!(row, DiffRow::Rail))
            .unwrap();
        assert!(
            matches!(built.rows[rail_idx - 1], DiffRow::Content { .. }),
            "the rail follows the first file's own content"
        );
        assert!(
            matches!(built.rows[rail_idx + 1], DiffRow::FileHeader { .. }),
            "the rail precedes the second file's header"
        );
    }

    /// Three files draw exactly two rails — one for every boundary, never one
    /// per file.
    #[test]
    fn three_files_draw_exactly_two_rails() {
        let built = build_diff_rows(&diff(&["a.rs", "b.rs", "c.rs"]), 0, 20);
        let rail_count = built
            .rows
            .iter()
            .filter(|row| matches!(row, DiffRow::Rail))
            .count();
        assert_eq!(rail_count, 2);
    }

    /// [`diff_overlay_anchors`] locates the rail and the second file's header
    /// at the exact rows the text renderer draws them at — driven through
    /// the real `AppState`, not a hand-rolled offset walk, so this fails if
    /// the overlay's positions and the drawn ones ever come from two
    /// different derivations. It also pins the overlay to the *same* source
    /// the text draws from: the focused pane's agent edit log.
    #[test]
    fn overlay_anchors_land_on_the_same_rows_the_text_renderer_draws() {
        let ws = crate::workspace::Workspace::test_new("one");
        let pane_id = ws.tabs[0].root_pane;

        let mut app = crate::app::state::AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        let terminal_id = app.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("ensure_test_terminals must have backfilled this terminal")
            .agent_edit_log
            .set_or_clear("both".into(), diff(&["a.rs", "b.rs"]));

        let outer = Rect::new(0, 0, 40, 20);
        let anchors = diff_overlay_anchors(&app, outer).expect("a diff to anchor against");
        // a.rs is 4 terminal rows (2-row header, 1 hunk line, 1 content
        // line), so the rail between the two files lands right after it.
        assert_eq!(anchors.rail_rows, vec![outer.y + 1 + 4]);
        assert_eq!(anchors.file_rows.len(), 2);
        assert_eq!(anchors.file_rows[0], ("a.rs".to_string(), outer.y + 1));
        assert_eq!(anchors.file_rows[1], ("b.rs".to_string(), outer.y + 1 + 5));
    }
}

/// The Changes zone reads the *focused pane's* agent edit log, not the
/// Space's `git diff` — so what it shows follows focus, and an untouched
/// pane in a git checkout full of uncommitted work still shows nothing.
#[cfg(test)]
mod the_changes_zone_follows_the_focused_pane {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;

    fn sample(marker: &str) -> GitDiffText {
        GitDiffText {
            lines: vec![GitDiffLine {
                kind: GitDiffLineKind::Added,
                text: format!("+{marker}"),
            }],
            truncated: false,
        }
    }

    /// Records `diff` against the pane `pane_id`'s attached terminal, the way
    /// `pane.report_edit_diff` does server-side.
    fn record_edit(
        app: &mut AppState,
        pane_id: crate::layout::PaneId,
        path: &str,
        diff: GitDiffText,
    ) {
        let terminal_id = app.workspaces[0]
            .tabs
            .iter()
            .find_map(|tab| tab.panes.get(&pane_id))
            .expect("pane must exist in some tab")
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("ensure_test_terminals must have backfilled this terminal")
            .agent_edit_log
            .set_or_clear(path.to_string(), diff);
    }

    #[test]
    fn focused_pane_agent_edit_lines_reads_the_terminal_not_the_workspace() {
        let ws = Workspace::test_new("one");
        let pane_id = ws.tabs[0].root_pane;

        let mut app = AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        // Nothing reported yet: an empty log, not "no active space".
        assert_eq!(app.focused_pane_agent_edit_lines(0), Some(Vec::new()));

        record_edit(&mut app, pane_id, "a.rs", sample("hello"));

        assert_eq!(
            app.focused_pane_agent_edit_lines(0),
            Some(sample("hello").lines)
        );
    }

    /// A workspace index that names no workspace has no log to read — the
    /// `None` the renderer turns into "no active space", distinct from the
    /// `Some(vec![])` above.
    #[test]
    fn focused_pane_agent_edit_lines_is_none_for_an_unfocused_workspace_index() {
        let ws = Workspace::test_new("one");
        let mut app = AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        assert_eq!(app.focused_pane_agent_edit_lines(1), None);
    }

    /// Switching tabs switches which pane's edits the zone shows: each tab's
    /// root pane has its own terminal, and so its own edit log.
    #[test]
    fn switching_the_active_tab_switches_which_panes_edits_show() {
        let mut ws = Workspace::test_new("one");
        let second_tab = ws.test_add_tab(Some("second"));
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.tabs[second_tab].root_pane;
        assert_ne!(first_pane, second_pane);

        let mut app = AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        record_edit(&mut app, first_pane, "first.rs", sample("first"));
        record_edit(&mut app, second_pane, "second.rs", sample("second"));

        assert_eq!(app.workspaces[0].active_tab_index(), 0);
        assert_eq!(
            app.focused_pane_agent_edit_lines(0),
            Some(sample("first").lines)
        );

        app.workspaces[0].switch_tab(second_tab);

        assert_eq!(
            app.focused_pane_agent_edit_lines(0),
            Some(sample("second").lines),
            "the zone must follow focus, not stay on the tab it started on"
        );
    }

    /// The scroll clamp reads the same source the content does, so the two
    /// can never disagree about how far the pane can scroll.
    #[test]
    fn normalized_diff_scroll_clamps_against_the_agent_edit_log() {
        let ws = Workspace::test_new("one");
        let pane_id = ws.tabs[0].root_pane;

        let mut app = AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        let long = GitDiffText {
            lines: (0..10)
                .map(|i| GitDiffLine {
                    kind: GitDiffLineKind::Added,
                    text: format!("+line {i}"),
                })
                .collect(),
            truncated: false,
        };
        record_edit(&mut app, pane_id, "a.rs", long);

        let area = Rect::new(0, 0, 40, 4);
        // 10 lines in a 4-row area scroll at most 6 lines down.
        assert_eq!(normalized_diff_scroll(&app, area, 99), 6);
        assert_eq!(normalized_diff_scroll(&app, area, 2), 2);
    }

    /// The clamp counts through `focused_pane_agent_edit_line_count` so a
    /// frame never clones the whole log to read a length off it. That is only
    /// safe while the count answers exactly what the flattening accessor
    /// would — including which cases are `None` — across several files, which
    /// is where a hand-rolled sum would be free to drift.
    #[test]
    fn the_count_only_accessor_agrees_with_the_flattening_one() {
        let ws = Workspace::test_new("one");
        let pane_id = ws.tabs[0].root_pane;

        let mut app = AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.ensure_test_terminals();

        let count = |app: &AppState| app.focused_pane_agent_edit_line_count(0);
        let flattened_count = |app: &AppState| {
            app.focused_pane_agent_edit_lines(0)
                .map(|lines| lines.len())
        };

        assert_eq!(count(&app), Some(0));
        assert_eq!(count(&app), flattened_count(&app));

        record_edit(&mut app, pane_id, "a.rs", sample("first"));
        record_edit(&mut app, pane_id, "b.rs", sample("second"));

        assert_eq!(count(&app), flattened_count(&app));
        assert_eq!(count(&app), Some(sample("first").lines.len() * 2));

        // The `None` cases have to line up too, or the clamp would fall back
        // to 0 for a pane whose content still draws.
        assert_eq!(app.focused_pane_agent_edit_line_count(1), None);
        assert_eq!(app.focused_pane_agent_edit_lines(1), None);
    }
}
