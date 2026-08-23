use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::status::state_icon;
use super::text::display_width_u16;
use super::widgets::panel_contrast_fg;
use crate::app::AppState;

const MIN_TAB_WIDTH: u16 = 8;
const NEW_TAB_WIDTH: u16 = 3;
const TAB_SCROLL_BUTTON_WIDTH: u16 = 3;
const ZOOM_INDICATOR: &str = "ZOOM";
// The narrowest overflowing tab strip worth keeping interactive: one
// minimum-width tab, both scroll controls, and the new-tab control.
const MIN_TAB_STRIP_WIDTH: u16 =
    MIN_TAB_WIDTH + NEW_TAB_WIDTH + TAB_SCROLL_BUTTON_WIDTH.saturating_mul(2);

#[derive(Debug, Clone, Default)]
pub(crate) struct TabBarView {
    pub scroll: usize,
    pub tab_hit_areas: Vec<Rect>,
    pub scroll_left_hit_area: Rect,
    pub scroll_right_hit_area: Rect,
    pub new_tab_hit_area: Rect,
}

/// Which label decorations the tab bar draws, resolved from config plus the
/// current sidebar state.
///
/// Both decorations occupy a fixed width that does not depend on agent state,
/// so tab geometry stays stable while agents change state and only changes when
/// the user changes the layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TabLabelDecor {
    pub state_dot: bool,
    pub index: bool,
}

impl TabLabelDecor {
    pub(crate) fn from_state(app: &AppState) -> Self {
        Self {
            state_dot: app.show_tab_state_dots.enabled(app.sidebar_collapsed),
            index: app.show_tab_numbers.enabled(app.sidebar_collapsed),
        }
    }
}

/// Number prefix drawn before the tab title, if any.
///
/// Auto-named tabs already render their 1-based position as the title, so
/// prefixing them would produce `1 1`.
fn tab_index_prefix(
    ws: &crate::workspace::Workspace,
    tab_idx: usize,
    decor: TabLabelDecor,
) -> Option<String> {
    if !decor.index {
        return None;
    }
    let tab = ws.tabs.get(tab_idx)?;
    if tab.is_auto_named() {
        return None;
    }
    Some(format!("{} ", tab_idx + 1))
}

/// Columns the decorations reserve ahead of the tab title.
fn tab_decor_width(ws: &crate::workspace::Workspace, tab_idx: usize, decor: TabLabelDecor) -> u16 {
    // The state dot is always a single cell (see `status::state_dot`), plus one
    // separating space, and is reserved whether or not any pane reports state.
    let dot = if decor.state_dot { 2 } else { 0 };
    let index = tab_index_prefix(ws, tab_idx, decor)
        .map(|prefix| display_width_u16(&prefix))
        .unwrap_or(0);
    dot + index
}

fn tab_width(ws: &crate::workspace::Workspace, tab_idx: usize, decor: TabLabelDecor) -> u16 {
    display_width_u16(&tab_chrome_label(ws, tab_idx))
        .saturating_add(tab_decor_width(ws, tab_idx, decor))
        .saturating_add(4)
        .max(MIN_TAB_WIDTH)
}

fn tab_chrome_label(ws: &crate::workspace::Workspace, tab_idx: usize) -> String {
    let name = ws
        .tab_display_name(tab_idx)
        .unwrap_or_else(|| (tab_idx + 1).to_string());
    if ws.tabs.get(tab_idx).is_some_and(|tab| tab.zoomed) {
        format!("{name} Z")
    } else {
        name
    }
}

#[derive(Clone, Copy)]
struct VisibleStatusSegment<'a> {
    text: &'a str,
    accent: bool,
}

fn visible_status_segments(app: &AppState) -> Vec<VisibleStatusSegment<'_>> {
    let zoomed = app
        .active
        .and_then(|index| app.workspaces.get(index))
        .is_some_and(|workspace| workspace.zoomed);
    app.tab_bar_right
        .iter()
        .filter_map(|segment| match segment {
            crate::app::state::TabBarStatusSegment::Zoom if zoomed => Some(VisibleStatusSegment {
                text: ZOOM_INDICATOR,
                accent: true,
            }),
            crate::app::state::TabBarStatusSegment::Text(Some(text))
                if display_width_u16(text) > 0 =>
            {
                Some(VisibleStatusSegment {
                    text,
                    accent: false,
                })
            }
            crate::app::state::TabBarStatusSegment::Zoom
            | crate::app::state::TabBarStatusSegment::Text(_) => None,
        })
        .collect()
}

fn tab_bar_status_width(app: &AppState) -> u16 {
    let segments = visible_status_segments(app);
    let content_width = segments.iter().fold(0_u16, |width, segment| {
        width.saturating_add(display_width_u16(segment.text))
    });
    let separators = u16::try_from(segments.len().saturating_sub(1)).unwrap_or(u16::MAX);
    content_width
        .saturating_add(display_width_u16(&app.tab_bar_right_separator).saturating_mul(separators))
}

/// Widest decoration prefix any tab in `ws` can draw.
///
/// Derived from the tab count rather than measured per tab, so it allocates
/// nothing on the render path and does not move when a tab is renamed.
fn max_tab_decor_width(ws: &crate::workspace::Workspace, decor: TabLabelDecor) -> u16 {
    // Matches `tab_decor_width`: one cell for the state mark plus a separating
    // space, and the widest 1-based position plus a separating space.
    let dot: u16 = if decor.state_dot { 2 } else { 0 };
    let index = if decor.index {
        u16::try_from(ws.tabs.len().max(1).ilog10() + 2).unwrap_or(u16::MAX)
    } else {
        0
    };
    dot.saturating_add(index)
}

/// The narrowest strip worth keeping interactive, once label decorations are
/// accounted for.
///
/// `MIN_TAB_STRIP_WIDTH` sizes an undecorated label. A state mark and a jump
/// number are drawn inside the tab, so on exactly the rows where the status is
/// already competing for space they would eat the title instead. Reserving the
/// decoration width here makes the status yield first, so a decorated tab keeps
/// the same readable minimum an undecorated one gets.
fn min_tab_strip_width(app: &AppState) -> u16 {
    let decor = TabLabelDecor::from_state(app);
    let decor_width = app
        .active
        .and_then(|index| app.workspaces.get(index))
        .map(|ws| max_tab_decor_width(ws, decor))
        .unwrap_or(0);
    MIN_TAB_STRIP_WIDTH.saturating_add(decor_width)
}

fn tab_bar_status_area(app: &AppState, area: Rect) -> Option<Rect> {
    let width = tab_bar_status_width(app);
    if width == 0 {
        return None;
    }
    let reserved = width.saturating_add(1);
    (area.width.saturating_sub(reserved) >= min_tab_strip_width(app))
        .then(|| Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1))
}

// Tabs win over status decoration on narrow rows. The extra reserved cell is
// the gap between the interactive strip and the right-aligned status entries.
pub(crate) fn tab_bar_content_area(app: &AppState, area: Rect) -> Rect {
    let reserved = tab_bar_status_area(app, area)
        .map(|status| status.width.saturating_add(1))
        .unwrap_or(0);
    Rect {
        width: area.width.saturating_sub(reserved),
        ..area
    }
}

fn layout_tab_hit_areas(
    ws: &crate::workspace::Workspace,
    area: Rect,
    scroll: usize,
    decor: TabLabelDecor,
) -> Vec<Rect> {
    let mut rects = vec![Rect::default(); ws.tabs.len()];
    if area.width == 0 || area.height == 0 {
        return rects;
    }

    let mut x = area.x;
    let right = area.x + area.width;
    for (idx, rect) in rects.iter_mut().enumerate().skip(scroll) {
        if x >= right {
            break;
        }
        let desired = tab_width(ws, idx, decor);
        let remaining = right.saturating_sub(x);
        let width = desired.min(remaining).max(1);
        *rect = Rect::new(x, area.y, width, 1);
        x = x.saturating_add(width + 1);
    }
    rects
}

fn centered_tab_scroll(
    ws: &crate::workspace::Workspace,
    area: Rect,
    decor: TabLabelDecor,
) -> usize {
    let mut best_scroll = ws.active_tab;
    let mut best_distance = u16::MAX;
    let viewport_center = area.x.saturating_mul(2).saturating_add(area.width);

    for scroll in 0..=ws.active_tab {
        let rects = layout_tab_hit_areas(ws, area, scroll, decor);
        let Some(active_rect) = rects.get(ws.active_tab).copied() else {
            continue;
        };
        if active_rect.width == 0 {
            continue;
        }

        let active_center = active_rect
            .x
            .saturating_mul(2)
            .saturating_add(active_rect.width);
        let distance = active_center.abs_diff(viewport_center);
        if distance <= best_distance {
            best_distance = distance;
            best_scroll = scroll;
        }
    }

    best_scroll
}

fn trailing_tab_controls_x(tab_hit_areas: &[Rect], fallback_x: u16) -> u16 {
    tab_hit_areas
        .iter()
        .rev()
        .find(|rect| rect.width > 0)
        .map(|rect| rect.x + rect.width)
        .unwrap_or(fallback_x)
}

fn max_tab_scroll(ws: &crate::workspace::Workspace, area: Rect, decor: TabLabelDecor) -> usize {
    (0..ws.tabs.len())
        .find(|&scroll| {
            layout_tab_hit_areas(ws, area, scroll, decor)
                .last()
                .is_some_and(|rect| rect.width > 0)
        })
        .unwrap_or(0)
}

pub(crate) fn compute_tab_bar_view(
    ws: &crate::workspace::Workspace,
    area: Rect,
    current_scroll: usize,
    follow_active: bool,
    mouse_chrome: bool,
    decor: TabLabelDecor,
) -> TabBarView {
    if area.width == 0 || area.height == 0 {
        return TabBarView::default();
    }

    if !mouse_chrome {
        let max_scroll = max_tab_scroll(ws, area, decor);
        let scroll = if follow_active {
            centered_tab_scroll(ws, area, decor).min(max_scroll)
        } else {
            current_scroll.min(max_scroll)
        };
        return TabBarView {
            scroll,
            tab_hit_areas: layout_tab_hit_areas(ws, area, scroll, decor),
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area: Rect::default(),
        };
    }

    let area_right = area.x + area.width;
    let all_tabs_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(NEW_TAB_WIDTH),
        area.height,
    );
    let all_tabs = layout_tab_hit_areas(ws, all_tabs_area, 0, decor);
    let overflow = all_tabs.iter().any(|rect| rect.width == 0);
    if !overflow {
        let new_tab_x = trailing_tab_controls_x(&all_tabs, area.x);
        let new_tab_hit_area = Rect::new(
            new_tab_x,
            area.y,
            area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
            1,
        );
        return TabBarView {
            scroll: 0,
            tab_hit_areas: all_tabs,
            scroll_left_hit_area: Rect::default(),
            scroll_right_hit_area: Rect::default(),
            new_tab_hit_area,
        };
    }

    let left_hit_area = Rect::new(area.x, area.y, TAB_SCROLL_BUTTON_WIDTH.min(area.width), 1);
    let tab_area_x = left_hit_area.x + left_hit_area.width;
    let reserved_trailing_width = NEW_TAB_WIDTH.saturating_add(TAB_SCROLL_BUTTON_WIDTH);
    let tab_area_right = area_right.saturating_sub(reserved_trailing_width);
    let tab_area = Rect::new(
        tab_area_x,
        area.y,
        tab_area_right.saturating_sub(tab_area_x),
        area.height,
    );

    let max_scroll = max_tab_scroll(ws, tab_area, decor);
    let scroll = if follow_active {
        centered_tab_scroll(ws, tab_area, decor).min(max_scroll)
    } else {
        current_scroll.min(max_scroll)
    };
    let tab_hit_areas = layout_tab_hit_areas(ws, tab_area, scroll, decor);
    let trailing_x = trailing_tab_controls_x(&tab_hit_areas, tab_area_x).min(tab_area_right);
    let right_hit_area = Rect::new(
        trailing_x,
        area.y,
        area_right
            .saturating_sub(trailing_x)
            .min(TAB_SCROLL_BUTTON_WIDTH),
        1,
    );
    let new_tab_x = right_hit_area.x + right_hit_area.width;
    let new_tab_hit_area = Rect::new(
        new_tab_x,
        area.y,
        area_right.saturating_sub(new_tab_x).min(NEW_TAB_WIDTH),
        1,
    );

    TabBarView {
        scroll,
        tab_hit_areas,
        scroll_left_hit_area: left_hit_area,
        scroll_right_hit_area: right_hit_area,
        new_tab_hit_area,
    }
}

fn tab_drop_indicator_x(
    app: &AppState,
    ws: &crate::workspace::Workspace,
    insert_idx: usize,
) -> Option<u16> {
    let mut visible_tabs = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .filter(|(_, rect)| rect.width > 0);
    let first_visible = visible_tabs.clone().next()?;
    let last_visible = visible_tabs.next_back().unwrap_or(first_visible);

    if insert_idx == 0 {
        return Some(if first_visible.0 == 0 {
            first_visible.1.x
        } else {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        });
    }

    if let Some((_, rect)) = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(idx, rect)| *idx == insert_idx && rect.width > 0)
    {
        return Some(rect.x.saturating_sub(1));
    }

    if insert_idx >= ws.tabs.len() {
        return Some(if last_visible.0 + 1 >= ws.tabs.len() {
            last_visible.1.x + last_visible.1.width
        } else {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        });
    }

    None
}

pub(super) fn render_tab_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(active_ws_idx) = app.active else {
        return;
    };
    let Some(ws) = app.workspaces.get(active_ws_idx) else {
        return;
    };
    let p = &app.palette;
    let decor = TabLabelDecor::from_state(app);

    frame.render_widget(
        Paragraph::new(" ".repeat(area.width as usize)).style(Style::default().bg(p.panel_bg)),
        area,
    );

    let first_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let last_visible_idx = app
        .view
        .tab_hit_areas
        .iter()
        .enumerate()
        .rev()
        .find(|(_, rect)| rect.width > 0)
        .map(|(idx, _)| idx);
    let can_scroll_left = app.view.tab_scroll_left_hit_area.width > 0 && app.tab_scroll > 0;
    let can_scroll_right = app.view.tab_scroll_right_hit_area.width > 0
        && last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len());

    if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
        let style = if can_scroll_left {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" < ").style(style),
            app.view.tab_scroll_left_hit_area,
        );
    }

    if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
        let style = if can_scroll_right {
            Style::default().fg(p.overlay1).bg(p.surface0)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        };
        frame.render_widget(
            Paragraph::new(" > ").style(style),
            app.view.tab_scroll_right_hit_area,
        );
    }

    for (idx, tab) in ws.tabs.iter().enumerate() {
        let Some(rect) = app.view.tab_hit_areas.get(idx).copied() else {
            break;
        };
        if rect.width == 0 {
            continue;
        }
        let active = idx == ws.active_tab;
        let style = if active {
            let base = Style::default().fg(panel_contrast_fg(p)).bg(p.accent);
            if tab.is_auto_named() {
                base
            } else {
                base.add_modifier(Modifier::BOLD)
            }
        } else if tab.is_auto_named() {
            Style::default()
                .fg(p.overlay0)
                .bg(p.surface0)
                .add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(p.overlay1).bg(p.surface0)
        };
        let width = rect.width as usize;
        let name = tab_chrome_label(ws, idx);

        let mut spans = vec![Span::raw(" ")];
        let mut used = 1usize;
        if decor.state_dot {
            let (agg_state, agg_seen) = tab.aggregate_state(&app.terminals);
            let (dot, dot_style) = state_icon(agg_state, agg_seen, app.status_indicators, p);
            // `dot_style` carries only a foreground, so the tab's own background
            // (accent when active) is preserved underneath it.
            spans.push(Span::styled(dot, dot_style));
            spans.push(Span::raw(" "));
            used += 2;
        }
        if let Some(prefix) = tab_index_prefix(ws, idx, decor) {
            used += display_width_u16(&prefix) as usize;
            let number_style = if active {
                style
            } else {
                style.add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled(prefix, number_style));
        }
        spans.push(Span::raw(format!(
            "{:width$}",
            name,
            width = width.saturating_sub(used)
        )));
        frame.render_widget(Paragraph::new(Line::from(spans)).style(style), rect);
    }

    if let Some(crate::app::state::DragState {
        target:
            crate::app::state::DragTarget::TabReorder {
                ws_idx,
                insert_idx: Some(insert_idx),
                ..
            },
    }) = &app.drag
    {
        if *ws_idx == active_ws_idx {
            if let Some(x) = tab_drop_indicator_x(app, ws, *insert_idx) {
                frame.buffer_mut()[(x.min(area.x + area.width.saturating_sub(1)), area.y)]
                    .set_symbol("│")
                    .set_style(Style::default().fg(p.accent));
            }
        }
    }

    if app.mouse_capture && app.view.new_tab_hit_area.width > 0 {
        frame.render_widget(
            Paragraph::new(" + ").style(Style::default().fg(p.overlay1)),
            app.view.new_tab_hit_area,
        );
    }

    if first_visible_idx.is_some_and(|idx| idx > 0) {
        let x = if app.mouse_capture && app.view.tab_scroll_left_hit_area.width > 0 {
            app.view.tab_scroll_left_hit_area.x + app.view.tab_scroll_left_hit_area.width
        } else {
            area.x
        };
        if x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(p.overlay0));
        }
    }
    if last_visible_idx.is_some_and(|idx| idx + 1 < ws.tabs.len()) {
        let content = tab_bar_content_area(app, area);
        let content_right = content.x + content.width;
        let x = if app.mouse_capture && app.view.tab_scroll_right_hit_area.width > 0 {
            app.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        } else {
            content_right.saturating_sub(1)
        };
        if x >= area.x && x < area.x + area.width {
            frame.buffer_mut()[(x, area.y)]
                .set_symbol("…")
                .set_style(Style::default().fg(p.overlay0));
        }
    }

    if let Some(status_area) = tab_bar_status_area(app, area) {
        let segments = visible_status_segments(app);
        let separator_width = display_width_u16(&app.tab_bar_right_separator);
        let mut x = status_area.x;
        for (index, segment) in segments.iter().enumerate() {
            if index > 0 && separator_width > 0 {
                let rect = Rect::new(x, area.y, separator_width, 1);
                frame.render_widget(
                    Paragraph::new(app.tab_bar_right_separator.as_str())
                        .style(Style::default().fg(p.overlay0).bg(p.panel_bg)),
                    rect,
                );
                x = x.saturating_add(separator_width);
            }

            let width = display_width_u16(segment.text);
            let rect = Rect::new(x, area.y, width, 1);
            let style = if segment.accent {
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.overlay1).bg(p.panel_bg)
            };
            frame.render_widget(Paragraph::new(segment.text).style(style), rect);
            x = x.saturating_add(width);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AppState;
    use crate::workspace::Workspace;
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_row_text(buffer: &ratatui::buffer::Buffer, area: Rect, row: u16) -> String {
        (area.x..area.x + area.width)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn tab_bar_marks_zoomed_tabs_without_renaming_them() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        let custom_tab = ws.test_add_tab(Some("test"));
        ws.tabs[custom_tab].zoomed = true;

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            app.view.tab_bar_rect,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.contains(" 1 Z"), "tab row: {row:?}");
        assert!(row.contains(" test Z"), "tab row: {row:?}");
        assert_eq!(app.workspaces[0].tab_display_name(0).as_deref(), Some("1"));
        assert_eq!(
            app.workspaces[0].tab_display_name(custom_tab).as_deref(),
            Some("test")
        );
    }

    #[test]
    fn tab_bar_renders_ordered_status_entries_with_separator() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].zoomed = true;
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
            crate::app::state::TabBarStatusSegment::Text(Some("14:30".into())),
        ];
        app.tab_bar_right_separator = " · ".into();

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 60, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            content,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas.clone();

        let backend = TestBackend::new(60, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = buffer_row_text(buffer, app.view.tab_bar_rect, 0);
        assert!(
            row.ends_with("ZOOM · wintermute · 14:30"),
            "tab row: {row:?}"
        );
        let status_x = 60 - display_width_u16("ZOOM · wintermute · 14:30");
        assert_eq!(buffer[(status_x, 0)].style().bg, Some(app.palette.accent));
        for rect in &view.tab_hit_areas {
            assert!(rect.x + rect.width <= content.x + content.width);
        }
    }

    #[test]
    fn hidden_status_entries_do_not_leave_dangling_separators() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(None),
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
        ];
        app.tab_bar_right_separator = " | ".into();
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 40, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            content,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.ends_with("wintermute"), "tab row: {row:?}");
        assert!(!row.contains(" | "), "tab row: {row:?}");
    }

    #[test]
    fn status_reservation_keeps_a_minimum_width_tab_between_scroll_controls() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![crate::app::state::TabBarStatusSegment::Text(Some(
            "x".into(),
        ))];
        let mut workspace = Workspace::test_new("test");
        workspace.test_add_tab(None);
        workspace.test_add_tab(None);
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let too_narrow = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 1, 1);
        assert_eq!(tab_bar_content_area(&app, too_narrow), too_narrow);

        let wide_enough = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 2, 1);
        let content = tab_bar_content_area(&app, wide_enough);
        assert_eq!(content.width, MIN_TAB_STRIP_WIDTH);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            content,
            0,
            true,
            true,
            TabLabelDecor::from_state(&app),
        );
        assert!(view.tab_hit_areas[0].width >= MIN_TAB_WIDTH);
    }

    #[test]
    fn status_reservation_leaves_room_for_the_tab_label_decorations() {
        // A width that is wide enough for the status once, and would still be
        // wide enough if the state mark and jump number were free.
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![crate::app::state::TabBarStatusSegment::Text(Some(
            "x".into(),
        ))];
        app.show_tab_state_dots = crate::config::TabDecorationConfig::Always;
        app.show_tab_numbers = crate::config::TabDecorationConfig::Always;
        let mut workspace = Workspace::test_new("test");
        // Named, so the first tab carries both decorations: an auto-named tab
        // renders its position as its title and is deliberately not numbered.
        workspace.tabs[0].custom_name = Some("alpha".into());
        workspace.test_add_tab(Some("bravo"));
        workspace.test_add_tab(Some("charlie"));
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let decor_width = max_tab_decor_width(&app.workspaces[0], TabLabelDecor::from_state(&app));
        assert!(decor_width > 0, "decorations should reserve columns");
        assert_eq!(min_tab_strip_width(&app), MIN_TAB_STRIP_WIDTH + decor_width);

        // Undecorated this row would carry the status; decorated it must not,
        // because the columns the status wants are the tab title's.
        let borderline = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + 2, 1);
        assert_eq!(tab_bar_status_area(&app, borderline), None);
        assert_eq!(tab_bar_content_area(&app, borderline), borderline);

        let wide_enough = Rect::new(0, 0, MIN_TAB_STRIP_WIDTH + decor_width + 2, 1);
        let content = tab_bar_content_area(&app, wide_enough);
        assert_eq!(content.width, MIN_TAB_STRIP_WIDTH + decor_width);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            content,
            0,
            true,
            true,
            TabLabelDecor::from_state(&app),
        );
        // The first tab still gets its minimum plus everything the decorations
        // draw, so its title keeps the room an undecorated tab's title has.
        assert!(
            view.tab_hit_areas[0].width >= MIN_TAB_WIDTH + decor_width,
            "first tab {:?} lost columns to the status",
            view.tab_hit_areas[0]
        );
    }

    #[test]
    fn tab_bar_draws_state_marks_and_status_entries_on_the_same_row() {
        let mut app = AppState::test_new();
        app.show_tab_state_dots = crate::config::TabDecorationConfig::Always;
        app.show_tab_numbers = crate::config::TabDecorationConfig::Always;
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Zoom,
            crate::app::state::TabBarStatusSegment::Text(Some("wintermute".into())),
        ];
        app.tab_bar_right_separator = " · ".into();
        let mut ws = Workspace::test_new("test");
        ws.zoomed = true;
        ws.test_add_tab(Some("build"));
        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 70, 1);
        let content = tab_bar_content_area(&app, app.view.tab_bar_rect);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            content,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas.clone();

        let backend = TestBackend::new(70, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        // The status sits at the right edge, the decorated label at the left,
        // and neither has overwritten the other.
        assert!(row.ends_with("ZOOM · wintermute"), "tab row: {row:?}");
        assert!(row.contains("2 build"), "tab row: {row:?}");
        for rect in &view.tab_hit_areas {
            assert!(
                rect.x + rect.width <= content.x + content.width,
                "tab {rect:?} runs into the status area"
            );
        }
    }

    #[test]
    fn combined_status_entries_yield_to_tab_controls_on_narrow_rows() {
        let mut app = AppState::test_new();
        app.tab_bar_right = vec![
            crate::app::state::TabBarStatusSegment::Text(Some(
                "a-hostname-wider-than-the-whole-bar".into(),
            )),
            crate::app::state::TabBarStatusSegment::Text(Some("14:30".into())),
        ];
        app.workspaces = vec![Workspace::test_new("test")];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);

        assert_eq!(
            tab_bar_content_area(&app, app.view.tab_bar_rect),
            app.view.tab_bar_rect
        );
        assert_eq!(tab_bar_status_area(&app, app.view.tab_bar_rect), None);

        let view = compute_tab_bar_view(
            &app.workspaces[0],
            tab_bar_content_area(&app, app.view.tab_bar_rect),
            0,
            true,
            true,
            TabLabelDecor::from_state(&app),
        );
        assert!(view.tab_hit_areas[0].width > 0);
        assert!(view.new_tab_hit_area.width > 0);
    }

    #[test]
    fn active_auto_named_tab_keeps_readable_weight() {
        let mut app = AppState::test_new();
        let ws = Workspace::test_new("test");

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            app.view.tab_bar_rect,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let tab_rect = app.view.tab_hit_areas[0];
        let style = terminal.backend().buffer()[(tab_rect.x + 1, tab_rect.y)].style();

        assert_eq!(style.bg, Some(app.palette.accent));
        assert!(!style.add_modifier.contains(Modifier::DIM));
        assert!(!style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn zoom_marker_counts_toward_tab_width() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("abcdefgh".into());
        ws.tabs[0].zoomed = true;

        assert_eq!(tab_width(&ws, 0, TabLabelDecor::default()), 14);
    }

    #[test]
    fn tab_width_uses_display_width_for_cjk_labels() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());

        assert_eq!(
            tab_width(&ws, 0, TabLabelDecor::default()),
            display_width_u16("提交 herdr 的反馈") + 4
        );
    }

    #[test]
    fn tab_bar_renders_trailing_cjk_character() {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("提交 herdr 的反馈".into());

        app.active = Some(0);
        app.workspaces = vec![ws];
        app.view.tab_bar_rect = Rect::new(0, 0, 30, 1);
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            app.view.tab_bar_rect,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let row = buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0);
        assert!(row.contains('馈'), "tab row: {row:?}");
    }

    fn decorated_app() -> AppState {
        let mut app = AppState::test_new();
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("agents".into());
        ws.test_add_tab(Some("review"));
        ws.test_add_tab(None);

        app.workspaces = vec![ws];
        app.active = Some(0);
        app.view.tab_bar_rect = Rect::new(0, 0, 60, 1);
        app
    }

    fn render_row(app: &mut AppState) -> String {
        let view = compute_tab_bar_view(
            &app.workspaces[0],
            app.view.tab_bar_rect,
            0,
            true,
            false,
            TabLabelDecor::from_state(app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(app.view.tab_bar_rect.width, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(app, frame, app.view.tab_bar_rect))
            .unwrap();
        buffer_row_text(terminal.backend().buffer(), app.view.tab_bar_rect, 0)
    }

    #[test]
    fn auto_decorations_stay_off_while_the_sidebar_is_expanded() {
        let mut app = decorated_app();
        assert!(!app.sidebar_collapsed);

        let decor = TabLabelDecor::from_state(&app);
        assert_eq!(decor, TabLabelDecor::default());

        let row = render_row(&mut app);
        assert_eq!(row, " agents     review     3");
    }

    #[test]
    fn auto_decorations_turn_on_when_the_sidebar_collapses() {
        let mut app = decorated_app();
        app.sidebar_collapsed = true;

        let decor = TabLabelDecor::from_state(&app);
        assert!(decor.state_dot && decor.index);

        // Auto-named tab 3 already shows its position as its title, so it is
        // decorated with a dot but not double-numbered.
        let row = render_row(&mut app);
        assert_eq!(row, "   1 agents       2 review       3");
    }

    #[test]
    fn never_keeps_decorations_off_while_collapsed() {
        let mut app = decorated_app();
        app.sidebar_collapsed = true;
        app.show_tab_state_dots = crate::config::TabDecorationConfig::Never;
        app.show_tab_numbers = crate::config::TabDecorationConfig::Never;

        let row = render_row(&mut app);
        assert_eq!(row, " agents     review     3");
    }

    #[test]
    fn always_keeps_decorations_on_while_expanded() {
        let mut app = decorated_app();
        app.show_tab_state_dots = crate::config::TabDecorationConfig::Always;
        app.show_tab_numbers = crate::config::TabDecorationConfig::Always;
        assert!(!app.sidebar_collapsed);

        let row = render_row(&mut app);
        assert_eq!(row, "   1 agents       2 review       3");
    }

    #[test]
    fn state_dot_reflects_the_tab_rollup_and_keeps_the_tab_background() {
        let mut app = decorated_app();
        app.sidebar_collapsed = true;

        // Second tab holds a blocked agent; it is not the active tab, so only
        // the tab bar can advertise it.
        let pane_id = *app.workspaces[0].tabs[1]
            .layout
            .pane_ids()
            .first()
            .expect("tab has a pane");
        let terminal_id = app.workspaces[0].terminal_id(pane_id).unwrap().clone();
        let mut terminal_state =
            crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal_state.state = crate::detect::AgentState::Blocked;
        app.terminals.insert(terminal_id, terminal_state);
        for pane in app.workspaces[0].tabs[1].panes.values_mut() {
            pane.seen = false;
        }

        let view = compute_tab_bar_view(
            &app.workspaces[0],
            app.view.tab_bar_rect,
            0,
            true,
            false,
            TabLabelDecor::from_state(&app),
        );
        app.view.tab_hit_areas = view.tab_hit_areas;

        let backend = TestBackend::new(app.view.tab_bar_rect.width, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tab_bar(&app, frame, app.view.tab_bar_rect))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let blocked_rect = app.view.tab_hit_areas[1];
        let dot_cell = &buffer[(blocked_rect.x + 1, blocked_rect.y)];
        // Assert against `state_dot` itself, not a copy of the glyph it happens
        // to return today, so changing a state mark can't silently invalidate
        // this test.
        let (blocked_dot, blocked_style) = state_icon(
            crate::detect::AgentState::Blocked,
            false,
            app.status_indicators,
            &app.palette,
        );
        assert_eq!(dot_cell.symbol(), blocked_dot);
        assert_eq!(dot_cell.style().fg, blocked_style.fg);
        // The dot never repaints the tab chip's own background.
        assert_eq!(dot_cell.style().bg, Some(app.palette.surface0));

        let unknown_rect = app.view.tab_hit_areas[0];
        assert_eq!(
            buffer[(unknown_rect.x + 1, unknown_rect.y)].symbol(),
            " ",
            "a tab with no agent draws no mark, but still reserves the cell"
        );
    }

    #[test]
    fn decorations_reserve_stable_width_independent_of_agent_state() {
        let mut ws = Workspace::test_new("test");
        ws.tabs[0].set_custom_name("agents".into());

        let plain = tab_width(&ws, 0, TabLabelDecor::default());
        let decorated = tab_width(
            &ws,
            0,
            TabLabelDecor {
                state_dot: true,
                index: true,
            },
        );
        // dot + space + "1" + space
        assert_eq!(decorated, plain + 4);
    }

    #[test]
    fn index_prefix_is_skipped_for_auto_named_tabs() {
        let ws = Workspace::test_new("test");
        let decor = TabLabelDecor {
            state_dot: false,
            index: true,
        };

        assert_eq!(tab_index_prefix(&ws, 0, decor), None);
        assert_eq!(
            tab_width(&ws, 0, decor),
            tab_width(&ws, 0, TabLabelDecor::default()),
            "an auto-named tab reserves no width for a number it will not draw"
        );
    }
}
