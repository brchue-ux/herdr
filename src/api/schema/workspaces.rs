use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::common::AgentStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// The pane making this request. See
    /// [`crate::api::schema::PaneSplitParams::caller_pane_id`].
    ///
    /// A workspace's first pane is always created from somewhere else, so its
    /// origin never resolves to an owner. It is recorded anyway, because "this
    /// Space was spun up by that pane" is the fact that makes the cross-Space
    /// case observable rather than merely absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_pane_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceRenameParams {
    pub workspace_id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceMoveParams {
    pub workspace_id: String,
    pub insert_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceMoveBlockParams {
    pub workspace_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceReportMetadataParams {
    pub workspace_id: String,
    pub source: String,
    #[schemars(schema_with = "super::common::metadata_token_patch_schema")]
    pub tokens: HashMap<String, Option<String>>,
    /// Drop every token on this workspace before applying `tokens`.
    ///
    /// The revoke path for tokens published without a TTL, which never expire
    /// and survive a restart: clearing one by name needs a key the caller may
    /// not know, and cooperation from a publisher that may be long gone.
    #[serde(default)]
    pub clear_all_tokens: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 86_400_000))]
    pub ttl_ms: Option<u64>,
}

/// What happened between two workspaces.
///
/// Describes the fleet, never the drawing: Herdr decides on its own whether a
/// signal becomes a moving mark on a branch line, a brief emphasis, or nothing
/// at all on a layout that has no room for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSignalKind {
    /// Work moved from `from_workspace_id` to `to_workspace_id`.
    Transfer,
    /// `from_workspace_id` finished the work it was given.
    Completed,
    /// `from_workspace_id` failed the work it was given.
    Failed,
    /// `from_workspace_id` has nothing left to do and has gone quiet.
    Idle,
}

/// A transient relation between two runtime entities.
///
/// Fire-and-forget: never persisted, never part of agent state, never part of
/// seen/unseen. A report that is dropped costs a decoration and nothing else,
/// which is why every reason to drop one — an unknown workspace, a replayed
/// `seq`, a layout with nowhere to put it — answers success rather than an
/// error. A publisher can retry blindly and can never fail a turn on this call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceReportSignalParams {
    /// Publisher identity, as in `workspace.report_metadata`. `seq` is tracked
    /// per source.
    pub source: String,
    pub kind: WorkspaceSignalKind,
    /// Required for `completed`; the origin of a `transfer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_workspace_id: Option<String>,
    /// Required for `transfer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_workspace_id: Option<String>,
    /// Monotonic per source. A report at or below the last accepted value is
    /// ignored, so retries are safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// How long the signal stays meaningful. Clamped by the server; omit to let
    /// Herdr choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 86_400_000))]
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: usize,
    pub label: String,
    pub focused: bool,
    pub pane_count: usize,
    pub tab_count: usize,
    pub active_tab_id: String,
    pub agent_status: AgentStatus,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[schemars(schema_with = "super::common::metadata_token_values_schema")]
    pub tokens: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorkspaceWorktreeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceWorktreeInfo {
    pub repo_key: String,
    pub repo_name: String,
    pub repo_root: String,
    pub checkout_path: String,
    pub is_linked_worktree: bool,
}
