//! Runtime metadata carried across a live server handoff.
//!
//! Handoff replaces the server process while the fleet keeps running, and it
//! documents pane PTYs, agent identity, durable metadata, and live rate guards as preserved. The
//! session snapshot alone cannot deliver that: `persist::SessionSnapshot` is
//! the cold-start format, and it deliberately holds no metadata tokens and no
//! reported agent metadata, because a session file outlives its process and
//! would resurrect deadline-bearing values long after they meant anything. A
//! handoff has no such gap, so these values travel in the handoff manifest
//! instead, and never touch disk.
//!
//! Every timestamp here is a **duration, not an instant**. `Instant` is
//! process-local and meaningless once read by another process, so report times
//! cross as the age they had at export and are rebuilt against the importing
//! process's clock. That keeps two things the metadata rules depend on: TTL
//! deadlines land at the same wall-clock moment they would have, and the
//! newest-report-wins precedence between sources keeps its ordering.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Everything the exporting server must preserve that the cold session snapshot does not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct HandoffMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<WorkspaceHandoffMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panes: Vec<PaneHandoffMetadata>,
    /// Age of the most recently admitted ask comet, preserving the fleet-wide rate bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_comet_last_emitted_age: Option<Duration>,
}

impl HandoffMetadata {
    pub fn is_empty(&self) -> bool {
        self.workspaces.is_empty()
            && self.panes.is_empty()
            && self.ask_comet_last_emitted_age.is_none()
    }
}

impl PaneHandoffMetadata {
    /// Whether this entry says anything the importing server would not already
    /// have from a freshly restored pane. Most panes in a session have no
    /// reported metadata at all, and a manifest entry per pane regardless would
    /// be noise in the one artifact you read when a handoff goes wrong.
    pub fn carries_anything(&self) -> bool {
        !self.tokens.is_empty()
            || !self.agent_metadata.is_empty()
            || self.hook_authority.is_some()
            || !self.report_sequences.is_empty()
            || !self.hook_report_sequences.is_empty()
            || self.last_agent_state_change_seq.is_some()
            || self.last_agent_state_change_age.is_some()
            || !matches!(
                self.agent_state,
                None | Some(crate::api::schema::PaneAgentState::Unknown)
            )
    }
}

/// Workspace-scoped metadata, keyed by the stable workspace id so it survives
/// a restore that drops or reorders workspaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkspaceHandoffMetadata {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<HandoffToken>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub token_sequences: HashMap<String, u64>,
}

/// Pane-scoped metadata, keyed by the same pane id the runtime entries use.
///
/// Panes gate tokens and reported agent metadata through one sequence map, so
/// unlike a workspace there is a single `report_sequences` here rather than a
/// token-specific one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PaneHandoffMetadata {
    pub pane_id: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tokens: Vec<HandoffToken>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_metadata: Vec<HandoffAgentMetadata>,
    /// Agent lifecycle authority: who owns this pane's agent identity and what
    /// state they last reported. Without it a pane whose agent was reported
    /// over the API - rather than detected from the screen - stops being an
    /// agent at all, and drops out of the Agents panel entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_authority: Option<HandoffHookAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_state: Option<crate::api::schema::PaneAgentState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_state_change_seq: Option<u64>,
    /// How long ago the agent state last changed, at capture time.
    ///
    /// Carried as an age rather than an instant for the reason the module
    /// header gives: `Instant` is process-local and means nothing to the
    /// importing server, so the importer rebuilds it against its own clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_state_change_age: Option<Duration>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub report_sequences: HashMap<String, u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub hook_report_sequences: HashMap<String, u64>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub report_agents: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_sequence_sources: Vec<String>,
}

/// Serializable form of the pane's hook authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffHookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: crate::api::schema::PaneAgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub reported_age: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref_kind: Option<crate::agent_resume::AgentSessionRefKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref_value: Option<String>,
}

/// `crate::detect::AgentState` has no serde derive, and giving it one would put
/// a wire concern on a core type. The API enum already means exactly this and
/// is already serialized, so it is the transfer form.
pub(crate) fn agent_state_to_wire(
    state: crate::detect::AgentState,
) -> crate::api::schema::PaneAgentState {
    match state {
        crate::detect::AgentState::Idle => crate::api::schema::PaneAgentState::Idle,
        crate::detect::AgentState::Working => crate::api::schema::PaneAgentState::Working,
        crate::detect::AgentState::Blocked => crate::api::schema::PaneAgentState::Blocked,
        crate::detect::AgentState::Unknown => crate::api::schema::PaneAgentState::Unknown,
    }
}

pub(crate) fn agent_state_from_wire(
    state: crate::api::schema::PaneAgentState,
) -> crate::detect::AgentState {
    match state {
        crate::api::schema::PaneAgentState::Idle => crate::detect::AgentState::Idle,
        crate::api::schema::PaneAgentState::Working => crate::detect::AgentState::Working,
        crate::api::schema::PaneAgentState::Blocked => crate::detect::AgentState::Blocked,
        crate::api::schema::PaneAgentState::Unknown => crate::detect::AgentState::Unknown,
    }
}

/// One metadata token. `expires_in` is what is left of its TTL, absent when the
/// token never expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HandoffToken {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<Duration>,
}

/// One source's reported agent metadata. The `*_age` fields are how long ago
/// that field was reported; see the module note on why they are not instants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HandoffAgentMetadata {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applies_to_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_labels: HashMap<String, String>,
    pub reported_age: Duration,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_reported_age: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_agent_reported_age: Option<Duration>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub state_label_reported_age: HashMap<String, Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<Duration>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expiry_event_pending: bool,
}

/// How long ago `then` was, as of `now`. Clamped at zero: a report stamped
/// fractionally in the future must not become an enormous age.
pub(crate) fn age_of(then: Instant, now: Instant) -> Duration {
    now.saturating_duration_since(then)
}

/// Rebuild an exported age against this process's clock.
pub(crate) fn instant_from_age(age: Duration, now: Instant) -> Instant {
    now.checked_sub(age).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_age_round_trips_back_to_the_same_relative_ordering() {
        let export_now = Instant::now();
        let older = export_now - Duration::from_secs(30);
        let newer = export_now - Duration::from_secs(5);

        let import_now = export_now + Duration::from_secs(2);
        let older = instant_from_age(age_of(older, export_now), import_now);
        let newer = instant_from_age(age_of(newer, export_now), import_now);

        assert!(older < newer, "newest-report-wins ordering survives");
        assert_eq!(import_now.saturating_duration_since(newer).as_secs(), 5);
    }

    #[test]
    fn a_future_stamp_ages_to_zero_rather_than_wrapping() {
        let now = Instant::now();
        assert_eq!(age_of(now + Duration::from_secs(5), now), Duration::ZERO);
    }
}
