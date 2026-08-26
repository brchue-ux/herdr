use bytes::Bytes;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, PaneClearAgentAuthorityParams, PaneCurrentParams,
    PaneDeclareAgentParams, PaneDirection, PaneDormantReappearParams, PaneDormantReappearReason,
    PaneDormantReappearResult, PaneEdgesParams, PaneEdgesResult, PaneFocusDirectionParams,
    PaneFocusDirectionReason, PaneFocusDirectionResult, PaneInfo, PaneLayoutPane, PaneLayoutParams,
    PaneLayoutRect, PaneLayoutSnapshot, PaneLayoutSplit, PaneListParams, PaneMinimizeParams,
    PaneMinimizeReason, PaneMinimizeResult, PaneMoveDestination, PaneMoveParams, PaneMoveReason,
    PaneMoveResult, PaneNeighborParams, PaneNeighborResult, PaneProcessInfo, PaneProcessInfoParams,
    PaneProcessInfoProcess, PaneReadParams, PaneReadResult, PaneReleaseAgentParams,
    PaneRenameParams, PaneReportAgentParams, PaneReportAgentSessionParams,
    PaneReportEditDiffParams, PaneReportMetadataParams, PaneResizeParams, PaneResizeReason,
    PaneResizeResult, PaneSendInputParams, PaneSendKeysParams, PaneSendTextParams, PaneSplitParams,
    PaneSwapParams, PaneSwapReason, PaneSwapResult, PaneTarget, PaneZoomMode, PaneZoomParams,
    PaneZoomReason, PaneZoomResult, ResponseResult,
};
use crate::app::actions::{PaneZoomCommand, PaneZoomNoopReason};
use crate::app::App;
#[cfg(test)]
use crate::app::Mode;
use crate::layout::{find_in_direction, NavDirection, PaneId};
use crate::workspace::GitDiffText;

use super::super::api_helpers::{
    detect_state_from_api, encode_api_keys, normalize_metadata_source, normalize_metadata_tokens,
    normalize_metadata_ttl, normalize_reported_agent_label, MAX_METADATA_TOKEN_KEYS_PER_RESOURCE,
};
#[cfg(test)]
use super::super::api_helpers::{METADATA_SOURCE_MAX_CHARS, METADATA_TTL_MAX_MS};
use super::responses::{encode_error, encode_success};

/// Ceiling on one `pane.report_edit_diff` payload, in lines: anything past
/// the parser's own cap could not be shown anyway, so it is rejected at the
/// door rather than silently truncated into a diff the caller thinks was
/// stored whole.
///
/// *Derived from* `GIT_DIFF_MAX_LINES` rather than restated as `4000`, and
/// checked against the same post-synthesis text the parser is handed (see
/// [`App::handle_pane_report_edit_diff`]). Those two together are what make
/// `crate::workspace::GitDiffText::truncated` unreachable on this path —
/// `crate::ui::diff_pane::focused_pane_diff` states the resulting invariant,
/// and neither half may be loosened without putting a truncation branch back
/// in the renderer.
const MAX_EDIT_DIFF_LINES_PER_REQUEST: usize = crate::workspace::GIT_DIFF_MAX_LINES;

impl App {
    pub(super) fn handle_pane_split(&mut self, id: String, params: PaneSplitParams) -> String {
        let target = if let Some(target_pane_id) = params.target_pane_id.as_deref() {
            self.parse_pane_id(target_pane_id)
        } else if let Some(workspace_id) = params.workspace_id.as_deref() {
            self.parse_workspace_id(workspace_id).and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        } else {
            self.state.active.and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        };
        let Some((ws_idx, target_pane_id)) = target else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let extra_env = match super::env::normalize_launch_env(params.env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        // Resolved before the split, while the caller is certainly still live.
        let origin = self.resolve_pane_origin(params.caller_pane_id.as_deref());
        let (rows, cols) = self.state.estimate_pane_size();
        let split_cwd = params.cwd.map(std::path::PathBuf::from).or_else(|| {
            let follow_cwd = self.launch_cwd_for_pane_in_workspace(ws_idx, target_pane_id);
            Some(self.resolve_new_terminal_cwd(follow_cwd))
        });
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.host_terminal_theme;
        let host_terminal_appearance = self.state.host_terminal_appearance;
        let previous_focus = self.state.current_pane_focus_target();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let direction = match params.direction {
            crate::api::schema::SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            crate::api::schema::SplitDirection::Down => ratatui::layout::Direction::Vertical,
        };
        let shell_config = crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode);
        let split_result = match params.ratio {
            Some(ratio) => ws.split_pane_with_ratio(
                target_pane_id,
                direction,
                ratio,
                rows,
                cols,
                split_cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                extra_env,
                params.focus,
            ),
            None => ws.split_pane(
                target_pane_id,
                direction,
                rows,
                cols,
                split_cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                extra_env,
                params.focus,
            ),
        };
        let (target_tab_idx, mut new_pane) = match split_result {
            Some(Ok(result)) => result,
            Some(Err(err)) => return encode_error(id, "pane_split_failed", err.to_string()),
            None => return encode_error(id, "pane_not_found", "pane not found"),
        };
        new_pane.terminal.created_by = origin;
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, target_tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, new_pane.pane_id);
            self.state.settle_terminal_mode_after_focus();
        }
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        self.schedule_session_save();
        let pane = self.pane_info(ws_idx, new_pane.pane_id).unwrap();
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: pane.clone() },
        });
        self.emit_layout_updated_event(ws_idx, target_tab_idx);

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_list(&mut self, id: String, params: PaneListParams) -> String {
        match self.collect_panes_for_workspace(params.workspace_id.as_deref()) {
            Ok(panes) => encode_success(id, ResponseResult::PaneList { panes }),
            Err((code, message)) => encode_error(id, &code, message),
        }
    }

    pub(super) fn handle_pane_current(&mut self, id: String, params: PaneCurrentParams) -> String {
        let target = match params.caller_pane_id.as_deref() {
            Some(caller_pane_id) => self.parse_pane_id(caller_pane_id),
            None => self.resolve_optional_pane(None),
        };
        let Some((ws_idx, pane_id)) = target else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };

        encode_success(id, ResponseResult::PaneCurrent { pane })
    }

    pub(super) fn handle_pane_get(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_focus(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(_tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.state.mark_active_tab_seen();
        self.state.settle_terminal_mode_after_focus();

        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_layout(&mut self, id: String, params: PaneLayoutParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(id, ResponseResult::PaneLayout { layout })
    }

    pub(super) fn handle_pane_process_info(
        &mut self,
        id: String,
        params: PaneProcessInfoParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some((runtime, _workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let shell_pid = runtime.child_pid();
        let foreground_job = shell_pid.and_then(crate::detect::foreground_job);
        let foreground_process_group_id = foreground_job.as_ref().map(|job| job.process_group_id);
        let foreground_processes = foreground_job
            .map(|job| {
                job.processes
                    .into_iter()
                    .map(|process| PaneProcessInfoProcess {
                        pid: process.pid,
                        name: process.name,
                        argv0: process.argv0,
                        argv: process.argv,
                        cmdline: process.cmdline,
                        cwd: crate::platform::process_cwd(process.pid)
                            .map(|cwd| cwd.display().to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();

        encode_success(
            id,
            ResponseResult::PaneProcessInfo {
                process_info: PaneProcessInfo {
                    pane_id: public_pane_id,
                    shell_pid,
                    foreground_process_group_id,
                    tty: None,
                    foreground_processes,
                },
            },
        )
    }

    pub(super) fn handle_pane_neighbor(
        &mut self,
        id: String,
        params: PaneNeighborParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(source_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let neighbor_pane_id = self
            .directional_pane_target(ws_idx, tab_idx, pane_id, params.direction)
            .and_then(|pane_id| self.public_pane_id(ws_idx, pane_id));
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneNeighbor {
                neighbor: PaneNeighborResult {
                    pane_id: source_public_id,
                    direction: params.direction,
                    neighbor_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_edges(&mut self, id: String, params: PaneEdgesParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(tab) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let area = self.state.view.terminal_area;
        let Some(info) = tab
            .layout
            .panes(area)
            .into_iter()
            .find(|info| info.id == pane_id)
        else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneEdges {
                edges: PaneEdgesResult {
                    pane_id: pane_public_id,
                    left: info.rect.x <= area.x,
                    right: info.rect.x + info.rect.width >= area.x + area.width,
                    up: info.rect.y <= area.y,
                    down: info.rect.y + info.rect.height >= area.y + area.height,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_focus_direction(
        &mut self,
        id: String,
        params: PaneFocusDirectionParams,
    ) -> String {
        let Some((ws_idx, source_pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref())
        else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(source_pane_id)
        else {
            return pane_not_found(
                id,
                &self
                    .public_pane_id(ws_idx, source_pane_id)
                    .unwrap_or_default(),
            );
        };
        let Some(source_public_id) = self.public_pane_id(ws_idx, source_pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let target =
            self.directional_pane_target(ws_idx, tab_idx, source_pane_id, params.direction);
        let reason = target
            .is_none()
            .then_some(PaneFocusDirectionReason::NoNeighbor);

        if let Some(target_pane_id) = target {
            self.state.focus_pane_in_workspace(ws_idx, target_pane_id);
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.state.settle_terminal_mode_after_focus();
        }
        let focused_pane_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .map(|tab| tab.layout.focused())
            .and_then(|pane_id| self.public_pane_id(ws_idx, pane_id));
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneFocusDirection {
                focus: PaneFocusDirectionResult {
                    changed: target.is_some(),
                    reason,
                    source_pane_id: source_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_resize(&mut self, id: String, params: PaneResizeParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };

        let amount = params
            .amount
            .filter(|amount| amount.is_finite())
            .unwrap_or(0.05)
            .abs()
            .min(0.5);
        let direction: NavDirection = params.direction.into();
        let area = self.state.view.terminal_area;
        let changed = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
            .is_some_and(|tab| tab.layout.resize_pane(pane_id, direction, amount, area));
        if changed {
            self.schedule_session_save();
        }

        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();
        if changed {
            self.emit_layout_updated_snapshot(layout.clone());
        }

        encode_success(
            id,
            ResponseResult::PaneResize {
                resize: PaneResizeResult {
                    changed,
                    reason: (!changed).then_some(PaneResizeReason::Unchanged),
                    pane_id: pane_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_swap(&mut self, id: String, params: PaneSwapParams) -> String {
        let directional = params.direction.is_some();
        let explicit = params.source_pane_id.is_some() || params.target_pane_id.is_some();
        if directional == explicit {
            return encode_error(
                id,
                "invalid_pane_swap",
                "provide either direction with optional pane_id, or source_pane_id and target_pane_id",
            );
        }

        let (ws_idx, tab_idx, source_pane_id, target_pane_id, reason) = if let Some(direction) =
            params.direction
        {
            let Some((ws_idx, source_pane_id)) =
                self.resolve_swap_source(params.pane_id.as_deref())
            else {
                return encode_error(id, "pane_not_found", "source pane not found");
            };
            let Some(tab_idx) =
                self.state.workspaces[ws_idx].find_tab_index_for_pane(source_pane_id)
            else {
                return pane_not_found(
                    id,
                    &self
                        .public_pane_id(ws_idx, source_pane_id)
                        .unwrap_or_default(),
                );
            };
            let target = self.directional_pane_target(ws_idx, tab_idx, source_pane_id, direction);
            match target {
                Some(target_pane_id) => {
                    (ws_idx, tab_idx, source_pane_id, Some(target_pane_id), None)
                }
                None => (
                    ws_idx,
                    tab_idx,
                    source_pane_id,
                    None,
                    Some(PaneSwapReason::NoNeighbor),
                ),
            }
        } else {
            let Some(source_raw) = params.source_pane_id.as_deref() else {
                return encode_error(id, "invalid_pane_swap", "missing source_pane_id");
            };
            let Some(target_raw) = params.target_pane_id.as_deref() else {
                return encode_error(id, "invalid_pane_swap", "missing target_pane_id");
            };
            let source = self
                .parse_pane_id(source_raw)
                .and_then(|(ws_idx, pane_id)| {
                    let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
                    Some((ws_idx, tab_idx, pane_id))
                });
            let target = self
                .parse_pane_id(target_raw)
                .and_then(|(ws_idx, pane_id)| {
                    let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
                    Some((ws_idx, tab_idx, pane_id))
                });
            let response_context = source
                .map(|(ws_idx, tab_idx, _)| (ws_idx, tab_idx))
                .or_else(|| target.map(|(ws_idx, tab_idx, _)| (ws_idx, tab_idx)))
                .or_else(|| {
                    let ws_idx = self.state.active?;
                    let tab_idx = self.state.workspaces.get(ws_idx)?.active_tab_index();
                    Some((ws_idx, tab_idx))
                });
            let Some((ws_idx, tab_idx)) = response_context else {
                return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
            };
            let source_pane_id = source
                .map(|(_, _, pane_id)| pane_id)
                .or_else(|| {
                    self.state
                        .workspaces
                        .get(ws_idx)?
                        .tabs
                        .get(tab_idx)
                        .map(|tab| tab.layout.focused())
                })
                .unwrap_or(PaneId::from_raw(0));
            let target_pane_id = target.map(|(_, _, pane_id)| pane_id);
            let reason = match (source, target) {
                (None, _) | (_, None) => Some(PaneSwapReason::NotFound),
                (Some((_, _, source)), Some((_, _, target))) if source == target => {
                    Some(PaneSwapReason::SamePane)
                }
                (Some((source_ws, source_tab, _)), Some((target_ws, target_tab, _)))
                    if source_ws != target_ws || source_tab != target_tab =>
                {
                    Some(PaneSwapReason::CrossTab)
                }
                _ => None,
            };
            (ws_idx, tab_idx, source_pane_id, target_pane_id, reason)
        };

        let mut changed = false;
        if reason.is_none() {
            if let Some(target_pane_id) = target_pane_id {
                let previous_focus = self.state.current_pane_focus_target();
                if let Some(tab) = self
                    .state
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.tabs.get_mut(tab_idx))
                {
                    changed = tab.layout.swap_panes(source_pane_id, target_pane_id);
                    tab.layout.focus_pane(source_pane_id);
                    if changed {
                        self.state.switch_workspace_tab(ws_idx, tab_idx);
                        self.state
                            .record_pane_focus_change(previous_focus, ws_idx, source_pane_id);
                        self.state.mark_session_dirty();
                        self.schedule_session_save();
                    }
                }
            }
        }

        let source_public_id = match params.source_pane_id {
            Some(raw) => self
                .parse_pane_id(&raw)
                .and_then(|(idx, pane_id)| {
                    self.state
                        .workspaces
                        .get(idx)?
                        .find_tab_index_for_pane(pane_id)?;
                    self.public_pane_id(idx, pane_id)
                })
                .unwrap_or(raw),
            None => self
                .public_pane_id(ws_idx, source_pane_id)
                .unwrap_or_default(),
        };
        let target_public_id = match params.target_pane_id {
            Some(raw) => self
                .parse_pane_id(&raw)
                .and_then(|(idx, pane_id)| {
                    self.state
                        .workspaces
                        .get(idx)?
                        .find_tab_index_for_pane(pane_id)?;
                    self.public_pane_id(idx, pane_id)
                })
                .or(Some(raw)),
            None => target_pane_id.and_then(|pane_id| self.public_pane_id(ws_idx, pane_id)),
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();
        if changed {
            self.emit_layout_updated_snapshot(layout.clone());
        }

        encode_success(
            id,
            ResponseResult::PaneSwap {
                swap: PaneSwapResult {
                    changed,
                    reason,
                    source_pane_id: source_public_id,
                    target_pane_id: target_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_move(&mut self, id: String, params: PaneMoveParams) -> String {
        let PaneMoveParams {
            pane_id,
            destination,
            focus,
        } = params;
        let Some((source_ws_idx, source_pane_id)) = self.parse_pane_id(&pane_id) else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let Some(source_tab_idx) =
            self.state.workspaces[source_ws_idx].find_tab_index_for_pane(source_pane_id)
        else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let previous_pane_id = self
            .public_pane_id(source_ws_idx, source_pane_id)
            .unwrap_or_else(|| pane_id.clone());
        let previous_workspace_id = self.public_workspace_id(source_ws_idx);
        let Some(previous_tab_id) = self.public_tab_id(source_ws_idx, source_tab_idx) else {
            return encode_error(id, "tab_not_found", "source tab not found");
        };
        let Some(source_terminal_id) = self
            .state
            .workspaces
            .get(source_ws_idx)
            .and_then(|ws| ws.tabs.get(source_tab_idx))
            .and_then(|tab| tab.terminal_id(source_pane_id))
            .cloned()
        else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let recovery_context = PaneMoveRecoveryContext {
            source_ws_idx,
            previous_workspace_id: previous_workspace_id.clone(),
            previous_workspace_label: self.state.workspaces[source_ws_idx].custom_name.clone(),
            previous_tab_label: self.state.workspaces[source_ws_idx].tabs[source_tab_idx]
                .custom_name
                .clone(),
            previous_worktree_space: self.state.workspaces[source_ws_idx].worktree_space.clone(),
            identity_cwd: self.state.workspaces[source_ws_idx].identity_cwd.clone(),
        };

        if self.state.workspaces[source_ws_idx].tabs[source_tab_idx].zoomed {
            let Some(layout) = self.pane_layout_snapshot(source_ws_idx, source_tab_idx) else {
                return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
            };
            let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                return encode_error(id, "pane_not_found", "source pane not found");
            };
            return encode_unchanged_pane_move(
                id,
                PaneMoveReason::ZoomedTab,
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane,
                Some(layout.clone()),
                layout,
            );
        }

        let resolved = match destination {
            PaneMoveDestination::Tab {
                tab_id,
                target_pane_id,
                split,
                ratio,
            } => {
                let Some((target_ws_idx, target_tab_idx)) = self.parse_tab_id(&tab_id) else {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
                };
                if source_ws_idx == target_ws_idx && source_tab_idx == target_tab_idx {
                    let Some(layout) = self.pane_layout_snapshot(source_ws_idx, source_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                        return encode_error(id, "pane_not_found", "source pane not found");
                    };
                    return encode_unchanged_pane_move(
                        id,
                        PaneMoveReason::SameTab,
                        previous_pane_id,
                        previous_workspace_id,
                        previous_tab_id,
                        pane,
                        Some(layout.clone()),
                        layout,
                    );
                }
                if self.state.workspaces[target_ws_idx].tabs[target_tab_idx].zoomed {
                    let Some(source_layout) =
                        self.pane_layout_snapshot(source_ws_idx, source_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(target_layout) =
                        self.pane_layout_snapshot(target_ws_idx, target_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                        return encode_error(id, "pane_not_found", "source pane not found");
                    };
                    return encode_unchanged_pane_move(
                        id,
                        PaneMoveReason::ZoomedTab,
                        previous_pane_id,
                        previous_workspace_id,
                        previous_tab_id,
                        pane,
                        Some(source_layout),
                        target_layout,
                    );
                }
                let target_pane_id = match target_pane_id {
                    Some(raw) => {
                        let Some((pane_ws_idx, pane_id)) = self.parse_pane_id(&raw) else {
                            return encode_error(
                                id,
                                "target_pane_not_found",
                                format!("target pane {raw} not found"),
                            );
                        };
                        let pane_tab_idx =
                            self.state.workspaces[pane_ws_idx].find_tab_index_for_pane(pane_id);
                        if pane_ws_idx != target_ws_idx || pane_tab_idx != Some(target_tab_idx) {
                            return encode_error(
                                id,
                                "target_pane_not_found",
                                format!("target pane {raw} is not in tab {tab_id}"),
                            );
                        }
                        pane_id
                    }
                    None => self.state.workspaces[target_ws_idx].tabs[target_tab_idx]
                        .layout
                        .focused(),
                };
                let Some(target_tab_id) = self.public_tab_id(target_ws_idx, target_tab_idx) else {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
                };
                ResolvedPaneMoveDestination::ExistingTab {
                    tab_id: target_tab_id,
                    target_pane_id,
                    split,
                    ratio: ratio.unwrap_or(0.5),
                    cross_workspace: source_ws_idx != target_ws_idx,
                }
            }
            PaneMoveDestination::NewTab {
                workspace_id,
                label,
            } => {
                let target_workspace_id = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return encode_error(
                            id,
                            "workspace_not_found",
                            format!("workspace {workspace_id} not found"),
                        );
                    };
                    self.public_workspace_id(ws_idx)
                } else {
                    previous_workspace_id.clone()
                };
                ResolvedPaneMoveDestination::NewTab {
                    workspace_id: target_workspace_id,
                    label,
                }
            }
            PaneMoveDestination::NewWorkspace { label, tab_label } => {
                ResolvedPaneMoveDestination::NewWorkspace { label, tab_label }
            }
        };

        let previous_focus = self.state.current_pane_focus_target();
        let taken = match self
            .state
            .workspaces
            .get_mut(source_ws_idx)
            .and_then(|ws| ws.take_pane_for_move(source_pane_id))
        {
            Some(taken) => taken,
            None => return encode_error(id, "pane_move_failed", "source pane could not be moved"),
        };
        let source_removed_tab_id = taken.removed_tab_idx.map(|_| previous_tab_id.clone());
        let source_workspace_empty = taken.workspace_empty;
        let moved = taken.moved;
        let cross_workspace = match &resolved {
            ResolvedPaneMoveDestination::ExistingTab {
                cross_workspace, ..
            } => *cross_workspace,
            ResolvedPaneMoveDestination::NewTab { workspace_id, .. } => {
                workspace_id != &previous_workspace_id
            }
            ResolvedPaneMoveDestination::NewWorkspace { .. } => true,
        };
        if cross_workspace {
            if let Some(ws) = self.state.workspaces.get_mut(source_ws_idx) {
                ws.unregister_moved_pane(source_pane_id);
            }
            self.state
                .public_pane_id_aliases
                .insert(previous_pane_id.clone(), source_pane_id);
        }

        let mut closed_workspace_id = None;
        if source_workspace_empty && cross_workspace {
            self.state.workspaces.remove(source_ws_idx);
            closed_workspace_id = Some(previous_workspace_id.clone());
            if self.state.workspaces.is_empty() {
                self.state.active = None;
                self.state.selected = 0;
            } else {
                if let Some(active) = self.state.active {
                    if active == source_ws_idx {
                        self.state.active =
                            Some(source_ws_idx.min(self.state.workspaces.len() - 1));
                    } else if active > source_ws_idx {
                        self.state.active = Some(active - 1);
                    }
                }
                if self.state.selected == source_ws_idx {
                    self.state.selected = source_ws_idx.min(self.state.workspaces.len() - 1);
                } else if self.state.selected > source_ws_idx {
                    self.state.selected -= 1;
                }
            }
        }

        let mut created_workspace = false;
        let mut created_tab = false;
        let (target_ws_idx, target_tab_idx, moved_pane_id) = match resolved {
            ResolvedPaneMoveDestination::ExistingTab {
                tab_id,
                target_pane_id,
                split,
                ratio,
                cross_workspace: _,
            } => {
                let Some((target_ws_idx, target_tab_idx)) = self.parse_tab_id(&tab_id) else {
                    self.recover_failed_pane_move(recovery_context, moved);
                    return encode_error(id, "pane_move_failed", "target tab disappeared");
                };
                let direction = split_direction_to_layout(split);
                let moved_pane_id = match self.state.workspaces[target_ws_idx]
                    .insert_moved_pane_into_tab(
                        target_tab_idx,
                        target_pane_id,
                        moved,
                        direction,
                        ratio,
                        focus,
                    ) {
                    Ok(pane_id) => pane_id,
                    Err(moved) => {
                        self.recover_failed_pane_move(recovery_context, moved);
                        return encode_error(
                            id,
                            "pane_move_failed",
                            "target pane could not be split",
                        );
                    }
                };
                (target_ws_idx, target_tab_idx, moved_pane_id)
            }
            ResolvedPaneMoveDestination::NewTab {
                workspace_id,
                label,
            } => {
                let Some(target_ws_idx) = self.parse_workspace_id(&workspace_id) else {
                    self.recover_failed_pane_move(recovery_context, moved);
                    return encode_error(id, "pane_move_failed", "target workspace disappeared");
                };
                let moved_pane_id = moved.pane_id;
                let target_tab_idx = self.state.workspaces[target_ws_idx]
                    .create_tab_from_existing_pane(
                        moved,
                        label,
                        self.event_tx.clone(),
                        self.render_notify.clone(),
                        self.render_dirty.clone(),
                    );
                created_tab = true;
                (target_ws_idx, target_tab_idx, moved_pane_id)
            }
            ResolvedPaneMoveDestination::NewWorkspace { label, tab_label } => {
                let identity_cwd = self
                    .state
                    .terminals
                    .get(&source_terminal_id)
                    .map(|terminal| terminal.cwd.clone())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
                let moved_pane_id = moved.pane_id;
                let workspace = crate::workspace::Workspace::from_existing_pane(
                    label,
                    tab_label,
                    identity_cwd,
                    moved,
                    self.event_tx.clone(),
                    self.render_notify.clone(),
                    self.render_dirty.clone(),
                );
                self.state.workspaces.push(workspace);
                let target_ws_idx = self.state.workspaces.len() - 1;
                created_workspace = true;
                created_tab = true;
                (target_ws_idx, 0, moved_pane_id)
            }
        };

        if focus || self.state.active.is_none() {
            self.state
                .switch_workspace_tab(target_ws_idx, target_tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, target_ws_idx, moved_pane_id);
            self.state.settle_terminal_mode_after_focus();
        }
        let created_workspace = created_workspace.then(|| self.workspace_info(target_ws_idx));
        let created_tab = if created_tab {
            self.tab_info(target_ws_idx, target_tab_idx)
        } else {
            None
        };

        self.state.remove_alias_shadowed_by_new_pane(moved_pane_id);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        let Some(pane) = self.pane_info(target_ws_idx, moved_pane_id) else {
            return encode_error(id, "pane_move_failed", "moved pane is unavailable");
        };
        let source_layout = if closed_workspace_id.is_none() {
            self.parse_tab_id(&previous_tab_id)
                .and_then(|(ws_idx, tab_idx)| self.pane_layout_snapshot(ws_idx, tab_idx))
        } else {
            None
        };
        let Some(target_layout) = self.pane_layout_snapshot(target_ws_idx, target_tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = target_layout.focused_pane_id.clone();
        let move_result = PaneMoveResult {
            changed: true,
            reason: None,
            previous_pane_id: previous_pane_id.clone(),
            previous_workspace_id: previous_workspace_id.clone(),
            previous_tab_id: previous_tab_id.clone(),
            pane: Box::new(pane.clone()),
            source_layout: source_layout.clone().map(Box::new),
            target_layout: Box::new(target_layout),
            created_workspace: created_workspace.clone(),
            created_tab: created_tab.clone(),
            closed_workspace_id: closed_workspace_id.clone(),
            closed_tab_id: source_removed_tab_id.clone(),
            focused_pane_id,
        };
        if let Some(closed_tab_id) = &source_removed_tab_id {
            self.emit_event(EventEnvelope {
                event: EventKind::TabClosed,
                data: EventData::TabClosed {
                    tab_id: closed_tab_id.clone(),
                    workspace_id: previous_workspace_id.clone(),
                },
            });
        }
        if let Some(closed_workspace_id) = &closed_workspace_id {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id: closed_workspace_id.clone(),
                    workspace: None,
                },
            });
        }
        if let Some(workspace) = &created_workspace {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceCreated,
                data: EventData::WorkspaceCreated {
                    workspace: workspace.clone(),
                },
            });
        }
        if let Some(tab) = &created_tab {
            self.emit_event(EventEnvelope {
                event: EventKind::TabCreated,
                data: EventData::TabCreated { tab: tab.clone() },
            });
        }
        self.emit_event(EventEnvelope {
            event: EventKind::PaneMoved,
            data: EventData::PaneMoved {
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane: Box::new(pane),
                created_workspace,
                created_tab,
                closed_workspace_id,
                closed_tab_id: source_removed_tab_id,
            },
        });
        if let Some(source_layout) = source_layout {
            self.emit_layout_updated_snapshot(source_layout);
        }
        self.emit_layout_updated_snapshot((*move_result.target_layout).clone());

        encode_success(id, ResponseResult::PaneMove { move_result })
    }

    fn recover_failed_pane_move(
        &mut self,
        context: PaneMoveRecoveryContext,
        moved: crate::workspace::MovedPane,
    ) {
        if let Some(ws_idx) = self.parse_workspace_id(&context.previous_workspace_id) {
            self.state.workspaces[ws_idx].create_tab_from_existing_pane(
                moved,
                context.previous_tab_label,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
        } else {
            let mut workspace = crate::workspace::Workspace::from_existing_pane(
                context.previous_workspace_label,
                context.previous_tab_label,
                context.identity_cwd,
                moved,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
            workspace.id = context.previous_workspace_id;
            workspace.worktree_space = context.previous_worktree_space;
            let insert_idx = context.source_ws_idx.min(self.state.workspaces.len());
            if let Some(active) = self.state.active {
                if active >= insert_idx {
                    self.state.active = Some(active + 1);
                }
            }
            if self.state.selected >= insert_idx && !self.state.workspaces.is_empty() {
                self.state.selected += 1;
            }
            self.state.workspaces.insert(insert_idx, workspace);
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
    }

    pub(super) fn handle_pane_zoom(&mut self, id: String, params: PaneZoomParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let command = match params.mode {
            PaneZoomMode::Toggle => PaneZoomCommand::Toggle,
            PaneZoomMode::On => PaneZoomCommand::On,
            PaneZoomMode::Off => PaneZoomCommand::Off,
        };
        let Some(outcome) = self.state.apply_pane_zoom(ws_idx, pane_id, command) else {
            return pane_not_found(id, &pane_public_id);
        };
        if outcome.changed || outcome.focus_changed {
            self.schedule_session_save();
        }
        self.state.settle_terminal_mode_after_focus();
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();
        if outcome.changed || outcome.focus_changed {
            self.emit_layout_updated_snapshot(layout.clone());
        }

        encode_success(
            id,
            ResponseResult::PaneZoom {
                zoom: PaneZoomResult {
                    changed: outcome.changed || outcome.focus_changed,
                    zoom_changed: outcome.changed,
                    focus_changed: outcome.focus_changed,
                    reason: outcome.reason.map(|reason| match reason {
                        PaneZoomNoopReason::SinglePane => PaneZoomReason::SinglePane,
                        PaneZoomNoopReason::AlreadyZoomed => PaneZoomReason::AlreadyZoomed,
                        PaneZoomNoopReason::AlreadyUnzoomed => PaneZoomReason::AlreadyUnzoomed,
                    }),
                    pane_id: pane_public_id,
                    focused_pane_id,
                    zoomed: outcome.zoomed,
                    layout,
                },
            },
        )
    }

    /// Detaches the whole tab `params.pane_id` (or the focused pane, if omitted)
    /// belongs to from its workspace's live tree — pane and tab together, as one unit
    /// — without touching any of its panes' `TerminalRuntime`s. Mirrors
    /// `handle_tab_close` exactly, except it stops before the kill calls
    /// (`remove_unattached_terminal_ids`/`shutdown_detached_terminal_runtimes`) and
    /// hands the removed `Tab` to the dormant registry instead.
    pub(super) fn handle_pane_minimize(
        &mut self,
        id: String,
        params: PaneMinimizeParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        if self.state.workspaces[ws_idx].tabs.len() <= 1 {
            return encode_success(
                id,
                ResponseResult::PaneMinimize {
                    minimize: PaneMinimizeResult {
                        changed: false,
                        reason: Some(PaneMinimizeReason::OnlyTabInWorkspace),
                        terminal_ids: Vec::new(),
                        workspace_id,
                    },
                },
            );
        }

        let terminal_ids = self.state.terminal_ids_for_tab(ws_idx, tab_idx);
        let pane_ids: Vec<PaneId> = self.state.workspaces[ws_idx].tabs[tab_idx]
            .panes
            .keys()
            .copied()
            .collect();
        let Some(tab) = self.state.workspaces[ws_idx].take_tab_for_dormancy(tab_idx) else {
            return encode_error(id, "pane_minimize_failed", "tab could not be minimized");
        };
        self.state.remove_plugin_pane_records(pane_ids);
        self.state
            .dormant_tabs
            .insert(tab, workspace_id.clone(), std::time::Instant::now());
        self.schedule_session_save();

        encode_success(
            id,
            ResponseResult::PaneMinimize {
                minimize: PaneMinimizeResult {
                    changed: true,
                    reason: None,
                    terminal_ids: terminal_ids.iter().map(|t| t.to_string()).collect(),
                    workspace_id,
                },
            },
        )
    }

    /// Reattaches a dormant pane's tab, keyed by `terminal_id` — the only handle that
    /// survives minimize, since the pane's own `pane_id` stops resolving the moment its
    /// tab leaves the live tree. Emits the same `pane.created` + `layout.updated` events
    /// a genuine new pane spawn would, by design: nothing about the event stream lets a
    /// subscriber tell a reappear apart from a fresh spawn.
    pub(super) fn handle_pane_dormant_reappear(
        &mut self,
        id: String,
        params: PaneDormantReappearParams,
    ) -> String {
        let terminal_id = crate::terminal::TerminalId::from_string(params.terminal_id.clone());

        // Idempotent: already attached to a live pane (this call retried after a
        // timeout, or the pane already reappeared through some other path). Report
        // success with wherever it currently lives rather than erroring or re-moving it.
        if let Some((ws_idx, pane_id)) = self.state.find_live_pane_by_terminal_id(&terminal_id) {
            let Some(pane) = self.pane_info(ws_idx, pane_id) else {
                return encode_error(id, "pane_not_found", "pane not found");
            };
            return encode_success(
                id,
                ResponseResult::PaneDormantReappear {
                    reappear: PaneDormantReappearResult {
                        changed: false,
                        reason: Some(PaneDormantReappearReason::AlreadyVisible),
                        pane: Box::new(pane),
                    },
                },
            );
        }

        let Some((dormant_id, pane_id)) = self.state.dormant_tabs.find_by_terminal_id(&terminal_id)
        else {
            // Covers both "never dormant" (stale caller bookkeeping) and "was dormant
            // but its process already died" — the PaneDied-for-dormant-panes fix
            // (`handle_dormant_pane_died`) already removes a dead pane's registry entry
            // reactively, so by the time this runs there is no separate zombie state
            // left to distinguish from "not found" here.
            return encode_error(id, "not_found", "terminal not found");
        };
        let Some(reappeared) = self.state.reappear_dormant_tab(dormant_id, params.focus) else {
            return encode_error(id, "not_found", "terminal not found");
        };
        self.schedule_session_save();
        // `reappear_dormant_tab` reinserts the exact `Tab` value that was removed —
        // unlike the original per-pane design, tab-level dormancy never rebuilds
        // `PaneState` or reassigns `pane_id`, so the id `find_by_terminal_id` returned
        // above is still the right key to look this pane up by.
        let Some(pane) = self.pane_info(reappeared.ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: pane.clone() },
        });
        self.emit_layout_updated_event(reappeared.ws_idx, reappeared.tab_idx);

        encode_success(
            id,
            ResponseResult::PaneDormantReappear {
                reappear: PaneDormantReappearResult {
                    changed: true,
                    reason: None,
                    pane: Box::new(pane),
                },
            },
        )
    }

    pub(super) fn handle_pane_rename(&mut self, id: String, params: PaneRenameParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        match params.label.map(|label| label.trim().to_string()) {
            Some(label) if !label.is_empty() => terminal.set_manual_label(label),
            _ => terminal.clear_manual_label(),
        }
        self.state.mark_session_dirty();
        let pane = self.pane_info(ws_idx, pane_id).unwrap();

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_read(&mut self, id: String, params: PaneReadParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some((pane, workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let agent = self.pane_agent(ws_idx, pane_id);
        let outcome = crate::app::api_helpers::read_terminal_snapshot(
            pane,
            agent,
            params.source,
            params.format,
            params.lines,
        );

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: public_pane_id,
                    workspace_id,
                    tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                    source: params.source,
                    format: params.format,
                    text: outcome.snapshot.text,
                    revision: 0,
                    truncated: outcome.snapshot.truncated,
                    transcript_applied: outcome.transcript_applied,
                },
            },
        )
    }

    pub(super) fn handle_pane_report_agent(
        &mut self,
        id: String,
        params: PaneReportAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            state: detect_state_from_api(params.state),
            message: params.message,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_agent_session(
        &mut self,
        id: String,
        params: PaneReportAgentSessionParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::AgentSessionReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            seq: params.seq,
            session_start_source: crate::agent_resume::normalize_session_start_source(
                params.session_start_source,
            ),
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_metadata(
        &mut self,
        id: String,
        params: PaneReportMetadataParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let agent_label = match params.agent.as_deref() {
            Some(agent) => match normalize_reported_agent_label(agent) {
                Some(agent_label) => Some(agent_label),
                None => return invalid_agent(id),
            },
            None => None,
        };
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        let raw_title_set = params.title.is_some();
        let raw_display_agent_set = params.display_agent.is_some();
        let raw_state_labels_set = !params.state_labels.is_empty();
        let tokens = if params.tokens.is_empty() {
            // A bulk clear is a token request even with no keys named, and has
            // to take the same sequence and reporting path a patch does.
            params.clear_all_tokens.then(std::collections::HashMap::new)
        } else {
            match normalize_metadata_tokens(params.tokens) {
                Ok(tokens) => Some(tokens),
                Err(message) => return encode_error(id, "invalid_metadata_token", message),
            }
        };
        let ttl = match normalize_metadata_ttl(params.ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return encode_error(id, "invalid_metadata_ttl", message),
        };
        let title = normalize_presentation_text(params.title);
        let display_agent = normalize_presentation_text(params.display_agent);
        let applies_to_source = match params.applies_to_source {
            Some(applies_to_source) => match normalize_metadata_source(applies_to_source) {
                Ok(applies_to_source) => Some(applies_to_source),
                Err(message) => return encode_error(id, "invalid_metadata_source", message),
            },
            None => None,
        };
        let state_labels = match normalize_state_labels(params.state_labels) {
            Ok(labels) => labels,
            Err(status) => {
                return encode_error(
                    id,
                    "invalid_state_label",
                    format!("unknown state label: {status}"),
                );
            }
        };
        if raw_title_set && params.clear_title
            || raw_display_agent_set && params.clear_display_agent
            || raw_state_labels_set && params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "cannot set and clear the same metadata field",
            );
        }
        if title.is_none()
            && display_agent.is_none()
            && state_labels.is_empty()
            && tokens.is_none()
            && !params.clear_title
            && !params.clear_display_agent
            && !params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "missing metadata field to set or clear",
            );
        }
        let presentation_requested = title.is_some()
            || display_agent.is_some()
            || !state_labels.is_empty()
            || params.clear_title
            || params.clear_display_agent
            || params.clear_state_labels;
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if terminal.metadata_report_blocked_by_process_exit(
            &source,
            agent_label.as_deref(),
            applies_to_source.as_deref(),
        ) {
            return encode_success(id, ResponseResult::Ok {});
        }
        if !terminal.metadata_report_sequence_is_fresh(&source, params.seq) {
            return encode_success(id, ResponseResult::Ok {});
        }
        let metadata_agent = crate::terminal::TerminalState::metadata_report_agent(
            &source,
            agent_label.as_deref(),
            applies_to_source.as_deref(),
        );
        if let Some(tokens) = tokens.as_ref() {
            // A bulk clear replaces the token set, so the limit applies to what
            // the patch sets rather than to what it is layered on top of.
            let key_count_after = if params.clear_all_tokens {
                tokens.values().filter(|value| value.is_some()).count()
            } else {
                terminal.metadata_tokens.key_count_after_patch(tokens)
            };
            if key_count_after > MAX_METADATA_TOKEN_KEYS_PER_RESOURCE {
                return encode_error(
                    id,
                    "metadata_token_limit",
                    format!(
                        "pane metadata may contain at most {MAX_METADATA_TOKEN_KEYS_PER_RESOURCE} tokens"
                    ),
                );
            }
        }
        match terminal.accept_metadata_report(&source, params.seq, tokens.is_some(), metadata_agent)
        {
            Ok(true) => {}
            Ok(false) => return encode_success(id, ResponseResult::Ok {}),
            Err(()) => {
                return encode_error(
                    id,
                    "metadata_sequence_source_limit",
                    format!(
                        "pane metadata may track at most {} sequenced sources",
                        crate::metadata_tokens::MAX_SEQUENCE_SOURCES
                    ),
                );
            }
        }
        let token_changed = tokens.is_some_and(|tokens| {
            let cleared = params.clear_all_tokens && terminal.metadata_tokens.clear();
            let changed = terminal
                .metadata_tokens
                .patch(tokens, ttl, std::time::Instant::now())
                || cleared;
            if changed {
                terminal.revision = terminal.revision.saturating_add(1);
            }
            changed
        });

        if presentation_requested {
            self.handle_internal_event(crate::events::AppEvent::HookMetadataReported {
                pane_id,
                source,
                agent_label,
                applies_to_source,
                title,
                display_agent,
                state_labels,
                clear_title: params.clear_title,
                clear_display_agent: params.clear_display_agent,
                clear_state_labels: params.clear_state_labels,
                seq: None,
                ttl,
            });
        }
        if token_changed {
            self.sync_agent_metadata_deadline();
            self.emit_pane_updated(ws_idx, pane_id);
            // Published tokens are durable now, so a token that changed and was
            // never saved would come back as its previous value on the next
            // restart.
            self.state.mark_session_dirty();
        }

        encode_success(id, ResponseResult::Ok {})
    }

    /// Records one file's cumulative agent-edit diff on the pane's terminal,
    /// replacing whatever that file last reported. See
    /// [`PaneReportEditDiffParams`] for the replace-not-append contract.
    pub(super) fn handle_pane_report_edit_diff(
        &mut self,
        id: String,
        params: PaneReportEditDiffParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return pane_not_found(id, &params.pane_id);
        };

        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };

        if params.clear_all {
            terminal.agent_edit_log.clear();
            return encode_success(id, ResponseResult::Ok {});
        }

        // Normalized like every other caller-supplied string on this API
        // (`normalize_presentation_text`, `normalize_metadata_tokens`): a raw
        // `file` is interpolated into the synthetic header line below, so a
        // newline or other control character in it would splice extra lines
        // into the parsed diff rather than name one file.
        let Some(file) = normalize_edit_diff_file(params.file) else {
            return encode_error(id, "invalid_edit_diff_file", "file must not be empty");
        };

        let diff_text = params.diff.unwrap_or_default();

        // The renderer starts a file's card — and the rail between two files —
        // off a literal `diff --git a/x b/x` line
        // (`crate::ui::diff_pane::build_diff_rows`), which is stricter than
        // `classify_diff_line`'s `FileHeader` tagging. GNU `diff -u` output,
        // which is what the capture hooks send, carries `---`/`+++` and never
        // that line, so without this a multi-file log would render as
        // unlabelled runs of hunks with no divider. Synthesized from the file
        // the request already names rather than asked of the hooks, so every
        // caller gets it. Skipped for an empty diff: that is the
        // clear-this-file path, and a header-only entry would never clear.
        if diff_text.is_empty() {
            terminal.agent_edit_log.set_or_clear(
                file,
                GitDiffText {
                    lines: Vec::new(),
                    truncated: false,
                },
            );
            return encode_success(id, ResponseResult::Ok {});
        }

        let synthesized = format!("diff --git a/{file} b/{file}\n{diff_text}");

        // Counted on the synthesized text, not on `diff_text` alone: the
        // header line is one of the lines the parser will keep, so a body of
        // exactly `MAX_EDIT_DIFF_LINES_PER_REQUEST` lines checked before
        // synthesis passed here and then lost its last line to the parser's
        // own cap — silently, under a doc promise that an over-long report is
        // rejected whole rather than stored short.
        let line_count = synthesized.lines().count();
        if line_count > MAX_EDIT_DIFF_LINES_PER_REQUEST {
            return encode_error(
                id,
                "invalid_edit_diff_too_large",
                format!(
                    "diff exceeds {MAX_EDIT_DIFF_LINES_PER_REQUEST} lines ({line_count} given, \
                     including the synthesized header line)"
                ),
            );
        }

        let (lines, truncated) = crate::workspace::parse_unified_diff_lines(&synthesized);
        debug_assert!(
            !truncated,
            "the request cap is GIT_DIFF_MAX_LINES and is checked on this exact text, so the \
             parser cannot have cut it short",
        );
        terminal
            .agent_edit_log
            .set_or_clear(file, GitDiffText { lines, truncated });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_clear_agent_authority(
        &mut self,
        id: String,
        params: PaneClearAgentAuthorityParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAuthorityCleared {
            pane_id,
            source: params.source,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_release_agent(
        &mut self,
        id: String,
        params: PaneReleaseAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
            pane_id,
            source: params.source,
            known_agent: crate::detect::parse_agent_label(&agent_label),
            agent_label,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_declare_agent(
        &mut self,
        id: String,
        params: PaneDeclareAgentParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let declared_agent = match params.agent.as_deref() {
            None => None,
            Some(agent) => {
                let Some(agent_label) = normalize_reported_agent_label(agent) else {
                    return invalid_agent(id);
                };
                let Some(agent) = crate::detect::parse_agent_label(&agent_label) else {
                    return encode_error(
                        id,
                        "unknown_agent",
                        format!("no agent detection manifest is bundled for {agent_label}"),
                    );
                };
                Some(agent)
            }
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if terminal.set_declared_agent(declared_agent) {
            if let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) {
                runtime.set_declared_agent(declared_agent);
            }
            self.state.mark_session_dirty();
        }
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_send_text(
        &mut self,
        id: String,
        params: PaneSendTextParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(params.text)) {
            return encode_error(id, "pane_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_input(
        &mut self,
        id: String,
        params: PaneSendInputParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let bytes = match super::super::api_helpers::encode_api_input(
            runtime,
            &params.text,
            &params.keys,
        ) {
            Ok(bytes) => bytes,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "pane_send_failed", err.to_string());
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_close(&mut self, id: String, target: PaneTarget) -> String {
        match self.close_pane(id.clone(), &target) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(response) => response,
        }
    }

    /// Close a pane; `Err` carries the encoded error response.
    pub(super) fn close_pane(&mut self, id: String, target: &PaneTarget) -> Result<(), String> {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return Err(pane_not_found(id, &target.pane_id));
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return Err(pane_not_found(id, &target.pane_id));
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        let layout_update_target = self.layout_update_target_after_pane_removal(ws_idx, pane_id);
        if self.state.close_pane_would_close_workspace(ws_idx, pane_id)
            && self.state.confirm_implicit_worktree_group_close(ws_idx)
        {
            return Err(encode_error(
                id,
                "confirmation_required",
                "closing this pane would close a worktree group",
            ));
        }
        let workspace_snapshot = self.workspace_info(ws_idx);
        let terminal_id = self.state.terminal_id_for_pane(ws_idx, pane_id);
        let should_close_workspace = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return Err(pane_not_found(id, &target.pane_id));
            };
            ws.close_pane(pane_id)
        };
        self.state.remove_plugin_pane_records([pane_id]);
        if should_close_workspace {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id,
                    workspace: Some(workspace_snapshot),
                },
            });
        } else {
            self.state.remove_unattached_terminal_ids(terminal_id);
            self.shutdown_detached_terminal_runtimes();
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id,
                },
            });
            if let Some((ws_idx, tab_idx)) = layout_update_target {
                self.emit_layout_updated_event(ws_idx, tab_idx);
            }
        }

        Ok(())
    }

    pub(super) fn handle_pane_send_keys(
        &mut self,
        id: String,
        params: PaneSendKeysParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let encoded_keys = match encode_api_keys(runtime, &params.keys) {
            Ok(encoded_keys) => encoded_keys,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        for bytes in encoded_keys {
            if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn normalize_presentation_text(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    let normalized: String = trimmed
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
}

/// Ceiling on a reported edited-file path, in characters. Set to the same
/// 4096 `crate::agent_resume` allows an agent session path rather than to
/// [`normalize_presentation_text`]'s 80 — this is a real path, not a label,
/// and 80 characters would cut most of them off mid-directory.
const MAX_EDIT_DIFF_FILE_CHARS: usize = 4096;

/// `pane.report_edit_diff`'s `file`, cleaned the same way every other
/// caller-supplied string on this API is: trimmed, stripped of control
/// characters, and bounded.
///
/// `None` when nothing is left, which is the `invalid_edit_diff_file` case.
///
/// The control-character strip is the load-bearing one. `file` is
/// interpolated into a synthetic `diff --git a/{file} b/{file}` line, so a
/// newline inside it would end that line early and splice the remainder into
/// the parsed diff as content — one report describing itself as two files, or
/// as a file plus a hunk. The path is still not resolved or checked against
/// the pane's cwd; it is display text, and only has to stay one line of it.
fn normalize_edit_diff_file(value: String) -> Option<String> {
    let normalized: String = value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_EDIT_DIFF_FILE_CHARS)
        .collect();
    let normalized = normalized.trim();
    (!normalized.is_empty()).then(|| normalized.to_string())
}

fn normalize_state_labels(
    labels: std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    labels
        .into_iter()
        .map(|(status, label)| {
            let status = status.trim().to_ascii_lowercase();
            if !matches!(
                status.as_str(),
                "idle" | "working" | "blocked" | "done" | "unknown"
            ) {
                return Err(status);
            }
            Ok(normalize_presentation_text(Some(label)).map(|label| (status, label)))
        })
        .filter_map(Result::transpose)
        .collect()
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

impl App {
    fn resolve_optional_pane(&self, pane_id: Option<&str>) -> Option<(usize, PaneId)> {
        match pane_id {
            Some(pane_id) => self.parse_pane_id(pane_id),
            None => {
                let ws_idx = self.state.active?;
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            }
        }
    }

    fn resolve_swap_source(&self, pane_id: Option<&str>) -> Option<(usize, PaneId)> {
        self.resolve_optional_pane(pane_id)
    }

    fn directional_pane_target(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        source_pane_id: PaneId,
        direction: PaneDirection,
    ) -> Option<PaneId> {
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        let panes = tab.layout.panes(self.state.view.terminal_area);
        let source = panes.iter().find(|pane| pane.id == source_pane_id)?;
        find_in_direction(source, direction.into(), &panes)
    }

    pub(super) fn pane_layout_snapshot(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<PaneLayoutSnapshot> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let area = self.state.view.terminal_area;
        let focused_pane_id = self.public_pane_id(ws_idx, tab.layout.focused())?;
        let panes = crate::ui::apply_pane_chrome(
            tab.layout.panes(area),
            self.state.pane_borders,
            self.state.pane_gaps,
        )
        .into_iter()
        .filter_map(|pane| {
            Some(PaneLayoutPane {
                pane_id: self.public_pane_id(ws_idx, pane.id)?,
                focused: pane.is_focused,
                rect: pane.rect.into(),
            })
        })
        .collect();
        let splits = tab
            .layout
            .splits(area)
            .into_iter()
            .enumerate()
            .map(|(idx, split)| PaneLayoutSplit {
                id: split_path_id(idx, &split.path),
                direction: match split.direction {
                    ratatui::layout::Direction::Horizontal => {
                        crate::api::schema::SplitDirection::Right
                    }
                    ratatui::layout::Direction::Vertical => {
                        crate::api::schema::SplitDirection::Down
                    }
                },
                ratio: split.ratio,
                rect: split.area.into(),
            })
            .collect();

        Some(PaneLayoutSnapshot {
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            zoomed: tab.zoomed,
            area: area.into(),
            focused_pane_id,
            panes,
            splits,
        })
    }

    pub(crate) fn emit_layout_updated_event(&mut self, ws_idx: usize, tab_idx: usize) {
        if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
            self.emit_layout_updated_snapshot(layout);
        }
    }

    pub(super) fn emit_layout_updated_snapshot(&mut self, layout: PaneLayoutSnapshot) {
        self.emit_event(EventEnvelope {
            event: EventKind::LayoutUpdated,
            data: EventData::LayoutUpdated { layout },
        });
    }

    pub(crate) fn layout_update_target_after_pane_removal(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<(usize, usize)> {
        let tab_idx = self
            .state
            .workspaces
            .get(ws_idx)?
            .find_tab_index_for_pane(pane_id)?;
        let pane_count = self
            .state
            .workspaces
            .get(ws_idx)?
            .tabs
            .get(tab_idx)?
            .layout
            .pane_count();
        (pane_count > 1).then_some((ws_idx, tab_idx))
    }
}

impl From<PaneDirection> for NavDirection {
    fn from(direction: PaneDirection) -> Self {
        match direction {
            PaneDirection::Left => NavDirection::Left,
            PaneDirection::Right => NavDirection::Right,
            PaneDirection::Up => NavDirection::Up,
            PaneDirection::Down => NavDirection::Down,
        }
    }
}

enum ResolvedPaneMoveDestination {
    ExistingTab {
        tab_id: String,
        target_pane_id: PaneId,
        split: crate::api::schema::SplitDirection,
        ratio: f32,
        cross_workspace: bool,
    },
    NewTab {
        workspace_id: String,
        label: Option<String>,
    },
    NewWorkspace {
        label: Option<String>,
        tab_label: Option<String>,
    },
}

struct PaneMoveRecoveryContext {
    source_ws_idx: usize,
    previous_workspace_id: String,
    previous_workspace_label: Option<String>,
    previous_tab_label: Option<String>,
    previous_worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    identity_cwd: std::path::PathBuf,
}

fn encode_unchanged_pane_move(
    id: String,
    reason: PaneMoveReason,
    previous_pane_id: String,
    previous_workspace_id: String,
    previous_tab_id: String,
    pane: PaneInfo,
    source_layout: Option<PaneLayoutSnapshot>,
    target_layout: PaneLayoutSnapshot,
) -> String {
    let focused_pane_id = target_layout.focused_pane_id.clone();
    encode_success(
        id,
        ResponseResult::PaneMove {
            move_result: PaneMoveResult {
                changed: false,
                reason: Some(reason),
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane: Box::new(pane),
                source_layout: source_layout.map(Box::new),
                target_layout: Box::new(target_layout),
                created_workspace: None,
                created_tab: None,
                closed_workspace_id: None,
                closed_tab_id: None,
                focused_pane_id,
            },
        },
    )
}

fn split_direction_to_layout(
    direction: crate::api::schema::SplitDirection,
) -> ratatui::layout::Direction {
    match direction {
        crate::api::schema::SplitDirection::Right => ratatui::layout::Direction::Horizontal,
        crate::api::schema::SplitDirection::Down => ratatui::layout::Direction::Vertical,
    }
}

impl From<ratatui::layout::Rect> for PaneLayoutRect {
    fn from(rect: ratatui::layout::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

fn split_path_id(idx: usize, path: &[bool]) -> String {
    if path.is_empty() {
        return format!("split_{idx}_root");
    }
    let path = path
        .iter()
        .map(|right| if *right { "1" } else { "0" })
        .collect::<Vec<_>>()
        .join("");
    format!("split_{idx}_{path}")
}

fn invalid_agent(id: String) -> String {
    encode_error(id, "invalid_agent", "agent label must not be empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, SplitDirection, SuccessResponse},
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_test_workspace() -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("metadata")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        (app, public_pane_id)
    }

    fn app_with_send_key_runtime(
        capacity: usize,
    ) -> (App, String, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, capacity);
        app.state.insert_test_runtime(pane_id, runtime);
        (app, public_pane_id, rx)
    }

    fn app_with_scrollback_runtime() -> (App, String, PaneId) {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let lines = (0..20)
            .map(|line| format!("line {line:02}\n"))
            .collect::<String>();
        let runtime = crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
            20,
            5,
            1000,
            lines.as_bytes(),
        );
        app.state.insert_test_runtime(pane_id, runtime);
        (app, public_pane_id, pane_id)
    }

    fn metadata_params(pane_id: String) -> PaneReportMetadataParams {
        PaneReportMetadataParams {
            pane_id,
            source: "user:metadata.test-1".into(),
            agent: None,
            applies_to_source: None,
            title: Some("activity".into()),
            display_agent: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
            clear_all_tokens: false,
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            seq: None,
            ttl_ms: None,
        }
    }

    fn metadata_error_code(response: &str) -> String {
        let response: ErrorResponse = serde_json::from_str(response).unwrap();
        response.error.code
    }

    #[tokio::test]
    async fn api_pane_send_keys_accepts_control_navigation_chords() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(4);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec![
                    "ctrl+h".into(),
                    "ctrl+j".into(),
                    "ctrl+k".into(),
                    "ctrl+l".into(),
                ],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x08]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0a]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0b]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0c]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_encodes_shift_tab_as_backtab() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["shift+tab".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"\x1b[Z"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_get_exposes_scroll_metrics() {
        let (mut app, public_pane_id, pane_id) = app_with_scrollback_runtime();
        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.scroll_up(3);

        let response = app.handle_pane_get(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        let scroll = pane.scroll.expect("scroll metrics");
        assert_eq!(scroll.offset_from_bottom, 3);
        assert!(scroll.max_offset_from_bottom >= scroll.offset_from_bottom);
        assert_eq!(scroll.viewport_rows, 5);
    }

    #[tokio::test]
    async fn api_pane_read_reports_when_older_rows_are_omitted() {
        let (mut app, public_pane_id, _pane_id) = app_with_scrollback_runtime();

        let response = app.handle_pane_read(
            "req".into(),
            PaneReadParams {
                pane_id: public_pane_id,
                source: crate::api::schema::ReadSource::Recent,
                lines: Some(2),
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
                intent: crate::api::schema::ReadIntent::Interactive,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneRead { read } = success.result else {
            panic!("expected pane read response");
        };
        assert!(read.text.contains("line 19"));
        assert!(read.truncated);
    }

    #[tokio::test]
    async fn api_pane_send_keys_preserves_legacy_control_c_aliases() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(3);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["C-c".into(), "c-c".into(), "ctrl+c".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_accepts_literal_plus() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["+".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"+"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_sends_shifted_punctuation_as_text_in_kitty_mode() {
        let (mut app, pane_id) = app_with_test_workspace();
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[>7u",
                1,
            );
        app.state.insert_test_runtime(internal_pane_id, runtime);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["shift+?".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"?"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_brackets_text_and_enter_atomically() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.lookup_runtime_sender(0, internal_pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004h");

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: "A != B".into(),
                keys: vec!["Enter".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[200~A != B\x1b[201~\r")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_keys_accept_key_combo_chords() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: String::new(),
                keys: vec!["ctrl+j".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0a]));
        assert!(rx.try_recv().is_err());
    }

    fn declared_agent_from_response(response: &str) -> Option<String> {
        let success: SuccessResponse = serde_json::from_str(response).unwrap();
        match success.result {
            crate::api::schema::ResponseResult::PaneInfo { pane } => pane.declared_agent,
            other => panic!("expected pane info, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn api_pane_declare_agent_records_and_clears_a_declaration() {
        let (mut app, pane_id) = app_with_test_workspace();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneDeclareAgent(PaneDeclareAgentParams {
                pane_id: pane_id.clone(),
                agent: Some("claude".into()),
            }),
        });
        assert_eq!(
            declared_agent_from_response(&response).as_deref(),
            Some("claude")
        );

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneDeclareAgent(PaneDeclareAgentParams {
                pane_id,
                agent: None,
            }),
        });
        assert_eq!(declared_agent_from_response(&response), None);
    }

    #[tokio::test]
    async fn api_pane_declare_agent_accepts_an_alias_and_stores_the_canonical_label() {
        let (mut app, pane_id) = app_with_test_workspace();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneDeclareAgent(PaneDeclareAgentParams {
                pane_id,
                agent: Some("claude-code".into()),
            }),
        });

        assert_eq!(
            declared_agent_from_response(&response).as_deref(),
            Some("claude")
        );
    }

    #[tokio::test]
    async fn api_pane_declare_agent_rejects_an_agent_with_no_manifest() {
        let (mut app, pane_id) = app_with_test_workspace();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneDeclareAgent(PaneDeclareAgentParams {
                pane_id: pane_id.clone(),
                agent: Some("not-an-agent".into()),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "unknown_agent");

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneDeclareAgent(PaneDeclareAgentParams {
                pane_id,
                agent: Some("   ".into()),
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_agent");
    }

    #[tokio::test]
    async fn api_pane_send_keys_rejects_invalid_keys_before_writing() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(2);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["ctrl+h".into(), "not-a-key".into()],
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert_eq!(error.error.message, "unsupported key not-a-key");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_rejects_prefix_bindings_before_writing_text_or_keys() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(4);
        let raw_key = " prefix+h ".to_string();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: "hello".into(),
                keys: vec!["ctrl+h".into(), raw_key.clone()],
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert_eq!(error.error.message, format!("unsupported key {raw_key}"));
        assert!(rx.try_recv().is_err());
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
        app.state.ensure_test_terminals();
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app
    }

    fn seed_terminal_states(app: &mut App) {
        for ws in &app.state.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    app.state
                        .terminals
                        .entry(pane.attached_terminal_id.clone())
                        .or_insert_with(|| {
                            crate::terminal::TerminalState::new(
                                pane.attached_terminal_id.clone(),
                                std::path::PathBuf::from("/herdr-test"),
                            )
                        });
                }
            }
        }
    }

    #[test]
    fn api_pane_close_closes_linked_worktree_workspace_only() {
        let mut app = app_with_linked_worktree();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_pane_close(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
    }

    /// A caller that says nothing gets no origin. Creation still succeeds —
    /// the field is optional on purpose, so a script that predates it keeps
    /// working — it just produces an unowned pane.
    #[test]
    fn an_absent_caller_records_no_origin() {
        let app = app_with_linked_worktree();
        assert_eq!(app.resolve_pane_origin(None), None);
        assert_eq!(app.resolve_pane_origin(Some("")), None);
        assert_eq!(app.resolve_pane_origin(Some("   ")), None);
    }

    /// The ordinary case: the caller names a live pane, and the record carries
    /// both that pane and the Space it was standing in.
    #[test]
    fn a_live_caller_records_its_pane_and_its_workspace() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.ensure_test_terminals();
        let root_public = app.public_pane_id(0, root).unwrap();

        let origin = app
            .resolve_pane_origin(Some(&root_public))
            .expect("a live caller resolves");

        assert_eq!(origin.pane_id, root_public);
        assert_eq!(origin.workspace_id, app.public_workspace_id(0));
    }

    /// The case that must not be papered over: a caller naming a pane that has
    /// already closed records **nothing**. A dangling parent is worse than no
    /// parent, because the tree would nest rows under something absent.
    #[test]
    fn a_stale_caller_records_nothing_rather_than_a_dangling_parent() {
        let app = app_with_linked_worktree();
        assert_eq!(app.resolve_pane_origin(Some("w9:p9")), None);
        assert_eq!(app.resolve_pane_origin(Some("p_4242")), None);
    }

    #[test]
    fn api_pane_current_prefers_caller_pane_id() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams {
                caller_pane_id: Some(right_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneCurrent { pane } = success.result else {
            panic!("expected pane current response");
        };
        assert_eq!(pane.pane_id, right_public);
        assert!(!pane.focused);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_ne!(pane.pane_id, root_public);
    }

    #[test]
    fn api_pane_current_falls_back_to_focused_pane() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams::default(),
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneCurrent { pane } = success.result else {
            panic!("expected pane current response");
        };
        assert_eq!(pane.pane_id, root_public);
        assert!(pane.focused);
    }

    #[test]
    fn api_pane_current_dispatches_through_socket_request() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneCurrent(
                crate::api::schema::PaneCurrentParams::default(),
            ),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneCurrent { pane } = success.result else {
            panic!("expected pane current response");
        };
        assert_eq!(pane.pane_id, root_public);
    }

    #[test]
    fn api_pane_current_reports_invalid_caller_pane_id() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams {
                caller_pane_id: Some("missing".into()),
            },
        );

        assert_eq!(metadata_error_code(&response), "pane_not_found");
    }

    #[test]
    fn api_pane_current_reports_no_active_pane() {
        let mut app = app_with_linked_worktree();
        app.state.active = None;

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams::default(),
        );

        assert_eq!(metadata_error_code(&response), "pane_not_found");
    }

    #[test]
    fn api_pane_swap_explicit_source_and_target_preserves_focus_and_returns_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(swap.changed);
        assert_eq!(swap.reason, None);
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.focused_pane_id, swap.source_pane_id);
        assert_eq!(swap.layout.focused_pane_id, swap.source_pane_id);
        assert_eq!(swap.layout.panes.len(), 2);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
    }

    #[test]
    fn api_pane_swap_unfocused_source_updates_last_pane_history() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let focused = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(focused);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public),
                target_pane_id: Some(target_public),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(swap.changed);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));

        app.state.last_pane();

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(focused));
    }

    #[test]
    fn api_pane_swap_direction_no_neighbor_returns_unchanged_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                pane_id: Some(source_public.clone()),
                direction: Some(PaneDirection::Left),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NoNeighbor));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, None);
        assert_eq!(swap.layout.panes.len(), 1);
        assert!(app.event_hub.events_after(0).is_empty());
    }

    #[test]
    fn api_pane_swap_explicit_missing_target_returns_not_found_noop() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some("missing-pane".into()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NotFound));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some("missing-pane".into()));
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_missing_source_returns_not_found_noop() {
        let mut app = app_with_linked_worktree();
        let target = app.state.workspaces[0].tabs[0].root_pane;
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some("missing-pane".into()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NotFound));
        assert_eq!(swap.source_pane_id, "missing-pane");
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_cross_workspace_preserves_target_id() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::CrossTab));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.layout.workspace_id, app.public_workspace_id(0));
    }

    #[test]
    fn api_pane_move_to_existing_tab_preserves_internal_pane_and_terminal() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_tab_public = app.public_tab_id(0, 0).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public.clone(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: Some(0.25),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.reason, None);
        assert_eq!(move_result.previous_pane_id, source_public);
        assert_eq!(move_result.previous_tab_id, source_tab_public);
        assert_eq!(move_result.pane.pane_id, move_result.previous_pane_id);
        assert_eq!(move_result.pane.tab_id, target_tab_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(move_result.closed_tab_id, Some(source_tab_public));
        assert_eq!(move_result.closed_workspace_id, None);
        assert_eq!(move_result.target_layout.panes.len(), 2);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), source);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
    }

    #[test]
    fn api_pane_move_focuses_copy_mode_pane_back_into_copy_mode() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        seed_terminal_states(&mut app);
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.copy_mode.expect("copy mode").pane_id, source);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), source);
    }

    #[tokio::test]
    async fn key_release_follows_pane_moved_across_workspaces() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[>15u",
                2,
            );
        app.terminal_runtimes.insert(source_terminal_id, runtime);
        app.state.workspaces.push(Workspace::test_new("other"));
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.mode = Mode::Terminal;
        let source_public = app.public_pane_id(0, source).unwrap();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let target_pane_id = app.public_pane_id(1, target).unwrap();

        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('j'),
                    crossterm::event::KeyModifiers::empty(),
                ),
            )],
            false,
        );
        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id,
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::PaneMove { .. }));
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('j'),
                    crossterm::event::KeyModifiers::empty(),
                )
                .with_kind(crossterm::event::KeyEventKind::Release),
            )],
            false,
        );

        assert_eq!(
            rx.try_recv().expect("forwarded press"),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.try_recv().expect("forwarded release after pane move"),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(app.input_leases.is_empty());
    }

    #[test]
    fn api_pane_move_to_existing_tab_across_workspace_reassigns_public_pane_id() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        app.state
            .terminals
            .get_mut(&source_terminal)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let previous_pane_id = app.public_pane_id(0, source).unwrap();
        let previous_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(1);
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let target_pane_id = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: previous_pane_id.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id.clone(),
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.previous_pane_id, previous_pane_id);
        assert_eq!(move_result.previous_workspace_id, previous_workspace_id);
        assert_eq!(move_result.closed_workspace_id, Some(previous_workspace_id));
        assert_ne!(move_result.pane.pane_id, move_result.previous_pane_id);
        assert!(move_result
            .pane
            .pane_id
            .starts_with(&format!("{target_workspace_id}:p")));
        assert_eq!(move_result.pane.workspace_id, target_workspace_id);
        assert_eq!(move_result.pane.tab_id, target_tab_id);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert_eq!(app.parse_pane_id(&previous_pane_id), Some((0, source)));
        assert!(matches!(
            app.resolve_agent_target(&previous_pane_id),
            Err(crate::app::terminal_targets::TerminalTargetError::NotFound { .. })
        ));
        assert!(app.resolve_agent_target(&move_result.pane.pane_id).is_ok());
    }

    #[test]
    fn api_pane_move_legacy_target_tab_id_survives_source_workspace_removal() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(1);
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: "t_2_1".into(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(move_result.closed_workspace_id, Some(source_workspace_id));
        assert_eq!(move_result.pane.workspace_id, target_workspace_id);
        assert_eq!(move_result.pane.tab_id, target_tab_id);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
    }

    #[test]
    fn api_pane_move_to_new_tab_creates_tab_without_spawning_terminal() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(
            move_result
                .created_tab
                .as_ref()
                .map(|tab| tab.label.as_str()),
            Some("moved")
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(true)
        );
        assert_eq!(move_result.closed_tab_id, None);
        assert_eq!(move_result.pane.pane_id, source_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].terminal_id(right).is_some());
        assert_eq!(
            app.state.workspaces[0].tabs[1].terminal_id(source),
            Some(&source_terminal)
        );
        let envelopes = app.event_hub.events_after(0);
        let events: Vec<_> = envelopes
            .iter()
            .map(|(_, envelope)| envelope.event)
            .collect();
        assert_eq!(
            events,
            vec![
                EventKind::TabCreated,
                EventKind::PaneMoved,
                EventKind::LayoutUpdated,
                EventKind::LayoutUpdated,
            ]
        );
        match &envelopes[0].1.data {
            EventData::TabCreated { tab } => assert!(tab.focused),
            other => panic!("expected tab created event, got {other:?}"),
        }
        assert!(matches!(
            &envelopes[2].1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap()
        ));
        assert!(matches!(
            &envelopes[3].1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 1).unwrap()
        ));
    }

    #[test]
    fn api_pane_move_only_pane_to_new_tab_uses_app_render_handles() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_notify,
            &app.render_notify
        ));
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_dirty,
            &app.render_dirty
        ));
    }

    #[test]
    fn api_pane_move_to_new_workspace_closes_empty_source_workspace() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_workspace = app.public_workspace_id(0);

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("promoted".into()),
                    tab_label: Some("main".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.closed_workspace_id, Some(source_workspace));
        assert_eq!(
            move_result
                .created_workspace
                .as_ref()
                .map(|ws| ws.label.as_str()),
            Some("promoted")
        );
        assert_eq!(
            move_result.created_workspace.as_ref().map(|ws| ws.focused),
            Some(true)
        );
        assert_eq!(
            move_result
                .created_tab
                .as_ref()
                .map(|tab| tab.label.as_str()),
            Some("main")
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(true)
        );
        assert_ne!(move_result.pane.pane_id, source_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_notify,
            &app.render_notify
        ));
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_dirty,
            &app.render_dirty
        ));
        let envelopes = app.event_hub.events_after(0);
        let events: Vec<_> = envelopes
            .iter()
            .map(|(_, envelope)| envelope.event)
            .collect();
        assert_eq!(
            events,
            vec![
                EventKind::TabClosed,
                EventKind::WorkspaceClosed,
                EventKind::WorkspaceCreated,
                EventKind::TabCreated,
                EventKind::PaneMoved,
                EventKind::LayoutUpdated,
            ]
        );
        match &envelopes[2].1.data {
            EventData::WorkspaceCreated { workspace } => assert!(workspace.focused),
            other => panic!("expected workspace created event, got {other:?}"),
        }
        match &envelopes[3].1.data {
            EventData::TabCreated { tab } => assert!(tab.focused),
            other => panic!("expected tab created event, got {other:?}"),
        }
        match &envelopes[5].1.data {
            EventData::LayoutUpdated { layout } => assert_eq!(
                layout.tab_id,
                app.public_tab_id(0, 0)
                    .expect("created workspace should have a first tab")
            ),
            other => panic!("expected layout updated event, got {other:?}"),
        }
    }

    #[test]
    fn api_pane_move_same_tab_returns_same_tab_noop() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_tab = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: source_tab,
                    target_pane_id: None,
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(!move_result.changed);
        assert_eq!(move_result.reason, Some(PaneMoveReason::SameTab));
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn api_pane_move_rejects_target_pane_outside_target_tab() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let other_tab = app.state.workspaces[0].test_add_tab(Some("other"));
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let wrong_target = app
            .public_pane_id(0, app.state.workspaces[0].tabs[other_tab].root_pane)
            .unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(wrong_target),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "target_pane_not_found");
        assert_eq!(app.state.workspaces[0].tabs.len(), 3);
    }

    #[test]
    fn api_pane_move_existing_tab_no_focus_preserves_previous_target_focus() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let previously_focused = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].active_tab = target_tab;
        let explicit_target =
            app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[target_tab]
            .layout
            .focus_pane(previously_focused);
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let explicit_target_public = app.public_pane_id(0, explicit_target).unwrap();
        let previously_focused_public = app.public_pane_id(0, previously_focused).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(explicit_target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.focused_pane_id, previously_focused_public);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            previously_focused
        );
    }

    #[test]
    fn api_pane_move_recovery_restores_removed_source_workspace() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let previous_workspace_id = app.public_workspace_id(0);
        let context = PaneMoveRecoveryContext {
            source_ws_idx: 0,
            previous_workspace_id: previous_workspace_id.clone(),
            previous_workspace_label: app.state.workspaces[0].custom_name.clone(),
            previous_tab_label: app.state.workspaces[0].tabs[0].custom_name.clone(),
            previous_worktree_space: app.state.workspaces[0].worktree_space.clone(),
            identity_cwd: app.state.workspaces[0].identity_cwd.clone(),
        };
        let taken = app.state.workspaces[0]
            .take_pane_for_move(source)
            .expect("source pane should be movable");
        app.state.workspaces.remove(0);
        app.state.active = None;
        app.state.selected = 0;

        app.recover_failed_pane_move(context, taken.moved);

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].id, previous_workspace_id);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert_eq!(
            app.parse_pane_id(&format!("{previous_workspace_id}:p1")),
            Some((0, source))
        );
    }

    #[test]
    fn api_pane_move_to_zoomed_target_returns_target_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].tabs[target_tab].zoomed = true;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public.clone(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(!move_result.changed);
        assert_eq!(move_result.reason, Some(PaneMoveReason::ZoomedTab));
        assert_eq!(move_result.target_layout.tab_id, target_tab_public);
        assert_eq!(
            move_result
                .source_layout
                .as_ref()
                .map(|layout| layout.tab_id.as_str()),
            app.public_tab_id(0, 0).as_deref()
        );
    }

    #[test]
    fn api_pane_zoom_current_toggles_zoom() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let _right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom("req".into(), PaneZoomParams::default());

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, None);
        assert_eq!(zoom.pane_id, root_public);
        assert_eq!(zoom.focused_pane_id, zoom.pane_id);
        assert!(zoom.zoomed);
        assert!(zoom.layout.zoomed);
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap() && layout.zoomed
        ));

        let response = app.handle_pane_zoom("req".into(), PaneZoomParams::default());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(!zoom.zoomed);
        assert!(!zoom.layout.zoomed);
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap() && !layout.zoomed
        ));
    }

    #[test]
    fn api_pane_zoom_explicit_background_pane_updates_focus_history() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let _other = app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(first);
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(target_public.clone()),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(zoom.focus_changed);
        assert_eq!(zoom.pane_id, target_public);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
        assert!(app.state.workspaces[1].tabs[0].zoomed);

        app.state.last_pane();

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(first));
    }

    #[test]
    fn api_pane_zoom_focuses_copy_mode_pane_back_into_copy_mode() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let _other = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let _target_other =
            app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[1].tabs[0].layout.focus_pane(target);
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.mode = Mode::Terminal;
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(source_public),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.focus_changed);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.mode, Mode::Copy);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
    }

    #[test]
    fn api_pane_zoom_single_pane_returns_noop() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::Toggle,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::SinglePane));
        assert_eq!(zoom.pane_id, root_public);
        assert!(!zoom.zoomed);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
    }

    #[test]
    fn api_pane_zoom_on_and_off_are_idempotent() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let _right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::On,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::On,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyZoomed));
        assert!(zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public),
                mode: PaneZoomMode::Off,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(!zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: None,
                mode: PaneZoomMode::Off,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyUnzoomed));
        assert!(!zoom.zoomed);
    }

    #[test]
    fn api_pane_zoom_idempotent_mode_reports_focus_change() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        app.state.workspaces[0].tabs[0].zoomed = true;
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(right_public),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyZoomed));
        assert!(zoom.zoomed);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.focused_pane_id == app.public_pane_id(0, right).unwrap()
        ));
    }

    #[test]
    fn api_pane_zoom_params_serialize_modes() {
        let request = crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneZoom(PaneZoomParams {
                pane_id: Some("issue-1".into()),
                mode: PaneZoomMode::On,
            }),
        };

        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"method\":\"pane.zoom\""));
        assert!(encoded.contains("\"mode\":\"on\""));

        let decoded: crate::api::schema::Request = serde_json::from_str(&encoded).unwrap();
        let crate::api::schema::Method::PaneZoom(params) = decoded.method else {
            panic!("expected pane zoom request");
        };
        assert_eq!(params.pane_id, Some("issue-1".into()));
        assert_eq!(params.mode, PaneZoomMode::On);
    }

    #[test]
    fn api_pane_layout_returns_public_ids_rects_and_splits() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_layout(
            "req".into(),
            crate::api::schema::PaneLayoutParams {
                pane_id: Some(root_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneLayout { layout } = success.result else {
            panic!("expected pane layout response");
        };
        assert_eq!(layout.focused_pane_id, root_public);
        assert!(layout.panes.iter().any(|pane| pane.pane_id == root_public));
        assert!(layout.panes.iter().any(|pane| pane.pane_id == right_public));
        assert_eq!(layout.splits.len(), 1);
        assert_eq!(
            layout.splits[0].direction,
            crate::api::schema::SplitDirection::Right
        );
    }

    #[test]
    fn api_pane_neighbor_returns_directional_neighbor_public_id() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_neighbor(
            "req".into(),
            crate::api::schema::PaneNeighborParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneNeighbor { neighbor } = success.result else {
            panic!("expected pane neighbor response");
        };
        assert_eq!(neighbor.pane_id, root_public);
        assert_eq!(neighbor.direction, PaneDirection::Right);
        assert_eq!(neighbor.neighbor_pane_id, Some(right_public));
    }

    #[test]
    fn api_pane_edges_reports_physical_layout_edges() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_edges(
            "req".into(),
            crate::api::schema::PaneEdgesParams {
                pane_id: Some(right_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneEdges { edges } = success.result else {
            panic!("expected pane edges response");
        };
        assert_eq!(edges.pane_id, right_public);
        assert!(!edges.left);
        assert!(edges.right);
        assert!(edges.up);
        assert!(edges.down);
    }

    #[test]
    fn api_pane_resize_changes_target_ratio_without_changing_focus() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(right);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_resize(
            "req".into(),
            crate::api::schema::PaneResizeParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
                amount: Some(0.1),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneResize { resize } = success.result else {
            panic!("expected pane resize response");
        };
        assert!(resize.changed);
        assert_eq!(resize.reason, None);
        assert_eq!(resize.pane_id, root_public);
        assert_eq!(resize.focused_pane_id, right_public);
        assert_eq!(resize.layout.focused_pane_id, right_public);
        assert!((resize.layout.splits[0].ratio - 0.6).abs() < f32::EPSILON);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap()
                    && (layout.splits[0].ratio - 0.6).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn api_pane_focus_direction_focuses_neighbor() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_focus_direction(
            "req".into(),
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneFocusDirection { focus } = success.result else {
            panic!("expected pane focus direction response");
        };
        assert!(focus.changed);
        assert_eq!(focus.reason, None);
        assert_eq!(focus.source_pane_id, root_public);
        assert_eq!(focus.focused_pane_id, Some(right_public.clone()));
        assert_eq!(focus.layout.focused_pane_id, right_public);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn api_pane_focus_focuses_direct_target_across_tabs_and_workspaces() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let target_tab_idx = app.state.workspaces[1].test_add_tab(Some("target"));
        app.state.workspaces[1].switch_tab(target_tab_idx);
        let target_pane = app.state.workspaces[1].tabs[target_tab_idx].root_pane;
        app.state.ensure_test_terminals();
        let target_public = app.public_pane_id(1, target_pane).unwrap();
        app.state.switch_workspace(0);
        assert_eq!(app.state.active, Some(0));

        let response = app.handle_pane_focus(
            "req".into(),
            crate::api::schema::PaneTarget {
                pane_id: target_public.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        assert_eq!(pane.pane_id, target_public);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab, target_tab_idx);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert_eq!(app.state.mode, Mode::Terminal);
    }

    #[test]
    fn api_pane_focus_marks_already_focused_done_pane_seen() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state.terminals.get_mut(&terminal_id).unwrap().state = crate::detect::AgentState::Idle;
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let response = app.handle_pane_focus(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Idle);
    }

    #[test]
    fn api_pane_focus_rejects_invalid_pane_id() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_pane_focus(
            "req".into(),
            crate::api::schema::PaneTarget {
                pane_id: "pane_missing".into(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "pane_not_found");
    }

    #[test]
    fn api_pane_focus_direction_no_neighbor_is_noop() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_focus_direction(
            "req".into(),
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Left,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneFocusDirection { focus } = success.result else {
            panic!("expected pane focus direction response");
        };
        assert!(!focus.changed);
        assert_eq!(focus.reason, Some(PaneFocusDirectionReason::NoNeighbor));
        assert_eq!(focus.source_pane_id, root_public.clone());
        assert_eq!(focus.focused_pane_id, Some(root_public));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
    }

    #[test]
    fn pane_metadata_tokens_patch_and_clear_through_dispatcher() {
        let (mut app, pane_id) = app_with_test_workspace();
        for (tokens, expected) in [
            (
                std::collections::HashMap::from([
                    ("summary".into(), Some("reviewing auth".into())),
                    ("model".into(), Some("opus".into())),
                ]),
                std::collections::HashMap::from([
                    ("summary".into(), "reviewing auth".into()),
                    ("model".into(), "opus".into()),
                ]),
            ),
            (
                std::collections::HashMap::from([
                    ("summary".into(), Some("done".into())),
                    ("model".into(), None),
                ]),
                std::collections::HashMap::from([("summary".into(), "done".into())]),
            ),
        ] {
            let mut params = metadata_params(pane_id.clone());
            params.title = None;
            params.tokens = tokens;
            let response = app.handle_api_request(crate::api::schema::Request {
                id: "set".into(),
                method: crate::api::schema::Method::PaneReportMetadata(params),
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(success.result, ResponseResult::Ok {});

            let response = app.handle_api_request(crate::api::schema::Request {
                id: "get".into(),
                method: crate::api::schema::Method::PaneGet(PaneTarget {
                    pane_id: pane_id.clone(),
                }),
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::PaneInfo { pane } = success.result else {
                panic!("expected pane info");
            };
            assert_eq!(pane.tokens, expected);
        }
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert!(app.state.terminals[&terminal_id].agent_metadata.is_empty());
    }

    #[test]
    fn pane_tokens_are_independent_from_presentation_guards() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let mut params = metadata_params(pane_id);
        params.title = None;
        params.agent = Some("codex".into());
        params.tokens =
            std::collections::HashMap::from([("summary".into(), Some("global".into()))]);

        let response = app.handle_pane_report_metadata("guarded".into(), params);

        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id].metadata_tokens.values(),
            std::collections::HashMap::from([("summary".into(), "global".into())])
        );
    }

    #[test]
    fn pane_metadata_uses_one_sequence_for_presentation_and_tokens() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut presentation = metadata_params(pane_id.clone());
        presentation.seq = Some(10);
        let response = app.handle_pane_report_metadata("presentation".into(), presentation);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut stale_token = metadata_params(pane_id.clone());
        stale_token.title = None;
        stale_token.tokens =
            std::collections::HashMap::from([("summary".into(), Some("stale".into()))]);
        stale_token.seq = Some(9);
        let response = app.handle_pane_report_metadata("stale".into(), stale_token);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert!(app.state.terminals[&terminal_id]
            .metadata_tokens
            .values()
            .is_empty());
    }

    #[test]
    fn pane_metadata_ignored_after_process_exit_does_not_poison_sequence() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);

        let mut initial = metadata_params(pane_id.clone());
        initial.source = "custom:pi-metadata".into();
        initial.agent = Some("pi".into());
        initial.seq = Some(100);
        let response = app.handle_pane_report_metadata("initial".into(), initial);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut initial_tokens = metadata_params(pane_id.clone());
        initial_tokens.source = "custom:pi-tokens".into();
        initial_tokens.agent = Some("pi".into());
        initial_tokens.title = None;
        initial_tokens.tokens =
            std::collections::HashMap::from([("generation".into(), Some("old".into()))]);
        initial_tokens.seq = Some(100);
        let response = app.handle_pane_report_metadata("initial-tokens".into(), initial_tokens);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let exit_at = std::time::Instant::now() + std::time::Duration::from_millis(1);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                false,
                false,
                true,
                exit_at,
            );
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                None,
                AgentState::Unknown,
                false,
                false,
                false,
                false,
                exit_at + std::time::Duration::from_millis(1),
            );

        let mut stale = metadata_params(pane_id.clone());
        stale.source = "custom:pi-metadata".into();
        stale.agent = Some("pi".into());
        stale.title = Some("stale".into());
        stale.seq = Some(200);
        let response = app.handle_pane_report_metadata("stale".into(), stale);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut official = metadata_params(pane_id.clone());
        official.source = "herdr:pi".into();
        official.seq = Some(200);
        let response = app.handle_pane_report_metadata("official".into(), official);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.metadata_report_sequence_is_fresh("custom:pi-metadata", Some(1)));
        assert!(terminal.metadata_report_sequence_is_fresh("custom:pi-tokens", Some(1)));
        assert!(terminal.metadata_report_sequence_is_fresh("herdr:pi", Some(1)));

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                false,
                false,
                false,
                exit_at + std::time::Duration::from_millis(2),
            );
        let mut fresh = metadata_params(pane_id.clone());
        fresh.source = "custom:pi-metadata".into();
        fresh.agent = Some("pi".into());
        fresh.title = Some("fresh".into());
        fresh.seq = Some(1);
        let response = app.handle_pane_report_metadata("fresh".into(), fresh);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut fresh_tokens = metadata_params(pane_id);
        fresh_tokens.source = "custom:pi-tokens".into();
        fresh_tokens.agent = Some("pi".into());
        fresh_tokens.title = None;
        fresh_tokens.tokens =
            std::collections::HashMap::from([("generation".into(), Some("new".into()))]);
        fresh_tokens.seq = Some(1);
        let response = app.handle_pane_report_metadata("fresh-tokens".into(), fresh_tokens);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.agent_metadata["custom:pi-metadata"]
                .title
                .as_deref(),
            Some("fresh")
        );
        assert_eq!(
            terminal
                .metadata_tokens
                .values()
                .get("generation")
                .map(String::as_str),
            Some("new")
        );
    }

    /// The revoke path for a token that never expires: a caller who does not
    /// know the key, and cannot get the original publisher to cooperate, still
    /// has to be able to clear it.
    #[test]
    fn pane_report_metadata_clear_all_tokens_revokes_a_token_that_never_expires() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        let mut published = metadata_params(public_pane_id.clone());
        published.title = None;
        published.tokens = std::collections::HashMap::from([
            ("owner".into(), Some("firstmate".into())),
            ("summary".into(), Some("review".into())),
        ]);
        let response = app.handle_pane_report_metadata("publish".into(), published);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        // No TTL was given, so nothing will ever sweep these on its own.
        assert!(app.state.terminals[&terminal_id]
            .metadata_tokens
            .next_expiry()
            .is_none());

        // A different source, naming no keys at all, revokes the lot.
        let mut revoke = metadata_params(public_pane_id);
        revoke.title = None;
        revoke.source = "user:operator".into();
        revoke.clear_all_tokens = true;
        let response = app.handle_pane_report_metadata("revoke".into(), revoke);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert!(app.state.terminals[&terminal_id]
            .metadata_tokens
            .values()
            .is_empty());
        assert!(app.state.session_dirty, "the revoke has to reach disk");
    }

    #[test]
    fn pane_report_metadata_clear_all_tokens_replaces_rather_than_merges() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        let mut published = metadata_params(public_pane_id.clone());
        published.title = None;
        published.tokens =
            std::collections::HashMap::from([("owner".into(), Some("stale".into()))]);
        app.handle_pane_report_metadata("publish".into(), published);

        let mut replace = metadata_params(public_pane_id);
        replace.title = None;
        replace.clear_all_tokens = true;
        replace.tokens = std::collections::HashMap::from([("summary".into(), Some("new".into()))]);
        let response = app.handle_pane_report_metadata("replace".into(), replace);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.state.terminals[&terminal_id].metadata_tokens.values(),
            std::collections::HashMap::from([("summary".into(), "new".into())])
        );
    }

    #[test]
    fn pane_report_metadata_accepts_documented_source_chars_and_max_ttl() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = metadata_params(pane_id);
        params.ttl_ms = Some(METADATA_TTL_MAX_MS);

        let response = app.handle_pane_report_metadata("req".into(), params);

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
    }

    #[test]
    fn pane_report_metadata_rejects_invalid_source_shape() {
        let (mut app, pane_id) = app_with_test_workspace();
        for source in ["", "user metadata", "user/metadata", "user:\u{7f}metadata"] {
            let mut params = metadata_params(pane_id.clone());
            params.source = source.into();

            let response = app.handle_pane_report_metadata("req".into(), params);

            assert_eq!(metadata_error_code(&response), "invalid_metadata_source");
        }
    }

    #[test]
    fn pane_report_metadata_rejects_long_source() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = metadata_params(pane_id);
        params.source = "a".repeat(METADATA_SOURCE_MAX_CHARS + 1);

        let response = app.handle_pane_report_metadata("req".into(), params);

        assert_eq!(metadata_error_code(&response), "invalid_metadata_source");
    }

    #[test]
    fn pane_report_metadata_rejects_invalid_applies_to_source() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = metadata_params(pane_id);
        params.applies_to_source = Some("herdr source".into());

        let response = app.handle_pane_report_metadata("req".into(), params);

        assert_eq!(metadata_error_code(&response), "invalid_metadata_source");
    }

    #[test]
    fn pane_report_metadata_rejects_ttl_outside_supported_range() {
        let (mut app, pane_id) = app_with_test_workspace();
        for ttl_ms in [0, METADATA_TTL_MAX_MS + 1] {
            let mut params = metadata_params(pane_id.clone());
            params.ttl_ms = Some(ttl_ms);

            let response = app.handle_pane_report_metadata("req".into(), params);

            assert_eq!(metadata_error_code(&response), "invalid_metadata_ttl");
        }
    }

    // -----------------------------------------------------------------
    // pane.report_edit_diff
    // -----------------------------------------------------------------

    fn edit_diff_params(pane_id: String) -> PaneReportEditDiffParams {
        PaneReportEditDiffParams {
            pane_id,
            file: "src/foo.rs".into(),
            diff: Some("--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -0,0 +1 @@\n+fn main() {}\n".into()),
            clear_all: false,
        }
    }

    /// The pane's agent-edit log, reached the way production does: pane id ->
    /// attached terminal -> that terminal's log.
    fn agent_edit_log<'a>(app: &'a App, pane_id: &str) -> &'a crate::agent_edit_log::AgentEditLog {
        let (ws_idx, resolved_pane_id) = app.parse_pane_id(pane_id).unwrap();
        let terminal_id = app.state.workspaces[ws_idx]
            .pane_state(resolved_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        &app.state
            .terminals
            .get(&terminal_id)
            .unwrap()
            .agent_edit_log
    }

    #[test]
    fn pane_report_edit_diff_stores_the_file_entry() {
        let (mut app, pane_id) = app_with_test_workspace();
        let params = edit_diff_params(pane_id.clone());

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert!(!agent_edit_log(&app, &pane_id).is_empty());
    }

    #[test]
    fn pane_report_edit_diff_replaces_not_appends_same_file() {
        let (mut app, pane_id) = app_with_test_workspace();
        app.handle_pane_report_edit_diff("req1".into(), edit_diff_params(pane_id.clone()));

        let mut second = edit_diff_params(pane_id.clone());
        second.diff =
            Some("--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -0,0 +1 @@\n+fn other() {}\n".into());
        app.handle_pane_report_edit_diff("req2".into(), second);

        let joined = agent_edit_log(&app, &pane_id)
            .flatten()
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("+fn other() {}"), "joined: {joined}");
        assert!(!joined.contains("+fn main() {}"), "joined: {joined}");
    }

    #[test]
    fn pane_report_edit_diff_with_empty_diff_clears_the_file() {
        let (mut app, pane_id) = app_with_test_workspace();
        app.handle_pane_report_edit_diff("req1".into(), edit_diff_params(pane_id.clone()));

        let mut clearing = edit_diff_params(pane_id.clone());
        clearing.diff = Some(String::new());
        app.handle_pane_report_edit_diff("req2".into(), clearing);

        assert!(agent_edit_log(&app, &pane_id).is_empty());
    }

    #[test]
    fn pane_report_edit_diff_clear_all_empties_the_whole_log() {
        let (mut app, pane_id) = app_with_test_workspace();
        app.handle_pane_report_edit_diff("req1".into(), edit_diff_params(pane_id.clone()));

        let mut clear_all = edit_diff_params(pane_id.clone());
        clear_all.clear_all = true;
        clear_all.diff = None;
        app.handle_pane_report_edit_diff("req2".into(), clear_all);

        assert!(agent_edit_log(&app, &pane_id).is_empty());
    }

    /// Hook-fed diffs are GNU `diff -u` output with no `diff --git` line, and
    /// the Changes zone starts a file's card — and the rail between two files
    /// — off exactly that line. This drives two files through the real
    /// handler and then asks the renderer's own row walk
    /// (`diff_overlay_anchors`, which shares `build_diff_rows` with the drawn
    /// text) what boundaries it found, rather than hand-writing a header into
    /// a fixture the production path would never produce.
    #[test]
    fn reported_non_git_diffs_still_render_one_card_per_file_and_a_rail_between() {
        let (mut app, pane_id) = app_with_test_workspace();
        app.state.active = Some(0);

        for path in ["/tmp/a.rs", "/tmp/b.rs"] {
            let mut params = edit_diff_params(pane_id.clone());
            params.file = path.into();
            // `diff -u --label` output: `---`/`+++`, never `diff --git`.
            params.diff = Some(format!("--- {path}\n+++ {path}\n@@ -1 +1 @@\n-old\n+new\n"));
            let response = app.handle_pane_report_edit_diff("req".into(), params);
            let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        }

        let anchors = crate::ui::diff_pane::diff_overlay_anchors(
            &app.state,
            ratatui::layout::Rect::new(0, 0, 40, 20),
        )
        .expect("two reported files must render as diff content");

        assert_eq!(
            anchors
                .file_rows
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>(),
            vec!["/tmp/a.rs".to_string(), "/tmp/b.rs".to_string()],
            "each reported file must get its own header card"
        );
        assert_eq!(
            anchors.rail_rows.len(),
            1,
            "two files have exactly one boundary to stand a rail on"
        );
    }

    #[test]
    fn pane_report_edit_diff_rejects_unknown_pane() {
        let (mut app, _pane_id) = app_with_test_workspace();
        let params = edit_diff_params("h-does-not-exist".into());

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "pane_not_found");
    }

    #[test]
    fn pane_report_edit_diff_rejects_oversized_diff() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = edit_diff_params(pane_id.clone());
        params.diff = Some("+x\n".repeat(MAX_EDIT_DIFF_LINES_PER_REQUEST + 1));

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_edit_diff_too_large");
        assert!(agent_edit_log(&app, &pane_id).is_empty());
    }

    /// The cap counts the synthesized `diff --git` line, because the parser
    /// does. A body one line short of the cap therefore fills it exactly, and
    /// every one of its lines has to survive.
    #[test]
    fn a_body_that_fills_the_cap_with_its_synthesized_header_is_stored_whole() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = edit_diff_params(pane_id.clone());
        params.diff = Some("+x\n".repeat(MAX_EDIT_DIFF_LINES_PER_REQUEST - 1));

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let stored = agent_edit_log(&app, &pane_id).flatten();
        assert_eq!(
            stored.len(),
            MAX_EDIT_DIFF_LINES_PER_REQUEST,
            "header plus every body line, nothing dropped"
        );
        assert_eq!(stored.last().unwrap().text, "+x");
    }

    /// The off-by-one this cap check moved to close: a body of exactly the cap
    /// passed the old pre-synthesis check, then lost its last line to the
    /// parser's own identical cap once the header was prepended — silently,
    /// under a documented promise that an over-long report is rejected whole
    /// rather than stored short.
    #[test]
    fn a_body_the_synthesized_header_pushes_over_the_cap_is_rejected_not_trimmed() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = edit_diff_params(pane_id.clone());
        params.diff = Some("+x\n".repeat(MAX_EDIT_DIFF_LINES_PER_REQUEST));

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_edit_diff_too_large");
        assert!(
            error
                .error
                .message
                .contains(&(MAX_EDIT_DIFF_LINES_PER_REQUEST + 1).to_string()),
            "the count reported is the one that was checked: {}",
            error.error.message
        );
        assert!(agent_edit_log(&app, &pane_id).is_empty());
    }

    /// `file` is interpolated into the synthesized `diff --git a/x b/x` line,
    /// so an unnormalized newline inside it would end that line early and
    /// splice its tail into the parsed diff as content.
    #[test]
    fn a_newline_in_file_does_not_splice_extra_lines_into_the_stored_diff() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = edit_diff_params(pane_id.clone());
        params.file = "src/foo.rs\n+spliced\n@@ -1 +1 @@".into();
        params.diff = Some("@@ -1 +1 @@\n-old\n+new\n".into());

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let stored = agent_edit_log(&app, &pane_id).flatten();
        assert_eq!(
            stored.len(),
            4,
            "one header line plus the diff's own three: {stored:?}"
        );
        assert!(
            stored[0].text.starts_with("diff --git "),
            "the header is still one whole line: {:?}",
            stored[0].text
        );
        assert!(
            !stored.iter().any(|line| line.text == "+spliced"),
            "the newline's tail must not become a diff line: {stored:?}"
        );
    }

    #[test]
    fn pane_report_edit_diff_rejects_a_file_that_normalizes_to_nothing() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = edit_diff_params(pane_id.clone());
        params.file = " \n\t ".into();

        let response = app.handle_pane_report_edit_diff("req".into(), params);

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_edit_diff_file");
        assert!(agent_edit_log(&app, &pane_id).is_empty());
    }

    // -----------------------------------------------------------------
    // Dormant tab minimize / reappear
    // -----------------------------------------------------------------

    fn app_with_two_tab_workspace() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("main")];
        app.state.workspaces[0].test_add_tab(Some("worker"));
        app.state.ensure_test_terminals();
        app
    }

    /// Returns (app, worker pane id, worker terminal id, worker pane's current public id).
    fn minimizable_worker_fixture() -> (App, PaneId, crate::terminal::TerminalId, String) {
        let app = app_with_two_tab_workspace();
        let worker_pane_id = app.state.workspaces[0].tabs[1].root_pane;
        let worker_terminal_id = app.state.workspaces[0].tabs[1]
            .terminal_id(worker_pane_id)
            .unwrap()
            .clone();
        let public_pane_id = app.public_pane_id(0, worker_pane_id).unwrap();
        (app, worker_pane_id, worker_terminal_id, public_pane_id)
    }

    #[test]
    fn api_pane_minimize_detaches_tab_but_keeps_terminal_alive() {
        let (mut app, _worker_pane_id, worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();

        let response = app.handle_pane_minimize(
            "req".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMinimize { minimize } = success.result else {
            panic!("expected pane minimize response");
        };
        assert!(minimize.changed);
        assert_eq!(minimize.reason, None);
        assert_eq!(minimize.terminal_ids, vec![worker_terminal_id.to_string()]);

        assert_eq!(
            app.state.workspaces[0].tabs.len(),
            1,
            "the minimized tab must leave the live tree"
        );
        assert_eq!(app.state.dormant_tabs.len(), 1);
        assert!(
            app.state.terminals.contains_key(&worker_terminal_id),
            "minimizing must not kill the pane's terminal"
        );
    }

    #[test]
    fn api_pane_minimize_refuses_only_tab_in_workspace() {
        let (mut app, public_pane_id) = app_with_test_workspace();

        let response = app.handle_pane_minimize(
            "req".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMinimize { minimize } = success.result else {
            panic!("expected pane minimize response");
        };
        assert!(!minimize.changed);
        assert_eq!(
            minimize.reason,
            Some(PaneMinimizeReason::OnlyTabInWorkspace)
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.dormant_tabs.len(), 0);
    }

    #[test]
    fn api_pane_dormant_reappear_restores_same_pane_and_terminal_id() {
        let (mut app, worker_pane_id, worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();
        app.handle_pane_minimize(
            "req1".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);

        let response = app.handle_pane_dormant_reappear(
            "req2".into(),
            PaneDormantReappearParams {
                terminal_id: worker_terminal_id.to_string(),
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneDormantReappear { reappear } = success.result else {
            panic!("expected pane dormant reappear response");
        };
        assert!(reappear.changed);
        assert_eq!(reappear.reason, None);
        assert_eq!(reappear.pane.terminal_id, worker_terminal_id.to_string());

        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert_eq!(app.state.dormant_tabs.len(), 0);
        assert_eq!(
            app.state.workspaces[0].tabs[1].root_pane, worker_pane_id,
            "reappear must reuse the exact same PaneId, not allocate a new one"
        );
        assert_eq!(
            reappear.pane.pane_id,
            app.public_pane_id(0, worker_pane_id).unwrap()
        );
    }

    #[test]
    fn api_pane_dormant_reappear_is_idempotent_when_already_visible() {
        let (mut app, _worker_pane_id, worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();
        app.handle_pane_minimize(
            "req1".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );

        let first = app.handle_pane_dormant_reappear(
            "req2".into(),
            PaneDormantReappearParams {
                terminal_id: worker_terminal_id.to_string(),
                focus: false,
            },
        );
        let first_success: SuccessResponse = serde_json::from_str(&first).unwrap();
        let ResponseResult::PaneDormantReappear {
            reappear: first_reappear,
        } = first_success.result
        else {
            panic!("expected pane dormant reappear response");
        };
        assert!(first_reappear.changed);

        let second = app.handle_pane_dormant_reappear(
            "req3".into(),
            PaneDormantReappearParams {
                terminal_id: worker_terminal_id.to_string(),
                focus: false,
            },
        );
        let second_success: SuccessResponse = serde_json::from_str(&second).unwrap();
        let ResponseResult::PaneDormantReappear {
            reappear: second_reappear,
        } = second_success.result
        else {
            panic!("expected pane dormant reappear response");
        };
        assert!(!second_reappear.changed);
        assert_eq!(
            second_reappear.reason,
            Some(PaneDormantReappearReason::AlreadyVisible)
        );
        assert_eq!(second_reappear.pane.pane_id, first_reappear.pane.pane_id);
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
    }

    #[test]
    fn api_pane_dormant_reappear_errors_for_unknown_terminal_id() {
        let (mut app, _public_pane_id) = app_with_test_workspace();

        let response = app.handle_pane_dormant_reappear(
            "req".into(),
            PaneDormantReappearParams {
                terminal_id: "term_does_not_exist".into(),
                focus: false,
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "not_found");
    }

    #[test]
    fn dormant_pane_status_write_back_updates_terminal_state_while_hidden() {
        let (mut app, worker_pane_id, worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();
        app.handle_pane_minimize(
            "req".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );
        assert_eq!(
            app.state.terminals[&worker_terminal_id].state,
            AgentState::Unknown
        );

        app.state
            .handle_app_event(crate::events::AppEvent::StateChanged {
                pane_id: worker_pane_id,
                agent: Some(Agent::Claude),
                state: AgentState::Blocked,
                visible_blocker: true,
                visible_working: false,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });

        assert_eq!(
            app.state.terminals[&worker_terminal_id].state,
            AgentState::Blocked,
            "a dormant pane's detected status must not freeze"
        );
    }

    #[test]
    fn dormant_pane_auto_reappears_when_state_transitions_to_working() {
        let (mut app, worker_pane_id, _worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();
        app.handle_pane_minimize(
            "req".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);

        app.state
            .handle_app_event(crate::events::AppEvent::StateChanged {
                pane_id: worker_pane_id,
                agent: Some(Agent::Claude),
                state: AgentState::Working,
                visible_blocker: false,
                visible_working: true,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });

        assert_eq!(
            app.state.dormant_tabs.len(),
            0,
            "bucket B must auto-reappear the instant the pane starts working"
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert_eq!(app.state.dormant_reappeared_pending.len(), 1);
    }

    #[test]
    fn dormant_pane_does_not_auto_reappear_on_transition_to_blocked() {
        let (mut app, worker_pane_id, _worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();
        app.handle_pane_minimize(
            "req".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );

        app.state
            .handle_app_event(crate::events::AppEvent::StateChanged {
                pane_id: worker_pane_id,
                agent: Some(Agent::Claude),
                state: AgentState::Blocked,
                visible_blocker: true,
                visible_working: false,
                process_exited: false,
                observed_at: std::time::Instant::now(),
            });

        assert_eq!(
            app.state.dormant_tabs.len(),
            1,
            "bucket A stays dormant until an explicit pane.dormant.reappear call"
        );
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.dormant_reappeared_pending.len(), 0);
    }

    #[test]
    fn dormant_pane_died_cleans_up_registry_and_reports_exit() {
        let (mut app, worker_pane_id, worker_terminal_id, public_pane_id) =
            minimizable_worker_fixture();
        app.handle_pane_minimize(
            "req".into(),
            PaneMinimizeParams {
                pane_id: Some(public_pane_id),
            },
        );
        assert_eq!(app.state.dormant_tabs.len(), 1);

        app.handle_internal_event(crate::events::AppEvent::PaneDied {
            pane_id: worker_pane_id,
            exit: Some(crate::events::PaneExitStatus {
                code: 1,
                signal: None,
            }),
        });

        assert_eq!(
            app.state.dormant_tabs.len(),
            0,
            "PaneDied must clean up a dormant pane's registry entry, not leave a zombie"
        );
        assert!(!app.state.terminals.contains_key(&worker_terminal_id));

        let events = app.event_hub.events_after(0);
        assert!(
            events.iter().any(|(_, envelope)| matches!(
                &envelope.data,
                crate::api::schema::EventData::PaneDormantExited { terminal_id, .. }
                    if *terminal_id == worker_terminal_id.to_string()
            )),
            "a dormant pane dying must still be reported externally"
        );
    }
}
