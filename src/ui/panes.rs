use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::scrollbar::{render_pane_scrollbar, should_show_scrollbar};
#[cfg(test)]
use super::text::display_width;
use super::text::truncate_end;
use super::widgets::panel_contrast_fg;
use crate::app::state::Palette;
use crate::app::{AppState, Mode};
use crate::layout::{PaneId, PaneInfo};
use crate::popup_size::resolve_popup_geometry;
use crate::terminal::{TerminalRuntime, TerminalRuntimeRegistry};

pub(crate) fn pane_is_scrolled_back(rt: &TerminalRuntime) -> bool {
    rt.scroll_metrics()
        .is_some_and(|metrics| metrics.offset_from_bottom > 0)
}

// Terminal triview — restyling a Claude Code pane's own PTY output into a
// transcript zone, a cropped composer zone, and a new bottom zone carrying
// `crate::app::pane_command_log::PaneCommandLog` — is Claude-only and
// visual-only by design. Every other agent's pane, and every state where
// this pane's own shape cannot be confidently read this frame, renders
// through the unmodified `TerminalRuntime::render` path below: an explicit
// branch on agent identity and pane state, never an accident of detection
// quietly returning nothing.

/// Minimum spare rows below the composer for the bottom command-log zone to
/// be worth drawing — fewer than this would read as a sliver rather than a
/// real zone, so the pane falls back to a normal full render instead.
///
/// Claude Code's own composer always leaves exactly two rows below its own
/// bottom border in every observed state (idle and working alike): a
/// model/context/cost line, then a keybinding hint line. It pins these to the
/// literal bottom of the screen regardless of pane height, so "spare rows
/// below the composer" never exceeds 2 on a real live pane. A threshold of 3
/// here demanded one more row than a real Claude Code screen ever has,
/// meaning triview could never engage outside a synthetic test fixture.
const MIN_TRIVIEW_BOTTOM_ROWS: u16 = 2;

/// Minimum pane height even worth attempting triview detection against — a
/// cheap bailout before paying for a text read of the live grid.
const MIN_TRIVIEW_PANE_ROWS: u16 = 10;

fn pane_detected_agent(
    app: &AppState,
    ws_idx: usize,
    pane_id: PaneId,
) -> Option<crate::detect::Agent> {
    let ws = app.workspaces.get(ws_idx)?;
    let terminal_id = ws.terminal_id(pane_id)?;
    app.terminals.get(terminal_id)?.detected_agent
}

fn should_attempt_claude_triview(
    app: &AppState,
    ws_idx: usize,
    info: &PaneInfo,
    terminal_active: bool,
    rt: &TerminalRuntime,
) -> bool {
    info.is_focused
        && terminal_active
        && info.inner_rect.height >= MIN_TRIVIEW_PANE_ROWS
        && !pane_is_scrolled_back(rt)
        && pane_detected_agent(app, ws_idx, info.id) == Some(crate::detect::Agent::Claude)
}

/// Draws the herdr-owned parts of a triview pane: the two divider rows left
/// where Claude's own composer border was cropped out, and the command-log
/// bottom zone. The transcript and composer zones themselves were already
/// drawn straight from the live grid by
/// [`TerminalRuntime::render_claude_triview`] — this only fills what that
/// call deliberately left blank.
fn render_claude_triview_chrome(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    layout: crate::pane::ClaudeTriviewLayout,
) {
    let area = info.inner_rect;
    let divider_style = Style::default().fg(app.palette.surface_dim);

    let top_divider_y = area.y + layout.transcript_rows;
    render_triview_divider(frame, area, top_divider_y, divider_style);

    let bottom_divider_y = top_divider_y + 1 + layout.composer_rows;
    render_triview_divider(frame, area, bottom_divider_y, divider_style);

    let log_y = bottom_divider_y + 1;
    let pane_bottom = area.y + area.height;
    if log_y >= pane_bottom {
        return;
    }
    let log_area = Rect {
        x: area.x,
        y: log_y,
        width: area.width,
        height: pane_bottom - log_y,
    };
    render_pane_command_log(app, frame, info.id, log_area);
}

fn render_triview_divider(frame: &mut Frame, pane_area: Rect, y: u16, style: Style) {
    if y >= pane_area.y + pane_area.height || pane_area.width == 0 {
        return;
    }
    let rect = Rect {
        x: pane_area.x,
        y,
        width: pane_area.width,
        height: 1,
    };
    let rule = "─".repeat(pane_area.width as usize);
    frame.render_widget(Paragraph::new(rule).style(style), rect);
}

/// The bottom zone's own content: this pane's own recent shell commands,
/// oldest at the top, newest anchored to the zone's own bottom row so the
/// zone reads the same whether it holds one command or all
/// [`crate::app::pane_command_log::PANE_COMMAND_LOG_MAX`] of them.
fn render_pane_command_log(app: &AppState, frame: &mut Frame, pane_id: PaneId, area: Rect) {
    let commands: Vec<&str> = app.pane_command_log.lines(pane_id).collect();
    let visible_rows = area.height as usize;
    let shown = &commands[commands.len().saturating_sub(visible_rows)..];
    let pad_rows = visible_rows.saturating_sub(shown.len());

    let mut lines: Vec<Line> = Vec::with_capacity(visible_rows);
    lines.extend(std::iter::repeat_n(Line::from(""), pad_rows));
    lines.extend(shown.iter().map(|command| {
        Line::from(vec![
            Span::styled("● ", Style::default().fg(app.palette.accent)),
            Span::styled(
                (*command).to_string(),
                Style::default().fg(app.palette.subtext0),
            ),
        ])
    }));

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(app.palette.panel_bg)),
        area,
    );
}

fn pane_border_title(label: &str, pane_width: u16, _focused: bool) -> Option<String> {
    let label = label.trim();
    if label.is_empty() || pane_width <= 4 {
        return None;
    }
    let max_label_width = pane_width.saturating_sub(4) as usize;
    Some(format!(" {} ", truncate_end(label, max_label_width)))
}

fn stable_terminal_inner_rect(pane_inner: Rect, pane_scrollbars: bool) -> Rect {
    if !pane_scrollbars || pane_inner.width <= 4 {
        return pane_inner;
    }

    Rect::new(
        pane_inner.x,
        pane_inner.y,
        pane_inner.width.saturating_sub(1),
        pane_inner.height,
    )
}

pub(crate) fn pane_inner_rect(area: Rect, borders: Borders) -> Rect {
    if borders.is_empty() {
        area
    } else {
        Block::default().borders(borders).inner(area)
    }
}

fn ranges_overlap(a_start: u16, a_len: u16, b_start: u16, b_len: u16) -> bool {
    a_start < b_start.saturating_add(b_len) && b_start < a_start.saturating_add(a_len)
}

fn pane_to_right<'a>(info: &PaneInfo, panes: &'a [PaneInfo]) -> Option<&'a PaneInfo> {
    let right = info.rect.x.saturating_add(info.rect.width);
    panes.iter().find(|other| {
        other.id != info.id
            && other.rect.x == right
            && ranges_overlap(
                info.rect.y,
                info.rect.height,
                other.rect.y,
                other.rect.height,
            )
    })
}

fn pane_below<'a>(info: &PaneInfo, panes: &'a [PaneInfo]) -> Option<&'a PaneInfo> {
    let bottom = info.rect.y.saturating_add(info.rect.height);
    panes.iter().find(|other| {
        other.id != info.id
            && other.rect.y == bottom
            && ranges_overlap(info.rect.x, info.rect.width, other.rect.x, other.rect.width)
    })
}

fn shrink_for_one_cell_gap(size: u16) -> u16 {
    if size > 1 {
        size - 1
    } else {
        size
    }
}

pub(crate) fn apply_pane_chrome(
    panes: Vec<PaneInfo>,
    pane_borders: bool,
    pane_gaps: bool,
) -> Vec<PaneInfo> {
    let multi_pane = panes.len() > 1;
    panes
        .iter()
        .cloned()
        .map(|mut info| {
            let right_neighbor = multi_pane.then(|| pane_to_right(&info, &panes)).flatten();
            let below_neighbor = multi_pane.then(|| pane_below(&info, &panes)).flatten();

            if multi_pane && pane_gaps && !pane_borders {
                if right_neighbor.is_some() {
                    info.rect.width = shrink_for_one_cell_gap(info.rect.width);
                }
                if below_neighbor.is_some() {
                    info.rect.height = shrink_for_one_cell_gap(info.rect.height);
                }
            }

            info.borders = if !multi_pane || !pane_borders {
                Borders::NONE
            } else {
                let mut borders = Borders::ALL;
                if !pane_gaps {
                    if right_neighbor.is_some() {
                        borders.remove(Borders::RIGHT);
                    }
                    if below_neighbor.is_some() {
                        borders.remove(Borders::BOTTOM);
                    }
                }
                borders
            };
            info
        })
        .collect()
}

fn runtime_for_tab_pane<'a>(
    terminal_runtimes: &'a TerminalRuntimeRegistry,
    tab: &'a crate::workspace::Tab,
    pane_id: crate::layout::PaneId,
) -> Option<(&'a crate::terminal::TerminalId, &'a TerminalRuntime)> {
    let terminal_id = tab.terminal_id(pane_id)?;
    #[cfg(test)]
    if let Some(runtime) = tab.runtimes.get(&pane_id) {
        return Some((terminal_id, runtime));
    }
    terminal_runtimes
        .get(terminal_id)
        .map(|runtime| (terminal_id, runtime))
}

/// What a pane's runtime needs resized this frame, and what it is sized to now.
///
/// Both come off the same runtime borrow, because resolving the resize needs
/// `&mut AppState` and so cannot hold one. `grid` is what keeps
/// [`crate::app::pane_resize_reflow`] honest about panes something else already
/// resized — every tab that is not the active one, on every frame.
#[derive(Clone, Copy)]
struct PaneResizePlan {
    target: (u16, u16),
    grid: (u16, u16),
}

fn stable_scrollbar_gutter(
    rt: &TerminalRuntime,
    pane_inner: Rect,
    pane_scrollbars: bool,
) -> (Rect, Option<Rect>) {
    let inner_rect = stable_terminal_inner_rect(pane_inner, pane_scrollbars);
    if inner_rect == pane_inner {
        return (inner_rect, None);
    }
    let gutter = Rect::new(
        pane_inner.x + pane_inner.width.saturating_sub(1),
        pane_inner.y,
        1,
        pane_inner.height,
    );
    let scrollbar_rect = rt
        .scroll_metrics()
        .filter(|metrics| should_show_scrollbar(*metrics))
        .map(|_| gutter);

    (inner_rect, scrollbar_rect)
}

/// Resize every visible runtime in a tab to the geometry it would receive if the tab were selected.
pub(super) fn resize_tab_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    tab: &crate::workspace::Tab,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let multi_pane = tab.layout.pane_count() > 1;

    if tab.zoomed {
        let focused_id = tab.layout.focused();
        if let Some((terminal_id, rt)) = runtime_for_tab_pane(terminal_runtimes, tab, focused_id) {
            let borders = if multi_pane && app.pane_borders {
                Borders::ALL
            } else {
                Borders::NONE
            };
            let pane_inner = pane_inner_rect(area, borders);
            let inner_rect = stable_terminal_inner_rect(pane_inner, app.pane_scrollbars);
            if !app.direct_attach_resize_locks.contains(terminal_id) {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
        return;
    }

    for info in apply_pane_chrome(tab.layout.panes(area), app.pane_borders, app.pane_gaps) {
        let pane_inner = pane_inner_rect(info.rect, info.borders);

        if let Some((terminal_id, rt)) = runtime_for_tab_pane(terminal_runtimes, tab, info.id) {
            let inner_rect = stable_terminal_inner_rect(pane_inner, app.pane_scrollbars);
            if !app.direct_attach_resize_locks.contains(terminal_id) {
                rt.resize(
                    inner_rect.height,
                    inner_rect.width,
                    cell_size.width_px,
                    cell_size.height_px,
                );
            }
        }
    }
}

/// Compute pane layout info and optionally resize pane runtimes to match.
pub(super) fn compute_pane_infos(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> Vec<PaneInfo> {
    let Some(ws_idx) = app.active else {
        return Vec::new();
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return Vec::new();
    };

    let multi_pane = ws.layout.pane_count() > 1;

    if ws.zoomed {
        let focused_id = ws.layout.focused();
        let borders = if multi_pane && app.pane_borders {
            Borders::ALL
        } else {
            Borders::NONE
        };
        let pane_inner = pane_inner_rect(area, borders);
        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        let mut resize_plan: Option<PaneResizePlan> = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, focused_id) {
            (inner_rect, scrollbar_rect) =
                stable_scrollbar_gutter(rt, pane_inner, app.pane_scrollbars);
            resize_plan = Some(PaneResizePlan {
                target: (inner_rect.height, inner_rect.width),
                grid: rt.current_size(),
            });
        }
        let locked_terminal_id = ws.terminal_id(focused_id).cloned();
        if let (true, Some(plan), Some(terminal_id)) =
            (resize_panes, resize_plan, locked_terminal_id)
        {
            if !app.direct_attach_resize_locks.contains(&terminal_id) {
                let (target_rows, target_cols) = plan.target;
                let (eased_rows, eased_cols) = app.pane_resize_reflow.resolve(
                    terminal_id,
                    target_rows,
                    target_cols,
                    plan.grid,
                    std::time::Instant::now(),
                );
                if let Some(rt) =
                    app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, focused_id)
                {
                    rt.resize(
                        eased_rows,
                        eased_cols,
                        cell_size.width_px,
                        cell_size.height_px,
                    );
                }
                inner_rect.height = eased_rows;
                inner_rect.width = eased_cols;
            }
        }
        return vec![PaneInfo {
            id: focused_id,
            rect: area,
            inner_rect,
            scrollbar_rect,
            borders,
            is_focused: true,
        }];
    }

    let mut pane_infos = apply_pane_chrome(ws.layout.panes(area), app.pane_borders, app.pane_gaps);

    for info in &mut pane_infos {
        let pane_inner = pane_inner_rect(info.rect, info.borders);

        let mut inner_rect = pane_inner;
        let mut scrollbar_rect = None;
        let mut resize_plan: Option<PaneResizePlan> = None;
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            (inner_rect, scrollbar_rect) =
                stable_scrollbar_gutter(rt, pane_inner, app.pane_scrollbars);
            resize_plan = Some(PaneResizePlan {
                target: (inner_rect.height, inner_rect.width),
                grid: rt.current_size(),
            });
        }
        let locked_terminal_id = ws.terminal_id(info.id).cloned();
        if let (true, Some(plan), Some(terminal_id)) =
            (resize_panes, resize_plan, locked_terminal_id)
        {
            if !app.direct_attach_resize_locks.contains(&terminal_id) {
                let (target_rows, target_cols) = plan.target;
                let (eased_rows, eased_cols) = app.pane_resize_reflow.resolve(
                    terminal_id,
                    target_rows,
                    target_cols,
                    plan.grid,
                    std::time::Instant::now(),
                );
                if let Some(rt) =
                    app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
                {
                    rt.resize(
                        eased_rows,
                        eased_cols,
                        cell_size.width_px,
                        cell_size.height_px,
                    );
                }
                inner_rect.height = eased_rows;
                inner_rect.width = eased_cols;
            }
        }

        info.inner_rect = inner_rect;
        info.scrollbar_rect = scrollbar_rect;
    }

    pane_infos
}

pub(super) fn render_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    pane_infos: &[PaneInfo],
    split_borders: &[crate::layout::SplitBorder],
) {
    let Some(ws_idx) = app.active else {
        return;
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return;
    };

    let multi_pane = ws.layout.pane_count() > 1;
    let terminal_active = app.mode == Mode::Terminal;

    for info in pane_infos {
        if let Some(rt) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id) {
            let show_cursor = info.is_focused
                && terminal_active
                && !pane_is_scrolled_back(rt)
                && app.pane_exposes_host_cursor(ws_idx, info.id);

            let triview = should_attempt_claude_triview(app, ws_idx, info, terminal_active, rt)
                .then(|| {
                    rt.render_claude_triview(
                        frame,
                        info.inner_rect,
                        show_cursor,
                        MIN_TRIVIEW_BOTTOM_ROWS,
                    )
                })
                .flatten();
            match triview {
                Some(layout) => render_claude_triview_chrome(app, frame, info, layout),
                None => rt.render(frame, info.inner_rect, show_cursor),
            }
            render_pane_scrollbar(app, frame, info, rt);

            let should_dim = !info.is_focused && multi_pane && !terminal_active;
            if should_dim {
                let inner = info.inner_rect;
                let buf = frame.buffer_mut();
                for y in inner.y..inner.y + inner.height {
                    for x in inner.x..inner.x + inner.width {
                        let cell = &mut buf[(x, y)];
                        cell.set_style(cell.style().add_modifier(Modifier::DIM));
                    }
                }
            }

            // Selection and copy-mode search highlighting assume the drawn
            // grid maps linearly onto `info.inner_rect`, which the composer
            // zone above no longer does once its border chrome is cropped
            // out. Skipped here rather than drawn wrong; revisit if triview
            // panes need mouse selection.
            if triview.is_none() {
                let (copy_search_top, copy_search_bottom, copy_search_matches) =
                    validated_copy_mode_search_matches(app, info, rt);
                render_copy_mode_search_highlights(
                    app,
                    frame,
                    info,
                    copy_search_top,
                    copy_search_bottom,
                    &copy_search_matches,
                    false,
                );
                render_selection_highlight(
                    &app.selection,
                    frame,
                    info.id,
                    info.inner_rect,
                    rt.scroll_metrics(),
                    &app.palette,
                    app.host_terminal_theme,
                );
                render_copy_mode_search_highlights(
                    app,
                    frame,
                    info,
                    copy_search_top,
                    copy_search_bottom,
                    &copy_search_matches,
                    true,
                );
                render_copy_mode_cursor(app, frame, info);
            }
        }
    }

    render_pane_borders(app, ws, pane_infos, split_borders, frame);
}

pub(crate) fn popup_pane_rects(app: &AppState, area: Rect) -> Option<(Rect, Rect)> {
    let popup = app.popup_pane.as_ref()?;
    resolve_popup_geometry(popup.width, popup.height, area)
        .map(|geometry| (geometry.outer, geometry.inner))
}

pub(super) fn resize_popup_pane(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let Some(popup) = app.popup_pane.as_ref() else {
        return;
    };
    let Some((_outer, inner)) = popup_pane_rects(app, area) else {
        return;
    };
    if app.direct_attach_resize_locks.contains(&popup.terminal_id) {
        return;
    }
    if let Some(rt) = terminal_runtimes.get(&popup.terminal_id) {
        rt.resize(
            inner.height,
            inner.width,
            cell_size.width_px,
            cell_size.height_px,
        );
    }
}

pub(super) fn render_popup_pane(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    frame: &mut Frame,
    area: Rect,
) {
    let Some(popup) = app.popup_pane.as_ref() else {
        return;
    };
    let Some((outer, inner)) = popup_pane_rects(app, area) else {
        return;
    };
    let Some(rt) = terminal_runtimes.get(&popup.terminal_id) else {
        return;
    };
    let title = app
        .terminals
        .get(&popup.terminal_id)
        .and_then(|terminal| terminal.manual_label.as_deref())
        .unwrap_or("popup");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.palette.accent))
        .title(pane_border_title(title, outer.width, true).unwrap_or_default())
        .style(Style::default().bg(app.palette.panel_bg));
    frame.render_widget(Clear, outer);
    frame.render_widget(block, outer);
    rt.render(frame, inner, !pane_is_scrolled_back(rt));
}

#[derive(Clone, Copy, Default)]
struct LineCell {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

fn render_pane_borders(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    pane_infos: &[PaneInfo],
    split_borders: &[crate::layout::SplitBorder],
    frame: &mut Frame,
) {
    if !app.pane_borders || pane_infos.iter().all(|info| info.borders.is_empty()) {
        return;
    }

    let mut cells = std::collections::HashMap::<(u16, u16), LineCell>::new();
    for info in pane_infos {
        add_pane_border_cells(&mut cells, info);
    }
    add_split_border_cells(app.pane_gaps, split_borders, &mut cells);

    let buf = frame.buffer_mut();
    let area = buf.area;
    for ((x, y), line) in cells {
        if x < area.x
            || x >= area.x.saturating_add(area.width)
            || y < area.y
            || y >= area.y.saturating_add(area.height)
        {
            continue;
        }
        let focused = pane_infos
            .iter()
            .any(|info| info.is_focused && line_touches_pane(x, y, info, app.pane_gaps));
        let symbol = line_cell_symbol(line);
        if symbol.is_empty() {
            continue;
        }
        let cell = &mut buf[(x, y)];
        cell.set_symbol(symbol);
        let color = if focused {
            app.palette.accent
        } else {
            app.palette.overlay0
        };
        cell.set_style(Style::default().fg(color));
    }

    render_pane_border_titles(app, ws, pane_infos, frame);
}

fn add_split_border_cells(
    pane_gaps: bool,
    split_borders: &[crate::layout::SplitBorder],
    cells: &mut std::collections::HashMap<(u16, u16), LineCell>,
) {
    if pane_gaps {
        return;
    }

    for split in split_borders {
        match split.direction {
            ratatui::layout::Direction::Horizontal => {
                let x = split.pos;
                let end = split.area.y.saturating_add(split.area.height);
                for y in split.area.y..=end {
                    if !cells.contains_key(&(x, y)) {
                        continue;
                    }
                    let left = x
                        .checked_sub(1)
                        .and_then(|left_x| cells.get(&(left_x, y)))
                        .is_some_and(|cell| cell.left || cell.right);
                    let right = cells
                        .get(&(x.saturating_add(1), y))
                        .is_some_and(|cell| cell.left || cell.right);
                    let cell = cells.entry((x, y)).or_default();
                    cell.up |= y > split.area.y;
                    cell.down |= y + 1 < end;
                    cell.left |= left;
                    cell.right |= right;
                }
            }
            ratatui::layout::Direction::Vertical => {
                let y = split.pos;
                let end = split.area.x.saturating_add(split.area.width);
                for x in split.area.x..=end {
                    if !cells.contains_key(&(x, y)) {
                        continue;
                    }
                    let up = y
                        .checked_sub(1)
                        .and_then(|up_y| cells.get(&(x, up_y)))
                        .is_some_and(|cell| cell.up || cell.down);
                    let down = cells
                        .get(&(x, y.saturating_add(1)))
                        .is_some_and(|cell| cell.up || cell.down);
                    let cell = cells.entry((x, y)).or_default();
                    cell.left |= x > split.area.x;
                    cell.right |= x + 1 < end;
                    cell.up |= up;
                    cell.down |= down;
                }
            }
        }
    }
}

fn add_pane_border_cells(
    cells: &mut std::collections::HashMap<(u16, u16), LineCell>,
    info: &PaneInfo,
) {
    let rect = info.rect;
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let right = rect.x.saturating_add(rect.width).saturating_sub(1);
    let bottom = rect.y.saturating_add(rect.height).saturating_sub(1);

    if info.borders.contains(Borders::TOP) {
        for x in rect.x..=right {
            let cell = cells.entry((x, rect.y)).or_default();
            cell.left |= x > rect.x;
            cell.right |= x < right;
        }
    }
    if info.borders.contains(Borders::BOTTOM) {
        for x in rect.x..=right {
            let cell = cells.entry((x, bottom)).or_default();
            cell.left |= x > rect.x;
            cell.right |= x < right;
        }
    }
    if info.borders.contains(Borders::LEFT) {
        for y in rect.y..=bottom {
            let cell = cells.entry((rect.x, y)).or_default();
            cell.up |= y > rect.y;
            cell.down |= y < bottom;
        }
    }
    if info.borders.contains(Borders::RIGHT) {
        for y in rect.y..=bottom {
            let cell = cells.entry((right, y)).or_default();
            cell.up |= y > rect.y;
            cell.down |= y < bottom;
        }
    }
}

fn line_touches_pane(x: u16, y: u16, info: &PaneInfo, pane_gaps: bool) -> bool {
    let rect = info.rect;
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    let right = rect.x.saturating_add(rect.width).saturating_sub(1);
    let bottom = rect.y.saturating_add(rect.height).saturating_sub(1);
    let in_rows = y >= rect.y && y <= bottom;
    let in_cols = x >= rect.x && x <= right;
    let own_border =
        (in_rows && (x == rect.x || x == right)) || (in_cols && (y == rect.y || y == bottom));

    if pane_gaps {
        return own_border;
    }

    let shared_right = rect.x.saturating_add(rect.width);
    let shared_bottom = rect.y.saturating_add(rect.height);
    own_border
        || (in_rows && x == shared_right)
        || (in_cols && y == shared_bottom)
        || (x == shared_right && y == shared_bottom)
}

fn render_pane_border_titles(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    pane_infos: &[PaneInfo],
    frame: &mut Frame,
) {
    let buf = frame.buffer_mut();
    let area = buf.area;
    for info in pane_infos {
        if !info.borders.contains(Borders::TOP) || info.rect.width <= 4 {
            continue;
        }
        let Some(title) = ws
            .pane_state(info.id)
            .and_then(|pane| app.terminals.get(&pane.attached_terminal_id))
            .and_then(|terminal| terminal.border_label(app.show_agent_labels_on_pane_borders))
            .and_then(|label| pane_border_title(&label, info.rect.width, info.is_focused))
        else {
            continue;
        };
        let y = info.rect.y;
        if y < area.y || y >= area.y.saturating_add(area.height) {
            continue;
        }
        let start_x = info.rect.x.saturating_add(1);
        let end_x = info
            .rect
            .x
            .saturating_add(info.rect.width)
            .saturating_sub(1)
            .min(area.x.saturating_add(area.width));
        if start_x >= end_x {
            continue;
        }
        let color = if info.is_focused {
            app.palette.accent
        } else {
            app.palette.overlay0
        };
        let mut style = Style::default().fg(color);
        if info.is_focused {
            style = style.add_modifier(Modifier::BOLD);
        }
        buf.set_stringn(
            start_x,
            y,
            title,
            end_x.saturating_sub(start_x) as usize,
            style,
        );
    }
}

fn line_cell_symbol(line: LineCell) -> &'static str {
    match (line.up, line.down, line.left, line.right) {
        (true, true, true, true) => "┼",
        (true, true, true, false) => "┤",
        (true, true, false, true) => "├",
        (true, false, true, true) => "┴",
        (false, true, true, true) => "┬",
        (true, true, false, false) | (true, false, false, false) | (false, true, false, false) => {
            "│"
        }
        (false, false, true, true) | (false, false, true, false) | (false, false, false, true) => {
            "─"
        }
        (false, true, false, true) => "┌",
        (false, true, true, false) => "┐",
        (true, false, false, true) => "└",
        (true, false, true, false) => "┘",
        _ => "",
    }
}

fn render_copy_mode_cursor(app: &AppState, frame: &mut Frame, info: &PaneInfo) {
    if app.mode != Mode::Copy {
        return;
    }
    let Some(copy_mode) = app.copy_mode.as_ref() else {
        return;
    };
    if copy_mode.pane_id != info.id
        || copy_mode.cursor_row >= info.inner_rect.height
        || copy_mode.cursor_col >= info.inner_rect.width
    {
        return;
    }

    let x = info.inner_rect.x + copy_mode.cursor_col;
    let y = info.inner_rect.y + copy_mode.cursor_row;
    let cell = &mut frame.buffer_mut()[(x, y)];
    cell.set_style(
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD),
    );
}

fn validated_copy_mode_search_matches(
    app: &AppState,
    info: &PaneInfo,
    rt: &crate::terminal::TerminalRuntime,
) -> (u32, u32, Vec<(usize, crate::pane::TerminalTextMatch)>) {
    let Some(copy_mode) = app.copy_mode.as_ref() else {
        return (0, 0, Vec::new());
    };
    if copy_mode.pane_id != info.id {
        return (0, 0, Vec::new());
    }
    let Some(metrics) = rt.scroll_metrics() else {
        return (0, 0, Vec::new());
    };
    let top = metrics
        .max_offset_from_bottom
        .saturating_sub(metrics.offset_from_bottom)
        .min(u32::MAX as usize) as u32;
    let bottom = top.saturating_add(u32::from(info.inner_rect.height.saturating_sub(1)));
    let first_visible = copy_mode
        .search
        .matches
        .partition_point(|text_match| text_match.end.row < top);
    let visible = &copy_mode.search.matches[first_visible..];
    let visible_len = visible.partition_point(|text_match| text_match.start.row <= bottom);
    let candidates = visible[..visible_len].to_vec();
    let validity = rt.text_matches_are_current(&candidates);

    let matches = candidates
        .into_iter()
        .zip(validity)
        .enumerate()
        .filter_map(|(offset, (text_match, is_current))| {
            is_current.then_some((first_visible + offset, text_match))
        })
        .collect();
    (top, bottom, matches)
}

fn render_copy_mode_search_highlights(
    app: &AppState,
    frame: &mut Frame,
    info: &PaneInfo,
    top: u32,
    bottom: u32,
    matches: &[(usize, crate::pane::TerminalTextMatch)],
    current_only: bool,
) {
    let Some(copy_mode) = app.copy_mode.as_ref() else {
        return;
    };
    let current = copy_mode.search.current;
    let style = if current_only {
        Style::default()
            .fg(panel_contrast_fg(&app.palette))
            .bg(app.palette.accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface1)
    };

    for &(index, text_match) in matches {
        if (current == Some(index)) != current_only {
            continue;
        }
        let start_row = text_match.start.row.max(top);
        let end_row = text_match.end.row.min(bottom);
        for absolute_row in start_row..=end_row {
            let viewport_row = absolute_row.saturating_sub(top) as u16;
            let start_col = if absolute_row == text_match.start.row {
                text_match.start.col
            } else {
                0
            };
            let end_col = if absolute_row == text_match.end.row {
                text_match.end.col
            } else {
                info.inner_rect.width.saturating_sub(1)
            };
            for col in start_col..=end_col.min(info.inner_rect.width.saturating_sub(1)) {
                let x = info.inner_rect.x.saturating_add(col);
                let y = info.inner_rect.y.saturating_add(viewport_row);
                frame.buffer_mut()[(x, y)].set_style(style);
            }
        }
    }
}

fn render_selection_highlight(
    selection: &Option<crate::selection::Selection>,
    frame: &mut Frame,
    pane_id: crate::layout::PaneId,
    inner: Rect,
    scroll_metrics: Option<crate::pane::ScrollMetrics>,
    p: &Palette,
    host_theme: crate::terminal_theme::TerminalTheme,
) {
    if let Some(sel) = selection {
        if sel.is_visible() && sel.pane_id == pane_id {
            let buf = frame.buffer_mut();
            let style = automatic_selection_style(p, host_theme);
            for y in 0..inner.height {
                for x in 0..inner.width {
                    if sel.contains(y, x, scroll_metrics) {
                        let cell = &mut buf[(inner.x + x, inner.y + y)];
                        cell.set_style(style);
                    }
                }
            }
        }
    }
}

use super::color::{color_to_rgb, mix_rgb, relative_luminance, terminal_theme_to_rgb};

fn automatic_selection_style(
    p: &Palette,
    host_theme: crate::terminal_theme::TerminalTheme,
) -> Style {
    let bg = automatic_selection_bg(p, host_theme);
    Style::reset().fg(selection_fg_for_bg(bg, p)).bg(bg)
}

fn automatic_selection_bg(p: &Palette, host_theme: crate::terminal_theme::TerminalTheme) -> Color {
    let Some(background) = host_theme.background.map(terminal_theme_to_rgb) else {
        return selection_palette_background(p);
    };

    let target = if relative_luminance(background) < 0.5 {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    };
    let selected = mix_rgb(background, target, 0.28);
    Color::Rgb(selected.0, selected.1, selected.2)
}

fn selection_palette_background(p: &Palette) -> Color {
    if p.panel_bg == Color::Reset {
        p.surface_dim
    } else {
        p.panel_bg
    }
}

fn selection_fg_for_bg(bg: Color, p: &Palette) -> Color {
    color_to_rgb(bg)
        .map(|bg| {
            if relative_luminance(bg) < 0.5 {
                Color::White
            } else {
                Color::Black
            }
        })
        .unwrap_or_else(|| panel_contrast_fg(p))
}

pub(super) fn render_empty(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(Span::styled(
            "  No workspaces yet",
            Style::default().fg(p.overlay0),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  A workspace is one project context.",
            Style::default().fg(p.overlay1),
        )),
        Line::from(Span::styled(
            "  Its root pane (top-left) sets the default repo or folder name.",
            Style::default().fg(p.overlay1),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Press ", Style::default().fg(p.overlay0)),
            Span::styled(
                app.keybinds
                    .new_workspace
                    .label()
                    .unwrap_or_else(|| "unset".to_string()),
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to create one", Style::default().fg(p.overlay0)),
        ]),
    ];
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(p.surface_dim)),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::PaneId;
    use crate::selection::Selection;
    use crate::terminal::TerminalRuntime;
    use crate::terminal::TerminalState;
    use crate::workspace::Workspace;

    fn render_view_pane_borders(app: &AppState, ws: &Workspace, frame: &mut Frame) {
        render_pane_borders(
            app,
            ws,
            &app.view.pane_infos,
            &app.view.split_borders,
            frame,
        );
    }

    #[test]
    fn pane_detected_agent_returns_the_pane_s_own_terminal_agent() {
        let mut app = AppState::test_new();

        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let terminal_id = ws.tabs[0].panes[&pane_id].attached_terminal_id.clone();

        let mut terminal_state = TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal_state.detected_agent = Some(crate::detect::Agent::Claude);
        app.terminals.insert(terminal_id, terminal_state);
        app.workspaces = vec![ws];

        assert_eq!(
            pane_detected_agent(&app, 0, pane_id),
            Some(crate::detect::Agent::Claude)
        );
    }

    #[test]
    fn pane_detected_agent_is_none_for_an_unknown_pane() {
        let mut app = AppState::test_new();
        app.workspaces = vec![Workspace::test_new("test")];
        let bogus_pane = crate::layout::PaneId::from_raw(999_999);
        assert_eq!(pane_detected_agent(&app, 0, bogus_pane), None);
    }

    #[tokio::test]
    async fn should_attempt_claude_triview_requires_focus_terminal_mode_and_claude_agent() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;

        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let terminal_id = ws.tabs[0].panes[&pane_id].attached_terminal_id.clone();
        let mut terminal_state = TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal_state.detected_agent = Some(crate::detect::Agent::Claude);
        app.terminals.insert(terminal_id.clone(), terminal_state);
        app.workspaces = vec![ws];

        let rt = TerminalRuntime::test_with_scrollback_bytes(
            40,
            MIN_TRIVIEW_PANE_ROWS,
            1024,
            b"ready\n",
        );

        let mut focused_info = PaneInfo {
            id: pane_id,
            rect: Rect::new(0, 0, 40, MIN_TRIVIEW_PANE_ROWS),
            inner_rect: Rect::new(0, 0, 40, MIN_TRIVIEW_PANE_ROWS),
            scrollbar_rect: None,
            borders: Borders::NONE,
            is_focused: true,
        };
        assert!(should_attempt_claude_triview(
            &app,
            0,
            &focused_info,
            true,
            &rt
        ));

        // Not focused: no other pane should ever get the special treatment.
        let mut unfocused_info = focused_info.clone();
        unfocused_info.is_focused = false;
        assert!(!should_attempt_claude_triview(
            &app,
            0,
            &unfocused_info,
            true,
            &rt
        ));

        // Not in terminal mode.
        assert!(!should_attempt_claude_triview(
            &app,
            0,
            &focused_info,
            false,
            &rt
        ));

        // Too short a pane to bother.
        focused_info.inner_rect.height = MIN_TRIVIEW_PANE_ROWS - 1;
        assert!(!should_attempt_claude_triview(
            &app,
            0,
            &focused_info,
            true,
            &rt
        ));
    }

    #[test]
    fn pane_border_title_trims_and_truncates() {
        assert_eq!(
            pane_border_title(" claude ", 20, false).as_deref(),
            Some(" claude ")
        );
        assert_eq!(
            pane_border_title(" claude ", 20, true).as_deref(),
            Some(" claude ")
        );
        assert_eq!(pane_border_title("", 20, false), None);
        assert_eq!(
            pane_border_title("abcdef", 8, false).as_deref(),
            Some(" abc… ")
        );
        assert_eq!(
            pane_border_title("abcdef", 8, true).as_deref(),
            Some(" abc… ")
        );
        assert_eq!(pane_border_title("abcdef", 4, false), None);
    }

    #[test]
    fn pane_border_title_truncates_cjk_by_display_width() {
        let title = pane_border_title("1 模块组织（已定）", 12, false).unwrap();

        assert_eq!(title, " 1 模块… ");
        assert!(display_width(title.as_str()) <= 10);
    }

    #[test]
    fn pane_border_renderer_places_adjacent_cjk_by_display_width() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.view.terminal_area = Rect::new(0, 0, 12, 3);
        let ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        app.view.pane_infos = vec![PaneInfo {
            id: pane_id,
            rect: Rect::new(0, 0, 12, 3),
            inner_rect: Rect::default(),
            scrollbar_rect: None,
            borders: Borders::ALL,
            is_focused: false,
        }];

        let terminal_id = ws.tabs[0].panes[&pane_id].attached_terminal_id.clone();
        let mut terminal_state = TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal_state.set_manual_label("1 模块组织（已定）".into());
        app.terminals.insert(terminal_id, terminal_state);

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 3)).unwrap();
        terminal
            .draw(|frame| render_view_pane_borders(&app, &ws, frame))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(4, 0)].symbol(), "模");
        assert_eq!(buffer[(5, 0)].symbol(), " ");
        assert_eq!(buffer[(6, 0)].symbol(), "块");
    }

    #[test]
    fn default_horizontal_split_uses_one_shared_divider_column() {
        let mut workspace = Workspace::test_new("test");
        let root = workspace.tabs[0].root_pane;
        let right = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(root);

        let infos = apply_pane_chrome(
            workspace.tabs[0].layout.panes(Rect::new(0, 0, 100, 20)),
            true,
            false,
        );
        let left = infos.iter().find(|info| info.id == root).unwrap();
        let right = infos.iter().find(|info| info.id == right).unwrap();

        assert_eq!(left.rect.x + left.rect.width, right.rect.x);
        assert!(!left.borders.contains(Borders::RIGHT));
        assert!(right.borders.contains(Borders::LEFT));
    }

    #[test]
    fn default_vertical_split_uses_one_shared_divider_row() {
        let mut workspace = Workspace::test_new("test");
        let root = workspace.tabs[0].root_pane;
        let bottom = workspace.test_split(ratatui::layout::Direction::Vertical);
        workspace.tabs[0].layout.focus_pane(root);

        let infos = apply_pane_chrome(
            workspace.tabs[0].layout.panes(Rect::new(0, 0, 100, 20)),
            true,
            false,
        );
        let top = infos.iter().find(|info| info.id == root).unwrap();
        let bottom = infos.iter().find(|info| info.id == bottom).unwrap();

        assert_eq!(top.rect.y + top.rect.height, bottom.rect.y);
        assert!(!top.borders.contains(Borders::BOTTOM));
        assert!(bottom.borders.contains(Borders::TOP));
    }

    #[test]
    fn pane_gaps_keep_independent_bordered_panes() {
        let mut workspace = Workspace::test_new("test");
        let root = workspace.tabs[0].root_pane;
        let right = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(root);

        let infos = apply_pane_chrome(
            workspace.tabs[0].layout.panes(Rect::new(0, 0, 100, 20)),
            true,
            true,
        );
        let left = infos.iter().find(|info| info.id == root).unwrap();
        let right = infos.iter().find(|info| info.id == right).unwrap();

        assert_eq!(left.rect.x + left.rect.width, right.rect.x);
        assert_eq!(left.borders, Borders::ALL);
        assert_eq!(right.borders, Borders::ALL);
    }

    #[test]
    fn borderless_pane_gaps_add_one_empty_cell_between_panes() {
        let mut workspace = Workspace::test_new("test");
        let root = workspace.tabs[0].root_pane;
        let right = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.tabs[0].layout.focus_pane(root);

        let infos = apply_pane_chrome(
            workspace.tabs[0].layout.panes(Rect::new(0, 0, 100, 20)),
            false,
            true,
        );
        let left = infos.iter().find(|info| info.id == root).unwrap();
        let right = infos.iter().find(|info| info.id == right).unwrap();

        assert_eq!(left.rect, Rect::new(0, 0, 49, 20));
        assert_eq!(right.rect, Rect::new(50, 0, 50, 20));
        assert!(left.borders.is_empty());
        assert!(right.borders.is_empty());
    }

    #[test]
    fn disabled_pane_borders_make_inner_rect_equal_visual_rect() {
        let mut workspace = Workspace::test_new("test");
        workspace.test_split(ratatui::layout::Direction::Horizontal);

        let infos = apply_pane_chrome(
            workspace.tabs[0].layout.panes(Rect::new(0, 0, 100, 20)),
            false,
            false,
        );

        for info in infos {
            assert!(info.borders.is_empty());
            assert_eq!(pane_inner_rect(info.rect, info.borders), info.rect);
        }
    }

    #[test]
    fn global_pane_border_renderer_composes_junctions_and_focus_style() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.view.terminal_area = Rect::new(0, 0, 4, 4);
        app.view.pane_infos = vec![
            PaneInfo {
                id: PaneId::from_raw(1),
                rect: Rect::new(0, 0, 2, 2),
                inner_rect: Rect::default(),
                scrollbar_rect: None,
                borders: Borders::TOP | Borders::LEFT,
                is_focused: true,
            },
            PaneInfo {
                id: PaneId::from_raw(2),
                rect: Rect::new(2, 0, 2, 2),
                inner_rect: Rect::default(),
                scrollbar_rect: None,
                borders: Borders::TOP | Borders::LEFT | Borders::RIGHT,
                is_focused: false,
            },
            PaneInfo {
                id: PaneId::from_raw(3),
                rect: Rect::new(0, 2, 2, 2),
                inner_rect: Rect::default(),
                scrollbar_rect: None,
                borders: Borders::TOP | Borders::LEFT | Borders::BOTTOM,
                is_focused: false,
            },
            PaneInfo {
                id: PaneId::from_raw(4),
                rect: Rect::new(2, 2, 2, 2),
                inner_rect: Rect::default(),
                scrollbar_rect: None,
                borders: Borders::ALL,
                is_focused: false,
            },
        ];
        app.view.split_borders = vec![
            crate::layout::SplitBorder {
                pos: 2,
                direction: ratatui::layout::Direction::Horizontal,
                ratio: 0.5,
                area: Rect::new(0, 0, 4, 4),
                path: vec![],
            },
            crate::layout::SplitBorder {
                pos: 2,
                direction: ratatui::layout::Direction::Vertical,
                ratio: 0.5,
                area: Rect::new(0, 0, 4, 4),
                path: vec![false],
            },
        ];
        let ws = Workspace::test_new("test");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(4, 4)).unwrap();

        terminal
            .draw(|frame| render_view_pane_borders(&app, &ws, frame))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 2)].symbol(), "┼");
        assert_eq!(buffer[(2, 2)].style().fg, Some(app.palette.accent));
        assert_eq!(buffer[(2, 1)].symbol(), "│");
        assert_eq!(buffer[(2, 1)].style().fg, Some(app.palette.accent));
    }

    #[test]
    fn gapped_pane_focus_does_not_color_neighbor_border() {
        let mut app = AppState::test_new();
        app.mode = Mode::Terminal;
        app.pane_gaps = true;
        app.view.terminal_area = Rect::new(0, 0, 4, 3);
        app.view.pane_infos = vec![
            PaneInfo {
                id: PaneId::from_raw(1),
                rect: Rect::new(0, 0, 2, 3),
                inner_rect: Rect::default(),
                scrollbar_rect: None,
                borders: Borders::ALL,
                is_focused: true,
            },
            PaneInfo {
                id: PaneId::from_raw(2),
                rect: Rect::new(2, 0, 2, 3),
                inner_rect: Rect::default(),
                scrollbar_rect: None,
                borders: Borders::ALL,
                is_focused: false,
            },
        ];
        let ws = Workspace::test_new("test");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(4, 3)).unwrap();

        terminal
            .draw(|frame| render_view_pane_borders(&app, &ws, frame))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].style().fg, Some(app.palette.accent));
        assert_eq!(buffer[(2, 1)].style().fg, Some(app.palette.overlay0));
    }

    #[tokio::test]
    async fn pane_scrollbar_gutter_is_reserved_before_scrollback_exists() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &mut app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(10, 3, 39, 8));
    }

    #[tokio::test]
    async fn zoomed_pane_scrollbar_gutter_is_reserved_before_scrollback_exists() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        workspace.zoomed = true;
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &mut app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(10, 3, 39, 8));
    }

    #[tokio::test]
    async fn zoomed_multi_pane_keeps_border_space() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let focused_pane = workspace.test_split(ratatui::layout::Direction::Horizontal);
        workspace.zoomed = true;
        workspace.tabs[0].runtimes.insert(
            focused_pane,
            TerminalRuntime::test_with_scrollback_bytes(40, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &mut app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.id, focused_pane);
        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, Rect::new(11, 4, 37, 6));
    }

    #[tokio::test]
    async fn tiny_pane_does_not_reserve_scrollbar_gutter() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(4, 8, 1024, b"ready\n"),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 4, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &mut app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, area);
    }

    #[tokio::test]
    async fn pane_scrollbar_setting_controls_reserved_column() {
        let mut app = AppState::test_new();
        let mut workspace = Workspace::test_new("test");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0].runtimes.insert(
            root_pane,
            TerminalRuntime::test_with_scrollback_bytes(
                40,
                8,
                1024,
                b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
            ),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let area = Rect::new(10, 3, 40, 8);
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        let infos = compute_pane_infos(
            &mut app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, Some(Rect::new(49, 3, 1, 8)));
        assert_eq!(info.inner_rect, Rect::new(10, 3, 39, 8));

        app.pane_scrollbars = false;
        let infos = compute_pane_infos(
            &mut app,
            &terminal_runtimes,
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        let info = &infos[0];

        assert_eq!(info.rect, area);
        assert_eq!(info.scrollbar_rect, None);
        assert_eq!(info.inner_rect, area);
    }

    #[test]
    fn selection_highlight_uses_one_uniform_style() {
        let palette = Palette::catppuccin();
        let host_theme = crate::terminal_theme::TerminalTheme {
            foreground: None,
            background: Some(crate::terminal_theme::RgbColor {
                r: 12,
                g: 14,
                b: 16,
            }),
            ..Default::default()
        };
        let expected_style = automatic_selection_style(&palette, host_theme);
        let selection = Some(Selection::range(PaneId::from_raw(1), 0, 0, 2, None));
        let backend = ratatui::backend::TestBackend::new(4, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                let buf = frame.buffer_mut();
                buf[(0, 0)].set_style(
                    Style::default()
                        .fg(Color::Rgb(10, 220, 120))
                        .bg(Color::Black),
                );
                buf[(1, 0)].set_style(
                    Style::default()
                        .fg(Color::Rgb(220, 180, 40))
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                );
                buf[(2, 0)].set_style(Style::default().fg(Color::Blue).bg(Color::Reset));
                render_selection_highlight(
                    &selection,
                    frame,
                    PaneId::from_raw(1),
                    Rect::new(0, 0, 4, 1),
                    None,
                    &palette,
                    host_theme,
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let first = buffer[(0, 0)].style();
        let second = buffer[(1, 0)].style();
        let third = buffer[(2, 0)].style();

        assert_eq!(first.fg, expected_style.fg);
        assert_eq!(second.fg, expected_style.fg);
        assert_eq!(third.fg, expected_style.fg);
        assert_eq!(first.bg, expected_style.bg);
        assert_eq!(second.bg, expected_style.bg);
        assert_eq!(third.bg, expected_style.bg);
        assert_eq!(first.add_modifier, expected_style.add_modifier);
        assert_eq!(second.add_modifier, expected_style.add_modifier);
        assert_eq!(third.add_modifier, expected_style.add_modifier);
        assert!(!second.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn automatic_selection_background_uses_host_background() {
        let bg = automatic_selection_bg(
            &Palette::terminal(),
            crate::terminal_theme::TerminalTheme {
                foreground: Some(crate::terminal_theme::RgbColor {
                    r: 230,
                    g: 230,
                    b: 230,
                }),
                background: Some(crate::terminal_theme::RgbColor {
                    r: 12,
                    g: 14,
                    b: 16,
                }),
                ..Default::default()
            },
        );

        let Color::Rgb(r, g, b) = bg else {
            panic!("selection background should resolve to rgb");
        };
        assert!(relative_luminance((r, g, b)) > relative_luminance((12, 14, 16)));
    }

    /// Closing a sibling pane while its workspace is on screen must grow the
    /// survivor to its final size on the very first frame after the close.
    ///
    /// This is the captain's "output scrolls up and snaps back down": the
    /// survivor's grid is genuinely smaller than the rect the layout hands it,
    /// and a paced grow drew it at that smaller size first —
    /// `GhosttyPaneTerminal::render` is top-aligned, so a terminal's
    /// bottom-anchored newest output jumped to the top of the pane with a blank
    /// tail below it, then crawled back down over the ease.
    ///
    /// Asserted against the pane's own pre-split size rather than a later
    /// frame, so the test has no wall-clock component at all.
    #[tokio::test]
    async fn closing_a_sibling_on_screen_grows_the_survivor_in_one_frame() {
        fn seeded_runtime() -> TerminalRuntime {
            let mut bytes = Vec::new();
            for line in 0..300 {
                bytes.extend_from_slice(format!("line {line:04} out\r\n").as_bytes());
            }
            TerminalRuntime::test_with_scrollback_bytes(90, 18, 1 << 20, &bytes)
        }

        fn drawn_rows(app: &AppState, pane_id: PaneId) -> u16 {
            app.view
                .pane_infos
                .iter()
                .find(|info| info.id == pane_id)
                .expect("pane info")
                .inner_rect
                .height
        }

        let mut runtimes = TerminalRuntimeRegistry::new();
        let ws = Workspace::test_new("alpha");
        let survivor = ws.tabs[0].root_pane;
        runtimes.insert(
            ws.tabs[0]
                .terminal_id(survivor)
                .expect("terminal id")
                .clone(),
            seeded_runtime(),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.selected = 0;

        let area = Rect::new(0, 0, 120, 50);

        // The size the layout gives this pane when it is alone in the tab.
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, area);
        let alone = drawn_rows(&app, survivor);

        // Split it, register the sibling's runtime, and settle the split.
        let sibling = app.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        runtimes.insert(
            app.workspaces[0].tabs[0]
                .terminal_id(sibling)
                .expect("terminal id")
                .clone(),
            seeded_runtime(),
        );
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, area);
        let split = drawn_rows(&app, survivor);
        assert!(
            split < alone,
            "the split should have shrunk the survivor: {alone} -> {split}"
        );

        // Close the sibling while the workspace is on screen — the genuine
        // grow, the one trigger that used to ease.
        // `close_pane` reports whether the *workspace* should close, so the
        // pane count is what says the sibling actually went away.
        app.workspaces[0].close_pane(sibling);
        assert_eq!(app.workspaces[0].tabs[0].layout.pane_count(), 1);
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, area);

        assert_eq!(
            drawn_rows(&app, survivor),
            alone,
            "the survivor must be drawn over its whole rect on the first frame \
             after the close, not at its old smaller size"
        );
        assert_eq!(
            app.runtime_for_pane_in_workspace(&runtimes, 0, survivor)
                .expect("runtime")
                .current_size()
                .0,
            alone,
            "and its grid must have been resized there in the same frame"
        );
    }

    /// A pane resized while its workspace was off screen must come back the
    /// size it already is — not be shrunk to the size it had when it was last
    /// on screen and then reflowed back up.
    ///
    /// The shrink is what the captain sees as a pane's output "force scrolled
    /// to the top": `GhosttyPaneTerminal::render` draws the grid's rows
    /// top-aligned into the pane's rect, so a grid reflowed down to 19 rows
    /// inside a 49-row rect puts the pane's newest output near the top with a
    /// blank tail under it.
    ///
    /// Deliberately asserted a frame at a time with no sleeping: the collapse
    /// happens on the very frame the workspace is re-entered, because a growth
    /// resolves to its starting size before it eases anywhere.
    #[tokio::test]
    async fn re_entering_a_workspace_does_not_shrink_a_pane_already_at_its_target() {
        fn seeded_runtime() -> TerminalRuntime {
            let mut bytes = Vec::new();
            for line in 0..300 {
                bytes.extend_from_slice(format!("line {line:04} out\r\n").as_bytes());
            }
            TerminalRuntime::test_with_scrollback_bytes(90, 18, 1 << 20, &bytes)
        }

        // Registered in the real registry under the workspace's own terminal
        // id, so the active-tab path and the background-tab path resolve to the
        // same runtime exactly as they do in production.
        fn workspace(
            name: &str,
            runtimes: &mut TerminalRuntimeRegistry,
        ) -> crate::workspace::Workspace {
            let ws = crate::workspace::Workspace::test_new(name);
            let pane_id = ws.tabs[0].root_pane;
            let terminal_id = ws.tabs[0]
                .terminal_id(pane_id)
                .expect("terminal id")
                .clone();
            runtimes.insert(terminal_id, seeded_runtime());
            ws
        }

        fn grid_rows(app: &AppState, runtimes: &TerminalRuntimeRegistry, ws_idx: usize) -> u16 {
            let pane_id = app.workspaces[ws_idx].tabs[0].root_pane;
            app.runtime_for_pane_in_workspace(runtimes, ws_idx, pane_id)
                .expect("runtime")
                .current_size()
                .0
        }

        let mut runtimes = TerminalRuntimeRegistry::new();
        let mut app = AppState::test_new();
        app.workspaces = vec![
            workspace("alpha", &mut runtimes),
            workspace("beta", &mut runtimes),
        ];
        let runtimes = runtimes;

        let short = Rect::new(0, 0, 120, 20);
        let tall = Rect::new(0, 0, 120, 50);

        // Alpha settles at the short geometry while it is on screen.
        app.active = Some(0);
        app.selected = 0;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, short);
        let settled_short = grid_rows(&app, &runtimes, 0);

        // Switch to beta, then the host terminal grows. Alpha is backgrounded,
        // so the background-tab path resizes its runtime straight to the tall
        // geometry without going through the resize-reflow.
        app.active = Some(1);
        app.selected = 1;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, short);
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, tall);
        let backgrounded = grid_rows(&app, &runtimes, 0);
        assert!(
            backgrounded > settled_short,
            "the background path should have grown alpha's grid: {settled_short} -> {backgrounded}"
        );

        // Go back into alpha.
        app.active = Some(0);
        app.selected = 0;
        crate::ui::compute_view_with_runtime_registry(&mut app, &runtimes, tall);

        assert_eq!(
            grid_rows(&app, &runtimes, 0),
            backgrounded,
            "re-entry must not reflow alpha's grid back down to its remembered size"
        );
        let info = app.view.pane_infos.first().expect("pane info");
        assert_eq!(
            info.inner_rect.height, backgrounded,
            "and the pane must be drawn over its whole rect, not just the top of it"
        );
    }
}
