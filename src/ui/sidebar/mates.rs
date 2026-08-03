//! The second-mate selector: a drop-down over the fleet's live rank-1 nodes.
//!
//! It is derived, never configured. With only the First Mate open there are no
//! second mates and so no control at all; open one and the control appears with
//! one entry; start a session and every mate that came up is in the list. The
//! entries come out of the same tree the sidebar draws, so a mate that goes away
//! leaves the drop-down at the same moment it leaves the tree.
//!
//! A drop-down rather than a tab row, for the reason the captain gave: with
//! every second mate live there is no horizontal room for tabs, and a drop-down
//! costs one control's width however many mates exist.
//!
//! Selecting a mate is deliberately only *observable* here — the control names
//! it and the tree marks it. Re-rooting the view onto that mate, and the
//! transition that carries it, belong to the view-transition work.

use ratatui::layout::Rect;

use super::WorkspaceListEntry;
use crate::app::AppState;
use crate::ui::text::{display_width_u16, middle_elide};

/// Rank in the fleet tree that counts as a second mate. Rank 0 is the First
/// Mate; workers and sub agents are rank 2 and below.
const SECOND_MATE_DEPTH: u8 = 1;

/// The closed control's chevron, and the marker on the open list's rows.
const CHEVRON: &str = "▾";

/// Label the control falls back to when nothing is selected.
const UNSELECTED_LABEL: &str = "mates";

/// Widest the control is allowed to get, so a long mate name cannot push the
/// `spaces` title off its own header row.
fn control_budget(area: Rect) -> u16 {
    area.width.saturating_sub(display_width_u16(" spaces") + 1)
}

/// The live second mates, in the order the tree draws them.
///
/// Names are the tree's own handles — a Space's label, a pane's agent name —
/// so what the drop-down says matches what the row says.
pub(crate) fn second_mates(app: &AppState) -> Vec<String> {
    let agents = super::sidebar_agent_entries(app);
    super::workspace_list_entries(app)
        .into_iter()
        .filter(|entry| entry.depth() == SECOND_MATE_DEPTH)
        .filter_map(|entry| match entry {
            WorkspaceListEntry::Workspace { ws_idx, .. } => super::space_tree_name(app, ws_idx),
            WorkspaceListEntry::Agent { entry_idx, .. } => agents
                .get(entry_idx)
                .and_then(|entry| entry.agent_name.clone()),
        })
        .collect()
}

/// The selected mate, if it is still live.
///
/// Resolved against the current fleet on every read rather than trusted from
/// state: a mate that finished and left the tree must not keep a control
/// pointing at it.
pub(crate) fn selected_mate(app: &AppState) -> Option<String> {
    let selected = app.selected_second_mate.as_deref()?;
    second_mates(app).into_iter().find(|mate| mate == selected)
}

fn control_label(app: &AppState) -> String {
    let name = selected_mate(app).unwrap_or_else(|| UNSELECTED_LABEL.to_string());
    format!("{CHEVRON} {name}")
}

/// The closed control, right-aligned on the Spaces header row.
///
/// `Rect::default()` when no second mate is live — the control does not exist
/// then, rather than existing and being empty.
pub(crate) fn selector_rect(app: &AppState, area: Rect) -> Rect {
    if area.width == 0 || area.height == 0 || second_mates(app).is_empty() {
        return Rect::default();
    }
    let budget = control_budget(area);
    if budget == 0 {
        return Rect::default();
    }
    let label = control_label(app);
    let width = display_width_u16(&label).min(budget);
    if width == 0 {
        return Rect::default();
    }
    Rect::new(area.x + area.width.saturating_sub(width), area.y, width, 1)
}

/// The control's drawn text, already fitted to [`selector_rect`].
pub(crate) fn selector_label(app: &AppState, area: Rect) -> String {
    let rect = selector_rect(app, area);
    if rect == Rect::default() {
        return String::new();
    }
    middle_elide(&control_label(app), rect.width as usize)
}

/// The open list, drawn directly under the control.
///
/// It hangs off the control's right edge so a long mate name grows leftwards
/// into the panel rather than off it.
pub(crate) fn menu_rect(app: &AppState, area: Rect) -> Rect {
    if !app.second_mate_selector_open {
        return Rect::default();
    }
    let control = selector_rect(app, area);
    if control == Rect::default() {
        return Rect::default();
    }
    let mates = second_mates(app);
    let rows = mates.len().min(area.height.saturating_sub(1) as usize);
    if rows == 0 {
        return Rect::default();
    }
    // Two cells of padding for the selected marker and a leading space.
    let widest = mates
        .iter()
        .map(|mate| display_width_u16(mate).saturating_add(2))
        .max()
        .unwrap_or(0);
    let width = widest.max(control.width).min(area.width);
    let right = control.x + control.width;
    Rect::new(
        right.saturating_sub(width).max(area.x),
        control.y + 1,
        width,
        rows as u16,
    )
}

/// Row text for the open list, one per visible mate.
pub(crate) fn menu_rows(app: &AppState, area: Rect) -> Vec<(String, bool)> {
    let rect = menu_rect(app, area);
    if rect == Rect::default() {
        return Vec::new();
    }
    let selected = selected_mate(app);
    second_mates(app)
        .into_iter()
        .take(rect.height as usize)
        .map(|mate| {
            let marked = selected.as_deref() == Some(mate.as_str());
            let mark = if marked { "•" } else { " " };
            let name = middle_elide(&mate, rect.width.saturating_sub(2) as usize);
            (format!("{mark} {name}"), marked)
        })
        .collect()
}

/// Which mate the open list has at `col`/`row`, if any.
pub(crate) fn menu_entry_at(app: &AppState, area: Rect, col: u16, row: u16) -> Option<String> {
    let rect = menu_rect(app, area);
    if rect == Rect::default()
        || col < rect.x
        || col >= rect.x + rect.width
        || row < rect.y
        || row >= rect.y + rect.height
    {
        return None;
    }
    second_mates(app).into_iter().nth((row - rect.y) as usize)
}

/// Whether `col`/`row` is on the closed control.
pub(crate) fn hits_selector(app: &AppState, area: Rect, col: u16, row: u16) -> bool {
    let rect = selector_rect(app, area);
    rect.width > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

/// Whether `col`/`row` is anywhere the selector currently owns — the control or
/// its open list.
///
/// The sidebar's divider grab band runs over the panel's last content column,
/// which is exactly where this control is right-aligned. Without carving it out
/// every press on the control becomes a divider drag and the drop-down never
/// opens.
pub(crate) fn owns_cell(app: &AppState, area: Rect, col: u16, row: u16) -> bool {
    if hits_selector(app, area, col, row) {
        return true;
    }
    let menu = menu_rect(app, area);
    menu.width > 0
        && col >= menu.x
        && col < menu.x + menu.width
        && row >= menu.y
        && row < menu.y + menu.height
}
