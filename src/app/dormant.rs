//! Server-owned registry of dormant (minimized) tabs.
//!
//! A dormant tab is a whole [`Tab`] value — its pane tree and split geometry together —
//! removed from `Workspace.tabs` without touching the `TerminalRuntime`s its panes are
//! attached to. This is the tab-level generalization of the detach cross-workspace
//! `pane.move` already performs transiently: the tab is out of the live tree, but every
//! pane inside it keeps its already-alive terminal, so a dormant worker keeps running
//! and keeps reporting agent status while invisible.
//!
//! The registry lives on [`crate::app::state::AppState`] rather than inside any
//! `Workspace`, because a dormant tab is — by construction — not a member of any
//! workspace's live tree.

use std::collections::HashMap;
use std::time::Instant;

use crate::layout::PaneId;
use crate::terminal::TerminalId;
use crate::workspace::Tab;

/// Stable handle for a dormant tab, independent of the live `PaneId`/tab-index
/// addressing that stops resolving the moment a tab leaves `Workspace.tabs`.
///
/// Derived from the tab's `root_pane` — already documented on [`Tab`] as the
/// "identity source for this tab's pane tree" — which never changes while the tab is
/// dormant (only its parent-tree membership does).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DormantTabId(PaneId);

pub(crate) struct DormantTabEntry {
    pub(crate) tab: Tab,
    /// Public workspace id the tab was minimized from, used as the default reappear
    /// destination. Looked up by id (not index) since workspace indices can shift while
    /// a tab sits dormant.
    pub(crate) origin_workspace_id: String,
    pub(crate) dormant_at: Instant,
}

/// Data describing a tab that was just reinserted into `Workspace.tabs`, for the caller
/// to emit events from — `AppState` mutation is pure and does not emit events itself.
pub(crate) struct ReappearedTab {
    pub(crate) ws_idx: usize,
    pub(crate) tab_idx: usize,
    pub(crate) pane_ids: Vec<PaneId>,
}

#[derive(Default)]
pub(crate) struct DormantTabRegistry {
    entries: HashMap<DormantTabId, DormantTabEntry>,
}

impl DormantTabRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(
        &mut self,
        tab: Tab,
        origin_workspace_id: String,
        dormant_at: Instant,
    ) -> DormantTabId {
        let id = DormantTabId(tab.root_pane);
        self.entries.insert(
            id,
            DormantTabEntry {
                tab,
                origin_workspace_id,
                dormant_at,
            },
        );
        id
    }

    pub(crate) fn remove(&mut self, id: DormantTabId) -> Option<DormantTabEntry> {
        self.entries.remove(&id)
    }

    pub(crate) fn get(&self, id: DormantTabId) -> Option<&DormantTabEntry> {
        self.entries.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: DormantTabId) -> Option<&mut DormantTabEntry> {
        self.entries.get_mut(&id)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Finds the dormant tab containing `pane_id`, if any.
    pub(crate) fn contains_pane(&self, pane_id: PaneId) -> Option<DormantTabId> {
        self.entries
            .iter()
            .find_map(|(id, entry)| entry.tab.panes.contains_key(&pane_id).then_some(*id))
    }

    /// The terminal id attached to `pane_id` within whichever dormant tab holds it.
    pub(crate) fn terminal_id_for_pane(&self, pane_id: PaneId) -> Option<TerminalId> {
        self.entries.values().find_map(|entry| {
            entry
                .tab
                .panes
                .get(&pane_id)
                .map(|pane| pane.attached_terminal_id.clone())
        })
    }

    /// Finds which dormant tab (and which pane inside it) is attached to `terminal_id`.
    /// `pane.dormant.reappear` is keyed by `TerminalId` — the only handle that survives
    /// both minimize and a later `pane.move`-style reassignment — rather than by
    /// `DormantTabId`, so a caller never needs to have cached the tab-level handle.
    pub(crate) fn find_by_terminal_id(
        &self,
        terminal_id: &TerminalId,
    ) -> Option<(DormantTabId, PaneId)> {
        self.entries.iter().find_map(|(id, entry)| {
            entry.tab.panes.iter().find_map(|(pane_id, pane)| {
                (&pane.attached_terminal_id == terminal_id).then_some((*id, *pane_id))
            })
        })
    }
}
