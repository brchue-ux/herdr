//! Opening, scrolling and closing the worker-summary view.
//!
//! The view is opened from the badge on a second mate's row, so the only thing
//! that has to be captured is the mate's tree handle; the list itself is
//! recomputed from live state every frame. See
//! [`crate::app::worker_summary`] for the scoping rule and
//! [`crate::ui::worker_summary`] for the panel.

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::state::{AppState, Mode, WorkerSummariesState};
use crate::app::App;

use super::modal::leave_modal;

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x + rect.width
        && row >= rect.y
        && row < rect.y + rect.height
}

impl AppState {
    /// The second mate whose summary badge covers this cell.
    ///
    /// Recomputed from the current card list rather than cached, for the same
    /// reason [`Self::workspace_group_chevron_at`] is: the cards move whenever
    /// the tree re-sorts, and a stale hit rect would open the wrong mate.
    pub(crate) fn worker_summary_badge_at(&self, col: u16, row: u16) -> Option<String> {
        if self.sidebar_collapsed {
            return None;
        }
        let cards = if self.view.workspace_card_areas.is_empty() {
            crate::ui::compute_workspace_card_areas(self, self.view.sidebar_rect)
        } else {
            self.view.workspace_card_areas.clone()
        };
        let entries = crate::ui::workspace_list_entries(self);
        let agents = crate::ui::sidebar_agent_entries(self);
        cards.iter().find_map(|card| {
            let (owner, count) = crate::ui::worker_summary_badge(self, &entries, &agents, card)?;
            rect_contains(crate::ui::worker_summary_badge_rect(card, count), col, row)
                .then_some(owner)
        })
    }

    pub(crate) fn open_worker_summaries(&mut self, owner: String) {
        self.worker_summaries = Some(WorkerSummariesState { owner, scroll: 0 });
        self.mode = Mode::WorkerSummaries;
    }

    fn scroll_worker_summaries(&mut self, delta: isize) {
        let visible = crate::ui::worker_summaries_visible_rows(self.screen_rect());
        let total = crate::ui::worker_summaries_total_rows(self, self.screen_rect());
        let max_scroll = total.saturating_sub(visible);
        if let Some(open) = self.worker_summaries.as_mut() {
            let next = open.scroll as isize + delta;
            open.scroll = next.clamp(0, max_scroll as isize) as usize;
        }
    }
}

impl App {
    pub(crate) fn handle_worker_summaries_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => self.close_worker_summaries(),
            KeyCode::Up | KeyCode::Char('k') => self.state.scroll_worker_summaries(-1),
            KeyCode::Down | KeyCode::Char('j') => self.state.scroll_worker_summaries(1),
            KeyCode::PageUp => self.state.scroll_worker_summaries(-8),
            KeyCode::PageDown => self.state.scroll_worker_summaries(8),
            KeyCode::Home => self.state.scroll_worker_summaries(isize::MIN / 2),
            KeyCode::End => self.state.scroll_worker_summaries(isize::MAX / 2),
            _ => {}
        }
    }

    pub(crate) fn close_worker_summaries(&mut self) {
        self.state.worker_summaries = None;
        leave_modal(&mut self.state);
    }

    /// Returns whether the event was consumed by the open summary view.
    pub(crate) fn handle_worker_summaries_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.state.mode != Mode::WorkerSummaries {
            return false;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.state.scroll_worker_summaries(-3),
            MouseEventKind::ScrollDown => self.state.scroll_worker_summaries(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let screen = self.state.screen_rect();
                let Some(popup) = crate::ui::worker_summaries_popup_rect(screen) else {
                    self.close_worker_summaries();
                    return true;
                };
                let inner = crate::ui::worker_summaries_inner_rect(popup);
                let actions = crate::ui::worker_summaries_action_row(inner);
                let close = crate::ui::worker_summaries_close_button_rect(actions);
                // Clicking the button closes, and so does clicking away from
                // the panel: a summary is something you glance at, so getting
                // out of it must never need aim.
                if rect_contains(close, mouse.column, mouse.row)
                    || !rect_contains(popup, mouse.column, mouse.row)
                {
                    self.close_worker_summaries();
                }
            }
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::input::app_for_mouse_test;

    #[test]
    fn opening_captures_the_mate_and_starts_at_the_top() {
        let mut app = app_for_mouse_test();
        app.state.open_worker_summaries("mate-alpha".into());
        assert_eq!(app.state.mode, Mode::WorkerSummaries);
        let open = app.state.worker_summaries.as_ref().expect("view is open");
        assert_eq!(open.owner, "mate-alpha");
        assert_eq!(open.scroll, 0);
    }

    #[test]
    fn esc_closes_and_drops_the_captured_mate() {
        let mut app = app_for_mouse_test();
        app.state.open_worker_summaries("mate-alpha".into());
        app.handle_worker_summaries_key(KeyEvent::from(KeyCode::Esc));
        assert_ne!(app.state.mode, Mode::WorkerSummaries);
        assert!(app.state.worker_summaries.is_none());
    }

    #[test]
    fn scrolling_never_leaves_the_body() {
        let mut app = app_for_mouse_test();
        app.state.open_worker_summaries("mate-alpha".into());
        // Nothing to scroll: every direction has to stay pinned at the top
        // rather than underflowing the offset.
        app.state.scroll_worker_summaries(-5);
        assert_eq!(app.state.worker_summaries.as_ref().unwrap().scroll, 0);
        app.state.scroll_worker_summaries(500);
        assert_eq!(app.state.worker_summaries.as_ref().unwrap().scroll, 0);
    }
}
