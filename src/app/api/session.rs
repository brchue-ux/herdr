use crate::api::schema::{ResponseResult, SessionSnapshot, SessionStatusSetParams};
use crate::app::App;

use super::responses::encode_success;

/// Ceiling on a stored session status, in characters.
///
/// The sidebar row it renders on is at most a few dozen columns wide, so this
/// is not a display budget - rendering truncates on its own. It only stops a
/// runaway publisher from parking an unbounded string in session state.
const MAX_SESSION_STATUS_CHARS: usize = 200;

/// Strip anything that would escape the one row the status is drawn on.
///
/// Control characters, and the OSC terminators in particular, would otherwise
/// let a published string move the cursor or reprogram the host terminal when
/// it reaches a client. A status that sanitizes down to nothing is the same as
/// no status at all, which is why this returns `Option`.
fn sanitize_session_status(value: &str) -> Option<String> {
    let sanitized = value
        .chars()
        .filter(|ch| !matches!(*ch, '\u{1b}' | '\u{7}' | '\u{9c}') && !ch.is_control())
        .take(MAX_SESSION_STATUS_CHARS)
        .collect::<String>()
        .trim()
        .to_string();
    (!sanitized.is_empty()).then_some(sanitized)
}

impl App {
    pub(super) fn handle_session_status_set(
        &mut self,
        id: String,
        params: SessionStatusSetParams,
    ) -> String {
        // A status that sanitizes down to nothing clears the slot rather than
        // parking an empty string there, so the row is empty in exactly one
        // state instead of two that render alike but compare differently.
        self.state.session_status = sanitize_session_status(&params.status);
        encode_success(
            id,
            ResponseResult::SessionStatus {
                status: self.state.session_status.clone(),
            },
        )
    }

    pub(super) fn handle_session_status_clear(&mut self, id: String) -> String {
        self.state.session_status = None;
        encode_success(id, ResponseResult::SessionStatus { status: None })
    }

    pub(super) fn handle_session_snapshot(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::SessionSnapshot {
                snapshot: Box::new(self.session_snapshot()),
            },
        )
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        let focused_workspace_id = self
            .state
            .active
            .map(|ws_idx| self.public_workspace_id(ws_idx));
        let focused_tab_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_tab_id(ws_idx, ws.active_tab)
        });
        let focused_pane_id = self.state.active.and_then(|ws_idx| {
            let ws = self.state.workspaces.get(ws_idx)?;
            self.public_pane_id(ws_idx, ws.focused_pane_id()?)
        });

        let mut workspaces = Vec::new();
        let mut tabs = Vec::new();
        let mut layouts = Vec::new();
        for (ws_idx, ws) in self.state.workspaces.iter().enumerate() {
            workspaces.push(self.workspace_info(ws_idx));
            for tab_idx in 0..ws.tabs.len() {
                if let Some(tab) = self.tab_info(ws_idx, tab_idx) {
                    tabs.push(tab);
                }
                if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
                    layouts.push(layout);
                }
            }
        }

        SessionSnapshot {
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
            focused_workspace_id,
            focused_tab_id,
            focused_pane_id,
            workspaces,
            tabs,
            panes: self.collect_panes_for_workspace(None).unwrap_or_default(),
            layouts,
            agents: self.collect_agent_infos(),
            agent_view: self.state.agent_views.active().map(|view| {
                crate::api::schema::AgentViewInfo {
                    source: view.source.clone(),
                    label: view.label.clone(),
                }
            }),
            status: self.state.session_status.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::api::schema::{EmptyParams, Method, ResponseResult, SuccessResponse};
    use crate::{config::Config, workspace::Workspace};

    fn app_with_two_tabs() -> crate::app::App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = crate::app::App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = Workspace::test_new("snapshot");
        workspace.test_add_tab(None);
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app
    }

    #[test]
    fn session_snapshot_bootstraps_runtime_resources() {
        let mut app = app_with_two_tabs();
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_snapshot".into(),
            method: Method::SessionSnapshot(EmptyParams::default()),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected session snapshot response");
        };
        assert_eq!(success.id, "req_snapshot");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.panes.len(), 2);
        assert_eq!(snapshot.layouts.len(), 2);
        assert_eq!(
            snapshot.focused_workspace_id.as_deref(),
            Some(snapshot.workspaces[0].workspace_id.as_str())
        );
        assert_eq!(
            snapshot.focused_tab_id.as_deref(),
            Some(snapshot.tabs[0].tab_id.as_str())
        );
        assert_eq!(
            snapshot.focused_pane_id.as_deref(),
            Some(snapshot.panes[0].pane_id.as_str())
        );
    }

    fn set_status(app: &mut crate::app::App, status: &str) -> Option<String> {
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_status_set".into(),
            method: Method::SessionStatusSet(crate::api::schema::SessionStatusSetParams {
                status: status.to_string(),
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionStatus { status } = success.result else {
            panic!("expected session status response");
        };
        status
    }

    fn snapshot_status(app: &mut crate::app::App) -> Option<String> {
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_snapshot".into(),
            method: Method::SessionSnapshot(EmptyParams::default()),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionSnapshot { snapshot } = success.result else {
            panic!("expected session snapshot response");
        };
        snapshot.status
    }

    #[test]
    fn setting_a_session_status_stores_it_and_reports_it_back() {
        let mut app = app_with_two_tabs();
        assert_eq!(snapshot_status(&mut app), None);

        assert_eq!(
            set_status(&mut app, "7d 62% · 5h 18%"),
            Some("7d 62% · 5h 18%".to_string())
        );
        assert_eq!(
            snapshot_status(&mut app),
            Some("7d 62% · 5h 18%".to_string())
        );
    }

    #[test]
    fn clearing_a_session_status_leaves_the_slot_unset() {
        let mut app = app_with_two_tabs();
        set_status(&mut app, "7d 62%");

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req_status_clear".into(),
            method: Method::SessionStatusClear(EmptyParams::default()),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::SessionStatus { status } = success.result else {
            panic!("expected session status response");
        };

        assert_eq!(status, None);
        assert_eq!(snapshot_status(&mut app), None);
    }

    /// Control characters would let a published status escape its one row and
    /// reprogram whatever terminal ends up drawing it.
    #[test]
    fn a_session_status_is_stripped_of_control_characters() {
        let mut app = app_with_two_tabs();

        assert_eq!(
            set_status(&mut app, "  7d\n62%\u{1b}]0;pwned\u{7}  "),
            Some("7d62%]0;pwned".to_string())
        );
    }

    /// A status that sanitizes down to nothing is the same fact as no status,
    /// so it clears the slot rather than parking an empty string in it.
    #[test]
    fn a_blank_session_status_clears_the_slot() {
        let mut app = app_with_two_tabs();
        set_status(&mut app, "7d 62%");

        assert_eq!(set_status(&mut app, "   \u{7} "), None);
        assert_eq!(snapshot_status(&mut app), None);
    }

    #[test]
    fn a_runaway_session_status_is_capped() {
        let mut app = app_with_two_tabs();

        let stored = set_status(&mut app, &"x".repeat(super::MAX_SESSION_STATUS_CHARS * 3));

        assert_eq!(
            stored.map(|status| status.chars().count()),
            Some(super::MAX_SESSION_STATUS_CHARS)
        );
    }
}
