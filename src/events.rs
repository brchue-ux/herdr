//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use std::time::Instant;

use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::workspace::{GitStatusCacheEntry, WorkspaceGitStatus};

#[derive(Debug)]
pub struct ApiWorktreeAddRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub source_workspace_id: Option<String>,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub label: Option<String>,
    pub focus: bool,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeAddResult {
    pub path: std::path::PathBuf,
    pub api_request: Option<ApiWorktreeAddRequest>,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct ApiWorktreeRemoveRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeRemoveResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub workspace: Option<Box<crate::api::schema::WorkspaceInfo>>,
    pub worktree: Option<Box<crate::api::schema::WorktreeInfo>>,
    pub forced: bool,
    pub api_request: Option<ApiWorktreeRemoveRequest>,
    pub result: Result<(), String>,
}

/// How a pane's child process ended.
///
/// Mirrors `portable_pty::ExitStatus`: a signalled process reports the signal
/// name and a synthesised code, so `signal` is the authoritative field whenever
/// it is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneExitStatus {
    pub code: u32,
    pub signal: Option<String>,
}

impl PaneExitStatus {
    pub fn success(&self) -> bool {
        self.signal.is_none() && self.code == 0
    }

    /// Short human-readable summary, e.g. `exit code 1` or `SIGTERM`.
    pub fn summary(&self) -> String {
        match &self.signal {
            Some(signal) => signal.clone(),
            None => format!("exit code {}", self.code),
        }
    }
}

impl From<portable_pty::ExitStatus> for PaneExitStatus {
    fn from(status: portable_pty::ExitStatus) -> Self {
        Self {
            code: status.exit_code(),
            signal: status.signal().map(str::to_string),
        }
    }
}

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A pane's child process exited.
    ///
    /// `exit` is `None` when herdr never owned the child (an adopted PTY after a
    /// live handoff), so the status is genuinely unknown rather than successful.
    PaneDied {
        pane_id: PaneId,
        exit: Option<PaneExitStatus>,
    },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
        visible_blocker: bool,
        visible_working: bool,
        process_exited: bool,
        observed_at: Instant,
    },
    /// Hook-authoritative agent state was reported for a pane.
    HookStateReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    },
    /// Agent session identity was reported without state authority.
    AgentSessionReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        session_start_source: Option<String>,
    },
    /// Display-only agent metadata was reported for a pane.
    HookMetadataReported {
        pane_id: PaneId,
        source: String,
        agent_label: Option<String>,
        applies_to_source: Option<String>,
        title: Option<String>,
        display_agent: Option<String>,
        state_labels: std::collections::HashMap<String, String>,
        clear_title: bool,
        clear_display_agent: bool,
        clear_state_labels: bool,
        seq: Option<u64>,
        ttl: Option<std::time::Duration>,
    },
    /// Hook authority was explicitly cleared for a pane.
    HookAuthorityCleared {
        pane_id: PaneId,
        source: Option<String>,
        seq: Option<u64>,
    },
    /// The current detected agent gracefully released this pane back to the shell.
    HookAgentReleased {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        known_agent: Option<Agent>,
        seq: Option<u64>,
    },
    /// A new version is available through the active installation manager.
    UpdateReady {
        version: String,
        install_command: String,
    },
    /// Remote agent detection manifest update check finished.
    AgentDetectionManifestsUpdated {
        updated: Vec<crate::detect::manifest_update::ManifestUpdateCommit>,
        status: crate::detect::manifest_update::ManifestUpdateStatus,
    },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite { content: Vec<u8> },
    /// A modal text input needs the clipboard's text because a paste shortcut
    /// arrived as a plain key carrying none. Only the machine the user is
    /// typing on can answer, so the main loop asks that input source rather
    /// than reading its own clipboard: the headless server forwards this to the
    /// client as `ServerMessage::RequestClipboardText`. Monolithic herdr *is*
    /// that machine and answers inline without ever emitting this.
    /// See `App::request_modal_clipboard_paste`.
    ClipboardRead {
        /// The input source that pressed the shortcut, which in the headless
        /// server is the id of the client connection it arrived on.
        source_id: u64,
        /// Correlates the answer with this request.
        request_id: u64,
    },
    /// Prefix-mode ASCII input-source request, emitted on entering/leaving the ASCII input
    /// realm. The foreground process applies the host-local TIS switch (`active = true`) /
    /// restore (`active = false`): the client in server mode (via server forwarding), the
    /// app itself in monolithic mode.
    PrefixInputSource { active: bool },
    /// A pane child reported its shell current directory through terminal
    /// metadata such as OSC 7.
    TerminalCwdReported {
        pane_id: PaneId,
        cwd: std::path::PathBuf,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed {
        results: Vec<WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, GitStatusCacheEntry)>,
    },
    /// An in-place act started from the notification tray finished.
    ///
    /// Carried back as an event rather than awaited, so a remote that has gone
    /// slow cannot pin the app loop and the popover reports what actually
    /// happened rather than assuming it worked.
    SignalTrayCommandFinished {
        signal_name: &'static str,
        ok: bool,
        message: String,
    },
    /// Background pull request refresh completed for workspaces.
    PullRequestsRefreshed {
        results: Vec<crate::app::WorkspacePullRequests>,
    },
    /// A plugin action or event command finished.
    PluginCommandFinished {
        log_id: String,
        finished_unix_ms: u64,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
    /// Background `git worktree add` completed.
    WorktreeAddFinished(Box<WorktreeAddResult>),
    /// Background `git worktree remove` completed.
    WorktreeRemoveFinished(Box<WorktreeRemoveResult>),
    /// A shell command was detected on a pane's screen — the sidebar's
    /// command-acknowledgement marker fires from this.
    ///
    /// Carries no command text and no tool name on purpose: the marker draws
    /// a glyph, never the command, so there is nothing here for a renderer to
    /// want. `observed_at` is when the detection task's own scan saw it,
    /// which is what the marker's visible lifetime is measured from. A caller
    /// that *does* want the command text reads [`Self::AgentCommandObserved`]
    /// instead, published alongside this one from the same detection pass.
    CommandAcknowledged {
        pane_id: PaneId,
        observed_at: Instant,
    },
    /// The same detection as [`Self::CommandAcknowledged`], carrying the
    /// command text so `crate::app::status_feed::StatusFeed` can hold a real
    /// line for it. Kept separate from `CommandAcknowledged` rather than
    /// adding a field to it: that event's whole point was carrying nothing a
    /// renderer could want, and this is the event for the one caller that
    /// now does.
    AgentCommandObserved {
        pane_id: PaneId,
        command: String,
        observed_at: Instant,
    },
    /// Claude displayed a green success circle for a completed ask in this pane.
    /// This is TUI presentation evidence; it is not part of the runtime wire protocol.
    PaneSuccessDetected {
        pane_id: PaneId,
        observed_at: Instant,
    },
    /// A bug or failure was detected in a pane's own output or state — the
    /// "meteor" visual-effect trigger candidate.
    ///
    /// Sourced only from herdr's own screen/state observation of `pane_id`.
    /// Never emitted for a firstmate/CI-originated fact (a green test run, a
    /// merged PR): those ride `herdr-outcome-publisher`'s existing
    /// `report-signal`/`report-metadata` channels instead, not this one.
    /// Carries no location, matching every other `AppEvent`: identity travels
    /// through the server, and a client resolves `pane_id` to a screen
    /// position at render time.
    // Next step: a producer that decides "this pane's output/state is a
    // failure" is separate, later work (see `src/app/pending_effects.rs`'s
    // module doc); this variant's dispatch into `PendingEffects` is exercised
    // by tests only until that producer is wired.
    #[allow(dead_code)]
    PaneIssueDetected {
        pane_id: PaneId,
        observed_at: Instant,
    },
}
