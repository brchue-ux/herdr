use bytes::Bytes;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Direction, Rect};
use tracing::warn;

use crate::{
    app::state::{
        AppState, ContextMenuKind, ContextMenuState, DragState, DragTarget, MenuListState, Mode,
        RightClickPassthroughGesture, TabPressState, ViewLayout, WorkspacePressState,
    },
    layout::{PaneInfo, SplitBorder},
    selection::Selection,
    terminal::TerminalRuntimeRegistry,
};

#[cfg(test)]
use super::WheelRouting;
use super::{
    modal::{
        apply_global_menu_action, confirm_close_cancel, global_menu_actions, leave_modal,
        modal_action_from_buttons, open_global_menu, open_new_tab_dialog, ModalAction,
    },
    settings::SettingsAction,
    ScrollbarClickTarget, TAB_DRAG_THRESHOLD, WORKSPACE_DRAG_THRESHOLD,
};

pub(super) enum MouseAction {
    NewWorkspace,
    Settings(SettingsAction),
    FocusWorkspace {
        ws_idx: usize,
    },
    FocusTab {
        tab_idx: usize,
    },
    FocusPane {
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    },
    FocusToastTarget,
    MoveWorkspace {
        source_ws_idx: usize,
        insert_idx: usize,
    },
    MoveWorkspaceBlock {
        params: crate::api::schema::WorkspaceMoveBlockParams,
    },
    MoveTab {
        ws_idx: usize,
        source_tab_idx: usize,
        insert_idx: usize,
    },
    SetSplitRatio {
        path: Vec<bool>,
        ratio: f32,
    },
    RenameModal(ModalAction),
    ConfirmCloseAccept,
    ContextMenu {
        menu: ContextMenuState,
        idx: usize,
    },
}

enum MobileMouseResult {
    Ignored,
    Consumed,
    Action(MouseAction),
}

impl AppState {
    pub(crate) fn handle_pane_mouse_only(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        mouse: MouseEvent,
    ) {
        if self.mode != Mode::Terminal {
            return;
        }
        let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() else {
            return;
        };

        match mouse.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                self.forward_pane_reported_wheel(terminal_runtimes, &info, mouse);
            }
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                self.forward_pane_mouse_button(terminal_runtimes, &info, mouse);
            }
            MouseEventKind::Moved => {
                self.forward_pane_mouse_motion(terminal_runtimes, &info, mouse);
            }
        }
    }

    pub(super) fn handle_mouse(
        &mut self,
        terminal_runtimes: &mut TerminalRuntimeRegistry,
        mouse: MouseEvent,
    ) -> Option<MouseAction> {
        self.track_sidebar_divider_hover(mouse);

        if self.mode == Mode::Onboarding {
            self.handle_onboarding_mouse(mouse);
            return None;
        }

        if self.mode == Mode::Terminal
            && self.clickable_toast_at(mouse.column, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            return Some(MouseAction::FocusToastTarget);
        }

        if self.mode == Mode::Terminal
            && self.clickable_toast_at(mouse.column, mouse.row)
            && matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
        {
            return None;
        }

        if self.mode == Mode::Settings {
            return self.handle_settings_mouse(mouse).map(MouseAction::Settings);
        }

        let launcher_enabled = self.view.layout != ViewLayout::Mobile
            && !self.sidebar_collapsed
            && matches!(
                self.mode,
                Mode::Terminal
                    | Mode::Navigate
                    | Mode::Resize
                    | Mode::GlobalMenu
                    | Mode::KeybindHelp
            );
        let launcher = self.global_launcher_rect();
        let launcher_hit = launcher_enabled
            && mouse.column >= launcher.x
            && mouse.column < launcher.x + launcher.width
            && mouse.row >= launcher.y
            && mouse.row < launcher.y + launcher.height;

        if matches!(mouse.kind, MouseEventKind::Moved) && self.mode == Mode::GlobalMenu {
            let actions = global_menu_actions(self);
            let hovered = self
                .global_menu_item_at(mouse.column, mouse.row)
                .and_then(|action| actions.iter().position(|item| *item == action));
            self.global_menu.hover(hovered);
            return None;
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && launcher_hit {
            if self.mode == Mode::GlobalMenu {
                leave_modal(self);
            } else {
                open_global_menu(self);
            }
            return None;
        }

        if self.mode == Mode::GlobalMenu {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(action) = self.global_menu_item_at(mouse.column, mouse.row) {
                    apply_global_menu_action(self, action);
                } else {
                    leave_modal(self);
                }
            }
            return None;
        }

        if self.mode == Mode::KeybindHelp {
            return None;
        }

        if self.view.layout == ViewLayout::Mobile {
            match self.handle_mobile_mouse(mouse) {
                MobileMouseResult::Ignored => {}
                MobileMouseResult::Consumed => return None,
                MobileMouseResult::Action(action) => return Some(action),
            }
        }

        let sidebar = self.view.sidebar_rect;
        let in_sidebar = mouse.column >= sidebar.x
            && mouse.column < sidebar.x + sidebar.width
            && mouse.row >= sidebar.y
            && mouse.row < sidebar.y + sidebar.height;

        if self.handle_right_click_passthrough(terminal_runtimes, mouse, in_sidebar) {
            return None;
        }

        if self.mode == Mode::OpenExistingWorktree {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(open) = &mut self.worktree_open {
                        open.select_previous_filtered();
                    }
                    return None;
                }
                MouseEventKind::ScrollDown => {
                    if let Some(open) = &mut self.worktree_open {
                        open.select_next_filtered();
                    }
                    return None;
                }
                _ => {}
            }
        }

        if matches!(
            self.mode,
            Mode::NewLinkedWorktree | Mode::OpenExistingWorktree | Mode::ConfirmRemoveWorktree
        ) && !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        {
            return None;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                self.selection_autoscroll = None;
                self.workspace_press = None;

                if self.mode == Mode::ConfirmClose {
                    let popup = self.confirm_close_rect();
                    let inner = Rect::new(
                        popup.x + 1,
                        popup.y + 1,
                        popup.width.saturating_sub(2),
                        popup.height.saturating_sub(2),
                    );
                    let (confirm, cancel) = crate::ui::confirm_close_button_rects(inner);
                    match modal_action_from_buttons(
                        mouse.column,
                        mouse.row,
                        &[
                            (confirm, ModalAction::Confirm),
                            (cancel, ModalAction::Cancel),
                        ],
                    ) {
                        Some(ModalAction::Confirm) => {
                            return Some(MouseAction::ConfirmCloseAccept);
                        }
                        Some(ModalAction::Cancel) | None => confirm_close_cancel(self),
                        _ => {}
                    }
                    return None;
                }

                if self.mode == Mode::NewLinkedWorktree {
                    if let Some(inner) =
                        crate::ui::new_linked_worktree_inner_rect(self.screen_rect())
                    {
                        let (create, cancel) = crate::ui::new_linked_worktree_button_rects(inner);
                        match modal_action_from_buttons(
                            mouse.column,
                            mouse.row,
                            &[
                                (create, ModalAction::Confirm),
                                (cancel, ModalAction::Cancel),
                            ],
                        ) {
                            Some(ModalAction::Confirm) => {
                                self.request_submit_worktree_create = true;
                            }
                            Some(ModalAction::Cancel)
                                if !self
                                    .worktree_create
                                    .as_ref()
                                    .is_some_and(|create| create.creating) =>
                            {
                                self.worktree_create = None;
                                self.name_input.clear();
                                self.name_input_replace_on_type = false;
                                leave_modal(self);
                            }
                            _ => {}
                        }
                    }
                    return None;
                }

                if self.mode == Mode::OpenExistingWorktree {
                    if let Some(open) = self.worktree_open.as_ref() {
                        if let Some(inner) = crate::ui::open_existing_worktree_inner_rect(
                            self.screen_rect(),
                            open.entries.len(),
                        ) {
                            let filtered = open.filtered_indices();
                            let max_rows =
                                crate::ui::open_existing_worktree_max_visible_rows(inner);
                            let start =
                                crate::ui::open_existing_worktree_visible_start(open, max_rows);
                            if mouse.row == inner.y.saturating_add(1)
                                && mouse.column >= inner.x
                                && mouse.column < inner.x.saturating_add(inner.width)
                            {
                                if let Some(open) = &mut self.worktree_open {
                                    open.search_focused = true;
                                }
                                return None;
                            }
                            let row_idx = if rect_contains(inner, mouse.column, mouse.row) {
                                mouse
                                    .row
                                    .checked_sub(inner.y.saturating_add(3))
                                    .map(usize::from)
                                    .map(|row| row / 2)
                                    .filter(|row| *row < max_rows)
                                    .and_then(|row| filtered.get(start + row).copied())
                            } else {
                                None
                            };
                            if let Some(entry_idx) = row_idx {
                                if let Some(open) = &mut self.worktree_open {
                                    open.selected = entry_idx;
                                }
                                self.request_submit_worktree_open = true;
                                return None;
                            }

                            let (open_button, cancel) =
                                crate::ui::open_existing_worktree_button_rects(inner);
                            match modal_action_from_buttons(
                                mouse.column,
                                mouse.row,
                                &[
                                    (open_button, ModalAction::Confirm),
                                    (cancel, ModalAction::Cancel),
                                ],
                            ) {
                                Some(ModalAction::Confirm) => {
                                    self.request_submit_worktree_open = true;
                                }
                                Some(ModalAction::Cancel) => {
                                    self.worktree_open = None;
                                    leave_modal(self);
                                }
                                _ => {}
                            }
                        }
                    }
                    return None;
                }

                if self.mode == Mode::ConfirmRemoveWorktree {
                    if let Some(popup) = crate::ui::remove_worktree_popup_rect(self.screen_rect()) {
                        let inner = Rect::new(
                            popup.x + 1,
                            popup.y + 1,
                            popup.width.saturating_sub(2),
                            popup.height.saturating_sub(2),
                        );
                        let force_confirmation = self
                            .worktree_remove
                            .as_ref()
                            .is_some_and(|remove| remove.force_confirmation);
                        let (remove, cancel) =
                            crate::ui::remove_worktree_button_rects(inner, force_confirmation);
                        match modal_action_from_buttons(
                            mouse.column,
                            mouse.row,
                            &[
                                (remove, ModalAction::Confirm),
                                (cancel, ModalAction::Cancel),
                            ],
                        ) {
                            Some(ModalAction::Confirm) => {
                                self.request_submit_worktree_remove = true;
                            }
                            Some(ModalAction::Cancel)
                                if !self
                                    .worktree_remove
                                    .as_ref()
                                    .is_some_and(|remove| remove.removing) =>
                            {
                                self.worktree_remove = None;
                                leave_modal(self);
                            }
                            _ => {}
                        }
                    }
                    return None;
                }

                if matches!(
                    self.mode,
                    Mode::RenameWorkspace | Mode::RenameTab | Mode::RenamePane
                ) {
                    let action = self
                        .rename_modal_inner()
                        .map(crate::ui::rename_button_rects)
                        .and_then(|(save, clear, cancel)| {
                            modal_action_from_buttons(
                                mouse.column,
                                mouse.row,
                                &[
                                    (save, ModalAction::Save),
                                    (clear, ModalAction::Clear),
                                    (cancel, ModalAction::Cancel),
                                ],
                            )
                        })
                        .unwrap_or(ModalAction::Cancel);
                    return Some(MouseAction::RenameModal(action));
                }

                if self.mode == Mode::ContextMenu {
                    let item_idx = self.context_menu_item_at(mouse.column, mouse.row);
                    if let Some(menu) = self.context_menu.take() {
                        if let Some(idx) = item_idx {
                            return Some(MouseAction::ContextMenu { menu, idx });
                        } else {
                            leave_modal(self);
                        }
                    }
                    return None;
                }

                if let Some(grab_offset) = self.sidebar_divider_grab_at(mouse.column, mouse.row) {
                    self.drag = Some(DragState {
                        target: DragTarget::SidebarDivider { grab_offset },
                    });
                    self.set_manual_sidebar_width(AppState::divider_pos_from_grab(
                        mouse.column,
                        grab_offset,
                    ));
                    return None;
                }

                if !in_sidebar {
                    if let Some(border) = self.find_border_at(mouse.column, mouse.row) {
                        let grab_offset = match border.direction {
                            Direction::Horizontal => border.pos.saturating_sub(mouse.column),
                            Direction::Vertical => border.pos.saturating_sub(mouse.row),
                        };
                        self.drag = Some(DragState {
                            target: DragTarget::PaneSplit {
                                path: border.path.clone(),
                                direction: border.direction,
                                area: border.area,
                                grab_offset,
                            },
                        });
                        return None;
                    }

                    if let Some((pane_id, target)) =
                        self.scrollbar_target_at(terminal_runtimes, mouse.column, mouse.row)
                    {
                        self.focus_pane(pane_id);
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::PaneScrollbar {
                                        pane_id,
                                        grab_row_offset,
                                    },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_pane_scroll_offset(
                                    terminal_runtimes,
                                    pane_id,
                                    offset_from_bottom,
                                );
                            }
                        }
                        if self.mode != Mode::Terminal {
                            self.mode = Mode::Terminal;
                        }
                        return None;
                    }
                }

                if self.mode_bar_covers_tab_row(mouse.column, mouse.row) {
                    return None;
                }

                if self.on_tab_scroll_left_button(mouse.column, mouse.row) {
                    self.scroll_tabs_left();
                    return None;
                }
                if self.on_tab_scroll_right_button(mouse.column, mouse.row) {
                    self.scroll_tabs_right();
                    return None;
                }
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_at(mouse.column, mouse.row))
                {
                    self.tab_press = Some(TabPressState {
                        ws_idx,
                        tab_idx,
                        start_col: mouse.column,
                        start_row: mouse.row,
                    });
                    return None;
                }
                if self.on_new_tab_button(mouse.column, mouse.row) {
                    if self.prompt_new_tab_name {
                        open_new_tab_dialog(self);
                    } else {
                        self.request_new_tab = true;
                        self.mode = Mode::Terminal;
                    }
                    return None;
                }

                if in_sidebar {
                    if self.on_sidebar_toggle(mouse.column, mouse.row) {
                        self.sidebar_collapsed = !self.sidebar_collapsed;
                        return None;
                    }

                    if self.sidebar_collapsed {
                        if let Some(idx) = self.collapsed_workspace_at_row(mouse.row) {
                            self.mode = Mode::Terminal;
                            return Some(MouseAction::FocusWorkspace { ws_idx: idx });
                        }

                        return None;
                    }

                    let new_button = self.sidebar_new_button_rect();
                    let on_new_button = mouse.row >= new_button.y
                        && mouse.row < new_button.y + new_button.height
                        && mouse.column >= new_button.x
                        && mouse.column < new_button.x + new_button.width;
                    if on_new_button {
                        return Some(MouseAction::NewWorkspace);
                    }

                    if let Some(target) =
                        self.workspace_list_scrollbar_target_at(mouse.column, mouse.row)
                    {
                        match target {
                            ScrollbarClickTarget::Thumb { grab_row_offset } => {
                                self.drag = Some(DragState {
                                    target: DragTarget::WorkspaceListScrollbar { grab_row_offset },
                                });
                            }
                            ScrollbarClickTarget::Track { offset_from_bottom } => {
                                self.set_workspace_list_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        return None;
                    }

                    // Ahead of both row handlers: the badge sits inside a card,
                    // so leaving it to them would focus the mate's pane instead
                    // of opening the summaries the user aimed at.
                    if let Some(owner) = self.worker_summary_badge_at(mouse.column, mouse.row) {
                        self.open_worker_summaries(owner);
                        return None;
                    }

                    // The tray owns rows the tree was never laid out over, so
                    // this cannot steal a click from a row. It is tested before
                    // the tree anyway, for the same reason the summary badge is:
                    // a hit here is unambiguous and a fall-through would focus
                    // whatever the tree happens to think is at that row.
                    if let Some(signal) =
                        crate::ui::signal_tray_badge_at(self, mouse.column, mouse.row)
                    {
                        self.open_signal_tray_popup(signal);
                        return None;
                    }
                    if crate::ui::signal_tray_menu_at(self, mouse.column, mouse.row) {
                        self.open_signal_tray_legend();
                        return None;
                    }

                    if let Some(ws_idx) = self.workspace_group_chevron_at(mouse.column, mouse.row) {
                        if let Some((key, collapsed)) =
                            crate::ui::workspace_parent_group_state(self, ws_idx)
                        {
                            if collapsed {
                                self.collapsed_space_keys.remove(&key);
                            } else {
                                self.collapsed_space_keys.insert(key);
                            }
                            self.mark_session_dirty();
                            return None;
                        }
                    }

                    // Agent rows are tested first. They sit inside the same
                    // card list as Spaces, so leaving them to the Space handler
                    // would focus the workspace they live in instead of the
                    // pane the user actually clicked.
                    if let Some((ws_idx, pane_id)) = self.sidebar_agent_target_at(mouse.row) {
                        self.mode = Mode::Terminal;
                        return Some(MouseAction::FocusPane { ws_idx, pane_id });
                    }

                    if let Some(idx) = self.workspace_at_row(mouse.row) {
                        self.workspace_press = Some(WorkspacePressState {
                            ws_idx: idx,
                            start_col: mouse.column,
                            start_row: mouse.row,
                        });
                        return None;
                    }
                } else if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }

                    if self.forward_pane_mouse_button(terminal_runtimes, &info, mouse) {
                        self.selection = None;
                        self.selection_autoscroll = None;
                        return self.mouse_pane_focus_action(info.id);
                    }

                    let (row, col) = (
                        mouse.row - info.inner_rect.y,
                        mouse.column - info.inner_rect.x,
                    );
                    self.selection = Some(Selection::anchor(
                        info.id,
                        row,
                        col,
                        self.pane_scroll_metrics(terminal_runtimes, info.id),
                    ));
                    return self.mouse_pane_focus_action(info.id);
                } else if let Some(info) = self.view.pane_infos.iter().find(|p| {
                    mouse.column >= p.rect.x
                        && mouse.column < p.rect.x + p.rect.width
                        && mouse.row >= p.rect.y
                        && mouse.row < p.rect.y + p.rect.height
                }) {
                    let id = info.id;
                    if self.mode != Mode::Terminal {
                        self.mode = Mode::Terminal;
                    }
                    return self.mouse_pane_focus_action(id);
                }
            }

            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection.is_some() {
                    self.update_selection_drag(terminal_runtimes, mouse.column, mouse.row);
                    return None;
                }

                if self.drag.is_none() {
                    if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                        if self.forward_pane_mouse_button(terminal_runtimes, &info, mouse) {
                            self.selection = None;
                            self.selection_autoscroll = None;
                            return None;
                        }
                    }
                }

                let workspace_drop_target = self.workspace_drop_target_at_row(mouse.row);
                let tab_drop_index = self.tab_drop_index_at(mouse.column, mouse.row);
                if self.drag.is_none() {
                    if let Some(press) = &self.workspace_press {
                        let delta_col = mouse.column.abs_diff(press.start_col);
                        let delta_row = mouse.row.abs_diff(press.start_row);
                        let can_reorder = self.workspaces.get(press.ws_idx).is_some_and(|ws| {
                            ws.worktree_space()
                                .is_none_or(|space| !space.is_linked_worktree)
                        });
                        if can_reorder && delta_col.max(delta_row) >= WORKSPACE_DRAG_THRESHOLD {
                            self.drag = Some(DragState {
                                target: DragTarget::WorkspaceReorder {
                                    source_ws_idx: press.ws_idx,
                                    drop_target: workspace_drop_target,
                                },
                            });
                        }
                    } else if let Some(press) = &self.tab_press {
                        let delta_col = mouse.column.abs_diff(press.start_col);
                        let delta_row = mouse.row.abs_diff(press.start_row);
                        if delta_col.max(delta_row) >= TAB_DRAG_THRESHOLD {
                            self.drag = Some(DragState {
                                target: DragTarget::TabReorder {
                                    ws_idx: press.ws_idx,
                                    source_tab_idx: press.tab_idx,
                                    insert_idx: tab_drop_index,
                                },
                            });
                        }
                    }
                }

                if let Some(DragState {
                    target: DragTarget::WorkspaceReorder { drop_target, .. },
                }) = &mut self.drag
                {
                    *drop_target = workspace_drop_target;
                } else if let Some(DragState {
                    target:
                        DragTarget::TabReorder {
                            ws_idx, insert_idx, ..
                        },
                }) = &mut self.drag
                {
                    if self.active == Some(*ws_idx) {
                        *insert_idx = tab_drop_index;
                    }
                } else if let Some(drag) = &self.drag {
                    match &drag.target {
                        DragTarget::WorkspaceReorder { .. } | DragTarget::TabReorder { .. } => {}
                        DragTarget::WorkspaceListScrollbar { grab_row_offset } => {
                            if let Some(offset_from_bottom) =
                                self.workspace_list_offset_for_drag_row(mouse.row, *grab_row_offset)
                            {
                                self.set_workspace_list_offset_from_bottom(offset_from_bottom);
                            }
                        }
                        DragTarget::PaneSplit {
                            path,
                            direction,
                            area,
                            grab_offset,
                        } => {
                            let ratio = match direction {
                                Direction::Horizontal => {
                                    (mouse
                                        .column
                                        .saturating_add(*grab_offset)
                                        .saturating_sub(area.x))
                                        as f32
                                        / area.width.max(1) as f32
                                }
                                Direction::Vertical => {
                                    (mouse
                                        .row
                                        .saturating_add(*grab_offset)
                                        .saturating_sub(area.y))
                                        as f32
                                        / area.height.max(1) as f32
                                }
                            };
                            let ratio = ratio.clamp(0.1, 0.9);
                            let path = path.clone();
                            return Some(MouseAction::SetSplitRatio { path, ratio });
                        }
                        DragTarget::PaneScrollbar {
                            pane_id,
                            grab_row_offset,
                        } => {
                            if let Some(offset_from_bottom) = self.scrollbar_offset_for_pane_row(
                                terminal_runtimes,
                                *pane_id,
                                mouse.row,
                                *grab_row_offset,
                            ) {
                                self.set_pane_scroll_offset(
                                    terminal_runtimes,
                                    *pane_id,
                                    offset_from_bottom,
                                );
                            }
                        }
                        DragTarget::SidebarDivider { grab_offset } => {
                            self.set_manual_sidebar_width(AppState::divider_pos_from_grab(
                                mouse.column,
                                *grab_offset,
                            ));
                        }
                        DragTarget::ReleaseNotesScrollbar { .. }
                        | DragTarget::ProductAnnouncementScrollbar { .. }
                        | DragTarget::KeybindHelpScrollbar { .. } => {}
                    }
                }
            }

            MouseEventKind::Up(MouseButton::Left) => {
                // Mouse-up either finishes a drag selection or releases after a
                // double-click word selection; the latter is already finalized.
                if let Some(selection) = self.selection.as_ref() {
                    let was_click = selection.was_just_click();
                    let was_finalized = selection.is_finalized();

                    self.workspace_press = None;
                    self.tab_press = None;
                    self.drag = None;
                    self.selection_autoscroll = None;
                    if was_click {
                        self.selection = None;
                    } else if was_finalized {
                        // Double-click already finalized this word selection.
                    } else if self.copy_on_select {
                        self.copy_selection(terminal_runtimes);
                    } else if let Some(selection) = self.selection.as_mut() {
                        selection.finish();
                    }
                    return None;
                }

                if self.drag.is_none() {
                    if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                        if self.forward_pane_mouse_button(terminal_runtimes, &info, mouse) {
                            self.selection = None;
                            self.selection_autoscroll = None;
                            self.workspace_press = None;
                            self.tab_press = None;
                            self.drag = None;
                            return None;
                        }
                    }
                }

                let workspace_press = self.workspace_press.take();
                let tab_press = self.tab_press.take();
                // The held look describes a live drag. Hover tracking runs at
                // the top of this function, while the drag is still set, so the
                // release has to retire it here or a divider let go inside the
                // detent band stays lit as caught.
                self.sidebar_divider_detent = false;
                match self.drag.take() {
                    Some(DragState {
                        target:
                            DragTarget::WorkspaceReorder {
                                source_ws_idx,
                                drop_target: Some(drop_target),
                            },
                    }) => {
                        if let Some(params) =
                            self.workspace_move_block_params(source_ws_idx, drop_target)
                        {
                            if self
                                .workspaces
                                .get(source_ws_idx)
                                .is_some_and(|workspace| workspace.worktree_space().is_some())
                            {
                                return Some(MouseAction::MoveWorkspaceBlock { params });
                            }
                            let insert_idx = params
                                .before_workspace_id
                                .as_ref()
                                .and_then(|id| {
                                    self.workspaces
                                        .iter()
                                        .position(|workspace| workspace.id == *id)
                                })
                                .unwrap_or(self.workspaces.len());
                            return Some(MouseAction::MoveWorkspace {
                                source_ws_idx,
                                insert_idx,
                            });
                        }
                    }
                    Some(DragState {
                        target:
                            DragTarget::TabReorder {
                                ws_idx,
                                source_tab_idx,
                                insert_idx: Some(insert_idx),
                            },
                    }) => {
                        if self.active == Some(ws_idx) {
                            self.mode = Mode::Terminal;
                            return Some(MouseAction::MoveTab {
                                ws_idx,
                                source_tab_idx,
                                insert_idx,
                            });
                        }
                    }
                    Some(_) => {}
                    None => {
                        if let Some(press) = workspace_press {
                            self.mode = Mode::Terminal;
                            // A Space row goes to the Space's *pane*, not merely
                            // to the Space. Focusing a workspace you are already
                            // in changes nothing, which made a click on a second
                            // mate's row a silent no-op for as long as any pane
                            // inside that mate had focus. Every row now navigates
                            // whatever the current focus is — see
                            // `AppState::workspace_home_pane`.
                            if let Some(pane_id) = self.workspace_home_pane(press.ws_idx) {
                                return Some(MouseAction::FocusPane {
                                    ws_idx: press.ws_idx,
                                    pane_id,
                                });
                            }
                            return Some(MouseAction::FocusWorkspace {
                                ws_idx: press.ws_idx,
                            });
                        }
                        if let Some(press) = tab_press {
                            if self.active == Some(press.ws_idx) {
                                self.mode = Mode::Terminal;
                                return Some(MouseAction::FocusTab {
                                    tab_idx: press.tab_idx,
                                });
                            }
                        }
                    }
                }
            }

            MouseEventKind::Up(MouseButton::Middle) | MouseEventKind::Drag(MouseButton::Middle)
                if !in_sidebar =>
            {
                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    let _ = self.forward_pane_mouse_button(terminal_runtimes, &info, mouse);
                }
            }

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.mode_bar_covers_tab_row(mouse.column, mouse.row) => {}

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.on_tab_bar(mouse.column, mouse.row) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                            if !ws.tabs.is_empty() {
                                let prev = if ws.active_tab == 0 {
                                    ws.tabs.len() - 1
                                } else {
                                    ws.active_tab - 1
                                };
                                return Some(MouseAction::FocusTab { tab_idx: prev });
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if let Some(ws) = self.active.and_then(|i| self.workspaces.get(i)) {
                            if !ws.tabs.is_empty() {
                                let next = (ws.active_tab + 1) % ws.tabs.len();
                                return Some(MouseAction::FocusTab { tab_idx: next });
                            }
                        }
                    }
                    _ => {}
                }
            }

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if !in_sidebar && self.scroll_selection_with_wheel(terminal_runtimes, mouse) => {}

            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !in_sidebar => {
                self.selection = None;
                self.selection_autoscroll = None;
                self.handle_terminal_wheel(terminal_runtimes, mouse);
            }

            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                if self.mode == Mode::Terminal && !in_sidebar =>
            {
                if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    self.forward_pane_reported_wheel(terminal_runtimes, &info, mouse);
                }
            }

            MouseEventKind::ScrollUp if in_sidebar => {
                if crate::ui::should_show_scrollbar(crate::ui::workspace_list_scroll_metrics(
                    self,
                    self.workspace_list_rect(),
                )) {
                    self.scroll_workspace_list(-1);
                } else {
                    self.move_selected_workspace_by_visible_delta(-1);
                }
            }
            MouseEventKind::ScrollDown if in_sidebar => {
                if crate::ui::should_show_scrollbar(crate::ui::workspace_list_scroll_metrics(
                    self,
                    self.workspace_list_rect(),
                )) {
                    self.scroll_workspace_list(1);
                } else {
                    self.move_selected_workspace_by_visible_delta(1);
                }
            }

            MouseEventKind::Moved if self.mode == Mode::ContextMenu => {
                let hovered = self.context_menu_item_at(mouse.column, mouse.row);
                if let Some(menu) = &mut self.context_menu {
                    menu.list.hover(hovered);
                }
            }

            MouseEventKind::Moved if self.mode == Mode::Terminal && !in_sidebar => {
                if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
                    let _ = self.forward_pane_mouse_motion(terminal_runtimes, &info, mouse);
                }
            }

            MouseEventKind::Down(MouseButton::Right) if in_sidebar && !self.sidebar_collapsed => {
                self.workspace_press = None;
                self.tab_press = None;
                if self
                    .workspace_list_scrollbar_target_at(mouse.column, mouse.row)
                    .is_some()
                {
                    return None;
                }
                if let Some(idx) = self.workspace_at_row(mouse.row) {
                    self.selected = idx;
                    let kind = self
                        .workspaces
                        .get(idx)
                        .and_then(|ws| {
                            let group_state = crate::ui::workspace_parent_group_state(self, idx);
                            let git_space = ws.git_space().cloned().or_else(|| {
                                ws.resolved_identity_cwd_from(&self.terminals, terminal_runtimes)
                                    .as_deref()
                                    .and_then(crate::workspace::git_space_metadata)
                            });
                            let is_linked_worktree = ws.worktree_space().map_or_else(
                                || {
                                    git_space
                                        .as_ref()
                                        .is_some_and(|space| space.is_linked_worktree)
                                },
                                |space| space.is_linked_worktree,
                            );
                            let show_git_menu = ws.worktree_space().is_some()
                                || git_space
                                    .as_ref()
                                    .is_some_and(|space| !space.is_linked_worktree);
                            show_git_menu.then_some(ContextMenuKind::GitWorkspace {
                                ws_idx: idx,
                                is_linked_worktree,
                                has_worktree_children: group_state.is_some(),
                                collapsed: group_state
                                    .as_ref()
                                    .is_some_and(|(_, collapsed)| *collapsed),
                            })
                        })
                        .unwrap_or(ContextMenuKind::Workspace { ws_idx: idx });
                    self.context_menu = Some(ContextMenuState {
                        kind,
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right)
                if !self.mode_bar_covers_tab_row(mouse.column, mouse.row)
                    && self.tab_at(mouse.column, mouse.row).is_some() =>
            {
                if let (Some(ws_idx), Some(tab_idx)) =
                    (self.active, self.tab_at(mouse.column, mouse.row))
                {
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Tab { ws_idx, tab_idx },
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            MouseEventKind::Down(MouseButton::Right) if !in_sidebar => {
                if let Some(info) = self.pane_mouse_target(mouse.column, mouse.row).cloned() {
                    let ws_idx = self.active?;
                    let tab_idx = self
                        .workspaces
                        .get(ws_idx)
                        .map(|ws| ws.active_tab_index())?;
                    let previous_focused_pane_id = self
                        .workspaces
                        .get(ws_idx)
                        .and_then(|ws| ws.focused_pane_id());
                    let source_pane_id =
                        previous_focused_pane_id.filter(|pane_id| *pane_id != info.id);
                    let has_manual_label = self
                        .workspaces
                        .get(ws_idx)
                        .and_then(|ws| ws.pane_state(info.id))
                        .and_then(|pane| self.terminals.get(&pane.attached_terminal_id))
                        .and_then(|terminal| terminal.manual_label.as_ref())
                        .is_some();
                    self.context_menu = Some(ContextMenuState {
                        kind: ContextMenuKind::Pane {
                            ws_idx,
                            tab_idx,
                            pane_id: info.id,
                            source_pane_id,
                            has_manual_label,
                        },
                        x: mouse.column,
                        y: mouse.row,
                        list: MenuListState::new(0),
                    });
                    self.mode = Mode::ContextMenu;
                }
            }

            _ => {}
        }

        None
    }

    fn handle_mobile_mouse(&mut self, mouse: MouseEvent) -> MobileMouseResult {
        if self.mode == Mode::Navigate {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_mobile_switcher_at(mouse.column, mouse.row, -1);
                    return MobileMouseResult::Consumed;
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_mobile_switcher_at(mouse.column, mouse.row, 1);
                    return MobileMouseResult::Consumed;
                }
                MouseEventKind::Down(MouseButton::Left) => {}
                _ => return MobileMouseResult::Consumed,
            }
        } else if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return MobileMouseResult::Ignored;
        }

        if self.mode != Mode::Navigate {
            if !matches!(self.mode, Mode::Terminal | Mode::Resize) {
                return MobileMouseResult::Ignored;
            }
            if rect_contains(self.view.mobile_menu_hit_area, mouse.column, mouse.row) {
                self.mobile_switcher_scroll = 0;
                self.mode = Mode::Navigate;
                return MobileMouseResult::Consumed;
            }
            return MobileMouseResult::Ignored;
        }

        let areas = crate::ui::mobile_switcher_areas(self);
        if rect_contains(areas.close, mouse.column, mouse.row) {
            self.mode = Mode::Terminal;
            return MobileMouseResult::Consumed;
        }

        match crate::ui::mobile_switcher_target_at(self, mouse.column, mouse.row) {
            Some(crate::ui::MobileSwitcherTarget::NewWorkspace) => {
                return MobileMouseResult::Action(MouseAction::NewWorkspace);
            }
            Some(crate::ui::MobileSwitcherTarget::Workspace(ws_idx)) => {
                self.mode = Mode::Terminal;
                return MobileMouseResult::Action(MouseAction::FocusWorkspace { ws_idx });
            }
            Some(crate::ui::MobileSwitcherTarget::NewTab) => {
                if self.prompt_new_tab_name {
                    open_new_tab_dialog(self);
                } else {
                    self.request_new_tab = true;
                    self.mode = Mode::Terminal;
                }
            }
            Some(crate::ui::MobileSwitcherTarget::Tab(tab_idx)) => {
                self.mode = Mode::Terminal;
                return MobileMouseResult::Action(MouseAction::FocusTab { tab_idx });
            }
            Some(crate::ui::MobileSwitcherTarget::Agent {
                ws_idx,
                tab_idx: _,
                pane_id,
            }) => {
                self.mode = Mode::Terminal;
                return MobileMouseResult::Action(MouseAction::FocusPane { ws_idx, pane_id });
            }
            Some(crate::ui::MobileSwitcherTarget::Menu(action_idx)) => {
                let actions = global_menu_actions(self);
                if let Some(action) = actions.get(action_idx).copied() {
                    apply_global_menu_action(self, action);
                }
            }
            None => {}
        }

        MobileMouseResult::Consumed
    }

    fn scroll_mobile_switcher_at(&mut self, _col: u16, _row: u16, delta: i16) {
        let max_scroll = crate::ui::mobile_switcher_max_scroll(self);
        apply_scroll(
            &mut self.mobile_switcher_scroll,
            delta.saturating_mul(2),
            max_scroll,
        );
    }

    pub(crate) fn screen_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        let terminal = self.view.terminal_area;
        let x = sidebar.x.min(terminal.x);
        let y = sidebar.y.min(terminal.y);
        let right = (sidebar.x + sidebar.width).max(terminal.x + terminal.width);
        let bottom = (sidebar.y + sidebar.height).max(terminal.y + terminal.height);
        Rect::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    /// Where the scene stops being void and starts being a body.
    ///
    /// Rec.709 luminance, on the scale a channel is measured in. The scene's own
    /// void floor and its starfield sit well under this; a body's disc and the
    /// sun's corona sit well over it. Deliberately a *luminance* threshold
    /// rather than a list of body rects: what A29 is protecting is the light,
    /// and the light is what can be measured without asking the renderer where
    /// it put anything.
    #[cfg(test)]
    const BRIGHT_SCENE_FLOOR: f32 = 48.0;

    /// The widest the status stream is ever drawn, in columns.
    ///
    /// A cap on top of the third, so the narrow half of A24's contrast stays
    /// narrow on a very wide window. Enough for a short herdr sentence and no
    /// more — the stream says *what happened*, and anything that needs a
    /// paragraph is a toast's job or a pane's.
    const STATUS_FEED_MAX_COLS: u16 = 44;

    /// The narrowest it is worth drawing at. Below this every line is elided to
    /// a stub, which says less than drawing nothing and costs the scene a
    /// rectangle.
    const STATUS_FEED_MIN_COLS: u16 = 20;

    /// Where the machine register's readout is drawn, in cells.
    ///
    /// The top-right of the terminal area, inset by one cell. Top-right rather than anywhere else
    /// because that is the corner a reader's eye is *least* often in — this is a readout about the
    /// substrate, glanced at, never worked in — and because the sidebar owns the left edge.
    ///
    /// Returns an empty rect when the screen is too small to hold it, which is what stops the
    /// readout from covering a terminal somebody is actually using on a narrow window. A corner
    /// that will not fit is not drawn at all; it is not shrunk until it is unreadable.
    pub(crate) fn machine_corner_rect(&self) -> Rect {
        const COLS: u16 = 26;
        const ROWS: u16 = 8;
        const INSET: u16 = 1;

        let screen = self.screen_rect();
        // Measured against the **main area** — the frame outside the sidebar — rather than against
        // the whole screen, because that is the area the readout actually spends. Against the
        // screen, a narrow terminal with a wide sidebar passes the check and then covers most of
        // the little sky it has: 60 columns with a 44-wide sidebar leaves 16, and a 26-wide corner
        // in it takes two thirds of the main area. This is what makes `SKY_CLEAR_FLOOR` a
        // structural guarantee rather than something that happens to hold at the sizes anyone
        // tried — see `the_scene_keeps_most_of_the_main_area_to_itself`.
        let main_width = screen.width.saturating_sub(self.view.sidebar_rect.width);
        if main_width < COLS * 3 || screen.height < (ROWS + INSET * 2) * 2 {
            return Rect::new(screen.x, screen.y, 0, 0);
        }
        Rect::new(
            screen.x + screen.width - COLS - INSET,
            screen.y + INSET,
            COLS,
            ROWS,
        )
    }

    /// Where herdr's own status stream is drawn, in cells.
    ///
    /// **The bottom third of the main area, and narrow** — A24, and its reason
    /// is functional rather than compositional. The card states it plainly:
    /// *assistant output is long, and a narrow column makes it scroll past too
    /// fast to read*. So the two halves of the contrast are not a taste: the
    /// **prose** — which in herdr is the pane text, real PTY output — spans the
    /// whole frame outside the sidebar, and the **stream**, which is six short
    /// lines herdr wrote itself and nobody scrolls, stays narrow underneath it.
    ///
    /// Bottom-left rather than bottom-right because the machine register already
    /// owns a corner and two readouts sharing an edge would read as one panel.
    ///
    /// Empty when the stream has nothing to say, when the scene is not drawing,
    /// or when the main area is too small to give it a third — the same rule
    /// [`Self::machine_corner_rect`] follows, and for the same reason: a readout
    /// that will not fit is not drawn at all.
    pub(crate) fn status_feed_rect(&self) -> Rect {
        const INSET: u16 = 1;

        let screen = self.screen_rect();
        let lines = self.status_feed.len() as u16;
        if lines == 0 {
            return Rect::new(screen.x, screen.y, 0, 0);
        }
        let main_width = screen.width.saturating_sub(self.view.sidebar_rect.width);
        // A third of the main area, and never more: the stream is the narrow
        // half of A24's contrast, and a stream that grew with the window would
        // stop being the narrow half on a wide one.
        let width = (main_width / 3).min(Self::STATUS_FEED_MAX_COLS);
        let height = lines.min(crate::app::status_feed::TERM_MAX as u16);
        if width < Self::STATUS_FEED_MIN_COLS || screen.height < (height + INSET * 2) * 3 {
            return Rect::new(screen.x, screen.y, 0, 0);
        }
        // Sat on the floor of the main area rather than centred in its bottom
        // third: the stream is a margin note, and a margin note floating in the
        // middle of a third is a panel.
        let y = screen.y + screen.height - height - INSET;
        Rect::new(
            screen.x + screen.width.saturating_sub(main_width) + INSET,
            y,
            width,
            height,
        )
    }

    /// How much of the main area the scene still has to itself, in cells.
    ///
    /// H8, in herdr's own terms: *"of the frame outside the worker-tree panel, at least 60% of the
    /// area carries no interface element over it. If the regions plus the tree crowd the sky out,
    /// the thing the captain liked is gone."*
    ///
    /// **The main area is the frame outside the sidebar**, because the sidebar is the worker-tree
    /// panel — it is what the clause measures *around*, not something it counts against the sky.
    ///
    /// **Pane text is not an interface element here, and that is a real distinction rather than a
    /// convenient one.** herdr's scene is an opaque wash placed *under* the text with no pane
    /// background of its own, so a terminal region is ink on the scene rather than a panel over
    /// it — which is the state the artifact had to retire a clause to reach ("the terminal is
    /// unboxed... they are ink on the scene"). What does count is anything that puts a *surface*
    /// between the reader and the sky: today that is the machine register's corner.
    ///
    /// Returns `(covered_cells, main_area_cells)`.
    pub(crate) fn sky_coverage(&self) -> (u32, u32) {
        let screen = self.screen_rect();
        let sidebar = self.view.sidebar_rect;
        let main_width = screen.width.saturating_sub(sidebar.width);
        let main = u32::from(main_width) * u32::from(screen.height);

        // Clipped to the main area rather than counted whole: a surface that hung off the screen
        // would otherwise be able to report more coverage than there is area to cover.
        let clipped = |rect: Rect| {
            u32::from(rect.width.min(main_width)) * u32::from(rect.height.min(screen.height))
        };
        let covered = clipped(self.machine_corner_rect()) + clipped(self.status_feed_rect());

        (covered.min(main), main)
    }

    /// How much of the scene's *bright* half is under terminal ink.
    ///
    /// # Why this exists beside [`Self::sky_clear_fraction`]
    ///
    /// A29's second clause, and it is there to stop a false pass. The clear-area
    /// number counts *surfaces* — panels put between the reader and the sky —
    /// and it improves the moment one is removed. But the sky is not uniformly
    /// interesting: almost all of its light is in a few discs, and a frame whose
    /// clear-area number is excellent can still have every one of those discs
    /// under a line of text. Un-boxing something improves the first number
    /// without one pixel of sky actually becoming visible, and this is what
    /// catches that.
    ///
    /// `scene` is the scene's own colour per cell, in row-major order, as
    /// `crate::solar_system::sample_cell_backgrounds` returns it. `inked` is one
    /// flag per cell, in the same order: whether that cell carries a glyph that
    /// is not a space. Returns `(inked_bright_cells, bright_cells)`.
    ///
    /// **Bright** is Rec.709 luminance above [`BRIGHT_SCENE_FLOOR`], which is
    /// this repo's own standing measurement apparatus — the same
    /// `0.2126 R + 0.7152 G + 0.0722 B` the scene comparisons are reported in.
    /// A disc is where the light is; the floor is what separates it from the
    /// void and the starfield.
    ///
    /// **Test-only for now, and deliberately.** It needs a rendered frame's
    /// glyphs, which is a render-time artefact and not something `AppState`
    /// holds; publishing it to production code before something needs it would
    /// be an API with no caller. What it exists for is to keep A29's second
    /// clause honest against
    /// [`Self::sky_clear_fraction`] — the pair is the measurement, not one
    /// number on its own.
    #[cfg(test)]
    pub(crate) fn ink_over_bright_scene(scene: &[(u8, u8, u8)], inked: &[bool]) -> (u32, u32) {
        let mut bright = 0u32;
        let mut over = 0u32;
        for (index, (r, g, b)) in scene.iter().enumerate() {
            let luminance =
                0.2126 * f32::from(*r) + 0.7152 * f32::from(*g) + 0.0722 * f32::from(*b);
            if luminance < Self::BRIGHT_SCENE_FLOOR {
                continue;
            }
            bright += 1;
            if inked.get(index).copied().unwrap_or(false) {
                over += 1;
            }
        }
        (over, bright)
    }

    /// The fraction of the main area carrying no interface element, `0.0..=1.0`. See
    /// [`Self::sky_coverage`]. A main area with no cells in it reads as fully clear, because a
    /// screen with nothing in it has not crowded the sky out.
    pub(crate) fn sky_clear_fraction(&self) -> f32 {
        let (covered, main) = self.sky_coverage();
        if main == 0 {
            return 1.0;
        }
        1.0 - covered as f32 / main as f32
    }

    pub(crate) fn context_menu_rect(&self) -> Option<Rect> {
        let menu = self.context_menu.as_ref()?;
        let screen = self.screen_rect();
        let max_item_w = menu
            .items()
            .iter()
            .map(|item| item.len() as u16)
            .max()
            .unwrap_or(0);
        let menu_w = (max_item_w + 4).max(14).min(screen.width.max(1));
        let menu_h = (menu.items().len() as u16 + 2).min(screen.height.max(1));
        let x = menu.x.min(screen.x + screen.width.saturating_sub(menu_w));
        let y = menu.y.min(screen.y + screen.height.saturating_sub(menu_h));
        Some(Rect::new(x, y, menu_w, menu_h))
    }

    pub(crate) fn confirm_close_rect(&self) -> Rect {
        crate::ui::confirm_close_popup_rect(self.view.terminal_area).unwrap_or_default()
    }

    fn context_menu_item_at(&self, col: u16, row: u16) -> Option<usize> {
        let menu_rect = self.context_menu_rect()?;
        let inner_x = menu_rect.x + 1;
        let inner_y = menu_rect.y + 1;
        let inner_w = menu_rect.width.saturating_sub(2);
        let inner_h = menu_rect.height.saturating_sub(2);
        let item_count = self
            .context_menu
            .as_ref()
            .map(|menu| menu.items().len() as u16)
            .unwrap_or(0);
        if col >= inner_x
            && col < inner_x + inner_w
            && row >= inner_y
            && row < inner_y + inner_h.min(item_count)
        {
            Some((row - inner_y) as usize)
        } else {
            None
        }
    }

    pub(super) fn tab_at(&self, col: u16, row: u16) -> Option<usize> {
        self.view
            .tab_hit_areas
            .iter()
            .enumerate()
            .find_map(|(idx, area)| {
                (area.width > 0
                    && row >= area.y
                    && row < area.y + area.height
                    && col >= area.x
                    && col < area.x + area.width)
                    .then_some(idx)
            })
    }

    fn mode_bar_covers_tab_row(&self, col: u16, row: u16) -> bool {
        self.tab_bar_position == crate::config::TabBarPositionConfig::Bottom
            && matches!(
                self.mode,
                Mode::Navigate | Mode::Prefix | Mode::Copy | Mode::Resize
            )
            && self.on_tab_bar(col, row)
    }

    pub(super) fn on_tab_bar(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_bar_rect;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn on_tab_scroll_left_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_scroll_left_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn on_tab_scroll_right_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.tab_scroll_right_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn tab_drop_index_at(&self, col: u16, row: u16) -> Option<usize> {
        if !self.on_tab_bar(col, row) {
            return None;
        }

        let visible_tabs: Vec<_> = self
            .view
            .tab_hit_areas
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.width > 0)
            .collect();
        let (first_idx, first_rect) = *visible_tabs.first()?;
        let (last_idx, last_rect) = *visible_tabs.last()?;

        if self.on_tab_scroll_left_button(col, row) {
            return Some(0);
        }
        if self.on_tab_scroll_right_button(col, row) {
            return self
                .active
                .and_then(|idx| self.workspaces.get(idx))
                .map(|ws| ws.tabs.len());
        }

        let left_edge = if first_idx == 0 {
            first_rect.x
        } else {
            self.view.tab_scroll_left_hit_area.x + self.view.tab_scroll_left_hit_area.width
        };
        let right_edge = if self
            .active
            .and_then(|idx| self.workspaces.get(idx))
            .is_some_and(|ws| last_idx + 1 >= ws.tabs.len())
        {
            last_rect.x + last_rect.width
        } else {
            self.view.tab_scroll_right_hit_area.x.saturating_sub(1)
        };

        if col <= left_edge {
            return Some(first_idx);
        }
        if col >= right_edge {
            return Some(last_idx + 1);
        }

        for (idx, rect) in visible_tabs {
            let midpoint = rect.x + rect.width / 2;
            if col < midpoint {
                return Some(idx);
            }
            if col < rect.x + rect.width {
                return Some(idx + 1);
            }
        }

        Some(last_idx + 1)
    }

    pub(super) fn on_new_tab_button(&self, col: u16, row: u16) -> bool {
        let area = self.view.new_tab_hit_area;
        area.width > 0
            && row >= area.y
            && row < area.y + area.height
            && col >= area.x
            && col < area.x + area.width
    }

    pub(super) fn find_border_at(&self, col: u16, row: u16) -> Option<&SplitBorder> {
        self.view.split_borders.iter().find(|b| match b.direction {
            Direction::Horizontal if self.pane_borders && !self.pane_gaps => {
                col == b.pos && row >= b.area.y && row < b.area.y + b.area.height
            }
            Direction::Horizontal if self.pane_borders && self.pane_gaps => {
                row >= b.area.y
                    && row < b.area.y + b.area.height
                    && col >= b.pos.saturating_sub(1)
                    && col <= b.pos
            }
            Direction::Horizontal if !self.pane_borders && self.pane_gaps => {
                row >= b.area.y
                    && row < b.area.y + b.area.height
                    && b.pos.checked_sub(1).is_some_and(|gap_col| {
                        col == gap_col && self.pane_frame_at(col, row).is_none()
                    })
            }
            Direction::Vertical if self.pane_borders && !self.pane_gaps => {
                row == b.pos && col >= b.area.x && col < b.area.x + b.area.width
            }
            Direction::Vertical if self.pane_borders && self.pane_gaps => {
                col >= b.area.x
                    && col < b.area.x + b.area.width
                    && row >= b.pos.saturating_sub(1)
                    && row <= b.pos
            }
            Direction::Vertical if !self.pane_borders && self.pane_gaps => {
                col >= b.area.x
                    && col < b.area.x + b.area.width
                    && b.pos.checked_sub(1).is_some_and(|gap_row| {
                        row == gap_row && self.pane_frame_at(col, row).is_none()
                    })
            }
            _ => false,
        })
    }

    pub(super) fn pane_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.inner_rect.x
                && col < p.inner_rect.x + p.inner_rect.width
                && row >= p.inner_rect.y
                && row < p.inner_rect.y + p.inner_rect.height
        })
    }

    pub(super) fn pane_mouse_target(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.pane_at(col, row)
            .or_else(|| self.pane_frame_at(col, row))
    }

    fn mouse_pane_focus_action(&self, pane_id: crate::layout::PaneId) -> Option<MouseAction> {
        let ws_idx = self.active?;
        (self
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.focused_pane_id())
            != Some(pane_id))
        .then_some(MouseAction::FocusPane { ws_idx, pane_id })
    }

    pub(crate) fn pane_info_by_id(&self, pane_id: crate::layout::PaneId) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|info| info.id == pane_id)
    }

    pub(super) fn pane_frame_at(&self, col: u16, row: u16) -> Option<&PaneInfo> {
        self.view.pane_infos.iter().find(|p| {
            col >= p.rect.x
                && col < p.rect.x + p.rect.width
                && row >= p.rect.y
                && row < p.rect.y + p.rect.height
        })
    }

    pub(super) fn focus_pane(&mut self, pane_id: crate::layout::PaneId) {
        let _ = pane_id;
    }

    fn clickable_toast_at(&self, col: u16, row: u16) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|toast| toast.target.is_some())
            && rect_contains(self.view.toast_hit_area, col, row)
    }

    #[cfg(test)]
    pub(crate) fn focus_toast_target(&mut self) {
        let Some(target) = self.toast.as_ref().and_then(|toast| toast.target.clone()) else {
            return;
        };
        let Some(ws_idx) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == target.workspace_id)
        else {
            return;
        };
        let Some(_tab_idx) = self.workspaces[ws_idx].find_tab_index_for_pane(target.pane_id) else {
            return;
        };

        self.focus_pane_in_workspace(ws_idx, target.pane_id);
        self.toast = None;
        self.settle_terminal_mode_after_focus();
    }

    pub(crate) fn scroll_pane_up(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        lines: usize,
    ) {
        if let Some(ws_idx) = self.active {
            if let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            {
                rt.scroll_up(lines);
            }
        }
    }

    pub(crate) fn scroll_pane_down(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        lines: usize,
    ) {
        if let Some(ws_idx) = self.active {
            if let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            {
                rt.scroll_down(lines);
            }
        }
    }

    pub(crate) fn pane_scroll_metrics(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::pane::ScrollMetrics> {
        self.active
            .and_then(|i| self.runtime_for_pane_in_workspace(terminal_runtimes, i, pane_id))
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
    }

    fn handle_right_click_passthrough(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        mouse: MouseEvent,
        in_sidebar: bool,
    ) -> bool {
        if let Some(gesture) = self.right_click_passthrough.clone() {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Right)
                | MouseEventKind::Up(MouseButton::Right) => {
                    let forwarded_mouse =
                        self.strip_right_click_passthrough_modifiers(mouse, gesture.modifiers);
                    let _ = self.forward_pane_mouse_button(
                        terminal_runtimes,
                        &gesture.pane_info,
                        forwarded_mouse,
                    );
                    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Right)) {
                        self.right_click_passthrough = None;
                    }
                    return true;
                }
                _ => {
                    self.right_click_passthrough = None;
                }
            }
        }

        if self.mode != Mode::Terminal
            || in_sidebar
            || !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right))
        {
            return false;
        }

        let Some(modifiers) = self.right_click_passthrough_modifiers else {
            return false;
        };
        if mouse.modifiers != modifiers {
            return false;
        }

        let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() else {
            return false;
        };

        self.focus_pane(info.id);
        let forwarded_mouse = self.strip_right_click_passthrough_modifiers(mouse, modifiers);
        if !self.forward_pane_mouse_button(terminal_runtimes, &info, forwarded_mouse) {
            return false;
        }

        self.selection = None;
        self.selection_autoscroll = None;
        self.workspace_press = None;
        self.tab_press = None;
        self.drag = None;
        self.context_menu = None;
        self.right_click_passthrough = Some(RightClickPassthroughGesture {
            pane_info: info,
            modifiers,
        });
        true
    }

    fn strip_right_click_passthrough_modifiers(
        &self,
        mouse: MouseEvent,
        modifiers: crossterm::event::KeyModifiers,
    ) -> MouseEvent {
        MouseEvent {
            modifiers: mouse.modifiers.difference(modifiers),
            ..mouse
        }
    }

    pub(super) fn handle_terminal_wheel(
        &mut self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        mouse: MouseEvent,
    ) {
        let lines_per_notch = self.mouse_scroll_lines;

        if let Some(info) = self.pane_at(mouse.column, mouse.row).cloned() {
            self.focus_pane(info.id);
            if self.forward_pane_wheel(terminal_runtimes, &info, mouse) {
                return;
            }
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_pane_up(terminal_runtimes, info.id, lines_per_notch)
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_pane_down(terminal_runtimes, info.id, lines_per_notch)
                }
                _ => {}
            }
            return;
        }

        if let Some(info) = self.pane_frame_at(mouse.column, mouse.row).cloned() {
            self.focus_pane(info.id);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll_pane_up(terminal_runtimes, info.id, lines_per_notch)
                }
                MouseEventKind::ScrollDown => {
                    self.scroll_pane_down(terminal_runtimes, info.id, lines_per_notch)
                }
                _ => {}
            }
            return;
        }

        if let Some(ws_idx) = self.active {
            if let Some(rt) = self.focused_runtime_in_workspace(terminal_runtimes, ws_idx) {
                match mouse.kind {
                    MouseEventKind::ScrollUp => rt.scroll_up(lines_per_notch),
                    MouseEventKind::ScrollDown => rt.scroll_down(lines_per_notch),
                    _ => {}
                }
            }
        }
    }

    pub(super) fn forward_pane_mouse_button(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        let column = mouse.column.saturating_sub(info.inner_rect.x);
        let row = mouse.row.saturating_sub(info.inner_rect.y);
        let Some(bytes) = rt.encode_mouse_button(mouse.kind, column, row, mouse.modifiers) else {
            return false;
        };
        rt.scroll_reset();
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, kind = ?mouse.kind, "failed to forward mouse button event");
        }
        true
    }

    pub(super) fn forward_pane_mouse_motion(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        let column = mouse.column.saturating_sub(info.inner_rect.x);
        let row = mouse.row.saturating_sub(info.inner_rect.y);
        let Some(bytes) = rt.encode_mouse_motion(mouse.kind, column, row, mouse.modifiers) else {
            return false;
        };
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, kind = ?mouse.kind, "failed to forward mouse motion event");
        }
        true
    }

    fn forward_pane_reported_wheel(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        if rt.wheel_routing() != Some(crate::pane::WheelRouting::MouseReport) {
            return false;
        }
        rt.scroll_reset();
        let column = mouse.column.saturating_sub(info.inner_rect.x);
        let row = mouse.row.saturating_sub(info.inner_rect.y);
        let Some(bytes) = rt.encode_mouse_wheel(mouse.kind, column, row, mouse.modifiers) else {
            warn!(pane = info.id.raw(), kind = ?mouse.kind, "failed to encode mouse wheel event");
            return true;
        };
        if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
            warn!(pane = info.id.raw(), err = %err, "failed to forward mouse wheel event");
        }
        true
    }

    pub(super) fn forward_pane_wheel(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        info: &PaneInfo,
        mouse: MouseEvent,
    ) -> bool {
        let Some(ws_idx) = self.active else {
            return false;
        };
        let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        else {
            return false;
        };
        match rt.wheel_routing() {
            Some(crate::pane::WheelRouting::HostScroll) | None => false,
            Some(crate::pane::WheelRouting::MouseReport) => {
                rt.scroll_reset();
                let column = mouse.column.saturating_sub(info.inner_rect.x);
                let row = mouse.row.saturating_sub(info.inner_rect.y);
                let Some(bytes) = rt.encode_mouse_wheel(mouse.kind, column, row, mouse.modifiers)
                else {
                    warn!(pane = info.id.raw(), kind = ?mouse.kind, "failed to encode mouse wheel event");
                    return true;
                };
                if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward mouse wheel event");
                }
                true
            }
            Some(crate::pane::WheelRouting::AlternateScroll) => {
                rt.scroll_reset();
                let Some(bytes) = rt.encode_alternate_scroll(mouse.kind) else {
                    return true;
                };
                if let Err(err) = rt.try_send_bytes(Bytes::from(bytes)) {
                    warn!(pane = info.id.raw(), err = %err, "failed to forward alternate-scroll key");
                }
                true
            }
        }
    }

    pub(super) fn set_pane_scroll_offset(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        offset_from_bottom: usize,
    ) {
        for ws_idx in 0..self.workspaces.len() {
            let Some(rt) = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)
            else {
                continue;
            };
            rt.set_scroll_offset_from_bottom(offset_from_bottom);
            return;
        }
    }

    pub(super) fn scrollbar_target_at(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        col: u16,
        row: u16,
    ) -> Option<(crate::layout::PaneId, ScrollbarClickTarget)> {
        let ws_idx = self.active?;
        let info = self.view.pane_infos.iter().find(|info| {
            crate::ui::pane_scrollbar_rect(info).is_some_and(|track| {
                col >= track.x
                    && col < track.x + track.width
                    && row >= track.y
                    && row < track.y + track.height
            })
        })?;
        let rt = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
        let metrics = rt.scroll_metrics()?;
        if metrics.max_offset_from_bottom == 0 {
            return None;
        }
        let track = crate::ui::pane_scrollbar_rect(info)?;
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some((info.id, ScrollbarClickTarget::Thumb { grab_row_offset }))
        } else {
            Some((
                info.id,
                ScrollbarClickTarget::Track {
                    offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
                },
            ))
        }
    }

    pub(super) fn scrollbar_offset_for_pane_row(
        &self,
        terminal_runtimes: &TerminalRuntimeRegistry,
        pane_id: crate::layout::PaneId,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let ws_idx = self.active?;
        let info = self
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)?;
        let track = crate::ui::pane_scrollbar_rect(info)?;
        let rt = self.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, pane_id)?;
        let metrics = rt.scroll_metrics()?;
        if metrics.max_offset_from_bottom == 0 {
            return None;
        }
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }
}

#[cfg(test)]
pub(super) fn wheel_routing(input_state: crate::pane::InputState) -> WheelRouting {
    if input_state.mouse_protocol_mode.reporting_enabled() {
        WheelRouting::MouseReport
    } else if input_state.alternate_screen && input_state.mouse_alternate_scroll {
        WheelRouting::AlternateScroll
    } else {
        WheelRouting::HostScroll
    }
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

fn apply_scroll(scroll: &mut usize, delta: i16, max_scroll: usize) {
    if delta.is_negative() {
        *scroll = scroll.saturating_sub(delta.unsigned_abs() as usize);
    } else {
        *scroll = scroll.saturating_add(delta as usize).min(max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::layout::{Direction, Rect};

    use super::super::{
        app_for_mouse_test, capture_snapshot, mouse, numbered_lines_bytes, root_layout_ratio,
    };
    use super::*;
    use crate::app::input::modal::handle_context_menu_key;
    use crate::{
        app::state::{ContextMenuKind, ContextMenuState, MenuListState, Mode, ViewLayout},
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    /// An `AppState` whose screen is exactly `cols` x `rows`.
    fn state_sized(cols: u16, rows: u16) -> crate::app::state::AppState {
        let mut state = crate::app::state::AppState::test_new();
        state.view.sidebar_rect = Rect::new(0, 0, 0, rows);
        state.view.terminal_area = Rect::new(0, 0, cols, rows);
        state
    }

    #[test]
    fn the_machine_corner_sits_top_right_inside_the_screen() {
        let state = state_sized(120, 40);
        let corner = state.machine_corner_rect();
        let screen = state.screen_rect();

        assert!(corner.width > 0 && corner.height > 0);
        // Top-right, inset — the corner a reader's eye is least often in, and the edge the sidebar
        // does not own.
        assert_eq!(corner.x + corner.width, screen.x + screen.width - 1);
        assert_eq!(corner.y, screen.y + 1);
        // Wholly inside the screen, which is what stops the readout being clipped into nonsense.
        assert!(corner.x >= screen.x);
        assert!(corner.y + corner.height <= screen.y + screen.height);
    }

    #[test]
    fn a_screen_too_small_for_the_machine_corner_does_not_get_a_shrunken_one() {
        // A readout that will not fit is not drawn at all; it is not squeezed until it is
        // unreadable, and it never takes more than half of a narrow screen. Somebody working in a
        // 60-column terminal has not asked for a third of it to become a diagnostic.
        for (cols, rows) in [(40u16, 40u16), (120, 6), (10, 10), (0, 0)] {
            let corner = state_sized(cols, rows).machine_corner_rect();
            assert_eq!(
                (corner.width, corner.height),
                (0, 0),
                "a {cols}x{rows} screen was given a corner anyway"
            );
        }
        // ...and a size that does fit gets the whole thing rather than a partial one.
        let fits = state_sized(120, 40).machine_corner_rect();
        assert_eq!((fits.width, fits.height), (26, 8));

        // The threshold is about the *main* area rather than the screen: the same 60-column
        // terminal fits a corner with no sidebar and does not fit one with a wide sidebar, because
        // the second has almost no sky left to spend.
        let mut narrow = state_sized(90, 24);
        assert_ne!(
            narrow.machine_corner_rect().width,
            0,
            "90 columns of main area is room for a 26-wide readout"
        );
        narrow.view.sidebar_rect = Rect::new(0, 0, 44, 24);
        narrow.view.terminal_area = Rect::new(44, 0, 46, 24);
        assert_eq!(
            narrow.machine_corner_rect().width,
            0,
            "a corner was drawn into 46 columns of main area"
        );
    }

    /// An `AppState` whose screen is `cols` x `rows` with a sidebar `sidebar` wide.
    fn state_with_sidebar(cols: u16, rows: u16, sidebar: u16) -> crate::app::state::AppState {
        let mut state = crate::app::state::AppState::test_new();
        state.view.sidebar_rect = Rect::new(0, 0, sidebar, rows);
        state.view.terminal_area = Rect::new(sidebar, 0, cols.saturating_sub(sidebar), rows);
        state
    }

    #[test]
    fn the_scene_keeps_most_of_the_main_area_to_itself() {
        // The composition bound the whole scene exists to satisfy: if the interface crowds the sky
        // out, the thing the scene is for is gone. Held at every size a terminal realistically
        // reaches, against herdr's own real sidebar widths — not one flattering geometry.
        // Swept down to sizes that genuinely do not fit, not only comfortable ones: the bound has
        // to hold on a 40-column terminal with a wide sidebar as much as on a 240-column one, and
        // that narrow case is exactly where a corner sized against the whole screen would cover
        // most of the sky the reader has left.
        for cols in [20u16, 40, 60, 80, 100, 120, 160, 200, 240, 400] {
            for rows in [4u16, 10, 16, 20, 30, 40, 50, 80] {
                for sidebar in [0u16, 26, 34, 44, 60] {
                    if sidebar >= cols {
                        continue;
                    }
                    let state = state_with_sidebar(cols, rows, sidebar);
                    let clear = state.sky_clear_fraction();
                    assert!(
                        clear >= crate::app::state::SKY_CLEAR_FLOOR,
                        "{cols}x{rows} with a {sidebar}-wide sidebar leaves the scene only \
                         {:.1}% of the main area",
                        clear * 100.0
                    );
                }
            }
        }
    }

    /// The status stream is counted against the clear-area floor too, and the
    /// floor still holds with it there.
    ///
    /// A48's six lines are a surface between the reader and the sky exactly as
    /// the machine corner is, and a clause that counted one and not the other
    /// would be a clause that could be satisfied by moving a panel rather than
    /// by removing one.
    #[test]
    fn the_status_stream_is_counted_against_the_clear_floor() {
        let mut with_stream = state_with_sidebar(160, 50, 34);
        let bare = with_stream.sky_coverage().0;
        let now = std::time::Instant::now();
        for index in 0..crate::app::status_feed::TERM_MAX {
            with_stream.status_feed.observe(
                Some(&crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::Finished,
                    title: format!("line {index}"),
                    context: "ctx".into(),
                    position: None,
                    target: None,
                }),
                now,
            );
        }
        let covered = with_stream.sky_coverage().0;
        assert!(
            covered > bare,
            "the stream drew six lines and cost the sky nothing, so it is not \
             being counted"
        );
        assert!(
            with_stream.sky_clear_fraction() >= crate::app::state::SKY_CLEAR_FLOOR,
            "the stream took the main area under the clear floor: {:.1}%",
            with_stream.sky_clear_fraction() * 100.0
        );

        // And it holds at every size, with the stream full, exactly as the
        // corner's own sweep does.
        for cols in [20u16, 40, 60, 80, 120, 200, 400] {
            for rows in [4u16, 10, 20, 40, 80] {
                for sidebar in [0u16, 26, 44, 60] {
                    if sidebar >= cols {
                        continue;
                    }
                    let mut state = state_with_sidebar(cols, rows, sidebar);
                    for index in 0..crate::app::status_feed::TERM_MAX {
                        state.status_feed.observe(
                            Some(&crate::app::state::ToastNotification {
                                kind: crate::app::state::ToastKind::Finished,
                                title: format!("line {index}"),
                                context: "ctx".into(),
                                position: None,
                                target: None,
                            }),
                            now,
                        );
                    }
                    let clear = state.sky_clear_fraction();
                    assert!(
                        clear >= crate::app::state::SKY_CLEAR_FLOOR,
                        "{cols}x{rows} with a {sidebar}-wide sidebar and a full stream \
                         leaves the scene only {:.1}% of the main area",
                        clear * 100.0
                    );
                }
            }
        }
    }

    /// A24's contrast, as geometry: **the stream is narrow and the prose is
    /// not.**
    ///
    /// The card's reason is functional and is not open to being re-narrowed for
    /// taste: *assistant output is long, and a narrow column makes it scroll
    /// past too fast to read*. In herdr the prose is the pane text — real PTY
    /// output, which spans the whole frame outside the sidebar — and the stream
    /// is six short lines nobody scrolls. So the two have to differ in width by
    /// construction, and the stream has to be in the bottom third.
    #[test]
    fn the_stream_is_narrow_and_the_prose_is_not() {
        let now = std::time::Instant::now();
        let mut drawn = 0;
        for cols in [80u16, 120, 200, 400] {
            for sidebar in [26u16, 44] {
                let mut state = state_with_sidebar(cols, 50, sidebar);
                for index in 0..crate::app::status_feed::TERM_MAX {
                    state.status_feed.observe(
                        Some(&crate::app::state::ToastNotification {
                            kind: crate::app::state::ToastKind::Finished,
                            title: format!("line {index}"),
                            context: "ctx".into(),
                            position: None,
                            target: None,
                        }),
                        now,
                    );
                }
                let stream = state.status_feed_rect();
                let prose = state.view.terminal_area;
                if stream.width == 0 {
                    // A main area too small to give the stream a readable third
                    // draws none at all, the same rule the machine corner
                    // follows. There is no contrast to measure there.
                    continue;
                }
                drawn += 1;
                assert!(
                    stream.width * 2 < prose.width,
                    "the stream is {} columns against {} of prose at {cols} columns, \
                     which is not a contrast",
                    stream.width,
                    prose.width
                );
                // In the bottom third, and at the sidebar's own edge.
                assert!(
                    stream.y >= state.screen_rect().y + state.screen_rect().height * 2 / 3,
                    "the stream is at row {} on a {}-row screen, not in the bottom third",
                    stream.y,
                    state.screen_rect().height
                );
                assert_eq!(
                    stream.x,
                    prose.x + 1,
                    "the stream did not start at the sidebar's edge"
                );
            }
        }
        assert!(
            drawn > 0,
            "no size in the sweep drew a stream, so the contrast is untested"
        );
    }

    /// A49: only the lines the corner actually stands over give width back.
    #[test]
    fn only_a_line_the_corner_stands_over_reserves_anything() {
        use crate::ui::status::corner_reservation;
        let corner = Rect::new(70, 1, 26, 8);

        // A line on a row the corner does not occupy reserves nothing, however
        // far right it reaches.
        let below = Rect::new(0, 20, 100, 1);
        assert_eq!(corner_reservation(below, corner), 0);
        let above = Rect::new(0, 0, 100, 1);
        assert_eq!(corner_reservation(above, corner), 0);

        // A line inside its rows, but stopping short of it, reserves nothing
        // either — *"a block that does not reach the panel reserves nothing"*.
        let short = Rect::new(0, 3, 70, 1);
        assert_eq!(corner_reservation(short, corner), 0);

        // And a line that does reach it gives back exactly what it overlaps
        // plus one gutter, so its last glyph is clear of the corner's edge.
        let through = Rect::new(0, 3, 100, 1);
        let reserved = corner_reservation(through, corner);
        assert_eq!(reserved, 100 - 70 + 1);
        assert!(
            through.x + (through.width - reserved) < corner.x,
            "the shortened line still runs under the corner"
        );

        // A corner that is not drawn costs nothing at all.
        assert_eq!(corner_reservation(through, Rect::new(70, 1, 0, 0)), 0);
    }

    #[test]
    fn the_ink_over_the_bright_scene_is_measured_over_the_light_and_not_the_void() {
        // Three cells of void, three of a body's disc. Two of the bright ones
        // carry a glyph.
        let void = (6u8, 9u8, 16u8);
        let disc = (210u8, 190u8, 140u8);
        let scene = vec![void, void, disc, disc, disc, void];
        let inked = vec![true, true, true, true, false, false];
        let (over, bright) = crate::app::state::AppState::ink_over_bright_scene(&scene, &inked);
        assert_eq!(bright, 3, "the void was counted as light");
        assert_eq!(over, 2);

        // A frame with nothing on it reports no ink and the same bright area,
        // which is what makes the number a fraction of the *light* rather than
        // of the frame.
        let (over, bright) =
            crate::app::state::AppState::ink_over_bright_scene(&scene, &[false; 6]);
        assert_eq!((over, bright), (0, 3));
    }

    #[test]
    fn the_sidebar_is_what_the_main_area_is_measured_around_not_against() {
        // The sidebar *is* the worker-tree panel — the thing the clause measures around. Counting
        // it as coverage would make a wide sidebar fail a bound that is not about it, and would
        // make the number un-actionable: there is nothing to do about it.
        let narrow = state_with_sidebar(200, 50, 26);
        let wide = state_with_sidebar(200, 50, 44);
        assert_eq!(narrow.sky_coverage().0, wide.sky_coverage().0);
        assert!(
            wide.sky_coverage().1 < narrow.sky_coverage().1,
            "a wider sidebar has to leave a smaller main area"
        );
    }

    #[test]
    fn a_screen_with_no_room_for_the_corner_is_wholly_clear() {
        // The corner is the only thing that puts a surface between the reader and the sky, so a
        // screen too small to hold one has nothing over it at all. This is also the case that
        // would divide by zero if the main area were empty.
        assert_eq!(state_with_sidebar(40, 40, 26).sky_clear_fraction(), 1.0);
        assert_eq!(state_with_sidebar(0, 0, 0).sky_clear_fraction(), 1.0);
        assert_eq!(state_with_sidebar(26, 40, 26).sky_clear_fraction(), 1.0);
    }

    #[test]
    fn the_corner_is_a_small_share_of_a_real_terminal() {
        // Reported rather than only bounded: "it passes" and "it takes 2% of the main area" are
        // different statements, and the second is the one that says whether there is room to add
        // anything else here later.
        let state = state_with_sidebar(200, 50, 34);
        let (covered, main) = state.sky_coverage();
        assert_eq!(main, (200 - 34) * 50);
        assert_eq!(covered, 26 * 8);
        assert!(
            state.sky_clear_fraction() > 0.95,
            "the corner covers {:.1}% of a 200x50 terminal",
            (1.0 - state.sky_clear_fraction()) * 100.0
        );
    }

    #[test]
    fn the_machine_corner_reserves_only_its_own_box() {
        // A49, in the form herdr can actually hold: the corner takes its own box and nothing else.
        // The implementation this rules out is the obvious one — a full-width band across the rows
        // the corner occupies — which would claim a whole screen's width of cells that have
        // nothing over them.
        let state = state_with_sidebar(200, 50, 34);
        let corner = state.machine_corner_rect();
        let (covered, _) = state.sky_coverage();
        assert_eq!(covered, u32::from(corner.width) * u32::from(corner.height));
        assert!(
            covered < u32::from(state.screen_rect().width) * u32::from(corner.height),
            "the reservation is a full-width band rather than the corner's own box"
        );
    }

    fn mark_worktree_space_member(workspace: &mut Workspace, ws_idx: usize, key: &str) {
        workspace.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/worktree-{ws_idx}").into(),
            is_linked_worktree: ws_idx != 0,
        });
    }

    #[tokio::test]
    async fn terminal_wheel_uses_configured_mouse_scroll_lines() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        ws.tabs[0].runtimes.insert(
            pane_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                16 * 1024,
                &numbered_lines_bytes(64),
            ),
        );

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.mouse_scroll_lines = 7;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollUp,
            info.inner_rect.x + 1,
            info.inner_rect.y + 1,
        ));

        let metrics = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .and_then(crate::terminal::TerminalRuntime::scroll_metrics)
            .expect("scroll metrics after wheel");
        assert_eq!(metrics.offset_from_bottom, 7);
    }

    #[tokio::test]
    async fn mouse_dispatcher_forwards_horizontal_wheel_to_mouse_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1000h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        assert!(
            app.state.mouse_capture,
            "reproduction must use the default Herdr mouse dispatcher"
        );

        let outer_column = info.inner_rect.x + 2;
        let outer_row = info.inner_rect.y + 3;
        for (button, expected_kind, ingress) in [
            (66, MouseEventKind::ScrollLeft, "monolithic"),
            (67, MouseEventKind::ScrollRight, "headless"),
        ] {
            let input = format!("\x1b[<{button};{};{}M", outer_column + 1, outer_row + 1);
            let mut events = crate::raw_input::parse_raw_input_bytes_sync(input.as_bytes());
            let event = events
                .pop()
                .expect("horizontal SGR wheel input should parse");
            let crate::raw_input::RawInputEvent::Mouse(mouse) = &event else {
                panic!("expected parsed mouse event");
            };
            assert!(events.is_empty(), "expected one parsed mouse event");
            assert_eq!(mouse.kind, expected_kind);

            if ingress == "monolithic" {
                assert!(app.handle_raw_input_event(event).await);
            } else {
                app.route_client_events(vec![event], false);
            }

            assert_eq!(
                input_rx
                    .try_recv()
                    .expect("horizontal wheel should reach pane"),
                Bytes::from(format!("\x1b[<{button};3;4M"))
            );
        }
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn horizontal_wheel_stays_inert_for_non_mouse_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"",
                1,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        let input = format!(
            "\x1b[<66;{};{}M",
            info.inner_rect.x + 3,
            info.inner_rect.y + 4
        );
        let event = crate::raw_input::parse_raw_input_bytes_sync(input.as_bytes())
            .pop()
            .expect("horizontal SGR wheel input should parse");

        assert!(app.handle_raw_input_event(event).await);

        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn configured_right_click_passthrough_forwards_full_gesture_to_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);

        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Down(MouseButton::Right), col, row)
        });
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Drag(MouseButton::Right), col + 1, row + 1)
        });
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Up(MouseButton::Right), col + 1, row + 1)
        });

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.context_menu.is_none());
        assert!(app.state.right_click_passthrough.is_none());
        assert_eq!(
            input_rx.try_recv().expect("forwarded right mouse down"),
            Bytes::from_static(b"\x1b[<2;3;4M")
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded right mouse drag"),
            Bytes::from_static(b"\x1b[<34;4;5M")
        );
        assert_eq!(
            input_rx.try_recv().expect("forwarded right mouse up"),
            Bytes::from_static(b"\x1b[<2;4;5m")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn captured_left_press_focuses_target_before_forwarding() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let source = ws.tabs[0].root_pane;
        let target = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(source);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app
            .state
            .pane_info_by_id(target)
            .expect("target pane info")
            .clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(target, runtime);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            info.inner_rect.x + 1,
            info.inner_rect.y + 1,
        ));

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(target));
        assert_eq!(
            input_rx.try_recv().expect("forwarded captured left press"),
            Bytes::from_static(b"\x1b[<0;2;2M")
        );
    }

    #[tokio::test]
    async fn pane_mouse_only_forwards_moved_events_for_any_motion_apps() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.state.handle_pane_mouse_only(
            &app.terminal_runtimes,
            mouse(
                MouseEventKind::Moved,
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            ),
        );

        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pane_mouse_motion_uses_computed_inner_rect_offsets() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.view.pane_infos[0].clone();
        assert!(info.inner_rect.x > 0, "sidebar offset should be present");
        assert!(info.inner_rect.y > 0, "tab bar offset should be present");

        app.state.handle_pane_mouse_only(
            &app.terminal_runtimes,
            mouse(
                MouseEventKind::Moved,
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            ),
        );

        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn mouse_dispatcher_downgrades_sgr_pixel_motion_to_cell_coordinates() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h\x1b[?1016h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.view.pane_infos[0].clone();
        assert!(info.inner_rect.x > 0, "sidebar offset should be present");
        assert!(info.inner_rect.y > 0, "tab bar offset should be present");

        app.handle_mouse(mouse(
            MouseEventKind::Moved,
            info.inner_rect.x + 2,
            info.inner_rect.y + 3,
        ));

        assert_eq!(
            input_rx.try_recv().expect("forwarded mouse motion"),
            Bytes::from_static(b"\x1b[<35;3;4M")
        );
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn mouse_dispatcher_does_not_forward_motion_behind_herdr_modes() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                18,
                0,
                b"\x1b[?1003h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let info = app.state.view.pane_infos[0].clone();

        app.handle_mouse(mouse(
            MouseEventKind::Moved,
            info.inner_rect.x + 2,
            info.inner_rect.y + 3,
        ));

        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn unset_right_click_passthrough_keeps_modified_right_click_as_herdr_menu() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;
        app.state.right_click_passthrough_modifiers = None;

        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(
                MouseEventKind::Down(MouseButton::Right),
                info.inner_rect.x + 2,
                info.inner_rect.y + 3,
            )
        });

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn pane_right_click_keeps_focus_and_swap_menu_swaps_with_focused_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let source = ws.tabs[0].root_pane;
        let target = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(source);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 20));
        let target_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == target)
            .expect("target pane info")
            .clone();
        let source_rect_before = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == source)
            .expect("source pane info")
            .rect;
        let target_rect_before = target_info.rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            target_info.inner_rect.x,
            target_info.inner_rect.y,
        ));

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        let menu = app.state.context_menu.as_mut().expect("pane context menu");
        assert!(matches!(
            menu.kind,
            ContextMenuKind::Pane {
                pane_id,
                source_pane_id: Some(source_pane_id),
                ..
            } if pane_id == target && source_pane_id == source
        ));
        let swap_idx = menu
            .items()
            .iter()
            .position(|item| *item == "Swap with focused pane")
            .expect("swap item");
        menu.list.highlighted = swap_idx;

        handle_context_menu_key(
            &mut app.state,
            &mut app.terminal_runtimes,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 20));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        assert_eq!(
            app.state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == source)
                .unwrap()
                .rect,
            target_rect_before
        );
        assert_eq!(
            app.state
                .view
                .pane_infos
                .iter()
                .find(|info| info.id == target)
                .unwrap()
                .rect,
            source_rect_before
        );
    }

    #[tokio::test]
    async fn normal_right_click_keeps_focus_and_exposes_swap_for_reporting_pane() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let source = ws.tabs[0].root_pane;
        let target = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(source);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 100, 20));
        let target_info = app
            .state
            .pane_info_by_id(target)
            .expect("target pane info")
            .clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                target_info.inner_rect.width,
                target_info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(target, runtime);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            target_info.inner_rect.x,
            target_info.inner_rect.y,
        ));

        assert!(input_rx.try_recv().is_err());
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        let menu = app.state.context_menu.as_mut().expect("pane context menu");
        assert!(matches!(
            menu.kind,
            ContextMenuKind::Pane {
                pane_id,
                source_pane_id: Some(source_pane_id),
                ..
            } if pane_id == target && source_pane_id == source
        ));
        assert!(menu.items().contains(&"Swap with focused pane"));
    }

    #[tokio::test]
    async fn right_click_passthrough_requires_exact_modifier_match() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
        let info = pane_infos[0].clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        ws.insert_test_runtime(pane_id, runtime);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.view.pane_infos = pane_infos;

        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);

        let col = info.inner_rect.x + 2;
        let row = info.inner_rect.y + 3;
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ..mouse(MouseEventKind::Down(MouseButton::Right), col, row)
        });

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn right_click_passthrough_does_not_forward_pane_frame_clicks() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let pane_id = ws.tabs[0].root_pane;
        let other_pane = ws.test_split(Direction::Vertical);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.right_click_passthrough_modifiers = Some(KeyModifiers::CONTROL);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("pane info")
            .clone();
        let (runtime, mut input_rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                info.inner_rect.width,
                info.inner_rect.height,
                0,
                b"\x1b[?1002h\x1b[?1006h",
                4,
            );
        app.state.insert_test_runtime(pane_id, runtime);
        app.state.insert_test_runtime(
            other_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(10, 5, b""),
        );

        assert!(app.state.pane_at(info.rect.x, info.rect.y).is_none());
        assert!(app
            .state
            .pane_mouse_target(info.rect.x, info.rect.y)
            .is_some());
        app.handle_mouse(MouseEvent {
            modifiers: KeyModifiers::CONTROL,
            ..mouse(
                MouseEventKind::Down(MouseButton::Right),
                info.rect.x,
                info.rect.y,
            )
        });

        assert_eq!(app.state.mode, Mode::ContextMenu);
        assert!(app.state.context_menu.is_some());
        assert!(app.state.right_click_passthrough.is_none());
        assert!(input_rx.try_recv().is_err());
    }

    fn sample_worktree_open_state() -> crate::app::state::WorktreeOpenState {
        crate::app::state::WorktreeOpenState {
            source_workspace_id: "source".into(),
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![
                crate::app::state::WorktreeOpenEntry {
                    path: "/repo/herdr".into(),
                    branch: Some("main".into()),
                    is_linked_worktree: false,
                    already_open_ws_idx: Some(0),
                },
                crate::app::state::WorktreeOpenEntry {
                    path: "/repo/herdr-issue".into(),
                    branch: Some("worktree/issue".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: None,
                },
            ],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        }
    }

    #[test]
    fn hovering_context_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 0 },
            x: 2,
            y: 2,
            list: MenuListState::new(0),
        });
        app.state.mode = Mode::ContextMenu;

        let menu = app.state.context_menu_rect().unwrap();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.context_menu.unwrap().list.highlighted, 1);
    }

    #[test]
    fn clicking_agent_toast_focuses_target_pane() {
        let mut app = app_for_mouse_test();
        let active = Workspace::test_new("active");
        let mut background = Workspace::test_new("background");
        let first_pane = background.tabs[0].root_pane;
        let target_pane = background.test_split(Direction::Horizontal);
        background.tabs[0].layout.focus_pane(first_pane);

        app.state.workspaces = vec![active, background];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.toast_config.delivery = crate::config::ToastDelivery::Herdr;
        app.state.toast_config.delay_seconds = 0;
        let target_terminal_id = app.state.workspaces[1]
            .panes
            .get(&target_pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&target_terminal_id)
            .unwrap()
            .state = AgentState::Working;

        app.state
            .handle_app_event(crate::events::AppEvent::StateChanged {
                pane_id: target_pane,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let hit = app.state.view.toast_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            hit.x + 1,
            hit.y + 1,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert!(app.state.toast.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);

        app.state.last_pane();

        assert_eq!(app.state.active, Some(0));
        assert_eq!(
            app.state.workspaces[0].focused_pane_id(),
            Some(app.state.workspaces[0].tabs[0].root_pane)
        );
    }

    #[test]
    fn toast_click_does_not_steal_mouse_from_settings_overlay() {
        let mut app = app_for_mouse_test();
        let active = Workspace::test_new("active");
        let background = Workspace::test_new("background");
        let target_pane = background.tabs[0].root_pane;
        let workspace_id = background.id.clone();

        app.state.workspaces = vec![active, background];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.toast = Some(crate::app::state::ToastNotification {
            kind: crate::app::state::ToastKind::Finished,
            title: "pi finished".into(),
            context: "background · 2".into(),
            position: None,
            target: Some(crate::app::state::ToastTarget {
                workspace_id,
                pane_id: target_pane,
            }),
        });
        app.state.mode = Mode::Settings;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let hit = app.state.view.toast_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            hit.x + 1,
            hit.y + 1,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(app.state.toast.is_some());
    }

    #[test]
    fn clicking_confirm_close_accepts_workspace_close() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::ConfirmClose;

        let popup = app.state.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (confirm, _) = crate::ui::confirm_close_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x,
            confirm.y,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn clicking_rename_save_submits_workspace_rename_through_api_path() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("old")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameWorkspace;
        app.state.name_input = "new".into();

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));
        let inner = app.state.rename_modal_inner().unwrap();
        let (save, _, _) = crate::ui::rename_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            save.x,
            save.y,
        ));

        assert_eq!(app.state.workspaces[0].custom_name.as_deref(), Some("new"));
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceRenamed)
        }));
    }

    #[test]
    fn clicking_open_worktree_row_selects_and_requests_open() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());
        let inner =
            crate::ui::open_existing_worktree_inner_rect(app.state.screen_rect(), 2).unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            inner.x + 1,
            inner.y + 5,
        ));

        assert_eq!(app.state.worktree_open.as_ref().unwrap().selected, 1);
        assert!(app.state.request_submit_worktree_open);
    }

    #[test]
    fn clicking_open_worktree_buttons_requests_open_or_cancels() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());
        let inner =
            crate::ui::open_existing_worktree_inner_rect(app.state.screen_rect(), 2).unwrap();
        let (open, _) = crate::ui::open_existing_worktree_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            open.x,
            open.y,
        ));

        assert!(app.state.worktree_open.is_some());
        assert!(app.state.request_submit_worktree_open);

        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());
        let inner =
            crate::ui::open_existing_worktree_inner_rect(app.state.screen_rect(), 2).unwrap();
        let (_, cancel) = crate::ui::open_existing_worktree_button_rects(inner);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            cancel.x,
            cancel.y,
        ));

        assert!(app.state.worktree_open.is_none());
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn scrolling_open_worktree_picker_moves_selection() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(sample_worktree_open_state());

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 1));
        assert_eq!(app.state.worktree_open.as_ref().unwrap().selected, 1);

        app.handle_mouse(mouse(MouseEventKind::ScrollUp, 1, 1));
        assert_eq!(app.state.worktree_open.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn clicking_remove_worktree_buttons_requests_remove_or_cancels() {
        let mut app = app_for_mouse_test();
        app.state.mode = Mode::ConfirmRemoveWorktree;
        app.state.worktree_remove = Some(crate::app::state::WorktreeRemoveState {
            workspace_id: "issue".into(),
            repo_root: "/repo/herdr".into(),
            path: "/repo/herdr-issue".into(),
            error: None,
            removing: false,
            force_confirmation: false,
        });
        let popup = crate::ui::remove_worktree_popup_rect(app.state.screen_rect()).unwrap();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (remove, _) = crate::ui::remove_worktree_button_rects(inner, false);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            remove.x,
            remove.y,
        ));

        assert!(app.state.worktree_remove.is_some());
        assert!(app.state.request_submit_worktree_remove);

        let mut app = app_for_mouse_test();
        app.state.mode = Mode::ConfirmRemoveWorktree;
        app.state.worktree_remove = Some(crate::app::state::WorktreeRemoveState {
            workspace_id: "issue".into(),
            repo_root: "/repo/herdr".into(),
            path: "/repo/herdr-issue".into(),
            error: None,
            removing: false,
            force_confirmation: false,
        });
        let popup = crate::ui::remove_worktree_popup_rect(app.state.screen_rect()).unwrap();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (_, cancel) = crate::ui::remove_worktree_button_rects(inner, false);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            cancel.x,
            cancel.y,
        ));

        assert!(app.state.worktree_remove.is_none());
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn clicking_confirm_close_accepts_after_workspace_context_menu_close() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 1 },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;
        handle_context_menu_key(
            &mut app.state,
            &mut app.terminal_runtimes,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.selected, 1);

        let popup = app.state.confirm_close_rect();
        let inner = Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        let (confirm, _) = crate::ui::confirm_close_button_rects(inner);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            confirm.x + 1,
            confirm.y,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
    }

    #[test]
    fn clicking_context_menu_close_routes_through_api_path() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.confirm_close = false;
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Workspace { ws_idx: 1 },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;

        let menu = app.state.context_menu_rect().unwrap();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "a");
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::WorkspaceClosed)
        }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keyboard_context_menu_split_keeps_new_runtime() {
        let mut app = app_for_mouse_test();
        app.state.default_shell = "/usr/bin/true".into();
        let (workspace, terminal, runtime) = Workspace::new(
            std::env::current_dir().unwrap_or_else(|_| "/".into()),
            24,
            80,
            app.state.pane_scrollback_limit_bytes,
            app.state.host_terminal_theme,
            app.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&app.state.default_shell, app.state.shell_mode),
            app.event_tx.clone(),
            app.render_notify.clone(),
            app.render_dirty.clone(),
        )
        .expect("workspace should spawn");
        app.state.workspaces = vec![workspace];
        app.terminal_runtimes.insert(terminal.id.clone(), runtime);
        app.state.terminals.insert(terminal.id.clone(), terminal);
        app.state.active = Some(0);
        app.state.selected = 0;
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let runtime_count = app.terminal_runtimes.len();
        app.state.context_menu = Some(ContextMenuState {
            kind: ContextMenuKind::Pane {
                ws_idx: 0,
                tab_idx: 0,
                pane_id,
                source_pane_id: None,
                has_manual_label: false,
            },
            x: 2,
            y: 2,
            list: MenuListState::new(1),
        });
        app.state.mode = Mode::ContextMenu;

        handle_context_menu_key(
            &mut app.state,
            &mut app.terminal_runtimes,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(app.state.mode, Mode::Terminal);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
        assert_eq!(app.terminal_runtimes.len(), runtime_count + 1);

        let runtimes: Vec<_> = app.terminal_runtimes.drain().collect();
        for (_terminal_id, runtime) in runtimes {
            runtime.shutdown();
        }
    }

    #[test]
    fn dragging_pane_split_updates_captured_layout_ratio() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let before = capture_snapshot(&app.state);
        let drag_row = border.area.y.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            border.pos,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            border.pos.saturating_add(6),
            drag_row,
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn pane_split_hitbox_does_not_overlap_right_pane_content() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_gaps = false;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert!(app
            .state
            .find_border_at(border.pos.saturating_sub(1), row)
            .is_none());
        assert!(app.state.find_border_at(border.pos, row).is_some());
        assert!(app
            .state
            .find_border_at(border.pos.saturating_add(1), row)
            .is_none());
    }

    #[test]
    fn pane_split_hitbox_does_not_overlap_bottom_pane_content() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_gaps = false;
        app.state.workspaces[0].test_split(Direction::Vertical);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let col = border.area.x.saturating_add(1);

        assert!(app
            .state
            .find_border_at(col, border.pos.saturating_sub(1))
            .is_none());
        assert!(app.state.find_border_at(col, border.pos).is_some());
        assert!(app
            .state
            .find_border_at(col, border.pos.saturating_add(1))
            .is_none());
    }

    #[test]
    fn borderless_no_gap_split_has_no_mouse_hitbox_over_content() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert!(app.state.find_border_at(border.pos, row).is_none());
    }

    #[test]
    fn bordered_pane_gaps_keep_both_split_borders_draggable() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert!(app
            .state
            .find_border_at(border.pos.saturating_sub(1), row)
            .is_some());
        assert!(app.state.find_border_at(border.pos, row).is_some());
        assert!(app
            .state
            .find_border_at(border.pos.saturating_add(1), row)
            .is_none());
    }

    #[test]
    fn borderless_pane_gap_is_not_a_pane_but_remains_split_draggable() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);
        let gap_col = border.pos.saturating_sub(1);

        assert!(app.state.pane_at(gap_col, row).is_none());
        assert!(app.state.find_border_at(gap_col, row).is_some());
        assert!(app.state.find_border_at(border.pos, row).is_none());
    }

    #[test]
    fn borderless_gap_hitbox_is_empty_when_first_split_side_has_one_cell() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 2, 4));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);
        let candidate_gap_col = border.pos.saturating_sub(1);

        assert!(app.state.pane_frame_at(candidate_gap_col, row).is_some());
        assert!(app.state.find_border_at(candidate_gap_col, row).is_none());
    }

    #[test]
    fn borderless_gap_hitbox_is_empty_when_first_split_side_has_zero_width() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("test")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.pane_borders = false;
        app.state.pane_gaps = true;
        app.state.workspaces[0].test_split(Direction::Horizontal);
        app.state.workspaces[0].tabs[0]
            .layout
            .set_ratio_at(&[], 0.1);
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 1, 4));
        let border = app.state.view.split_borders[0].clone();
        let row = border.area.y.saturating_add(1);

        assert_eq!(border.pos, 0);
        assert!(app.state.find_border_at(0, row).is_none());
    }

    #[test]
    fn selecting_from_right_pane_first_content_column_starts_selection() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let second_pane = ws.test_split(Direction::Horizontal);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let second_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();
        let col = second_info.inner_rect.x;
        let row = second_info.inner_rect.y;

        assert!(app.state.find_border_at(col, row).is_none());
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert!(app.state.drag.is_none());
        assert_eq!(
            app.state
                .selection
                .as_ref()
                .map(|selection| selection.pane_id),
            Some(second_pane)
        );
    }

    #[test]
    fn selecting_from_bottom_pane_first_content_row_starts_selection() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let second_pane = ws.test_split(Direction::Vertical);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let second_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();
        let col = second_info.inner_rect.x;
        let row = second_info.inner_rect.y;

        assert!(app.state.find_border_at(col, row).is_none());
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, row));

        assert!(app.state.drag.is_none());
        assert_eq!(
            app.state
                .selection
                .as_ref()
                .map(|selection| selection.pane_id),
            Some(second_pane)
        );
    }

    #[tokio::test]
    async fn dragging_vertical_pane_split_still_resizes_when_pane_mouse_reporting_is_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Vertical);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let pane_infos = app.state.view.pane_infos.clone();
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();

        app.state.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        app.state.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app
            .state
            .view
            .split_borders
            .iter()
            .find(|border| border.direction == Direction::Vertical)
            .expect("vertical split border")
            .clone();
        let before = capture_snapshot(&app.state);
        let drag_col = border.area.x.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            drag_col,
            border.pos,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            border.pos.saturating_add(4),
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[tokio::test]
    async fn dragging_horizontal_pane_split_still_resizes_when_pane_mouse_reporting_is_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Horizontal);

        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let pane_infos = app.state.view.pane_infos.clone();
        let first_info = pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();
        let second_info = pane_infos
            .iter()
            .find(|info| info.id == second_pane)
            .expect("second pane info")
            .clone();

        app.state.insert_test_runtime(
            first_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                first_info.inner_rect.width.max(1),
                first_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );
        app.state.insert_test_runtime(
            second_pane,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                second_info.inner_rect.width.max(1),
                second_info.inner_rect.height.max(1),
                b"\x1b[?1002h",
            ),
        );

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let border = app
            .state
            .view
            .split_borders
            .iter()
            .find(|border| border.direction == Direction::Horizontal)
            .expect("horizontal split border")
            .clone();
        let before = capture_snapshot(&app.state);
        let drag_row = border.area.y.saturating_add(1);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            border.pos,
            drag_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            border.pos.saturating_add(6),
            drag_row,
        ));

        let after = capture_snapshot(&app.state);
        assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
    }

    #[test]
    fn wheel_routing_prefers_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
            mouse_alternate_scroll: true,
            modify_other_keys: false,
            color_scheme_reporting: false,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::MouseReport);
    }

    #[test]
    fn wheel_over_tab_bar_switches_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        ws.test_add_tab(Some("three"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let tab_bar = app.state.view.tab_bar_rect;

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, tab_bar.x + 1, tab_bar.y));
        assert_eq!(app.state.workspaces[0].active_tab, 1);

        app.handle_mouse(mouse(MouseEventKind::ScrollUp, tab_bar.x + 1, tab_bar.y));
        assert_eq!(app.state.workspaces[0].active_tab, 0);

        app.handle_mouse(mouse(MouseEventKind::ScrollUp, tab_bar.x + 1, tab_bar.y));
        assert_eq!(app.state.workspaces[0].active_tab, 2);

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            tab_bar.x + tab_bar.width.saturating_sub(1),
            tab_bar.y,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn bottom_mode_bar_consumes_hidden_tab_mouse_actions() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Prefix;
        app.state.tab_bar_position = crate::config::TabBarPositionConfig::Bottom;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let second_tab = app.state.view.tab_hit_areas[1];
        let new_tab = app.state.view.new_tab_hit_area;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_tab.x,
            second_tab.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_tab.x,
            new_tab.y,
        ));

        app.state.drag = Some(DragState {
            target: DragTarget::SidebarDivider { grab_offset: 0 },
        });
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            second_tab.x,
            second_tab.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.context_menu.is_none());
        assert!(app.state.tab_press.is_none());
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn right_click_inactive_tab_opens_menu_without_switching_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let second_tab = app.state.view.tab_hit_areas[1];

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_tab.x + 1,
            second_tab.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
        let menu = app.state.context_menu.as_ref().expect("tab context menu");
        assert_eq!(
            menu.kind,
            ContextMenuKind::Tab {
                ws_idx: 0,
                tab_idx: 1
            }
        );
        assert_eq!(app.state.mode, Mode::ContextMenu);
    }

    #[test]
    fn clicking_tab_context_menu_close_leaves_context_menu_mode() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let second_tab = app.state.view.tab_hit_areas[1];

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            second_tab.x + 1,
            second_tab.y,
        ));

        let menu = app
            .state
            .context_menu_rect()
            .expect("tab context menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].display_name(), "one");
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| { matches!(event.event, crate::api::schema::EventKind::TabClosed) }));
    }

    #[test]
    fn clicking_pane_context_menu_close_leaves_context_menu_mode() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        let first_pane = ws.tabs[0].root_pane;
        let second_pane = ws.test_split(Direction::Horizontal);
        ws.tabs[0].layout.focus_pane(second_pane);
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let first_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == first_pane)
            .expect("first pane info")
            .clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            first_info.inner_rect.x + 1,
            first_info.inner_rect.y + 1,
        ));

        let menu_state = app.state.context_menu.as_ref().expect("pane context menu");
        let close_idx = menu_state
            .items()
            .iter()
            .position(|item| *item == "Close pane")
            .expect("close pane menu item");
        let menu = app
            .state
            .context_menu_rect()
            .expect("pane context menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1 + close_idx as u16,
        ));

        assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
        assert!(app.state.context_menu.is_none());
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(event.event, crate::api::schema::EventKind::PaneClosed)
        }));
    }

    #[test]
    fn clicking_pane_context_menu_close_last_parent_group_pane_keeps_confirmation_mode() {
        let mut app = app_for_mouse_test();
        let mut parent = Workspace::test_new("main");
        let pane_id = parent.tabs[0].root_pane;
        mark_worktree_space_member(&mut parent, 0, "repo-key");
        let mut child = Workspace::test_new("issue");
        mark_worktree_space_member(&mut child, 1, "repo-key");
        app.state.workspaces = vec![parent, child];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let pane_info = app
            .state
            .view
            .pane_infos
            .iter()
            .find(|info| info.id == pane_id)
            .expect("pane info")
            .clone();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            pane_info.inner_rect.x + 1,
            pane_info.inner_rect.y + 1,
        ));

        let menu_state = app.state.context_menu.as_ref().expect("pane context menu");
        let close_idx = menu_state
            .items()
            .iter()
            .position(|item| *item == "Close pane")
            .expect("close pane menu item");
        let menu = app
            .state
            .context_menu_rect()
            .expect("pane context menu rect");
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1 + close_idx as u16,
        ));

        assert_eq!(app.state.selected, 0);
        assert_eq!(app.state.mode, Mode::ConfirmClose);
        assert_eq!(app.state.workspaces.len(), 2);
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn wheel_over_overflowing_tab_bar_switches_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.tabs[0].set_custom_name("very-long-one".into());
        ws.test_add_tab(Some("very-long-two"));
        ws.test_add_tab(Some("very-long-three"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));
        assert!(app.state.view.tab_scroll_right_hit_area.width > 0);
        let tab_bar = app.state.view.tab_bar_rect;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            tab_bar.x + tab_bar.width.saturating_sub(2),
            tab_bar.y,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 1);

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            tab_bar.x + tab_bar.width.saturating_sub(2),
            tab_bar.y,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    #[test]
    fn wheel_outside_tab_bar_does_not_switch_tabs() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let terminal = app.state.view.terminal_area;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            terminal.x + 1,
            terminal.y + 1,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, 0);
    }

    #[test]
    fn mobile_switch_button_opens_switcher_and_workspace_row_switches_workspace() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        assert_eq!(app.state.view.layout, ViewLayout::Mobile);

        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Navigate);

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 4,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn mobile_workspace_panel_scroll_reaches_extra_workspaces() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = (0..12)
            .map(|idx| Workspace::test_new(&format!("ws-{idx}")))
            .collect();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_eq!(app.state.mode, Mode::Navigate);

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            viewport.x + 2,
            viewport.y,
        ));
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        assert_eq!(app.state.mobile_switcher_scroll, 2);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 2,
        ));

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn mobile_global_scroll_reaches_tabs_and_switches_tab() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("two"));
        ws.test_add_tab(Some("three"));
        ws.test_add_tab(Some("four"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 12));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            viewport.x + 2,
            viewport.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            viewport.x + 2,
            viewport.y,
        ));
        assert_eq!(app.state.mobile_switcher_scroll, 4);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 4,
        ));
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    #[test]
    fn mobile_switcher_new_workspace_opens_prompt_when_enabled() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_workspace_name = true;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert!(app.state.pending_workspace_create_cwd.is_some());
        assert!(app.state.name_input_replace_on_type);
        assert_eq!(app.state.workspaces.len(), 1);
    }

    #[test]
    fn desktop_new_workspace_opens_prompt_when_enabled() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_workspace_name = true;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_workspace = app.state.sidebar_new_button_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_workspace.x + 1,
            new_workspace.y,
        ));

        assert_eq!(app.state.mode, Mode::RenameWorkspace);
        assert!(app.state.pending_workspace_create_cwd.is_some());
        assert!(app.state.name_input_replace_on_type);
        assert_eq!(app.state.workspaces.len(), 1);
    }

    #[tokio::test]
    async fn desktop_new_workspace_creates_immediately_by_default() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_workspace = app.state.sidebar_new_button_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_workspace.x + 1,
            new_workspace.y,
        ));

        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(app.state.pending_workspace_create_cwd.is_none());
        crate::app::api::test_support::shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn mobile_switcher_new_tab_opens_dialog_when_enabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("logs"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 5,
        ));

        assert_eq!(app.state.mode, Mode::RenameTab);
        assert!(app.state.creating_new_tab);
    }

    #[test]
    fn mobile_switcher_new_tab_skips_dialog_when_prompt_disabled() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("one");
        ws.test_add_tab(Some("logs"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_tab_name = false;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            viewport.x + 2,
            viewport.y + 5,
        ));
        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!app.state.creating_new_tab);
        assert!(app.state.request_new_tab);
        assert!(app.state.requested_new_tab_name.is_none());
    }

    #[test]
    fn desktop_new_tab_button_skips_dialog_when_prompt_disabled() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        app.state.prompt_new_tab_name = false;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 120, 40));
        let new_tab_area = app.state.view.new_tab_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            new_tab_area.x + 1,
            new_tab_area.y,
        ));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!app.state.creating_new_tab);
        assert!(app.state.request_new_tab);
        assert!(app.state.requested_new_tab_name.is_none());
    }

    #[test]
    fn mobile_switcher_swallows_non_left_mouse_events() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_eq!(app.state.mode, Mode::Navigate);

        let viewport = crate::ui::mobile_switcher_areas(&app.state).viewport;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Right),
            viewport.x + 2,
            viewport.y + 2,
        ));

        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(app.state.context_menu.is_none());
    }

    #[test]
    fn mobile_switch_button_does_not_bypass_rename_modal() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::RenameTab;
        app.state.creating_new_tab = true;
        app.state.name_input = "new tab".into();

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Terminal);
        assert!(!app.state.creating_new_tab);
        assert!(!app.state.request_new_tab);
    }

    #[test]
    fn mobile_switcher_close_returns_to_terminal() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("one")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;

        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 44, 20));
        let switch = app.state.view.mobile_menu_hit_area;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            switch.x + 1,
            switch.y + 1,
        ));
        assert_eq!(app.state.mode, Mode::Navigate);

        let close = crate::ui::mobile_switcher_areas(&app.state).close;
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            close.x + 1,
            close.y,
        ));

        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn wheel_routing_uses_alternate_scroll_in_fullscreen_without_mouse_reporting() {
        let input_state = crate::pane::InputState {
            alternate_screen: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::None,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
            modify_other_keys: false,
            color_scheme_reporting: false,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::AlternateScroll);
    }

    #[test]
    fn wheel_routing_falls_back_to_host_scrollback() {
        let input_state = crate::pane::InputState {
            alternate_screen: false,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_protocol_mode: crate::input::MouseProtocolMode::None,
            mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
            mouse_alternate_scroll: true,
            modify_other_keys: false,
            color_scheme_reporting: false,
        };

        assert_eq!(wheel_routing(input_state), WheelRouting::HostScroll);
    }
}
