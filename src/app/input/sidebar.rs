use ratatui::layout::Rect;

use crate::app::state::{AppState, ViewLayout};

use super::ScrollbarClickTarget;

impl AppState {
    pub(super) fn workspace_list_rect(&self) -> Rect {
        let sidebar = self.view.sidebar_rect;
        if self.sidebar_collapsed || sidebar.width <= 1 || sidebar.height == 0 {
            return Rect::default();
        }
        crate::ui::workspace_list_rect(sidebar)
    }

    pub(super) fn workspace_list_scrollbar_target_at(
        &self,
        col: u16,
        row: u16,
    ) -> Option<ScrollbarClickTarget> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        if col < track.x
            || col >= track.x + track.width
            || row < track.y
            || row >= track.y + track.height
        {
            return None;
        }
        if let Some(grab_row_offset) = crate::ui::scrollbar_thumb_grab_offset(metrics, track, row) {
            Some(ScrollbarClickTarget::Thumb { grab_row_offset })
        } else {
            Some(ScrollbarClickTarget::Track {
                offset_from_bottom: crate::ui::scrollbar_offset_from_row(metrics, track, row),
            })
        }
    }

    pub(super) fn workspace_list_offset_for_drag_row(
        &self,
        row: u16,
        grab_row_offset: u16,
    ) -> Option<usize> {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        let track = crate::ui::workspace_list_scrollbar_rect(self, area)?;
        Some(crate::ui::scrollbar_offset_from_drag_row(
            metrics,
            track,
            row,
            grab_row_offset,
        ))
    }

    pub(super) fn set_workspace_list_offset_from_bottom(&mut self, offset_from_bottom: usize) {
        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = metrics
            .max_offset_from_bottom
            .saturating_sub(offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    pub(super) fn scroll_workspace_list(&mut self, delta: i16) {
        if delta.is_negative() {
            self.workspace_scroll = self
                .workspace_scroll
                .saturating_sub(delta.unsigned_abs() as usize);
            self.workspace_scroll = crate::ui::normalized_workspace_scroll(
                self,
                self.view.sidebar_rect,
                self.workspace_scroll,
            );
            return;
        }

        let area = self.workspace_list_rect();
        let metrics = crate::ui::workspace_list_scroll_metrics(self, area);
        self.workspace_scroll = self
            .workspace_scroll
            .saturating_add(delta as usize)
            .min(metrics.max_offset_from_bottom);
        self.workspace_scroll = crate::ui::normalized_workspace_scroll(
            self,
            self.view.sidebar_rect,
            self.workspace_scroll,
        );
    }

    /// The `new` / `menu` row at the bottom of the sidebar.
    ///
    /// One column short of the panel, because the collapse toggle draws in the
    /// panel's last cell on this very row. With two sections the footer sat
    /// mid-sidebar and the two never met; with one full-height panel they share
    /// a row, and a right-aligned `menu` label that took the last cell would
    /// both cover the toggle and swallow its clicks.
    pub(crate) fn sidebar_footer_rect(&self) -> Rect {
        let ws_area = self.workspace_list_rect();
        if ws_area == Rect::default() {
            return Rect::default();
        }
        let y = ws_area.y + ws_area.height.saturating_sub(1);
        let toggle = crate::ui::expanded_sidebar_toggle_rect(self.view.sidebar_rect);
        let width = if toggle.height > 0 && toggle.y == y {
            ws_area.width.saturating_sub(1)
        } else {
            ws_area.width
        };
        Rect::new(ws_area.x, y, width, 1)
    }

    pub(crate) fn sidebar_new_button_rect(&self) -> Rect {
        let footer = self.sidebar_footer_rect();
        let width = 5u16.min(footer.width.max(1));
        Rect::new(footer.x, footer.y, width, footer.height)
    }

    pub(crate) fn global_launcher_rect(&self) -> Rect {
        if self.view.layout == ViewLayout::Mobile {
            return self.view.mobile_menu_hit_area;
        }

        let footer = self.sidebar_footer_rect();
        let width = if self.global_menu_attention_badge_visible() {
            8
        } else {
            6
        }
        .min(footer.width.max(1));
        let x = footer.x + footer.width.saturating_sub(width);
        Rect::new(x, footer.y, width, footer.height)
    }

    pub(crate) fn global_menu_labels(&self) -> Vec<&'static str> {
        let mut labels = vec!["settings", "keybinds", "reload config"];
        if self.update_available.is_some() {
            labels.push("update ready");
        } else if self.latest_release_notes_available {
            labels.push("what's new");
        }
        labels.push("detach");
        labels
    }

    pub(crate) fn global_menu_rect(&self) -> Rect {
        let screen = self.screen_rect();
        let launcher = self.global_launcher_rect();
        let labels = self.global_menu_labels();
        let content_width = labels
            .iter()
            .map(|label| {
                let badge_width = if self.global_menu_item_has_badge(label) {
                    2
                } else {
                    0
                };
                label.chars().count() as u16 + badge_width
            })
            .max()
            .unwrap_or(8)
            .saturating_add(2);
        let menu_w = content_width.saturating_add(2).min(screen.width.max(1));
        let menu_h = (labels.len() as u16 + 2).min(screen.height.max(1));
        let max_x = screen.x + screen.width.saturating_sub(menu_w);
        let desired_x = launcher.x + launcher.width.saturating_sub(menu_w);
        let x = desired_x.min(max_x);
        let y = launcher.y.saturating_sub(menu_h);
        Rect::new(x, y, menu_w, menu_h)
    }

    /// Grab tolerance around a one-cell sidebar divider, in cells.
    ///
    /// This is the same one extra cell `find_border_at` accepts around a pane
    /// split border. Unlike a pane split, neither sidebar divider has dead
    /// space beside it, so the band is applied on one side only, biased toward
    /// the neighbour with less to lose.
    pub(super) const DIVIDER_GRAB_TOLERANCE: u16 = 1;

    /// Divider position for a pointer at `coord`, given the offset recorded
    /// when the divider was grabbed. Keeping the offset is what stops a press
    /// inside the tolerance band from snapping the divider under the cursor.
    pub(super) fn divider_pos_from_grab(coord: u16, grab_offset: i16) -> u16 {
        (i32::from(coord) + i32::from(grab_offset)).clamp(0, i32::from(u16::MAX)) as u16
    }

    /// Where the sidebar's vertical bar sits relative to a press, or `None`
    /// when the press is outside the grab band. Adding the returned offset to
    /// the press column gives the divider column, so a drag keeps the divider
    /// at the same distance from the cursor it was grabbed at.
    ///
    /// The band extends left, over the sidebar's last content column, the same
    /// direction `find_border_at` extends a pane split border. Extending right
    /// instead would cover the leftmost pane's first content column — herdr
    /// draws no border for a lone pane — and a press there is forwarded to the
    /// running program. Losing the right-hand edge of a workspace card is the
    /// cheaper of the two. The collapse toggle lives in that column and stays
    /// carved out.
    pub(super) fn sidebar_divider_grab_at(&self, col: u16, row: u16) -> Option<i16> {
        if self.sidebar_collapsed {
            return None;
        }
        let sidebar = self.view.sidebar_rect;
        if sidebar.width == 0 || row < sidebar.y || row >= sidebar.y + sidebar.height {
            return None;
        }

        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);
        if toggle.width > 0
            && col >= toggle.x
            && col < toggle.x + toggle.width
            && row >= toggle.y
            && row < toggle.y + toggle.height
        {
            return None;
        }

        let divider_col = sidebar.x + sidebar.width - 1;
        let offset = divider_col.checked_sub(col)?;
        if offset > Self::DIVIDER_GRAB_TOLERANCE {
            return None;
        }
        // The band must not reach past the sidebar's own left edge.
        if col < sidebar.x {
            return None;
        }

        // Worktree group chevrons sit in the tolerance column too; they keep
        // their clicks.
        if offset > 0 && self.workspace_group_chevron_at(col, row).is_some() {
            return None;
        }

        // So do both sidebar scrollbars: each panel is laid out inside
        // `sidebar.width - 1`, so its track is drawn on exactly the column the
        // band extends over. Swallowing a track press turns every scrollbar
        // drag into a sidebar resize, so the tracks keep their presses. Only
        // the tolerance column is given up, and only on the rows a track
        // actually covers; the bar itself stays grabbable everywhere.
        if offset > 0 && self.sidebar_scrollbar_track_at(col, row) {
            return None;
        }

        Some(offset as i16)
    }

    /// Whether the sidebar draws a scrollbar track over this cell. The track
    /// only exists while the tree overflows, so this is false whenever it fits
    /// and the band is at full width.
    fn sidebar_scrollbar_track_at(&self, col: u16, row: u16) -> bool {
        crate::ui::workspace_list_scrollbar_rect(self, self.workspace_list_rect()).is_some_and(
            |track| {
                col >= track.x
                    && col < track.x + track.width
                    && row >= track.y
                    && row < track.y + track.height
            },
        )
    }

    /// The workspace whose group chevron occupies this cell, when that chevron
    /// is live. Only parents of a worktree group render one.
    pub(super) fn workspace_group_chevron_at(&self, col: u16, row: u16) -> Option<usize> {
        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };
        cards.iter().find_map(|card| {
            let chevron = crate::ui::workspace_group_chevron_rect(card);
            (chevron.width > 0
                && col == chevron.x
                && row == chevron.y
                && crate::ui::workspace_parent_group_state(self, card.ws_idx).is_some())
            .then_some(card.ws_idx)
        })
    }

    pub(super) fn on_sidebar_divider(&self, col: u16, row: u16) -> bool {
        self.sidebar_divider_grab_at(col, row).is_some()
    }

    pub(super) fn on_sidebar_toggle(&self, col: u16, row: u16) -> bool {
        let rect = if self.sidebar_collapsed {
            crate::ui::collapsed_sidebar_toggle_rect(self.view.sidebar_rect)
        } else {
            crate::ui::expanded_sidebar_toggle_rect(self.view.sidebar_rect)
        };
        rect.width > 0
            && col >= rect.x
            && col < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub(super) fn set_manual_sidebar_width(&mut self, divider_col: u16) {
        let sidebar = self.view.sidebar_rect;
        let width = divider_col.saturating_sub(sidebar.x).saturating_add(1);
        self.sidebar_width = width.clamp(self.sidebar_min_width, self.sidebar_max_width);
        self.sidebar_width_source = crate::app::state::SidebarWidthSource::Manual;
        self.mark_session_dirty();
    }

    /// The tree row under `row`, Space or agent alike.
    fn sidebar_card_at_row(&self, row: u16) -> Option<crate::app::state::WorkspaceCardArea> {
        if self.sidebar_footer_rect() == Rect::default() {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };

        cards
            .into_iter()
            .find(|card| row >= card.rect.y && row < card.rect.y + card.rect.height)
    }

    /// The Space under `row`. An agent row is deliberately not one: it draws in
    /// the same list but selecting it focuses a pane, and a workspace drag must
    /// not pick it up.
    pub(super) fn workspace_at_row(&self, row: u16) -> Option<usize> {
        let card = self.sidebar_card_at_row(row)?;
        card.agent.is_none().then_some(card.ws_idx)
    }

    /// The pane an agent row under `row` points at.
    pub(super) fn sidebar_agent_target_at(
        &self,
        row: u16,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let card = self.sidebar_card_at_row(row)?;
        let agent = card.agent?;
        Some((card.ws_idx, agent.pane_id))
    }

    pub(super) fn collapsed_workspace_at_row(&self, row: u16) -> Option<usize> {
        if !self.sidebar_collapsed {
            return None;
        }

        let ws_area = crate::ui::collapsed_sidebar_sections(self.view.sidebar_rect);
        if ws_area == Rect::default() || row < ws_area.y || row >= ws_area.y + ws_area.height {
            return None;
        }

        let idx = (row - ws_area.y) as usize;
        (idx < self.workspaces.len()).then_some(idx)
    }

    pub(super) fn workspace_drop_target_at_row(
        &self,
        row: u16,
    ) -> Option<crate::app::state::WorkspaceDropTarget> {
        let area = self.workspace_list_rect();
        let footer = self.sidebar_footer_rect();
        if area == Rect::default() || row < area.y || row >= footer.y {
            return None;
        }

        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };
        crate::ui::workspace_drop_slots(self, &cards, area)
            .into_iter()
            .enumerate()
            .min_by_key(|(slot_idx, (_, slot_row))| (row.abs_diff(*slot_row), *slot_idx))
            .map(|(_, (target, _))| target)
    }

    pub(super) fn workspace_move_block_params(
        &self,
        source_ws_idx: usize,
        drop_target: crate::app::state::WorkspaceDropTarget,
    ) -> Option<crate::api::schema::WorkspaceMoveBlockParams> {
        let source = self.workspaces.get(source_ws_idx)?;
        if source
            .worktree_space()
            .is_some_and(|space| space.is_linked_worktree)
        {
            return None;
        }

        let roots = crate::ui::workspace_list_entries_expanded(self)
            .into_iter()
            .filter_map(|entry| match entry {
                crate::ui::WorkspaceListEntry::Workspace {
                    ws_idx,
                    indented: false,
                    ..
                } => Some(ws_idx),
                _ => None,
            })
            .collect::<Vec<_>>();
        let source_pos = roots.iter().position(|ws_idx| *ws_idx == source_ws_idx)?;
        let remaining_roots = roots
            .iter()
            .copied()
            .filter(|ws_idx| *ws_idx != source_ws_idx)
            .collect::<Vec<_>>();
        let insert_pos = match drop_target {
            crate::app::state::WorkspaceDropTarget::Before(target_ws_idx) => remaining_roots
                .iter()
                .position(|ws_idx| *ws_idx == target_ws_idx)?,
            crate::app::state::WorkspaceDropTarget::End => remaining_roots.len(),
        };
        if insert_pos == source_pos {
            return None;
        }

        let workspace_ids = match source.worktree_space() {
            Some(source_space) => {
                let mut ids = vec![source.id.clone()];
                ids.extend(
                    self.workspaces
                        .iter()
                        .filter(|workspace| workspace.id != source.id)
                        .filter(|workspace| {
                            workspace
                                .worktree_space()
                                .is_some_and(|space| space.key == source_space.key)
                        })
                        .map(|workspace| workspace.id.clone()),
                );
                ids
            }
            None => vec![source.id.clone()],
        };
        let before_workspace_id = match drop_target {
            crate::app::state::WorkspaceDropTarget::Before(target_ws_idx) => {
                let target = self.workspaces.get(target_ws_idx)?;
                let anchor = match crate::ui::workspace_parent_group_state(self, target_ws_idx)
                    .and_then(|_| target.worktree_space())
                {
                    Some(target_space) => self
                        .workspaces
                        .iter()
                        .find(|workspace| {
                            workspace
                                .worktree_space()
                                .is_some_and(|space| space.key == target_space.key)
                        })
                        .unwrap_or(target),
                    None => target,
                };
                Some(anchor.id.clone())
            }
            crate::app::state::WorkspaceDropTarget::End => None,
        };

        Some(crate::api::schema::WorkspaceMoveBlockParams {
            workspace_ids,
            before_workspace_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{MouseButton, MouseEventKind};
    use ratatui::layout::Rect;

    use super::super::{app_for_mouse_test, capture_snapshot, mouse, unique_temp_path};
    use crate::{
        app::state::{DragTarget, Mode},
        config::SidebarCollapsedModeConfig,
        detect::Agent,
        workspace::Workspace,
    };

    #[test]
    fn clicking_launcher_opens_global_menu() {
        let mut app = app_for_mouse_test();
        let rect = app.state.global_launcher_rect();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + rect.width.saturating_sub(1),
            rect.y,
        ));

        assert_eq!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn hovering_global_menu_updates_highlight() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

        assert_eq!(app.state.global_menu.highlighted, 1);
    }

    #[test]
    fn clicking_keybinds_menu_item_opens_help() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 2,
        ));

        assert_eq!(app.state.mode, Mode::KeybindHelp);
    }

    #[test]
    fn clicking_settings_menu_item_opens_settings() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 1,
        ));

        assert_eq!(app.state.mode, Mode::Settings);
    }

    #[test]
    fn clicking_reload_config_menu_item_requests_reload() {
        let mut app = app_for_mouse_test();
        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 3,
        ));

        assert!(app.state.request_reload_config);
        assert_eq!(app.state.mode, Mode::Navigate);
    }

    #[test]
    fn update_pending_menu_surfaces_update_ready_entry() {
        let mut app = app_for_mouse_test();
        app.state.update_available = Some("0.3.2".into());
        app.state.latest_release_notes_available = true;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "update ready",
                "detach"
            ]
        );
        assert!(!app.state.should_quit);
    }

    #[test]
    fn persistence_mode_menu_surfaces_detach_action() {
        let mut app = app_for_mouse_test();
        app.state.detach_exits = false;

        let launcher = app.state.global_launcher_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            launcher.x,
            launcher.y,
        ));

        assert_eq!(
            app.state.global_menu_labels(),
            vec!["settings", "keybinds", "reload config", "detach"]
        );

        let menu = app.state.global_menu_rect();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            menu.x + 2,
            menu.y + 4,
        ));

        assert!(app.state.detach_requested);
        assert!(!app.state.should_quit);
        assert_ne!(app.state.mode, Mode::GlobalMenu);
    }

    #[test]
    fn whats_new_remains_in_menu_for_latest_installed_release_notes() {
        let mut app = app_for_mouse_test();
        app.state.latest_release_notes_available = true;

        assert_eq!(
            app.state.global_menu_labels(),
            vec![
                "settings",
                "keybinds",
                "reload config",
                "what's new",
                "detach"
            ]
        );
    }

    #[test]
    fn clicking_collapsed_sidebar_toggle_expands_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
        app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

        let toggle = crate::ui::collapsed_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(!app.state.sidebar_collapsed);
    }

    #[test]
    fn hidden_collapsed_sidebar_has_no_mouse_expand_hotspot() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = true;
        app.state.sidebar_collapsed_mode = SidebarCollapsedModeConfig::Hidden;
        app.state.view.sidebar_rect = Rect::new(0, 0, 0, 20);
        app.state.view.terminal_area = Rect::new(0, 0, 80, 20);

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 19));

        assert!(app.state.sidebar_collapsed);
    }

    #[test]
    fn clicking_expanded_sidebar_toggle_collapses_sidebar() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_collapsed = false;
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
        app.state.view.terminal_area = Rect::new(26, 0, 80, 20);

        let toggle = crate::ui::expanded_sidebar_toggle_rect(app.state.view.sidebar_rect);
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            toggle.x,
            toggle.y,
        ));

        assert!(app.state.sidebar_collapsed);
        assert!(app.state.drag.is_none());
    }

    #[test]
    fn clicking_workspace_switches_on_mouse_up() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let target_row = app.state.view.workspace_card_areas[1].rect.y;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            target_row,
        ));
        assert_eq!(app.state.active, Some(0));
        assert!(app.state.workspace_press.is_some());

        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.workspace_press.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.active, Some(1));
        assert_eq!(snapshot.selected, 1);
    }

    #[test]
    fn clicking_worktree_parent_row_focuses_workspace_without_toggling() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0].rect;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            parent.x + 2,
            parent.y,
        ));

        assert_eq!(app.state.active, Some(0));
        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn clicking_worktree_parent_chevron_toggles_group_only() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![Workspace::test_new("main"), Workspace::test_new("issue")];
        for (idx, checkout_path) in ["/repo/herdr", "/repo/herdr-issue"].into_iter().enumerate() {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx > 0,
                });
        }
        app.state.active = None;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let parent = app.state.view.workspace_card_areas[0];
        let chevron = crate::ui::workspace_group_chevron_rect(&parent);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chevron.x,
            chevron.y,
        ));

        assert_eq!(app.state.active, None);
        assert!(app.state.workspace_press.is_none());
        assert!(app.state.collapsed_space_keys.contains("repo-key"));

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            chevron.x,
            chevron.y,
        ));

        assert!(!app.state.collapsed_space_keys.contains("repo-key"));
    }

    #[test]
    fn wheel_workspace_selection_follows_grouped_visual_order_without_scrollbar() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("main"),
            Workspace::test_new("normal"),
            Workspace::test_new("issue"),
        ];
        for (idx, checkout_path) in [(0, "/repo/herdr"), (2, "/repo/herdr-issue")] {
            app.state.workspaces[idx].worktree_space =
                Some(crate::workspace::WorktreeSpaceMembership {
                    key: "repo-key".into(),
                    label: "herdr".into(),
                    repo_root: "/repo/herdr".into(),
                    checkout_path: checkout_path.into(),
                    is_linked_worktree: idx != 0,
                });
        }
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Navigate;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 30));
        let list = app.state.workspace_list_rect();
        assert!(!crate::ui::should_show_scrollbar(
            crate::ui::workspace_list_scroll_metrics(&app.state, list)
        ));

        app.handle_mouse(mouse(MouseEventKind::ScrollDown, list.x + 1, list.y + 1));

        assert_eq!(app.state.selected, 2);
    }

    #[test]
    fn dragging_workspace_reorders_without_changing_identity() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        app.state.sidebar_spaces.rows = vec![vec![crate::config::SpaceSidebarToken::Workspace]];
        app.state.sidebar_spaces.row_gap = 0;
        let active_id = app.state.workspaces[1].id.clone();
        let selected_id = app.state.workspaces[2].id.clone();
        app.state.active = Some(1);
        app.state.selected = 2;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
        let packed_boundary_row = app.state.view.workspace_card_areas[1].rect.y;
        assert_eq!(
            app.state.workspace_drop_target_at_row(packed_boundary_row),
            Some(crate::app::state::WorkspaceDropTarget::Before(2))
        );

        let source_row = app.state.view.workspace_card_areas[1].rect.y;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::Before(0),
        )
        .unwrap();

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            2,
            source_row,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 1,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::Before(0)),
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names: Vec<_> = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.selected, 2);
        assert_eq!(app.state.workspaces[0].id, active_id);
        assert_eq!(app.state.workspaces[2].id, selected_id);
        let events = app.event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::WorkspaceMoved { .. }
        )));
        assert!(!events.iter().any(|(_, event)| matches!(
            event.data,
            crate::api::schema::EventData::WorkspaceReordered { .. }
        )));
        let snapshot = capture_snapshot(&app.state);
        let captured_names: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.custom_name.clone().unwrap())
            .collect();
        assert_eq!(captured_names, vec!["b", "a", "c"]);
    }

    #[test]
    fn clicking_tab_scroll_button_reveals_hidden_tabs_without_renaming() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("logs"));
        ws.test_add_tab(Some("review"));
        ws.test_add_tab(Some("ops"));
        ws.test_add_tab(Some("notes"));
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));

        let right = app.state.view.tab_scroll_right_hit_area;
        assert!(right.width > 0);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right.x + 1,
            right.y,
        ));

        assert_eq!(app.state.tab_scroll, 1);
        assert!(!app.state.tab_scroll_follow_active);
        assert_eq!(app.state.workspaces[0].active_tab, 0);
        assert_eq!(app.state.view.tab_hit_areas[0].width, 0);
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.state.workspaces[0].tabs[1].custom_name.as_deref(),
            Some("logs")
        );
    }

    #[test]
    fn clicking_last_visible_tab_at_right_edge_does_not_overscroll() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        for name in [
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ] {
            ws.test_add_tab(Some(name));
        }
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.tab_scroll = usize::MAX;
        app.state.tab_scroll_follow_active = false;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 65, 20));

        let last_idx = app.state.workspaces[0].tabs.len() - 1;
        let target = app.state.view.tab_hit_areas[last_idx];
        let clamped_scroll = app.state.tab_scroll;
        assert!(target.width > 0, "last tab should already be visible");

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            target.x + 1,
            target.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            target.x + 1,
            target.y,
        ));

        assert_eq!(app.state.workspaces[0].active_tab, last_idx);
        assert_eq!(app.state.tab_scroll, clamped_scroll);
        assert!(app.state.view.tab_hit_areas[last_idx].width > 0);
    }

    #[test]
    fn dragging_tab_reorders_auto_and_custom_names_without_materializing_numbers() {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("test");
        ws.test_add_tab(Some("foo"));
        ws.test_add_tab(None);
        let moved_root = ws.tabs[0].root_pane;
        app.state.workspaces = vec![ws];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        let source = app.state.view.tab_hit_areas[0];
        let last = app.state.view.tab_hit_areas[2];
        let drop_col = last.x + last.width;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            source.x + 1,
            source.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            drop_col,
            source.y,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::TabReorder {
                ws_idx: 0,
                source_tab_idx: 0,
                insert_idx: Some(3),
            })
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            drop_col,
            source.y,
        ));

        let labels: Vec<_> = app.state.workspaces[0]
            .tabs
            .iter()
            .enumerate()
            .map(|(tab_idx, _)| app.state.workspaces[0].tab_display_name(tab_idx).unwrap())
            .collect();
        assert_eq!(labels, vec!["foo", "2", "3"]);
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("foo")
        );
        assert!(app.state.workspaces[0].tabs[1].custom_name.is_none());
        assert!(app.state.workspaces[0].tabs[2].custom_name.is_none());
        assert_eq!(app.state.workspaces[0].tabs[0].number, 2);
        assert_eq!(app.state.workspaces[0].tabs[1].number, 3);
        assert_eq!(app.state.workspaces[0].tabs[2].number, 1);
        assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
        assert_eq!(app.state.workspaces[0].active_tab, 2);
    }

    fn temp_git_repo(branch: &str) -> std::path::PathBuf {
        let repo = unique_temp_path("sidebar-drop-slot-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(
            repo.join(".git/HEAD"),
            format!("ref: refs/heads/{branch}\n"),
        )
        .unwrap();
        repo
    }

    fn workspace_with_space(name: &str, key: &str) -> Workspace {
        let mut ws = Workspace::test_new(name);
        ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: key.into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: format!("/repo/{name}").into(),
            is_linked_worktree: name != "main",
        });
        ws
    }

    #[test]
    fn top_drop_slot_is_distinct_from_gap_below_first_workspace() {
        let mut app = app_for_mouse_test();
        let first_repo = temp_git_repo("main");
        let second_repo = temp_git_repo("main");

        let mut first = Workspace::test_new("a");
        let first_root = first.tabs[0].root_pane;
        first.identity_cwd = first_repo.clone();
        first.refresh_git_ahead_behind();

        let mut second = Workspace::test_new("b");
        let second_root = second.tabs[0].root_pane;
        second.identity_cwd = second_repo.clone();
        second.refresh_git_ahead_behind();

        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let first_terminal_id = app.state.workspaces[0].tabs[0].panes[&first_root]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&first_terminal_id).unwrap().cwd = first_repo.clone();
        let second_terminal_id = app.state.workspaces[1].tabs[0].panes[&second_root]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&second_terminal_id)
            .unwrap()
            .cwd = second_repo.clone();
        app.state.sidebar_spaces.row_gap = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

        // The empty header row and the first card's own row both mean "above
        // everything"; the gap row below that card is the first row that names
        // the second Space. Removing the `spaces` title moved every row up by
        // one without changing which slot owns it.
        assert_eq!(
            app.state.workspace_drop_target_at_row(0),
            Some(crate::app::state::WorkspaceDropTarget::Before(0))
        );
        assert_eq!(
            app.state.workspace_drop_target_at_row(1),
            Some(crate::app::state::WorkspaceDropTarget::Before(0))
        );
        assert_eq!(
            app.state.workspace_drop_target_at_row(2),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );
        assert_eq!(
            app.state.workspace_drop_target_at_row(3),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );

        let _ = fs::remove_dir_all(first_repo);
        let _ = fs::remove_dir_all(second_repo);
    }

    #[test]
    fn bottom_drop_slot_stays_below_last_workspace_not_footer() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("a"),
            Workspace::test_new("b"),
            Workspace::test_new("c"),
        ];
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 24));

        let cards = &app.state.view.workspace_card_areas;
        let bottom_slot = crate::ui::workspace_drop_indicator_row(
            &app.state,
            cards,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();

        let last = cards.last().unwrap().rect;
        assert_eq!(bottom_slot, last.y + last.height);
        assert!(bottom_slot < app.state.sidebar_footer_rect().y.saturating_sub(1));
    }

    #[test]
    fn grouped_sidebar_drop_slots_do_not_land_inside_compact_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(1);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let cards = &app.state.view.workspace_card_areas;
        let order = cards.iter().map(|card| card.ws_idx).collect::<Vec<_>>();
        assert_eq!(order, vec![0, 2, 1]);
        let issue = cards.iter().find(|card| card.ws_idx == 2).unwrap();
        let normal = cards.iter().find(|card| card.ws_idx == 1).unwrap();

        assert_eq!(
            app.state.workspace_drop_target_at_row(issue.rect.y),
            Some(crate::app::state::WorkspaceDropTarget::Before(1))
        );
        assert_eq!(
            crate::ui::workspace_drop_indicator_row(
                &app.state,
                cards,
                app.state.workspace_list_rect(),
                crate::app::state::WorkspaceDropTarget::End,
            ),
            Some(normal.rect.y + normal.rect.height)
        );
    }

    #[test]
    fn plain_drag_anchors_to_the_selected_parentless_linked_workspace() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("one", "repo-key"),
            workspace_with_space("two", "repo-key"),
            Workspace::test_new("normal"),
        ];
        let target_id = app.state.workspaces[1].id.clone();

        let params = app
            .state
            .workspace_move_block_params(2, crate::app::state::WorkspaceDropTarget::Before(1))
            .unwrap();

        assert_eq!(params.workspace_ids, [app.state.workspaces[2].id.clone()]);
        assert_eq!(
            params.before_workspace_id.as_deref(),
            Some(target_id.as_str())
        );
    }

    #[test]
    fn dragging_worktree_parent_reorders_the_complete_group() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(2);
        app.state.selected = 1;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let parent = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 0)
            .unwrap()
            .rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();
        let active_id = app.state.workspaces[2].id.clone();
        let selected_id = app.state.workspaces[1].id.clone();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, parent.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(matches!(
            app.state.drag.as_ref().map(|drag| &drag.target),
            Some(DragTarget::WorkspaceReorder {
                source_ws_idx: 0,
                drop_target: Some(crate::app::state::WorkspaceDropTarget::End),
            })
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "main", "issue"]
        );
        assert_eq!(
            app.state.workspaces[app.state.active.unwrap()].id,
            active_id
        );
        assert_eq!(app.state.workspaces[app.state.selected].id, selected_id);
    }

    #[test]
    fn dragging_collapsed_worktree_parent_still_moves_hidden_children() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("issue", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("main", "repo-key"),
            workspace_with_space("review", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 1;
        app.state.collapsed_space_keys.insert("repo-key".into());
        let active_id = app.state.workspaces[0].id.clone();
        let selected_id = app.state.workspaces[1].id.clone();
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));
        assert_eq!(app.state.view.workspace_card_areas.len(), 3);

        let parent = app.state.view.workspace_card_areas[0].rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::End,
        )
        .unwrap();
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, parent.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "main", "issue", "review"]
        );
        assert_eq!(
            app.state.workspaces[app.state.active.unwrap()].id,
            active_id
        );
        assert_eq!(app.state.workspaces[app.state.selected].id, selected_id);
    }

    #[test]
    fn dragging_worktree_space_member_does_not_reorder_workspaces() {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            workspace_with_space("main", "repo-key"),
            Workspace::test_new("normal"),
            workspace_with_space("issue", "repo-key"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 40));

        let source = app
            .state
            .view
            .workspace_card_areas
            .iter()
            .find(|card| card.ws_idx == 2)
            .unwrap()
            .rect;
        let target_row = crate::ui::workspace_drop_indicator_row(
            &app.state,
            &app.state.view.workspace_card_areas,
            app.state.workspace_list_rect(),
            crate::app::state::WorkspaceDropTarget::Before(0),
        )
        .unwrap();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, source.y));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            2,
            target_row,
        ));
        assert!(app.state.drag.is_none());
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

        let names = app
            .state
            .workspaces
            .iter()
            .map(|ws| ws.display_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["main", "normal", "issue"]);
    }

    #[test]
    fn dragging_sidebar_divider_sets_manual_width() {
        let mut app = app_for_mouse_test();

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

        assert_eq!(app.state.sidebar_width, 31);
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(31));
    }

    #[test]
    fn dragging_past_max_clamps_to_configured_max() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_max_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 50, 5));

        assert_eq!(app.state.sidebar_width, 30);
    }

    #[test]
    fn dragging_below_min_clamps_to_configured_min() {
        let mut app = app_for_mouse_test();
        app.state.sidebar_min_width = 22;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 5));

        assert_eq!(app.state.sidebar_width, 22);
    }

    #[test]
    fn double_clicking_sidebar_divider_resets_default_width() {
        let mut app = app_for_mouse_test();
        app.state.default_sidebar_width = 26;
        app.state.sidebar_width = 30;

        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
        app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

        assert_eq!(app.state.sidebar_width, 26);
        assert!(app.state.drag.is_none());
        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(26));
    }

    /// Desktop-sized area used by the real-geometry divider tests below. These
    /// tests deliberately go through `compute_view` and a real render instead of
    /// hand-setting `view.sidebar_rect`, so they fail if the divider hit targets
    /// ever drift away from the cells that are actually drawn.
    const DIVIDER_TEST_AREA: Rect = Rect {
        x: 0,
        y: 0,
        width: 120,
        height: 40,
    };

    fn app_for_divider_test() -> crate::app::App {
        let mut app = app_for_mouse_test();
        app.state.workspaces = vec![
            Workspace::test_new("alpha"),
            Workspace::test_new("beta"),
            Workspace::test_new("gamma"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        crate::ui::compute_view(&mut app.state, DIVIDER_TEST_AREA);
        app
    }

    fn recompute(app: &mut crate::app::App) {
        crate::ui::compute_view(&mut app.state, DIVIDER_TEST_AREA);
    }

    /// Renders the app and returns the column of the sidebar's vertical bar and
    /// the row of the horizontal separator between the two sidebar panels, read
    /// back out of the drawn buffer rather than from geometry helpers.
    fn drawn_divider_cells(app: &mut crate::app::App) -> Option<u16> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
            DIVIDER_TEST_AREA.width,
            DIVIDER_TEST_AREA.height,
        ))
        .expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app.state, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();

        let probe_row = DIVIDER_TEST_AREA.y + 4;
        (0..DIVIDER_TEST_AREA.width).find(|col| buffer[(*col, probe_row)].symbol() == "│")
    }

    fn drag_divider(app: &mut crate::app::App, from: (u16, u16), to: (u16, u16)) {
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            from.0,
            from.1,
        ));
        app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), to.0, to.1));
        app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1));
    }

    #[test]
    fn sidebar_divider_grab_band_covers_the_drawn_bar_and_one_cell_left() {
        let mut app = app_for_divider_test();
        let bar_col = drawn_divider_cells(&mut app);
        let bar_col = bar_col.expect("sidebar bar is drawn");
        let row = DIVIDER_TEST_AREA.y + 4;

        assert_eq!(app.state.sidebar_divider_grab_at(bar_col, row), Some(0));
        assert_eq!(app.state.sidebar_divider_grab_at(bar_col - 1, row), Some(1));
        // One cell wide and biased left; it never reaches the terminal area,
        // whose first column is the leftmost pane's content when that pane is
        // not split, nor a second column of sidebar content.
        assert_eq!(app.state.sidebar_divider_grab_at(bar_col + 1, row), None);
        assert_eq!(app.state.sidebar_divider_grab_at(bar_col - 2, row), None);
    }

    #[test]
    fn sidebar_divider_grab_band_never_covers_the_terminal_area_or_tab_bar() {
        let app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let tab_bar = app.state.view.tab_bar_rect;
        let terminal = app.state.view.terminal_area;
        assert!(tab_bar.width > 0, "fixture should render a tab bar");
        assert_eq!(
            terminal.x,
            sidebar.x + sidebar.width,
            "fixture should butt the terminal area against the sidebar"
        );

        for row in [tab_bar.y, terminal.y, terminal.y + terminal.height - 1] {
            assert_eq!(app.state.sidebar_divider_grab_at(terminal.x, row), None);
        }
    }

    #[test]
    fn sidebar_divider_grab_band_excludes_the_collapse_toggle_and_rows_outside_the_sidebar() {
        let app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let bar_col = sidebar.x + sidebar.width - 1;
        let toggle = crate::ui::expanded_sidebar_toggle_rect(sidebar);

        // The toggle sits in the tolerance column, so the band must not eat it.
        assert_eq!(toggle.x, bar_col - 1);
        assert_eq!(app.state.sidebar_divider_grab_at(toggle.x, toggle.y), None);

        assert_eq!(
            app.state
                .sidebar_divider_grab_at(bar_col, sidebar.y + sidebar.height),
            None
        );
    }

    /// A divider-test app whose Spaces list overflows, so the sidebar scrollbar
    /// is actually drawn. The track lives in the vertical divider's tolerance
    /// column, which is what these tests pin.
    ///
    /// The space count has to beat the body in *rows*, not in cards: a card is
    /// as tall as the rows that resolve to something, and a token that comes up
    /// empty - a git branch on a detached checkout, say - takes a row away. One
    /// space per body row plus a margin overflows however few of them resolve.
    fn app_for_sidebar_scroll_test() -> crate::app::App {
        let mut app = app_for_mouse_test();
        let mut ws = Workspace::test_new("alpha");
        for idx in 0..32 {
            ws.test_add_tab(Some(&format!("tab-{idx:02}")));
        }
        let mut workspaces = vec![ws];
        for idx in 0..DIVIDER_TEST_AREA.height {
            workspaces.push(Workspace::test_new(&format!("space-{idx:02}")));
        }
        app.state.workspaces = workspaces;
        app.state.ensure_test_terminals();

        let agents = [Agent::Claude, Agent::Codex, Agent::Gemini, Agent::Pi];
        let tab_count = app.state.workspaces[0].tabs.len();
        for tab_idx in 0..tab_count {
            let pane_id = app.state.workspaces[0].tabs[tab_idx].root_pane;
            let terminal_id = app.state.workspaces[0].tabs[tab_idx].panes[&pane_id]
                .attached_terminal_id
                .clone();
            if let Some(terminal) = app.state.terminals.get_mut(&terminal_id) {
                terminal.detected_agent = Some(agents[tab_idx % agents.len()]);
            }
        }

        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        crate::ui::compute_view(&mut app.state, DIVIDER_TEST_AREA);
        app
    }

    /// The Spaces scrollbar track, asserted to be drawn.
    fn sidebar_scrollbar_track(app: &crate::app::App) -> Rect {
        let area = app.state.workspace_list_rect();
        match crate::ui::workspace_list_scrollbar_rect(&app.state, area) {
            Some(track) => track,
            // Report the geometry: the fixture overflows by row count, and a
            // card that renders shorter here than it does elsewhere is exactly
            // how this stops being true without the fixture changing.
            None => panic!(
                "fixture should overflow the Spaces list: {} spaces in {area:?}, \
                 scroll metrics {:?}",
                app.state.workspaces.len(),
                crate::ui::workspace_list_scroll_metrics(&app.state, area),
            ),
        }
    }

    #[test]
    fn the_sidebar_scrollbar_track_sits_in_the_divider_tolerance_column() {
        let app = app_for_sidebar_scroll_test();
        let sidebar = app.state.view.sidebar_rect;
        let bar_col = sidebar.x + sidebar.width - 1;
        let workspaces = sidebar_scrollbar_track(&app);

        // The panel is laid out inside `sidebar.width - 1`, so its last column
        // - where the scrollbar is drawn - is exactly the one cell of divider
        // tolerance, never the bar itself.
        assert_eq!(workspaces.x, bar_col - 1);
    }

    #[test]
    fn pressing_the_workspace_list_scrollbar_scrolls_instead_of_resizing_the_sidebar() {
        let mut app = app_for_sidebar_scroll_test();
        let width_before = app.state.sidebar_width;
        let track = sidebar_scrollbar_track(&app);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            track.x,
            track.y,
        ));

        assert!(
            matches!(
                app.state.drag.as_ref().map(|drag| &drag.target),
                Some(DragTarget::WorkspaceListScrollbar { .. })
            ),
            "a press on the Spaces scrollbar must start a scroll drag"
        );
        assert_eq!(app.state.sidebar_width, width_before);
    }

    #[test]
    fn dragging_the_workspace_list_scrollbar_thumb_moves_the_list() {
        let mut app = app_for_sidebar_scroll_test();
        let track = sidebar_scrollbar_track(&app);
        assert_eq!(app.state.workspace_scroll, 0);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            track.x,
            track.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            track.x,
            track.y + track.height - 1,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            track.x,
            track.y + track.height - 1,
        ));

        assert!(app.state.workspace_scroll > 0);
        assert_eq!(app.state.sidebar_width, 26);
    }

    #[test]
    fn the_wheel_scrolls_the_tree_from_over_its_scrollbar_column() {
        let mut app = app_for_sidebar_scroll_test();
        let workspaces = sidebar_scrollbar_track(&app);

        app.handle_mouse(mouse(
            MouseEventKind::ScrollDown,
            workspaces.x,
            workspaces.y,
        ));
        assert_eq!(app.state.workspace_scroll, 1);
        app.handle_mouse(mouse(MouseEventKind::ScrollUp, workspaces.x, workspaces.y));
        assert_eq!(app.state.workspace_scroll, 0);
    }

    #[test]
    fn the_divider_grab_band_survives_beside_a_drawn_scrollbar() {
        let mut app = app_for_sidebar_scroll_test();
        let sidebar = app.state.view.sidebar_rect;
        let bar_col = sidebar.x + sidebar.width - 1;
        let workspaces = sidebar_scrollbar_track(&app);

        // The bar itself stays grabbable on every row, including the rows a
        // scrollbar track covers.
        assert_eq!(
            app.state.sidebar_divider_grab_at(bar_col, workspaces.y),
            Some(0)
        );

        // The tolerance column still works on rows no track covers - here the
        // Spaces header, above the track.
        let header_row = app.state.workspace_list_rect().y;
        assert!(header_row < workspaces.y);
        assert_eq!(
            app.state.sidebar_divider_grab_at(bar_col - 1, header_row),
            Some(1)
        );

        // And a drag from the tolerance column still resizes the sidebar.
        drag_divider(
            &mut app,
            (bar_col - 1, header_row),
            (bar_col - 1 + 6, header_row),
        );
        recompute(&mut app);
        assert_eq!(app.state.sidebar_width, sidebar.width + 6);
    }

    #[test]
    fn pressing_inside_a_grab_band_does_not_jump_either_divider() {
        let mut app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let bar_col = sidebar.x + sidebar.width - 1;
        let width_before = app.state.sidebar_width;

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar_col - 1,
            sidebar.y + 4,
        ));
        assert_eq!(app.state.sidebar_width, width_before);
    }

    #[test]
    fn a_non_moving_press_in_the_band_is_swallowed_rather_than_falling_through() {
        // The input layer commits to a divider drag on mouse-down, so the
        // underlying control does not get the press. Nothing moves either --
        // the grab offset keeps the divider where it was -- so the cost of a
        // mis-grab is a dead click, not a layout change. Distinguishing a click
        // from a drag here would need press-and-hold disambiguation, which was
        // deliberately not taken.
        let mut app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let bar_col = sidebar.x + sidebar.width - 1;
        let card = app.state.view.workspace_card_areas[1];
        assert_ne!(app.state.selected, card.ws_idx);

        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar_col - 1,
            card.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            bar_col - 1,
            card.rect.y,
        ));

        assert_ne!(
            app.state.selected, card.ws_idx,
            "the swallowed press does not select the workspace"
        );
        assert_eq!(
            app.state.sidebar_width, sidebar.width,
            "and it does not move the divider either"
        );

        // The same press one column further into the card still selects it.
        let mut app = app_for_divider_test();
        app.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            bar_col - 2,
            card.rect.y,
        ));
        app.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            bar_col - 2,
            card.rect.y,
        ));
        assert_eq!(app.state.selected, card.ws_idx);
    }

    #[test]
    fn dragging_from_the_grab_band_keeps_the_grab_offset() {
        let mut app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let bar_col = sidebar.x + sidebar.width - 1;

        // Grabbing one cell left of the bar and moving six cells right must
        // move the bar six cells, not five.
        drag_divider(
            &mut app,
            (bar_col - 1, sidebar.y + 4),
            (bar_col + 5, sidebar.y + 4),
        );
        recompute(&mut app);

        assert_eq!(app.state.view.sidebar_rect.width, sidebar.width + 6);
        let drawn_bar = drawn_divider_cells(&mut app);
        assert_eq!(drawn_bar, Some(bar_col + 6));
    }

    #[test]
    fn dragging_sidebar_divider_widens_the_panel_and_shrinks_terminal() {
        let mut app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let divider_col = sidebar.x + sidebar.width - 1;
        let terminal_before = app.state.view.terminal_area.width;
        let ws_before = crate::ui::sidebar_content_rect(sidebar);

        drag_divider(
            &mut app,
            (divider_col, sidebar.y + 4),
            (divider_col + 6, sidebar.y + 4),
        );
        recompute(&mut app);

        let sidebar_after = app.state.view.sidebar_rect;
        let ws_after = crate::ui::sidebar_content_rect(sidebar_after);

        assert_eq!(sidebar_after.width, sidebar.width + 6);
        assert_eq!(ws_after.width, ws_before.width + 6);
        assert_eq!(app.state.view.terminal_area.width, terminal_before - 6);

        let bar_col = drawn_divider_cells(&mut app);
        assert_eq!(bar_col, Some(divider_col + 6));
    }

    #[test]
    fn dragging_sidebar_divider_left_narrows_both_panels_and_grows_terminal() {
        let mut app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let divider_col = sidebar.x + sidebar.width - 1;
        let terminal_before = app.state.view.terminal_area.width;

        drag_divider(
            &mut app,
            (divider_col, sidebar.y + 4),
            (divider_col - 4, sidebar.y + 4),
        );
        recompute(&mut app);

        assert_eq!(app.state.view.sidebar_rect.width, sidebar.width - 4);
        assert_eq!(app.state.view.terminal_area.width, terminal_before + 4);
    }

    #[test]
    fn dragging_sidebar_divider_to_either_extreme_keeps_layout_usable() {
        for target_col in [0u16, DIVIDER_TEST_AREA.width - 1] {
            let mut app = app_for_divider_test();
            let sidebar = app.state.view.sidebar_rect;
            let divider_col = sidebar.x + sidebar.width - 1;

            drag_divider(
                &mut app,
                (divider_col, sidebar.y + 4),
                (target_col, sidebar.y + 4),
            );
            recompute(&mut app);

            let sidebar_after = app.state.view.sidebar_rect;
            assert!(sidebar_after.width >= app.state.sidebar_min_width);
            assert!(sidebar_after.width <= app.state.sidebar_max_width);
            assert!(app.state.view.terminal_area.width > 0);

            let content_after = crate::ui::sidebar_content_rect(sidebar_after);
            assert!(content_after.width > 0, "the tree must stay visible");
        }
    }

    #[test]
    fn the_dragged_divider_position_survives_a_session_snapshot() {
        let mut app = app_for_divider_test();
        let sidebar = app.state.view.sidebar_rect;
        let divider_col = sidebar.x + sidebar.width - 1;

        drag_divider(
            &mut app,
            (divider_col, sidebar.y + 4),
            (divider_col + 5, sidebar.y + 4),
        );

        let snapshot = capture_snapshot(&app.state);
        assert_eq!(snapshot.sidebar_width, Some(app.state.sidebar_width));
    }

    #[test]
    fn collapsed_sidebar_has_no_draggable_dividers() {
        for mode in [
            SidebarCollapsedModeConfig::Compact,
            SidebarCollapsedModeConfig::Hidden,
        ] {
            let mut app = app_for_divider_test();
            app.state.sidebar_collapsed = true;
            app.state.sidebar_collapsed_mode = mode;
            recompute(&mut app);

            let width_before = app.state.sidebar_width;

            for col in 0..DIVIDER_TEST_AREA.width.min(8) {
                for row in 0..DIVIDER_TEST_AREA.height.min(8) {
                    assert!(!app.state.on_sidebar_divider(col, row));
                }
            }

            assert_eq!(app.state.sidebar_width, width_before);
        }
    }

    #[test]
    fn mobile_layout_has_no_draggable_dividers() {
        let mut app = app_for_divider_test();
        let mobile_area = Rect::new(0, 0, app.state.mobile_width_threshold.saturating_sub(1), 40);
        crate::ui::compute_view(&mut app.state, mobile_area);

        let width_before = app.state.sidebar_width;

        for col in 0..mobile_area.width {
            assert!(!app.state.on_sidebar_divider(col, 4));
        }

        assert_eq!(app.state.sidebar_width, width_before);
    }

    #[test]
    fn layout_is_untouched_when_no_divider_is_ever_dragged() {
        let mut before = app_for_divider_test();
        let sidebar = before.state.view.sidebar_rect;
        let terminal_area = before.state.view.terminal_area;
        let content = crate::ui::sidebar_content_rect(sidebar);
        let bar_col = drawn_divider_cells(&mut before);

        let mut after = app_for_divider_test();
        // Ordinary clicks inside the sidebar and inside the terminal area, next
        // to but not on either handle.
        after.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            sidebar.x + 2,
            sidebar.y + 2,
        ));
        after.handle_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            sidebar.x + 2,
            sidebar.y + 2,
        ));
        recompute(&mut after);

        assert_eq!(after.state.view.sidebar_rect, sidebar);
        assert_eq!(after.state.view.terminal_area, terminal_area);
        assert_eq!(
            crate::ui::sidebar_content_rect(after.state.view.sidebar_rect),
            content
        );
        assert_eq!(drawn_divider_cells(&mut after), bar_col);
        assert_eq!(
            after.state.sidebar_width_source,
            crate::app::state::SidebarWidthSource::ConfigDefault
        );
    }
}
