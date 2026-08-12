//! Evaluate an Agents view against app state.
//!
//! Grammar, limits, source reservation, and tier precedence are owned by
//! [`crate::agent_view`]; this module only projects an already-validated spec
//! onto the Agents panel.

use std::cmp::Ordering;

use crate::api::schema::{
    AgentStatus, AgentViewBuiltinField, AgentViewBuiltinSortField, AgentViewContext,
    AgentViewField, AgentViewFilter, AgentViewSort, AgentViewSortField, AgentViewSortOrder,
    AgentViewValue,
};
use crate::ui::AgentPanelEntry;

use super::{AppState, Mode};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EvalValue {
    String(String),
    Bool(bool),
    Number(u64),
}

/// What an active view is keeping out of the Agents panel.
///
/// The panel is meant to be the always-on truth, so a filtered panel has to say
/// what it is holding back — especially a blocked agent waiting on the user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AgentViewHidden {
    pub hidden: usize,
    pub hidden_blocked: usize,
}

impl AgentViewHidden {
    pub(crate) fn any(self) -> bool {
        self.hidden > 0
    }
}

pub(crate) fn apply_agent_view(
    app: &AppState,
    entries: &mut Vec<AgentPanelEntry>,
) -> AgentViewHidden {
    let mut hidden = AgentViewHidden::default();
    if let Some(spec) = app.agent_views.active() {
        if let Some(filter) = &spec.filter {
            entries.retain(|entry| {
                if matches_filter(app, entry, filter) {
                    return true;
                }
                hidden.hidden += 1;
                if entry.state == crate::detect::AgentState::Blocked {
                    hidden.hidden_blocked += 1;
                }
                false
            });
        }
        if !spec.sort.is_empty() {
            entries.sort_by(|left, right| compare_entries(app, left, right, &spec.sort));
            return hidden;
        }
    }

    if matches!(
        app.agent_panel_sort,
        crate::app::state::AgentPanelSort::Priority
    ) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(super::api_helpers::tab_attention_priority(
                    entry.state,
                    entry.seen,
                )),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }
    hidden
}

pub(crate) fn presented_workspace_idx(app: &AppState) -> Option<usize> {
    if app.mode == Mode::Navigate {
        app.workspaces.get(app.selected).map(|_| app.selected)
    } else {
        app.active
    }
}

fn matches_filter(app: &AppState, entry: &AgentPanelEntry, filter: &AgentViewFilter) -> bool {
    match filter {
        AgentViewFilter::All { filters } => filters
            .iter()
            .all(|filter| matches_filter(app, entry, filter)),
        AgentViewFilter::Any { filters } => filters
            .iter()
            .any(|filter| matches_filter(app, entry, filter)),
        AgentViewFilter::Not { filter } => !matches_filter(app, entry, filter),
        AgentViewFilter::Eq { field, value } => {
            field_value(app, entry, field) == operand_value(app, value)
        }
        AgentViewFilter::In { field, values } => {
            let actual = field_value(app, entry, field);
            values
                .iter()
                .any(|value| actual == operand_value(app, value))
        }
        AgentViewFilter::Exists { field } => field_value(app, entry, field).is_some(),
    }
}

fn compare_entries(
    app: &AppState,
    left: &AgentPanelEntry,
    right: &AgentPanelEntry,
    sorts: &[AgentViewSort],
) -> Ordering {
    for sort in sorts {
        let left = sort_value(app, left, &sort.field);
        let right = sort_value(app, right, &sort.field);
        let ordering = compare_optional_values(left, right, sort.order);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn compare_optional_values(
    left: Option<EvalValue>,
    right: Option<EvalValue>,
    order: AgentViewSortOrder,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let ordering = left.cmp(&right);
            if matches!(order, AgentViewSortOrder::Desc) {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn field_value(
    app: &AppState,
    entry: &AgentPanelEntry,
    field: &AgentViewField,
) -> Option<EvalValue> {
    match field {
        AgentViewField::Builtin(field) => builtin_field_value(app, entry, *field),
        AgentViewField::Token { token } => entry.tokens.get(token).cloned().map(EvalValue::String),
    }
}

fn builtin_field_value(
    app: &AppState,
    entry: &AgentPanelEntry,
    field: AgentViewBuiltinField,
) -> Option<EvalValue> {
    match field {
        AgentViewBuiltinField::Status => {
            Some(EvalValue::String(status_name(entry.state, entry.seen)))
        }
        AgentViewBuiltinField::WorkspaceId => app
            .workspaces
            .get(entry.ws_idx)
            .map(|workspace| EvalValue::String(workspace.id.clone())),
        AgentViewBuiltinField::TabId => public_tab_id(app, entry).map(EvalValue::String),
        AgentViewBuiltinField::PaneId => public_pane_id(app, entry).map(EvalValue::String),
        AgentViewBuiltinField::Agent => entry.agent_kind_label.clone().map(EvalValue::String),
        AgentViewBuiltinField::Seen => Some(EvalValue::Bool(entry.seen)),
        AgentViewBuiltinField::StateChangeSeq => {
            entry.last_agent_state_change_seq.map(EvalValue::Number)
        }
        AgentViewBuiltinField::Relation => {
            Some(EvalValue::String(entry.relation.as_str().to_string()))
        }
    }
}

fn operand_value(app: &AppState, value: &AgentViewValue) -> Option<EvalValue> {
    match value {
        AgentViewValue::String(value) => Some(EvalValue::String(value.clone())),
        AgentViewValue::Bool(value) => Some(EvalValue::Bool(*value)),
        AgentViewValue::Number(value) => Some(EvalValue::Number(*value)),
        AgentViewValue::Context { context } => context_value(app, *context),
    }
}

fn context_value(app: &AppState, context: AgentViewContext) -> Option<EvalValue> {
    let ws_idx = presented_workspace_idx(app)?;
    let workspace = app.workspaces.get(ws_idx)?;
    match context {
        AgentViewContext::CurrentWorkspaceId => Some(EvalValue::String(workspace.id.clone())),
        AgentViewContext::CurrentTabId => {
            let tab_number = workspace.public_tab_number(workspace.active_tab)?;
            Some(EvalValue::String(
                crate::workspace::public_tab_id_for_number(&workspace.id, tab_number),
            ))
        }
    }
}

fn sort_value(
    app: &AppState,
    entry: &AgentPanelEntry,
    field: &AgentViewSortField,
) -> Option<EvalValue> {
    match field {
        AgentViewSortField::Token { token } => {
            entry.tokens.get(token).cloned().map(EvalValue::String)
        }
        AgentViewSortField::Builtin(field) => match field {
            AgentViewBuiltinSortField::WorkspaceOrder => {
                Some(EvalValue::Number(entry.ws_idx as u64))
            }
            AgentViewBuiltinSortField::TabOrder => app
                .workspaces
                .get(entry.ws_idx)
                .and_then(|workspace| workspace.public_tab_number(entry.tab_idx))
                .map(|number| EvalValue::Number(number as u64)),
            AgentViewBuiltinSortField::PaneOrder => app
                .workspaces
                .get(entry.ws_idx)
                .and_then(|workspace| workspace.public_pane_number(entry.pane_id))
                .map(|number| EvalValue::Number(number as u64)),
            AgentViewBuiltinSortField::Attention => Some(EvalValue::Number(u64::from(
                super::api_helpers::tab_attention_priority(entry.state, entry.seen),
            ))),
            AgentViewBuiltinSortField::Status => {
                Some(EvalValue::String(status_name(entry.state, entry.seen)))
            }
            AgentViewBuiltinSortField::Agent => {
                entry.agent_kind_label.clone().map(EvalValue::String)
            }
            AgentViewBuiltinSortField::Seen => Some(EvalValue::Bool(entry.seen)),
            AgentViewBuiltinSortField::StateChangeSeq => {
                entry.last_agent_state_change_seq.map(EvalValue::Number)
            }
        },
    }
}

fn status_name(state: crate::detect::AgentState, seen: bool) -> String {
    let status = match (state, seen) {
        (crate::detect::AgentState::Idle, false) => AgentStatus::Done,
        (crate::detect::AgentState::Idle, true) => AgentStatus::Idle,
        (crate::detect::AgentState::Working, _) => AgentStatus::Working,
        (crate::detect::AgentState::Blocked, _) => AgentStatus::Blocked,
        (crate::detect::AgentState::Unknown, _) => AgentStatus::Unknown,
    };
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
    .to_string()
}

fn public_tab_id(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let number = workspace.public_tab_number(entry.tab_idx)?;
    Some(crate::workspace::public_tab_id_for_number(
        &workspace.id,
        number,
    ))
}

fn public_pane_id(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let number = workspace.public_pane_number(entry.pane_id)?;
    Some(crate::workspace::public_pane_id_for_number(
        &workspace.id,
        number,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_view::AgentViewTier;
    use crate::api::schema::AgentViewSetParams;
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    fn state_with_agents() -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        for (ws_idx, agent_state) in [(0, AgentState::Idle), (1, AgentState::Working)] {
            let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = agent_state;
        }
        state
    }

    fn current_workspace_view() -> AgentViewSetParams {
        AgentViewSetParams {
            source: "example.views".to_string(),
            label: Some("current".to_string()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
                value: AgentViewValue::Context {
                    context: AgentViewContext::CurrentWorkspaceId,
                },
            }),
            sort: Vec::new(),
        }
    }

    #[test]
    fn current_workspace_filter_tracks_presented_workspace() {
        let mut state = state_with_agents();
        state
            .agent_views
            .set(AgentViewTier::Api, Some(current_workspace_view()));

        assert_eq!(crate::ui::agent_panel_entries(&state)[0].ws_idx, 0);

        state.mode = Mode::Navigate;
        state.selected = 1;
        let entries = crate::ui::agent_panel_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ws_idx, 1);

        state.mode = Mode::Settings;
        let entries = crate::ui::agent_panel_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ws_idx, 0);
    }

    #[test]
    fn boolean_filter_and_custom_sort_define_canonical_entries() {
        let mut state = state_with_agents();
        let first_pane = state.workspaces[0].tabs[0].root_pane;
        let first_terminal = state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&first_terminal).unwrap().state = AgentState::Working;
        state.agent_views.set(
            AgentViewTier::Api,
            Some(AgentViewSetParams {
                source: "example.views".to_string(),
                label: None,
                filter: Some(AgentViewFilter::All {
                    filters: vec![
                        AgentViewFilter::Eq {
                            field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                            value: AgentViewValue::String("working".to_string()),
                        },
                        AgentViewFilter::Not {
                            filter: Box::new(AgentViewFilter::Eq {
                                field: AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
                                value: AgentViewValue::String("missing".to_string()),
                            }),
                        },
                    ],
                }),
                sort: vec![AgentViewSort {
                    field: AgentViewSortField::Builtin(AgentViewBuiltinSortField::WorkspaceOrder),
                    order: AgentViewSortOrder::Desc,
                }],
            }),
        );

        let entries = crate::ui::agent_panel_entries(&state);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].ws_idx, 1);
        assert_eq!(entries[1].ws_idx, 0);
    }

    #[test]
    fn agent_filter_matches_custom_lifecycle_agent_label() {
        let mut state = state_with_agents();
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "test".to_string(),
                "custom-agent".to_string(),
                AgentState::Working,
                None,
                None,
            );
        state.agent_views.set(
            AgentViewTier::Api,
            Some(AgentViewSetParams {
                source: "example.views".to_string(),
                label: None,
                filter: Some(AgentViewFilter::Eq {
                    field: AgentViewField::Builtin(AgentViewBuiltinField::Agent),
                    value: AgentViewValue::String("custom-agent".to_string()),
                }),
                sort: Vec::new(),
            }),
        );

        let entries = crate::ui::agent_panel_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_kind_label.as_deref(), Some("custom-agent"));
    }

    #[test]
    fn hidden_rows_are_counted_and_blocked_rows_are_flagged() {
        let mut state = state_with_agents();
        let second_pane = state.workspaces[1].tabs[0].root_pane;
        let second_terminal = state.workspaces[1].tabs[0].panes[&second_pane]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&second_terminal).unwrap().state = AgentState::Blocked;
        state.agent_views.set(
            AgentViewTier::Api,
            Some(AgentViewSetParams {
                source: "example.views".to_string(),
                label: None,
                filter: Some(AgentViewFilter::Eq {
                    field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                    value: AgentViewValue::String("idle".to_string()),
                }),
                sort: Vec::new(),
            }),
        );

        let mut entries = crate::ui::all_agent_panel_entries(&state);
        let hidden = apply_agent_view(&state, &mut entries);

        assert_eq!(entries.len(), 1);
        assert_eq!(hidden.hidden, 1);
        assert_eq!(hidden.hidden_blocked, 1);
        assert!(hidden.any());
    }

    #[test]
    fn an_unfiltered_view_hides_nothing() {
        let mut state = state_with_agents();
        state.agent_views.set(
            AgentViewTier::Api,
            Some(AgentViewSetParams {
                source: "example.views".to_string(),
                label: None,
                filter: None,
                sort: vec![AgentViewSort {
                    field: AgentViewSortField::Builtin(AgentViewBuiltinSortField::WorkspaceOrder),
                    order: AgentViewSortOrder::Desc,
                }],
            }),
        );

        let mut entries = crate::ui::all_agent_panel_entries(&state);
        let hidden = apply_agent_view(&state, &mut entries);

        assert_eq!(entries.len(), 2);
        assert!(!hidden.any());
    }
}
