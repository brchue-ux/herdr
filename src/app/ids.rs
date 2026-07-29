use super::App;

impl App {
    pub(crate) fn find_pane(
        &self,
        pane_id: crate::layout::PaneId,
    ) -> Option<(usize, &crate::pane::PaneState)> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .find_map(|(ws_idx, ws)| ws.pane_state(pane_id).map(|pane| (ws_idx, pane)))
    }

    pub(super) fn public_workspace_id(&self, ws_idx: usize) -> String {
        self.state.workspaces[ws_idx].id.clone()
    }

    pub(super) fn public_tab_id(&self, ws_idx: usize, tab_idx: usize) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_number = ws.public_tab_number(tab_idx)?;
        Some(crate::workspace::public_tab_id_for_number(
            &ws.id, tab_number,
        ))
    }

    pub(super) fn public_pane_id(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<String> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_number = ws.public_pane_number(pane_id)?;
        Some(crate::workspace::public_pane_id_for_number(
            &ws.id,
            pane_number,
        ))
    }

    pub(super) fn pane_launch_env(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        extra_env: Vec<(String, String)>,
    ) -> Option<crate::pane::PaneLaunchEnv> {
        let workspace_id = self.public_workspace_id(ws_idx);
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let tab_id = self.public_tab_id(ws_idx, tab_idx)?;
        let pane_id = self.public_pane_id(ws_idx, pane_id)?;
        Some(
            crate::pane::PaneLaunchEnv::from_extra(extra_env).with_identity(
                workspace_id,
                tab_id,
                pane_id,
            ),
        )
    }

    /// Resolve a public workspace id to its current index.
    ///
    /// Resolution is by identity only. Workspace ids are stable identity that is
    /// independent of display order, so an id naming no workspace resolves to
    /// `None` and the caller surfaces `workspace_not_found`.
    ///
    /// This deliberately does not fall back to positional resolution. Accepting
    /// `w_N` and bare `N` as a 1-based position silently misdelivered requests:
    /// a caller holding a workspace id that no longer named any workspace got
    /// whichever workspace currently sat at that position, and the request
    /// reported success against the wrong target. Legacy `t_`/`p_` compound ids
    /// still resolve positionally through
    /// [`Self::parse_legacy_workspace_segment`].
    pub(super) fn parse_workspace_id(&self, id: &str) -> Option<usize> {
        self.state
            .workspaces
            .iter()
            .position(|workspace| workspace.id == id)
    }

    /// Resolve the workspace segment of a legacy `t_<ws>_<tab>` or
    /// `p_<ws>_<pane>` compound id.
    ///
    /// These shapes predate the `w1:t1` / `w1:p1` public ids and encoded the
    /// workspace as a 1-based position, so back-compat requires the positional
    /// fallback here. It is confined to these legacy shapes: a modern request
    /// that names a workspace directly must go through
    /// [`Self::parse_workspace_id`] and fail loudly on an unknown id.
    fn parse_legacy_workspace_segment(&self, raw: &str) -> Option<usize> {
        self.parse_workspace_id(raw)
            .or_else(|| {
                raw.strip_prefix("w_")?
                    .parse::<usize>()
                    .ok()?
                    .checked_sub(1)
            })
            .or_else(|| raw.parse::<usize>().ok()?.checked_sub(1))
    }

    pub(super) fn parse_tab_id(&self, id: &str) -> Option<(usize, usize)> {
        if let Some(rest) = id.strip_prefix("t_") {
            let (ws_raw, tab_raw) = rest.rsplit_once('_')?;
            let ws_idx = self.parse_legacy_workspace_segment(ws_raw)?;
            let tab_idx = tab_raw.parse::<usize>().ok()?.checked_sub(1)?;
            self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
            return Some((ws_idx, tab_idx));
        }

        let (ws_raw, tab_raw) = id.rsplit_once(':')?;
        let ws_idx = self.parse_workspace_id(ws_raw)?;
        let tab_idx = if let Some(encoded) = tab_raw.strip_prefix('t') {
            let tab_number = crate::workspace::decode_public_number(encoded)?;
            self.state
                .workspaces
                .get(ws_idx)?
                .tabs
                .iter()
                .position(|tab| tab.number == tab_number)?
        } else {
            tab_raw.parse::<usize>().ok()?.checked_sub(1)?
        };
        self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        Some((ws_idx, tab_idx))
    }

    fn resolve_raw_pane_id(&self, raw: u32) -> Option<crate::layout::PaneId> {
        if let Some(alias) = self.state.pane_id_aliases.get(&raw).copied() {
            return self.find_pane(alias).map(|_| alias);
        }
        let pane_id = crate::layout::PaneId::from_raw(raw);
        if self.find_pane(pane_id).is_some() {
            return Some(pane_id);
        }
        None
    }

    pub(super) fn parse_pane_id(&self, id: &str) -> Option<(usize, crate::layout::PaneId)> {
        if let Some(alias) = self.state.public_pane_id_aliases.get(id).copied() {
            return self.find_pane(alias).map(|(ws_idx, _)| (ws_idx, alias));
        }

        if let Some(rest) = id.strip_prefix("p_") {
            if let Some((ws_raw, pane_raw)) = rest.rsplit_once('_') {
                let ws_idx = self.parse_legacy_workspace_segment(ws_raw)?;
                let pane_id = self.resolve_raw_pane_id(pane_raw.parse::<u32>().ok()?)?;
                self.state.workspaces.get(ws_idx)?.pane_state(pane_id)?;
                return Some((ws_idx, pane_id));
            }

            let pane_id = self.resolve_raw_pane_id(rest.parse::<u32>().ok()?)?;
            return self.find_pane(pane_id).map(|(ws_idx, _)| (ws_idx, pane_id));
        }

        if let Some((ws_raw, pane_number_raw)) = id.rsplit_once(":p") {
            let ws_idx = self.parse_workspace_id(ws_raw)?;
            let pane_number = crate::workspace::decode_public_number(pane_number_raw)?;
            let ws = self.state.workspaces.get(ws_idx)?;
            let pane_id = ws
                .public_pane_numbers
                .iter()
                .find_map(|(pane_id, number)| (*number == pane_number).then_some(*pane_id))?;
            return Some((ws_idx, pane_id));
        }

        let (ws_raw, pane_number_raw) = id.rsplit_once('-')?;
        let ws_idx = self.parse_workspace_id(ws_raw)?;
        let pane_number = pane_number_raw.parse::<usize>().ok()?;
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_id = ws
            .public_pane_numbers
            .iter()
            .find_map(|(pane_id, number)| (*number == pane_number).then_some(*pane_id))?;
        Some((ws_idx, pane_id))
    }

    pub(crate) fn parse_current_public_pane_id(
        &self,
        id: &str,
    ) -> Option<(usize, crate::layout::PaneId)> {
        let (ws_idx, pane_id) = self.parse_pane_id(id)?;
        (self.public_pane_id(ws_idx, pane_id).as_deref() == Some(id)).then_some((ws_idx, pane_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, workspace::Workspace};

    fn app_with_workspaces(count: usize) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = (0..count)
            .map(|idx| Workspace::test_new(&format!("ws{idx}")))
            .collect();
        app
    }

    #[test]
    fn parse_workspace_id_resolves_public_ids() {
        let app = app_with_workspaces(3);

        for expected_idx in 0..3 {
            let id = app.state.workspaces[expected_idx].id.clone();
            assert_eq!(app.parse_workspace_id(&id), Some(expected_idx));
        }
    }

    // Positional forms are not identity. Accepting them let a request addressed
    // to a workspace that no longer exists land on whatever workspace currently
    // occupied that slot.
    #[test]
    fn parse_workspace_id_rejects_positional_forms() {
        let app = app_with_workspaces(3);

        for positional_id in ["1", "2", "3", "w_1", "w_2", "w_3", "0", "w_0"] {
            assert_eq!(
                app.parse_workspace_id(positional_id),
                None,
                "positional id {positional_id} must not resolve"
            );
        }
    }

    #[test]
    fn parse_workspace_id_rejects_id_of_removed_workspace() {
        let mut app = app_with_workspaces(3);
        let removed_id = app.state.workspaces[1].id.clone();

        app.state.workspaces.remove(1);

        assert_eq!(app.parse_workspace_id(&removed_id), None);
        // The surviving workspace that shifted into slot 1 keeps its own id.
        assert_eq!(
            app.parse_workspace_id(&app.state.workspaces[1].id.clone()),
            Some(1)
        );
    }

    // Modern compound ids embed a workspace id, so they inherit the same rule.
    #[test]
    fn modern_compound_ids_reject_positional_workspace_segments() {
        let app = app_with_workspaces(2);

        assert_eq!(app.parse_tab_id("1:t1"), None);
        assert_eq!(app.parse_tab_id("2:t1"), None);
        assert_eq!(app.parse_pane_id("1:p1"), None);
        assert_eq!(app.parse_pane_id("1-1"), None);
    }

    // The `t_<ws>_<tab>` shape predates `w1:t1` and encoded the workspace
    // positionally, so its back-compat fallback must survive.
    #[test]
    fn legacy_compound_ids_still_resolve_positional_workspace_segments() {
        let app = app_with_workspaces(2);

        assert_eq!(app.parse_tab_id("t_1_1"), Some((0, 0)));
        assert_eq!(app.parse_tab_id("t_2_1"), Some((1, 0)));
        // Out of range still fails rather than clamping onto a neighbour.
        assert_eq!(app.parse_tab_id("t_3_1"), None);
    }
}
