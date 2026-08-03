use serde::{Deserialize, Serialize};

use super::agents::{AgentInfo, AgentViewInfo};
use super::panes::{PaneInfo, PaneLayoutSnapshot};
use super::tabs::TabInfo;
use super::workspaces::WorkspaceInfo;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_pane_id: Option<String>,
    pub workspaces: Vec<WorkspaceInfo>,
    pub tabs: Vec<TabInfo>,
    pub panes: Vec<PaneInfo>,
    pub layouts: Vec<PaneLayoutSnapshot>,
    pub agents: Vec<AgentInfo>,
    /// The Agents view in force, if any. Absent when nothing is filtering or
    /// reordering the agent list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_view: Option<AgentViewInfo>,
    /// The session status string in force, if any. Absent when nothing has set
    /// one, which is the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Params for `session.status.set`.
///
/// The status is one caller-supplied line of text about this session. Herdr
/// never interprets it and never composes it: whatever the caller publishes is
/// what a client shows, so the same slot carries a quota readout, a build
/// number, or a deploy banner without Herdr learning about any of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionStatusSetParams {
    pub status: String,
}
