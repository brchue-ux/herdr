use std::path::PathBuf;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, WorkspaceCreateParams,
    WorkspaceMoveBlockParams, WorkspaceMoveParams, WorkspaceRenameParams,
    WorkspaceReportMetadataParams, WorkspaceReportSignalParams, WorkspaceSignalKind,
    WorkspaceTarget,
};
use crate::app::relation_signal::{CarrierId, RelationSignalKind};
use crate::app::App;

use super::super::api_helpers::{normalize_metadata_source, normalize_metadata_ttl};
use super::responses::{encode_error, encode_success};

impl App {
    pub(super) fn handle_workspace_list(&mut self, id: String) -> String {
        encode_success(
            id,
            ResponseResult::WorkspaceList {
                workspaces: self.workspace_list_info(),
            },
        )
    }

    pub(super) fn handle_workspace_get(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        let Some(_) = self.state.workspaces.get(index) else {
            return workspace_not_found(id, &target.workspace_id);
        };

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_create(
        &mut self,
        id: String,
        params: WorkspaceCreateParams,
    ) -> String {
        let cwd = params.cwd.map(PathBuf::from).unwrap_or_else(|| {
            let follow_cwd = self.workspace_creation_source().and_then(|ws_idx| {
                self.focused_pane_cwd_in_workspace(ws_idx)
                    .or_else(|| self.seed_cwd_from_workspace(ws_idx))
            });
            self.resolve_new_terminal_cwd(follow_cwd)
        });
        let extra_env = match super::env::normalize_launch_env(params.env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        // Resolved before the new workspace exists, so the caller is looked up
        // in the topology it was actually standing in.
        let origin = self.resolve_pane_origin(params.caller_pane_id.as_deref());
        match self.create_workspace_with_launch_env(cwd, params.focus, extra_env) {
            Ok(index) => {
                self.record_workspace_root_pane_origin(index, origin);
                if let Some(label) = params.label {
                    if let Some(workspace) = self.state.workspaces.get_mut(index) {
                        workspace.set_custom_name(label);
                        crate::logging::workspace_renamed(&workspace.id);
                    }
                }
                self.emit_workspace_open_events(index);
                encode_success(
                    id,
                    self.workspace_created_result(index)
                        .expect("new workspace should produce a complete create response"),
                )
            }
            Err(err) => encode_error(id, "workspace_create_failed", err.to_string()),
        }
    }

    /// Stamp a freshly created workspace's first pane with who asked for it.
    ///
    /// The origin never yields an owner here — the caller is by construction in
    /// some other Space — but recording it is what makes "this Space was spun
    /// up by that pane" readable instead of merely absent, which is the only
    /// way to tell a cross-Space creation apart from a call that forgot to say
    /// who it was.
    fn record_workspace_root_pane_origin(
        &mut self,
        ws_idx: usize,
        origin: Option<crate::api::schema::PaneOrigin>,
    ) {
        let Some(origin) = origin else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.first())
            .and_then(|tab| tab.panes.get(&tab.root_pane))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            terminal.created_by = Some(origin);
        }
    }

    pub(super) fn handle_workspace_focus(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &target.workspace_id);
        }
        self.state.switch_workspace(index);

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_rename(
        &mut self,
        id: String,
        params: WorkspaceRenameParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        let Some(ws) = self.state.workspaces.get_mut(index) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        ws.set_custom_name(params.label.clone());
        crate::logging::workspace_renamed(&ws.id);
        self.schedule_session_save();
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceRenamed,
            data: EventData::WorkspaceRenamed {
                workspace_id: self.public_workspace_id(index),
                label: params.label,
            },
        });

        encode_success(
            id,
            ResponseResult::WorkspaceInfo {
                workspace: self.workspace_info(index),
            },
        )
    }

    pub(super) fn handle_workspace_move(
        &mut self,
        id: String,
        params: WorkspaceMoveParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &params.workspace_id);
        }
        if params.insert_index > self.state.workspaces.len() {
            return encode_error(
                id,
                "workspace_move_failed",
                format!("insert_index {} is out of bounds", params.insert_index),
            );
        }

        let workspace_id = self.public_workspace_id(index);
        let insert_index = params.insert_index;
        let moved = self.state.move_workspace(index, insert_index);
        let workspaces = self.workspace_list_info();
        if moved {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceMoved,
                data: EventData::WorkspaceMoved {
                    workspace_id,
                    insert_index,
                    workspaces: workspaces.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::WorkspaceList { workspaces })
    }

    pub(super) fn handle_workspace_move_block(
        &mut self,
        id: String,
        params: WorkspaceMoveBlockParams,
    ) -> String {
        if params.workspace_ids.is_empty() {
            return encode_error(
                id,
                "workspace_move_block_failed",
                "workspace_ids must not be empty",
            );
        }

        let mut workspace_ids = Vec::with_capacity(params.workspace_ids.len());
        let mut seen_ids = std::collections::HashSet::new();
        for requested_id in &params.workspace_ids {
            let Some(index) = self.parse_workspace_id(requested_id) else {
                return workspace_not_found(id, requested_id);
            };
            let Some(workspace) = self.state.workspaces.get(index) else {
                return workspace_not_found(id, requested_id);
            };
            if !seen_ids.insert(workspace.id.clone()) {
                return encode_error(
                    id,
                    "workspace_move_block_failed",
                    format!("workspace {requested_id} appears more than once"),
                );
            }
            workspace_ids.push(workspace.id.clone());
        }

        let before_workspace_id = match params.before_workspace_id {
            Some(requested_id) => {
                let Some(index) = self.parse_workspace_id(&requested_id) else {
                    return workspace_not_found(id, &requested_id);
                };
                let Some(workspace) = self.state.workspaces.get(index) else {
                    return workspace_not_found(id, &requested_id);
                };
                if seen_ids.contains(&workspace.id) {
                    return encode_error(
                        id,
                        "workspace_move_block_failed",
                        "before_workspace_id must not be part of workspace_ids",
                    );
                }
                Some(workspace.id.clone())
            }
            None => None,
        };

        let moved = self
            .state
            .move_workspace_block(&workspace_ids, before_workspace_id.as_deref());
        let workspaces = self.workspace_list_info();
        if moved {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceReordered,
                data: EventData::WorkspaceReordered {
                    workspace_ids,
                    before_workspace_id,
                    workspaces: workspaces.clone(),
                },
            });
        }

        encode_success(id, ResponseResult::WorkspaceList { workspaces })
    }

    pub(super) fn handle_workspace_report_metadata(
        &mut self,
        id: String,
        params: WorkspaceReportMetadataParams,
    ) -> String {
        let Some(index) = self.parse_workspace_id(&params.workspace_id) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        let ttl = match normalize_metadata_ttl(params.ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return encode_error(id, "invalid_metadata_ttl", message),
        };
        // `clear_all_tokens` is a complete request on its own: revoking
        // everything must not require naming a token to keep.
        let tokens = if params.clear_all_tokens && params.tokens.is_empty() {
            std::collections::HashMap::new()
        } else {
            match super::super::api_helpers::normalize_metadata_tokens(params.tokens) {
                Ok(tokens) => tokens,
                Err(message) => return encode_error(id, "invalid_metadata_token", message),
            }
        };
        let Some(workspace) = self.state.workspaces.get_mut(index) else {
            return workspace_not_found(id, &params.workspace_id);
        };
        if !crate::metadata_tokens::sequence_is_fresh(
            &workspace.metadata_token_sequences,
            &source,
            params.seq,
        ) {
            return encode_success(id, ResponseResult::Ok {});
        }
        // A bulk clear replaces the token set, so the limit applies to what the
        // patch sets rather than to what it is layered on top of.
        let key_count_after = if params.clear_all_tokens {
            tokens.values().filter(|value| value.is_some()).count()
        } else {
            workspace.metadata_tokens.key_count_after_patch(&tokens)
        };
        if key_count_after > super::super::api_helpers::MAX_METADATA_TOKEN_KEYS_PER_RESOURCE {
            return encode_error(
                id,
                "metadata_token_limit",
                format!(
                    "workspace metadata may contain at most {} tokens",
                    super::super::api_helpers::MAX_METADATA_TOKEN_KEYS_PER_RESOURCE
                ),
            );
        }
        match crate::metadata_tokens::accept_sequence(
            &mut workspace.metadata_token_sequences,
            &source,
            params.seq,
        ) {
            Ok(true) => {}
            Ok(false) => return encode_success(id, ResponseResult::Ok {}),
            Err(()) => {
                return encode_error(
                    id,
                    "metadata_sequence_source_limit",
                    format!(
                        "workspace metadata may track at most {} sequenced sources",
                        crate::metadata_tokens::MAX_SEQUENCE_SOURCES
                    ),
                );
            }
        }
        let cleared = params.clear_all_tokens && workspace.metadata_tokens.clear();
        let changed = workspace
            .metadata_tokens
            .patch(tokens, ttl, std::time::Instant::now())
            || cleared;
        if changed {
            self.sync_agent_metadata_deadline();
            self.emit_workspace_token_updated(index);
            // Published tokens are durable now, so a token that changed and was
            // never saved would come back as its previous value on the next
            // restart.
            self.state.mark_session_dirty();
        }
        encode_success(id, ResponseResult::Ok {})
    }

    /// Records a transient relation between two workspaces.
    ///
    /// Every way this can come to nothing answers success. An unknown workspace
    /// id, a workspace that has since closed, a replayed `seq`, a source that
    /// has exhausted the sequence table — none of them are the publisher's
    /// problem to handle, because none of them mean anything went wrong with the
    /// fleet. Only a malformed report is an error.
    ///
    /// Accepting a signal deliberately does **not** request a repaint. It arms
    /// the loop's own clock, and that clock is what paints. A publisher
    /// reporting a thousand signals a second therefore still cannot make Herdr
    /// draw more often than it already would.
    pub(super) fn handle_workspace_report_signal(
        &mut self,
        id: String,
        params: WorkspaceReportSignalParams,
    ) -> String {
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        let ttl = match normalize_metadata_ttl(params.ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return encode_error(id, "invalid_metadata_ttl", message),
        };

        // Which end of the relation carries the signal is Herdr's decision, not
        // the publisher's: work arriving lands on the receiver's row, work
        // finishing leaves from the row that finished it.
        let (kind, carrier_id) = match params.kind {
            WorkspaceSignalKind::Transfer => (
                RelationSignalKind::Transfer,
                params.to_workspace_id.as_deref(),
            ),
            WorkspaceSignalKind::Completed => (
                RelationSignalKind::Completed,
                params.from_workspace_id.as_deref(),
            ),
            WorkspaceSignalKind::Failed => (
                RelationSignalKind::Failed,
                params.from_workspace_id.as_deref(),
            ),
            WorkspaceSignalKind::Idle => (
                RelationSignalKind::Idle,
                params.from_workspace_id.as_deref(),
            ),
        };
        let Some(carrier_id) = carrier_id else {
            return encode_error(
                id,
                "invalid_signal_target",
                match params.kind {
                    WorkspaceSignalKind::Transfer => {
                        "workspace signal kind transfer requires to_workspace_id"
                    }
                    WorkspaceSignalKind::Completed => {
                        "workspace signal kind completed requires from_workspace_id"
                    }
                    WorkspaceSignalKind::Failed => {
                        "workspace signal kind failed requires from_workspace_id"
                    }
                    WorkspaceSignalKind::Idle => {
                        "workspace signal kind idle requires from_workspace_id"
                    }
                },
            );
        };

        // The id names either end of the relation, and either end can be a
        // Space or a worker's own pane — workers are panes, so this is what
        // lets a mate->worker connector carry a signal at all. Workspace
        // resolution is tried first since a canonical workspace id and a
        // canonical pane id never collide.
        let Some(carrier) = self
            .parse_workspace_id(carrier_id)
            .and_then(|index| self.state.workspaces.get(index))
            .map(|workspace| CarrierId::Workspace(workspace.id.clone()))
            .or_else(|| {
                let (ws_idx, pane_id) = self.parse_pane_id(carrier_id)?;
                Some(CarrierId::Pane(self.public_pane_id(ws_idx, pane_id)?))
            })
        else {
            return encode_success(id, ResponseResult::Ok {});
        };

        // Resolved before the carrier is moved into the signal, and only for
        // the one kind that means it. `failed` is a different outcome and
        // `idle` is not an outcome at all; neither is what the captain asked to
        // be able to read off a resting card.
        let absorbed_by = (kind == RelationSignalKind::Completed)
            .then(|| self.absorbing_owner(&carrier))
            .flatten();

        let accepted = self.state.relation_signals.accept(
            &source,
            params.seq,
            kind,
            carrier,
            ttl,
            std::time::Instant::now(),
        );

        // A finished worker is one the mate above it has taken back, and the
        // residue on that mate's card is what is left once this charge has
        // arrived and expired.
        //
        // Counted for every report that is a *distinct event*, which is not the
        // same set as the reports that get an animation:
        //
        // - `Coalesced` still counts. That rule bounds how many frames a
        //   publisher can cost by refusing to restart a row's travel, and a
        //   ring costs no frames. Two workers finishing in the same breath are
        //   two workers.
        // - `StaleSequence` must not. `seq` is what makes reporting idempotent
        //   — "a report at or below the last accepted value is ignored, so
        //   retries are safe" — and a retry that silently added a ring would
        //   take that guarantee away from the one part of this report that is
        //   not transient.
        //
        // No repaint is requested, for the same reason `accept` asks for none:
        // the signal this rode in on arms the loop's own clock, and that clock
        // paints the card the ring is on. The ring is in the card's signature,
        // so the rebuild happens on the next frame the charge was already
        // going to cost.
        if !matches!(
            accepted,
            Err(crate::app::relation_signal::SignalDropped::StaleSequence)
        ) {
            if let Some(owner) = absorbed_by {
                self.state.residue.absorb(&owner);
            }
        }
        encode_success(id, ResponseResult::Ok {})
    }

    /// Who takes the credit when `carrier` reports that it finished.
    ///
    /// The row *above* the one that finished, resolved through the exact rule
    /// [`crate::app::agent_tree::resolve_owner`] already draws the tree with —
    /// a published `owner` token, or the structural edge — so a ring can only
    /// ever land on a mate the panel is really showing that worker under. A
    /// publisher does not have to name the absorber a second time, and cannot
    /// name one the tree disagrees with.
    ///
    /// Returns a tree *name*, which is what [`crate::app::residue`] is keyed
    /// by; see its module doc for why an id would be the wrong handle.
    fn absorbing_owner(&self, carrier: &CarrierId) -> Option<String> {
        match carrier {
            // A mate's own Space says who owns it with the same `owner` token
            // a worker's pane uses, so a second mate finishing under a first
            // mate rings the first mate's card by the same rule.
            CarrierId::Workspace(workspace_id) => {
                let index = self
                    .state
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == *workspace_id)?;
                crate::ui::sidebar::space_owner(&self.state, index)
            }
            CarrierId::Pane(public_pane_id) => {
                let (ws_idx, pane_id) = self.parse_pane_id(public_pane_id)?;
                let workspace = self.state.workspaces.get(ws_idx)?;
                let pane = workspace.pane_state(pane_id)?;
                let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
                crate::app::agent_tree::resolve_owner(
                    terminal
                        .metadata_tokens
                        .values()
                        .get(crate::app::agent_tree::OWNER_TOKEN)
                        .map(String::as_str),
                    terminal.created_by.as_ref(),
                    &workspace.id,
                    crate::ui::sidebar::space_tree_name(&self.state, ws_idx).as_deref(),
                )
            }
        }
    }

    pub(super) fn handle_workspace_close(&mut self, id: String, target: WorkspaceTarget) -> String {
        let Some(index) = self.parse_workspace_id(&target.workspace_id) else {
            return workspace_not_found(id, &target.workspace_id);
        };
        if self.state.workspaces.get(index).is_none() {
            return workspace_not_found(id, &target.workspace_id);
        }
        let workspace_id = self.public_workspace_id(index);
        let workspace = self.workspace_info(index);
        let pane_ids = self
            .state
            .workspaces
            .get(index)
            .map(|ws| {
                ws.tabs
                    .iter()
                    .flat_map(|tab| tab.layout.pane_ids())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.state.selected = index;
        self.state.close_selected_workspace();
        // A mate that is gone keeps no residue; see `AppState::prune_residue`.
        self.state.prune_residue();
        self.state.remove_plugin_pane_records(pane_ids);
        self.shutdown_detached_terminal_runtimes();
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceClosed,
            data: EventData::WorkspaceClosed {
                workspace_id,
                workspace: Some(workspace),
            },
        });

        encode_success(id, ResponseResult::Ok {})
    }

    fn workspace_list_info(&self) -> Vec<crate::api::schema::WorkspaceInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .map(|(idx, _)| self.workspace_info(idx))
            .collect()
    }
}

fn workspace_not_found(id: String, workspace_id: &str) -> String {
    encode_error(
        id,
        "workspace_not_found",
        format!("workspace {workspace_id} not found"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::schema::SuccessResponse, config::Config, workspace::Workspace};

    // `new_cwd = follow` must anchor on the focused pane for every creation
    // surface. Splits and tabs already do; a new workspace must follow the
    // focused pane too, not the source workspace's first-tab root pane.
    #[tokio::test]
    async fn workspace_create_follows_focused_pane_cwd_not_first_tab_root() {
        use super::super::test_support::{exiting_test_command, shutdown_test_runtimes};
        use crate::config::ShellModeConfig;

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.default_shell = exiting_test_command().into();
        app.state.shell_mode = ShellModeConfig::NonLogin;
        app.state.workspaces = vec![Workspace::test_new("spaces")];
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.ensure_test_terminals();

        // Second tab becomes the focused pane, away from tab 1's root pane.
        let response = app.handle_tab_create(
            "tab".into(),
            crate::api::schema::TabCreateParams {
                workspace_id: None,
                cwd: None,
                focus: true,
                label: None,
                env: Default::default(),
                caller_pane_id: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        // Drop runtimes so cwd resolution deterministically uses cached state.
        shutdown_test_runtimes(&mut app);

        let focused_cwd = std::env::temp_dir().join(format!(
            "herdr-ws-follow-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&focused_cwd).unwrap();
        let ws = &app.state.workspaces[0];
        let root_cwd = ws.identity_cwd.clone();
        let focused_pane = ws.focused_pane_id().unwrap();
        assert_ne!(focused_pane, ws.tabs[0].root_pane);
        let terminal_id = ws.terminal_id(focused_pane).cloned().unwrap();
        app.state.terminals.get_mut(&terminal_id).unwrap().cwd = focused_cwd.clone();

        let response = app.handle_workspace_create(
            "req".into(),
            WorkspaceCreateParams {
                cwd: None,
                focus: false,
                label: None,
                env: Default::default(),
                caller_pane_id: None,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::WorkspaceCreated { .. }
        ));
        let created_cwd = &app.state.workspaces[1].identity_cwd;
        assert_eq!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&focused_cwd)
        );
        assert_ne!(
            crate::worktree::canonical_or_original(created_cwd),
            crate::worktree::canonical_or_original(&root_cwd)
        );
        shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(&focused_cwd);
    }

    fn app_with_linked_worktree() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("issue")];
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app
    }

    #[test]
    fn api_workspace_close_closes_linked_worktree_workspace_only() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_workspace_close(
            "req".into(),
            WorkspaceTarget {
                workspace_id: app.state.workspaces[0].id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn api_workspace_close_event_includes_final_worktree_snapshot() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = app_with_linked_worktree().state.workspaces;
        let workspace_id = app.state.workspaces[0].id.clone();

        let response = app.handle_workspace_close(
            "req".into(),
            WorkspaceTarget {
                workspace_id: workspace_id.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceClosed {
                    workspace_id: closed_id,
                    workspace: Some(workspace),
                } if closed_id == &workspace_id
                    && workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
            )
        }));
    }

    #[test]
    fn workspace_metadata_tokens_patch_clear_and_emit_snapshot() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one")];
        let workspace_id = app.public_workspace_id(0);

        for (tokens, expected) in [
            (
                std::collections::HashMap::from([
                    ("summary".into(), Some("reviewing auth".into())),
                    ("jj_status".into(), Some("2 changes".into())),
                ]),
                std::collections::HashMap::from([
                    ("summary".into(), "reviewing auth".into()),
                    ("jj_status".into(), "2 changes".into()),
                ]),
            ),
            (
                std::collections::HashMap::from([
                    ("summary".into(), Some("done".into())),
                    ("jj_status".into(), None),
                ]),
                std::collections::HashMap::from([("summary".into(), "done".into())]),
            ),
        ] {
            let response = app.handle_api_request(crate::api::schema::Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorkspaceReportMetadata(
                    WorkspaceReportMetadataParams {
                        workspace_id: workspace_id.clone(),
                        source: "user:test".into(),
                        tokens,
                        clear_all_tokens: false,
                        seq: None,
                        ttl_ms: None,
                    },
                ),
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(success.result, ResponseResult::Ok {});
            assert_eq!(app.workspace_info(0).tokens, expected);
        }

        assert!(event_hub.events_after(0).iter().any(|(_, event)| matches!(
            &event.data,
            EventData::WorkspaceMetadataUpdated { workspace }
                if workspace.tokens.get("summary").map(String::as_str) == Some("done")
                    && !workspace.tokens.contains_key("jj_status")
        )));
    }

    /// The revoke path, workspace side. A token published with no TTL now
    /// outlives its publisher and the server, so clearing it must not require
    /// knowing its key.
    #[test]
    fn workspace_report_metadata_clear_all_tokens_revokes_tokens_that_never_expire() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("one")];
        let workspace_id = app.public_workspace_id(0);

        let response = app.handle_workspace_report_metadata(
            "publish".into(),
            WorkspaceReportMetadataParams {
                workspace_id: workspace_id.clone(),
                source: "user:test".into(),
                tokens: std::collections::HashMap::from([
                    ("jj_status".into(), Some("clean".into())),
                    ("summary".into(), Some("review".into())),
                ]),
                clear_all_tokens: false,
                seq: None,
                ttl_ms: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(app.state.workspaces[0]
            .metadata_tokens
            .next_expiry()
            .is_none());

        // Naming no keys, from a source that never published any of them.
        app.state.session_dirty = false;
        let response = app.handle_workspace_report_metadata(
            "revoke".into(),
            WorkspaceReportMetadataParams {
                workspace_id,
                source: "user:operator".into(),
                tokens: std::collections::HashMap::new(),
                clear_all_tokens: true,
                seq: None,
                ttl_ms: None,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});

        assert!(app.state.workspaces[0].metadata_tokens.values().is_empty());
        assert!(app.state.session_dirty, "the revoke has to reach disk");
    }

    #[test]
    fn workspace_token_ttl_expires_through_runtime_and_emits_update() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one")];
        let workspace_id = app.public_workspace_id(0);
        let response = app.handle_workspace_report_metadata(
            "req".into(),
            WorkspaceReportMetadataParams {
                workspace_id,
                source: "user:test".into(),
                tokens: std::collections::HashMap::from([(
                    "summary".into(),
                    Some("temporary".into()),
                )]),
                clear_all_tokens: false,
                seq: None,
                ttl_ms: Some(1),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let deadline = app.agent_metadata_deadline.expect("token deadline");

        app.expire_metadata_at(deadline, deadline);

        assert!(app.workspace_info(0).tokens.is_empty());
        assert!(event_hub.events_after(0).iter().any(|(_, event)| matches!(
            &event.data,
            EventData::WorkspaceMetadataUpdated { workspace } if workspace.tokens.is_empty()
        )));
    }

    fn app_with_two_workspaces() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("first"), Workspace::test_new("second")];
        app
    }

    fn report_signal(app: &mut App, params: WorkspaceReportSignalParams) -> String {
        app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorkspaceReportSignal(params),
        })
    }

    fn signal_params(kind: WorkspaceSignalKind) -> WorkspaceReportSignalParams {
        WorkspaceReportSignalParams {
            source: "firstmate".into(),
            kind,
            from_workspace_id: None,
            to_workspace_id: None,
            seq: None,
            ttl_ms: None,
        }
    }

    #[test]
    fn a_transfer_lands_on_the_receiving_workspace_and_a_completion_on_the_finishing_one() {
        for (kind, carrier_idx) in [
            (WorkspaceSignalKind::Transfer, 1),
            (WorkspaceSignalKind::Completed, 0),
        ] {
            let mut app = app_with_two_workspaces();
            let (from, to) = (app.public_workspace_id(0), app.public_workspace_id(1));
            let response = report_signal(
                &mut app,
                WorkspaceReportSignalParams {
                    from_workspace_id: Some(from),
                    to_workspace_id: Some(to),
                    ..signal_params(kind)
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(success.result, ResponseResult::Ok {});

            let carrier = app.state.workspaces[carrier_idx].id.clone();
            assert!(app
                .state
                .relation_signals
                .phase_for_workspace(&carrier)
                .is_some());
            let other = app.state.workspaces[1 - carrier_idx].id.clone();
            assert!(app
                .state
                .relation_signals
                .phase_for_workspace(&other)
                .is_none());
        }
    }

    #[test]
    fn a_transfer_can_land_on_a_worker_pane_rather_than_a_workspace() {
        // Workers are panes: this is the case the whole carrier extension is
        // for. Reporting a transfer to a pane id must resolve to a pane
        // carrier, not fall through to nothing and not get mistaken for a
        // workspace with a similarly shaped id.
        let mut app = app_with_two_workspaces();
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[1].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(1, pane_id).unwrap();

        let response = report_signal(
            &mut app,
            WorkspaceReportSignalParams {
                to_workspace_id: Some(public_pane_id.clone()),
                ..signal_params(WorkspaceSignalKind::Transfer)
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});

        assert!(app
            .state
            .relation_signals
            .phase_for_pane(&public_pane_id)
            .is_some());
        // The pane's own workspace must not answer for it: the two carriers
        // are different rows even though one nests under the other.
        let workspace_id = app.state.workspaces[1].id.clone();
        assert!(app
            .state
            .relation_signals
            .phase_for_workspace(&workspace_id)
            .is_none());
    }

    #[test]
    fn a_workspace_that_no_longer_exists_answers_success_and_records_nothing() {
        let mut app = app_with_two_workspaces();
        let response = report_signal(
            &mut app,
            WorkspaceReportSignalParams {
                to_workspace_id: Some("w_9999".into()),
                ..signal_params(WorkspaceSignalKind::Transfer)
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            success.result,
            ResponseResult::Ok {},
            "a mate that has already closed must not be able to fail its caller"
        );
        assert!(app.state.relation_signals.is_empty());
    }

    #[test]
    fn a_replayed_sequence_is_accepted_without_restarting_the_row() {
        let mut app = app_with_two_workspaces();
        let to = app.public_workspace_id(1);
        for _ in 0..3 {
            let response = report_signal(
                &mut app,
                WorkspaceReportSignalParams {
                    to_workspace_id: Some(to.clone()),
                    seq: Some(7),
                    ..signal_params(WorkspaceSignalKind::Transfer)
                },
            );
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(success.result, ResponseResult::Ok {});
        }
        assert_eq!(app.state.relation_signals.iter().count(), 1);

        // A report behind the watermark is ignored outright, so a retry that
        // races a newer one cannot rewind the row.
        let response = report_signal(
            &mut app,
            WorkspaceReportSignalParams {
                from_workspace_id: Some(to.clone()),
                seq: Some(6),
                ..signal_params(WorkspaceSignalKind::Completed)
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        let carrier = app.state.workspaces[1].id.clone();
        assert_eq!(
            app.state
                .relation_signals
                .phase_for_workspace(&carrier)
                .map(|phase| phase.direction),
            Some(crate::app::relation_signal::SignalDirection::Toward),
        );
    }

    #[test]
    fn a_signal_kind_without_the_workspace_it_needs_is_the_one_real_error() {
        for kind in [
            WorkspaceSignalKind::Transfer,
            WorkspaceSignalKind::Completed,
        ] {
            let mut app = app_with_two_workspaces();
            let response = report_signal(&mut app, signal_params(kind));
            let error: serde_json::Value = serde_json::from_str(&response).unwrap();
            assert_eq!(error["error"]["code"], "invalid_signal_target");
        }
    }

    #[test]
    fn a_publisher_lifetime_is_clamped_rather_than_trusted() {
        let mut app = app_with_two_workspaces();
        let to = app.public_workspace_id(1);
        let response = report_signal(
            &mut app,
            WorkspaceReportSignalParams {
                to_workspace_id: Some(to),
                ttl_ms: Some(86_400_000),
                ..signal_params(WorkspaceSignalKind::Transfer)
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});

        let carrier = app.state.workspaces[1].id.clone();
        app.state
            .relation_signals
            .advance(std::time::Instant::now() + crate::app::relation_signal::MAX_SIGNAL_TTL);
        assert_eq!(
            app.state.relation_signals.phase_for_workspace(&carrier),
            None,
            "a day-long ttl must not pin a row for a day"
        );
    }

    #[test]
    fn api_workspace_move_reorders_workspaces() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![
            Workspace::test_new("one"),
            Workspace::test_new("two"),
            Workspace::test_new("three"),
        ];
        app.state.active = Some(0);
        app.state.selected = 0;
        let moved_id = app.public_workspace_id(0);

        let response = app.handle_workspace_move(
            "req".into(),
            WorkspaceMoveParams {
                workspace_id: moved_id.clone(),
                insert_index: 3,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces[2].workspace_id, moved_id);
        assert_eq!(app.state.workspaces[2].display_name(), "one");
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceMoved {
                    workspace_id,
                    insert_index: 3,
                    workspaces,
                } if workspace_id == &moved_id
                    && workspaces[2].workspace_id == moved_id
            )
        }));
    }

    #[test]
    fn api_workspace_move_block_reorders_atomically() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![
            Workspace::test_new("child"),
            Workspace::test_new("normal"),
            Workspace::test_new("parent"),
            Workspace::test_new("tail"),
        ];
        let parent_id = app.public_workspace_id(2);
        let child_id = app.public_workspace_id(0);
        let tail_id = app.public_workspace_id(3);

        let response = app.handle_workspace_move_block(
            "req".into(),
            WorkspaceMoveBlockParams {
                workspace_ids: vec![parent_id.clone(), child_id.clone()],
                before_workspace_id: Some(tail_id.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(
            app.state
                .workspaces
                .iter()
                .map(|workspace| workspace.display_name())
                .collect::<Vec<_>>(),
            ["normal", "parent", "child", "tail"]
        );
        assert_eq!(workspaces[1].workspace_id, parent_id);
        assert_eq!(workspaces[2].workspace_id, child_id);
        let events = event_hub.events_after(0);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0].1.data,
            EventData::WorkspaceReordered {
                workspace_ids,
                before_workspace_id,
                workspaces,
            } if workspace_ids.first() == Some(&parent_id)
                && workspace_ids.get(1) == Some(&child_id)
                && workspace_ids.len() == 2
                && before_workspace_id.as_deref() == Some(tail_id.as_str())
                && workspaces[1].workspace_id == parent_id
        ));
    }

    #[test]
    fn api_workspace_move_noop_does_not_emit_event() {
        let event_hub = crate::api::EventHub::default();
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(&Config::default(), true, None, api_rx, event_hub.clone());
        app.state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        let moved_id = app.public_workspace_id(0);

        let response = app.handle_workspace_move(
            "req".into(),
            WorkspaceMoveParams {
                workspace_id: moved_id.clone(),
                insert_index: 1,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorkspaceList { workspaces } = success.result else {
            panic!("expected workspace list");
        };
        assert_eq!(workspaces[0].workspace_id, moved_id);
        assert!(event_hub.events_after(0).is_empty());
    }

    /// A fleet reporting a finished worker is the whole input the residue has.
    ///
    /// Built the way `image_card::tests::three_rank_pixel_app` builds it — a
    /// first mate, a second mate Space owning it, and a worker pane under the
    /// second mate — because the credit runs through the *tree's* own owner
    /// rule and a fixture that skips the tokens would be testing nothing.
    fn fleet_with_a_worker() -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut second_mate = Workspace::test_new("2ndmate-explore");
        let worker_pane = second_mate.test_split(ratatui::layout::Direction::Vertical);
        app.state.workspaces = vec![Workspace::test_new("firstmate"), second_mate];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);

        let now = std::time::Instant::now();
        app.state.workspaces[1].metadata_tokens.patch(
            std::collections::HashMap::from([("owner".to_string(), Some("firstmate".to_string()))]),
            None,
            now,
        );
        let worker_terminal = app.state.workspaces[1].tabs[0].panes[&worker_pane]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&worker_terminal).unwrap();
        terminal.set_agent_name("worker".to_string());
        terminal.metadata_tokens.patch(
            std::collections::HashMap::from([(
                "owner".to_string(),
                Some("2ndmate-explore".to_string()),
            )]),
            None,
            now,
        );
        let worker_id = app
            .public_pane_id(1, worker_pane)
            .expect("the worker pane has a public id");
        (app, worker_id)
    }

    fn report(
        app: &mut App,
        kind: WorkspaceSignalKind,
        from: &str,
        seq: Option<u64>,
    ) -> SuccessResponse {
        let response = app.handle_workspace_report_signal(
            "req".into(),
            WorkspaceReportSignalParams {
                source: "firstmate".into(),
                kind,
                from_workspace_id: Some(from.into()),
                to_workspace_id: None,
                seq,
                ttl_ms: None,
            },
        );
        serde_json::from_str(&response).expect("a signal report always answers success")
    }

    /// The credit follows the ownership tree, so the mate the worker is drawn
    /// under is the mate that gets the ring — and nobody else does.
    #[tokio::test]
    async fn a_finished_worker_leaves_a_ring_on_the_mate_that_owns_it() {
        let (mut app, worker_id) = fleet_with_a_worker();
        assert_eq!(app.state.residue.absorbed("2ndmate-explore"), 0);

        report(
            &mut app,
            WorkspaceSignalKind::Completed,
            &worker_id,
            Some(1),
        );
        report(
            &mut app,
            WorkspaceSignalKind::Completed,
            &worker_id,
            Some(2),
        );

        assert_eq!(app.state.residue.absorbed("2ndmate-explore"), 2);
        assert_eq!(app.state.residue.rings("2ndmate-explore"), 2);
        assert_eq!(
            app.state.residue.absorbed("firstmate"),
            0,
            "the worker's grandparent took credit for work it did not absorb"
        );
        assert_eq!(
            app.state.residue.absorbed("worker"),
            0,
            "the worker credited itself"
        );
    }

    /// A second mate finishing its own charter rings the first mate, by the
    /// same rule and with no second code path: a Space names its owner with the
    /// token a pane uses.
    #[tokio::test]
    async fn a_mate_finishing_rings_the_mate_above_it() {
        let (mut app, _) = fleet_with_a_worker();
        let mate_id = app.public_workspace_id(1);

        report(&mut app, WorkspaceSignalKind::Completed, &mate_id, Some(1));

        assert_eq!(app.state.residue.absorbed("firstmate"), 1);
    }

    /// `seq` is what makes reporting idempotent, and the ring is the one part
    /// of the report that is not transient — so a retry must not add one.
    #[tokio::test]
    async fn a_replayed_report_does_not_add_a_second_ring() {
        let (mut app, worker_id) = fleet_with_a_worker();

        report(
            &mut app,
            WorkspaceSignalKind::Completed,
            &worker_id,
            Some(7),
        );
        report(
            &mut app,
            WorkspaceSignalKind::Completed,
            &worker_id,
            Some(7),
        );
        report(
            &mut app,
            WorkspaceSignalKind::Completed,
            &worker_id,
            Some(3),
        );

        assert_eq!(app.state.residue.absorbed("2ndmate-explore"), 1);
    }

    /// Residue is what a mate *finished*. The other three kinds are different
    /// facts and must leave nothing behind — `failed` most of all, since a card
    /// that decorated failures the same way it decorates completions would be
    /// actively misreporting the fleet.
    #[tokio::test]
    async fn only_a_completed_report_leaves_residue() {
        for kind in [
            WorkspaceSignalKind::Transfer,
            WorkspaceSignalKind::Failed,
            WorkspaceSignalKind::Idle,
        ] {
            let (mut app, worker_id) = fleet_with_a_worker();
            // `transfer` carries on `to`, the others on `from`; either way the
            // carrier is the worker's own row.
            let response = app.handle_workspace_report_signal(
                "req".into(),
                WorkspaceReportSignalParams {
                    source: "firstmate".into(),
                    kind,
                    from_workspace_id: Some(worker_id.clone()),
                    to_workspace_id: Some(worker_id.clone()),
                    seq: None,
                    ttl_ms: None,
                },
            );
            let _: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(
                app.state.residue.absorbed("2ndmate-explore"),
                0,
                "a {kind:?} report left residue"
            );
        }
    }

    /// A worker nobody owns cannot ring anybody, and the report is still a
    /// success — the same rule an unresolvable carrier already follows.
    #[tokio::test]
    async fn a_worker_with_no_owner_credits_nobody() {
        let (mut app, worker_id) = fleet_with_a_worker();
        let worker_terminal = {
            let (ws_idx, pane_id) = app.parse_pane_id(&worker_id).unwrap();
            app.state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone()
        };
        app.state
            .terminals
            .get_mut(&worker_terminal)
            .unwrap()
            .metadata_tokens
            .patch(
                std::collections::HashMap::from([("owner".to_string(), None)]),
                None,
                std::time::Instant::now(),
            );

        let success = report(&mut app, WorkspaceSignalKind::Completed, &worker_id, None);
        assert!(matches!(success.result, ResponseResult::Ok {}));
        assert!(app.state.residue.is_empty());
    }

    /// A mate that is gone keeps no residue, so a long-lived server's owner
    /// budget belongs to the fleet that is running.
    #[tokio::test]
    async fn closing_a_mate_drops_its_residue() {
        let (mut app, worker_id) = fleet_with_a_worker();
        report(&mut app, WorkspaceSignalKind::Completed, &worker_id, None);
        assert_eq!(app.state.residue.absorbed("2ndmate-explore"), 1);

        let mate_id = app.public_workspace_id(1);
        let response = app.handle_workspace_close(
            "req".into(),
            WorkspaceTarget {
                workspace_id: mate_id,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(app.state.residue.absorbed("2ndmate-explore"), 0);
    }
}
