//! Guarded live swap of a running server onto a locally built binary.
//!
//! `herdr server live-handoff` is the raw primitive: it hands the running
//! server's pane file descriptors to a freshly spawned import process. It has
//! no pre-flight checks, no verification that the swap landed, and no recovery
//! guidance. `herdr update --handoff` wraps it, but only for installs managed
//! by Herdr's own updater.
//!
//! This module is the supported path for everyone else — packagers,
//! contributors, anyone testing a branch build.
//!
//! The rule that shapes the whole flow: **a client may only talk to a server on
//! the same protocol**. The driving client therefore has to be protocol-
//! compatible with the *running* server, which is why the new binary is only an
//! import target here. It becomes a client after the server has actually moved
//! to its protocol, never before.

use std::cmp::Ordering;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::api::client::{ApiClient, ApiClientError};
use crate::api::schema::{
    AgentStatus, EmptyParams, Method, Request, ResponseResult, ServerLiveHandoffParams,
    WorkspaceInfo,
};

/// The handoff response is only written once the old server has finished
/// pausing panes, transferring descriptors and confirming the commit.
const HANDOFF_REQUEST_TIMEOUT: Duration = Duration::from_secs(240);
/// How long to wait for the imported server to answer on the expected protocol.
const HANDOFF_CONFIRM_TIMEOUT: Duration = Duration::from_secs(30);
const HANDOFF_POLL_INTERVAL: Duration = Duration::from_millis(200);
const STATUS_POLL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to keep asking who owns the socket after a failed handoff.
const FAILURE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) const USAGE: &str = "usage: herdr server swap --exe <path> [--dry-run] [--yes] [--allow-downgrade] [--promote-client <path>|--no-promote-client]";

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SwapArgs {
    pub exe: PathBuf,
    pub dry_run: bool,
    pub allow_downgrade: bool,
    pub assume_yes: bool,
    pub promote: PromoteChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PromoteChoice {
    /// Overwrite the binary that is driving this command.
    DrivingClient,
    /// Overwrite an explicitly named client path.
    Path(PathBuf),
    /// Leave every installed client alone.
    Skip,
}

pub(super) fn parse_swap_args(args: &[String]) -> Result<SwapArgs, String> {
    let mut exe: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut allow_downgrade = false;
    let mut assume_yes = false;
    let mut promote = PromoteChoice::DrivingClient;

    let mut index = 0;
    while index < args.len() {
        let (flag, inline_value) = match args[index].split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (args[index].clone(), None),
        };

        let mut take_value = |flag: &str| -> Result<String, String> {
            if let Some(value) = inline_value.clone() {
                return Ok(value);
            }
            let value = args
                .get(index + 1)
                .cloned()
                .ok_or_else(|| format!("missing value for {flag}"))?;
            index += 1;
            Ok(value)
        };

        match flag.as_str() {
            "--exe" => exe = Some(PathBuf::from(take_value("--exe")?)),
            "--promote-client" => {
                promote = PromoteChoice::Path(PathBuf::from(take_value("--promote-client")?))
            }
            "--no-promote-client" => promote = PromoteChoice::Skip,
            "--dry-run" => dry_run = true,
            "--allow-downgrade" => allow_downgrade = true,
            "--yes" | "-y" => assume_yes = true,
            other => return Err(format!("unknown flag {other}")),
        }
        index += 1;
    }

    let exe = exe.ok_or_else(|| "missing required --exe <path>".to_string())?;
    Ok(SwapArgs {
        exe,
        dry_run,
        allow_downgrade,
        assume_yes,
        promote,
    })
}

// ---------------------------------------------------------------------------
// Facts and pre-flight decisions (pure)
// ---------------------------------------------------------------------------

/// Version and protocol reported by a herdr binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BinaryFacts {
    pub version: String,
    pub protocol: u32,
}

/// What the running server advertises over `ping`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServerFacts {
    pub version: Option<String>,
    pub protocol: Option<u32>,
    pub live_handoff: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreflightInput {
    pub client: BinaryFacts,
    pub server: Option<ServerFacts>,
    pub target: BinaryFacts,
    pub allow_downgrade: bool,
}

/// Direction of the swap, decided from protocol first and version second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SwapKind {
    /// Same protocol and same version: a plain refresh, the lowest-risk case.
    Refresh,
    /// Newer protocol, or same protocol with a newer version.
    Upgrade,
    /// Older protocol, or same protocol with an older version.
    Downgrade,
}

impl SwapKind {
    fn label(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Upgrade => "upgrade",
            Self::Downgrade => "downgrade",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreflightPlan {
    pub kind: SwapKind,
    pub server_protocol: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub(super) fn evaluate_preflight(input: &PreflightInput) -> Result<PreflightPlan, Refusal> {
    let Some(server) = input.server.as_ref() else {
        return Err(Refusal::new(
            "server_not_running",
            "no herdr server is running on this socket; there is nothing to swap. Start herdr first, or just run the new binary.",
        ));
    };

    let Some(server_protocol) = server.protocol else {
        return Err(Refusal::new(
            "server_protocol_unknown",
            "the running server did not report a protocol version; refusing to swap blind.",
        ));
    };

    if !server.live_handoff {
        return Err(Refusal::new(
            "live_handoff_unsupported",
            format!(
                "the running server (v{}) does not advertise the live_handoff capability; it cannot hand its panes to another binary. Restart it on a build that supports live handoff.",
                version_label(server.version.as_deref())
            ),
        ));
    }

    if input.client.protocol != server_protocol {
        return Err(Refusal::new(
            "protocol_mismatch",
            format!(
                "this client speaks protocol {} but the running server speaks protocol {}; a client may only drive a server on its own protocol. Run `herdr server swap` with the binary the server was started from (herdr v{}) and pass the new build to --exe.",
                input.client.protocol,
                server_protocol,
                version_label(server.version.as_deref())
            ),
        ));
    }

    let kind = swap_kind(server_protocol, server.version.as_deref(), &input.target);

    if kind == SwapKind::Downgrade && !input.allow_downgrade {
        return Err(Refusal::new(
            "downgrade_refused",
            format!(
                "the target binary is older than the running server (protocol {} v{} -> protocol {} v{}). Live handoff captures the session with the *running* server and the target has to load it back; nothing on the handoff path checks that the target understands a capture from a newer build. Unknown state is silently dropped rather than rejected, so a downgrade can lose session state without failing. Downgrades are unverified. If you accept that, re-run with --allow-downgrade.",
                server_protocol,
                version_label(server.version.as_deref()),
                input.target.protocol,
                input.target.version,
            ),
        ));
    }

    Ok(PreflightPlan {
        kind,
        server_protocol,
    })
}

fn swap_kind(server_protocol: u32, server_version: Option<&str>, target: &BinaryFacts) -> SwapKind {
    match target.protocol.cmp(&server_protocol) {
        Ordering::Greater => SwapKind::Upgrade,
        Ordering::Less => SwapKind::Downgrade,
        Ordering::Equal => {
            match server_version.and_then(|from| compare_versions(from, &target.version)) {
                Some(Ordering::Less) => SwapKind::Upgrade,
                Some(Ordering::Greater) => SwapKind::Downgrade,
                _ => SwapKind::Refresh,
            }
        }
    }
}

/// Compare dotted numeric versions, ignoring any pre-release suffix. Returns
/// `None` when either side does not parse, so callers can fall back to
/// treating the pair as equivalent rather than guessing a direction.
fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let left = numeric_version(left)?;
    let right = numeric_version(right)?;
    Some(left.cmp(&right))
}

fn numeric_version(value: &str) -> Option<Vec<u64>> {
    let core = value
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or_default();
    if core.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for component in core.split('.') {
        parts.push(component.parse::<u64>().ok()?);
    }
    Some(parts)
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ---------------------------------------------------------------------------
// In-flight work gate (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfirmDecision {
    /// Nothing is working; go ahead.
    Proceed,
    /// Ask the operator before interrupting working agents.
    Prompt,
    /// Working agents, no TTY to ask on, and no `--yes`.
    RefuseNonInteractive,
}

pub(super) fn confirm_decision(
    working_agents: usize,
    assume_yes: bool,
    interactive: bool,
) -> ConfirmDecision {
    if working_agents == 0 || assume_yes {
        return ConfirmDecision::Proceed;
    }
    if interactive {
        ConfirmDecision::Prompt
    } else {
        ConfirmDecision::RefuseNonInteractive
    }
}

// ---------------------------------------------------------------------------
// Inventory (pure)
// ---------------------------------------------------------------------------

/// The part of a workspace that a live handoff must preserve exactly.
///
/// Deliberately a narrow subset of [`WorkspaceInfo`]: the "after" side is read
/// back through the *new* binary, whose `WorkspaceInfo` may have grown or lost
/// fields relative to this build.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(super) struct InventoryEntry {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    pub tab_count: usize,
    pub pane_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Inventory {
    pub entries: Vec<InventoryEntry>,
}

impl Inventory {
    pub fn from_entries(mut entries: Vec<InventoryEntry>) -> Self {
        entries.sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
        Self { entries }
    }

    pub fn from_workspace_infos(workspaces: &[WorkspaceInfo]) -> Self {
        Self::from_entries(
            workspaces
                .iter()
                .map(|workspace| InventoryEntry {
                    workspace_id: workspace.workspace_id.clone(),
                    label: workspace.label.clone(),
                    tab_count: workspace.tab_count,
                    pane_count: workspace.pane_count,
                })
                .collect(),
        )
    }

    /// Parse a `workspace.list` response, from this process or from a
    /// subprocess running the new binary.
    pub fn from_response(value: &serde_json::Value) -> Result<Self, String> {
        if let Some(error) = value.get("error") {
            return Err(format!("workspace list failed: {error}"));
        }
        let workspaces = value
            .get("result")
            .and_then(|result| result.get("workspaces"))
            .ok_or_else(|| format!("workspace list response had no workspaces: {value}"))?;
        let entries: Vec<InventoryEntry> = serde_json::from_value(workspaces.clone())
            .map_err(|err| format!("could not read workspace list: {err}"))?;
        Ok(Self::from_entries(entries))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_tabs(&self) -> usize {
        self.entries.iter().map(|entry| entry.tab_count).sum()
    }

    pub fn total_panes(&self) -> usize {
        self.entries.iter().map(|entry| entry.pane_count).sum()
    }

    /// Human-readable differences, empty when the two inventories match.
    pub fn diff(&self, after: &Inventory) -> Vec<String> {
        let mut differences = Vec::new();
        for entry in &self.entries {
            match after
                .entries
                .iter()
                .find(|candidate| candidate.workspace_id == entry.workspace_id)
            {
                None => differences.push(format!(
                    "workspace {} ({}) is gone: had {} tabs / {} panes",
                    entry.workspace_id, entry.label, entry.tab_count, entry.pane_count
                )),
                Some(found) if found.tab_count != entry.tab_count => differences.push(format!(
                    "workspace {} tab count changed: {} -> {}",
                    entry.workspace_id, entry.tab_count, found.tab_count
                )),
                Some(found) if found.pane_count != entry.pane_count => differences.push(format!(
                    "workspace {} pane count changed: {} -> {}",
                    entry.workspace_id, entry.pane_count, found.pane_count
                )),
                Some(_) => {}
            }
        }
        for entry in &after.entries {
            if !self
                .entries
                .iter()
                .any(|candidate| candidate.workspace_id == entry.workspace_id)
            {
                differences.push(format!(
                    "workspace {} ({}) appeared: {} tabs / {} panes",
                    entry.workspace_id, entry.label, entry.tab_count, entry.pane_count
                ));
            }
        }
        differences
    }
}

// ---------------------------------------------------------------------------
// Failure classification (pure)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PostHandoffState {
    /// The target binary is serving: the swap actually landed.
    NewServerRunning,
    /// A server matching the target answered, but the old server was
    /// indistinguishable from the target to begin with, so a rollback looks
    /// exactly like a success from the outside.
    TargetLikeServerRunning {
        version: Option<String>,
        protocol: Option<u32>,
    },
    /// Something answered, and it is definitely not the target build.
    OldServerRunning {
        version: Option<String>,
        protocol: Option<u32>,
    },
    /// Nothing is listening on the socket.
    NoServerResponding,
    /// The socket could not be probed at all.
    Unknown(String),
}

/// `indistinguishable` says whether the server that was running before the
/// handoff reported the same version and protocol as the target binary. When
/// it did, `ping` alone cannot tell a rollback from a successful swap.
pub(super) fn classify_post_handoff(
    status: &Result<Option<crate::api::RuntimeStatus>, String>,
    target: &BinaryFacts,
    indistinguishable: bool,
) -> PostHandoffState {
    match status {
        Ok(Some(status)) => {
            let protocol_matches = status.protocol == Some(target.protocol);
            let version_matches = status.version.as_deref() == Some(target.version.as_str());
            match (protocol_matches && version_matches, indistinguishable) {
                (true, false) => PostHandoffState::NewServerRunning,
                (true, true) => PostHandoffState::TargetLikeServerRunning {
                    version: status.version.clone(),
                    protocol: status.protocol,
                },
                (false, _) => PostHandoffState::OldServerRunning {
                    version: status.version.clone(),
                    protocol: status.protocol,
                },
            }
        }
        Ok(None) => PostHandoffState::NoServerResponding,
        Err(err) => PostHandoffState::Unknown(err.clone()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FailureContext {
    pub new_exe: String,
    pub socket: String,
    pub start_command: String,
}

/// Never leave the operator guessing which server is live.
pub(super) fn failure_report(state: &PostHandoffState, context: &FailureContext) -> Vec<String> {
    match state {
        PostHandoffState::NewServerRunning => vec![
            "the target server is answering on the socket, so the handoff did land.".to_string(),
            "no client was promoted; re-run `herdr server swap` with the new binary to finish, or install it yourself.".to_string(),
        ],
        PostHandoffState::OldServerRunning { version, protocol } => vec![
            format!(
                "the original server is still running (v{} protocol {}) and still owns your panes.",
                version_label(version.as_deref()),
                protocol_label(*protocol)
            ),
            "nothing was changed and no client was promoted. Investigate before retrying."
                .to_string(),
        ],
        PostHandoffState::TargetLikeServerRunning { version, protocol } => vec![
            format!(
                "a server answering as v{} protocol {} still owns {}, so your panes are intact.",
                version_label(version.as_deref()),
                protocol_label(*protocol),
                context.socket
            ),
            "this swap was a refresh onto the same version and protocol, so that is either the original server after a rollback or the imported one; ping cannot tell them apart."
                .to_string(),
            "either way it is safe to keep working, and no client was promoted. Do not start a second server; check the server log if you need to know which one won."
                .to_string(),
        ],
        PostHandoffState::NoServerResponding => vec![
            format!("no server is responding on {}.", context.socket),
            "the panes of the old server are gone. Recover with:".to_string(),
            format!("  {}", context.start_command),
        ],
        PostHandoffState::Unknown(err) => vec![
            format!("could not determine which server owns {}: {err}", context.socket),
            format!(
                "check `{} status --json` before starting another server; starting one while the old server still holds the socket will fail.",
                context.new_exe
            ),
        ],
    }
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub(super) fn run_swap_command(args: &[String]) -> io::Result<i32> {
    if matches!(
        args.first().map(String::as_str),
        Some("help" | "--help" | "-h")
    ) {
        eprintln!("{USAGE}");
        return Ok(0);
    }

    let args = match parse_swap_args(args) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };

    if !cfg!(unix) {
        eprintln!("herdr server swap requires a Unix host; live handoff is not supported here");
        return Ok(1);
    }

    let new_exe = match resolve_target_exe(&args.exe) {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{err}");
            return Ok(1);
        }
    };

    let target = match read_binary_facts(&new_exe) {
        Ok(facts) => facts,
        Err(err) => {
            eprintln!("{err}");
            return Ok(1);
        }
    };

    let client = ApiClient::local();
    let socket = client.socket_path();
    let server = read_server_facts(&client)?;

    let input = PreflightInput {
        client: BinaryFacts {
            version: crate::build_info::version(),
            protocol: crate::protocol::PROTOCOL_VERSION,
        },
        server,
        target: target.clone(),
        allow_downgrade: args.allow_downgrade,
    };

    print_preflight_header(&new_exe, &socket, &input);

    // When the running server already reports the target's version and
    // protocol, `ping` cannot tell a rollback apart from a successful swap.
    let indistinguishable = input.server.as_ref().is_some_and(|server| {
        server.protocol == Some(target.protocol)
            && server.version.as_deref() == Some(target.version.as_str())
    });

    let plan = match evaluate_preflight(&input) {
        Ok(plan) => plan,
        Err(refusal) => {
            eprintln!("refusing to swap [{}]: {}", refusal.code, refusal.message);
            return Ok(1);
        }
    };
    eprintln!(
        "  swap kind   : {} (protocol {} -> {})",
        plan.kind.label(),
        plan.server_protocol,
        target.protocol
    );

    let before = match read_local_inventory(&client) {
        Ok(inventory) => inventory,
        Err(err) => {
            eprintln!("could not capture the workspace inventory: {err}");
            return Ok(1);
        }
    };
    if before.is_empty() {
        eprintln!(
            "refusing to swap [empty_inventory]: the running server reports no workspaces, so there is nothing to verify the swap against. Restart the server on the new binary instead."
        );
        return Ok(1);
    }
    eprintln!(
        "  inventory   : {} workspaces, {} tabs, {} panes",
        before.entries.len(),
        before.total_tabs(),
        before.total_panes()
    );

    let working = match count_working_agents(&client) {
        Ok(working) => working,
        Err(err) => {
            eprintln!("could not read agent status: {err}");
            return Ok(1);
        }
    };
    eprintln!(
        "  working now : {working} agent(s); live handoff drops in-flight API calls, waits and event subscriptions"
    );

    if args.dry_run {
        eprintln!("dry run: every pre-flight check passed, no handoff was performed.");
        for entry in &before.entries {
            eprintln!(
                "    {} {} tabs {} panes  {}",
                entry.workspace_id, entry.tab_count, entry.pane_count, entry.label
            );
        }
        return Ok(0);
    }

    match confirm_decision(working, args.assume_yes, io::stdin().is_terminal()) {
        ConfirmDecision::Proceed => {}
        ConfirmDecision::RefuseNonInteractive => {
            eprintln!(
                "refusing to swap [agents_working]: {working} agent(s) are working and there is no terminal to confirm on. Re-run with --yes to interrupt them anyway."
            );
            return Ok(1);
        }
        ConfirmDecision::Prompt => {
            if !prompt_yes_no(&format!(
                "{working} agent(s) are working; their in-flight calls will fail once. continue?"
            ))? {
                eprintln!("aborted; nothing was changed.");
                return Ok(1);
            }
        }
    }

    let context = FailureContext {
        new_exe: new_exe.display().to_string(),
        socket: socket.display().to_string(),
        start_command: start_command_for(&new_exe),
    };

    eprintln!("handing off to {} ...", new_exe.display());
    if let Err(err) = request_handoff(&client, &new_exe, &target) {
        eprintln!("live handoff failed: {err}");
        report_failure(&socket, &target, indistinguishable, &context);
        return Ok(1);
    }

    if let Err(err) = wait_for_target_server(&socket, &target) {
        eprintln!("{err}");
        report_failure(&socket, &target, indistinguishable, &context);
        return Ok(1);
    }
    eprintln!(
        "  server is now v{} protocol {}",
        target.version, target.protocol
    );

    // The new server speaks the new protocol, so the "after" inventory has to
    // be read by the new binary, not by this one.
    let after = match read_inventory_via(&new_exe, &socket) {
        Ok(inventory) => inventory,
        Err(err) => {
            eprintln!("the swap landed, but the inventory could not be read back: {err}");
            eprintln!("{}", manual_promote_hint(&new_exe, &args.promote));
            return Ok(1);
        }
    };

    let differences = before.diff(&after);
    if !differences.is_empty() {
        eprintln!("INVENTORY CHANGED across the handoff:");
        for difference in &differences {
            eprintln!("  {difference}");
        }
        eprintln!(
            "the new server is live; no client was promoted because the swap did not verify."
        );
        eprintln!("{}", manual_promote_hint(&new_exe, &args.promote));
        return Ok(1);
    }
    eprintln!(
        "  inventory identical: {} workspaces, {} tabs, {} panes survived",
        after.entries.len(),
        after.total_tabs(),
        after.total_panes()
    );

    match promote_client(&new_exe, &args.promote) {
        Ok(Promotion::Installed(path)) => eprintln!("  promoted client: {}", path.display()),
        Ok(Promotion::AlreadyTheTarget(path)) => {
            eprintln!("  client at {} already is the new binary", path.display())
        }
        Ok(Promotion::Skipped) => eprintln!("  client promotion skipped"),
        Err(err) => {
            eprintln!("the swap succeeded, but the client could not be promoted: {err}");
            eprintln!("{}", manual_promote_hint(&new_exe, &args.promote));
            return Ok(1);
        }
    }

    eprintln!("live swap complete.");
    Ok(0)
}

fn print_preflight_header(new_exe: &Path, socket: &Path, input: &PreflightInput) {
    eprintln!("live swap pre-flight");
    eprintln!("  socket      : {}", socket.display());
    eprintln!(
        "  new binary  : {} (v{} protocol {})",
        new_exe.display(),
        input.target.version,
        input.target.protocol
    );
    match input.server.as_ref() {
        Some(server) => eprintln!(
            "  live server : v{} protocol {} live_handoff={}",
            version_label(server.version.as_deref()),
            server
                .protocol
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            server.live_handoff
        ),
        None => eprintln!("  live server : not running"),
    }
    eprintln!(
        "  this client : v{} protocol {}",
        input.client.version, input.client.protocol
    );
}

fn report_failure(
    socket: &Path,
    target: &BinaryFacts,
    indistinguishable: bool,
    context: &FailureContext,
) {
    // Establish who owns the socket before suggesting anything at all.
    let status = probe_socket(socket);
    let state = classify_post_handoff(&status, target, indistinguishable);
    for line in failure_report(&state, context) {
        eprintln!("{line}");
    }
}

/// A server that rolls a failed handoff back has to rebind its public sockets
/// first, so "nothing answered" is only true once it stays true.
fn probe_socket(socket: &Path) -> Result<Option<crate::api::RuntimeStatus>, String> {
    let deadline = Instant::now() + FAILURE_PROBE_TIMEOUT;
    loop {
        match crate::api::read_runtime_status_at(socket, STATUS_POLL_TIMEOUT) {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) if Instant::now() >= deadline => return Ok(None),
            Ok(None) => {}
            Err(err) if Instant::now() >= deadline => return Err(err.to_string()),
            Err(_) => {}
        }
        std::thread::sleep(HANDOFF_POLL_INTERVAL);
    }
}

fn resolve_target_exe(exe: &Path) -> Result<PathBuf, String> {
    let resolved =
        std::fs::canonicalize(exe).map_err(|err| format!("cannot use {}: {err}", exe.display()))?;
    if !resolved.is_file() {
        return Err(format!("{} is not a file", resolved.display()));
    }
    if !is_executable(&resolved) {
        return Err(format!("{} is not executable", resolved.display()));
    }
    Ok(resolved)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[derive(Deserialize)]
struct ClientStatusLine {
    version: String,
    protocol: u32,
}

/// Ask the target binary what it is. This also proves it runs at all.
fn read_binary_facts(exe: &Path) -> Result<BinaryFacts, String> {
    let output = Command::new(exe)
        .args(["status", "client", "--json"])
        .output()
        .map_err(|err| format!("cannot run {}: {err}", exe.display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} status client --json exited with {}: {}",
            exe.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let status: ClientStatusLine = serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "could not read the version and protocol of {}: {err}",
            exe.display()
        )
    })?;
    Ok(BinaryFacts {
        version: status.version,
        protocol: status.protocol,
    })
}

fn read_server_facts(client: &ApiClient) -> io::Result<Option<ServerFacts>> {
    match client.status() {
        Ok(status) => Ok(Some(ServerFacts {
            version: status.version,
            protocol: status.protocol,
            live_handoff: status
                .capabilities
                .as_ref()
                .is_some_and(|capabilities| capabilities.live_handoff),
        })),
        Err(ApiClientError::Io(err))
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(io::Error::other(err)),
    }
}

fn read_local_inventory(client: &ApiClient) -> Result<Inventory, String> {
    let response = client
        .request(Request {
            id: "cli:server:swap:workspace-list".into(),
            method: Method::WorkspaceList(EmptyParams::default()),
        })
        .map_err(|err| err.to_string())?;
    match response.result {
        ResponseResult::WorkspaceList { workspaces } => {
            Ok(Inventory::from_workspace_infos(&workspaces))
        }
        other => Err(format!("unexpected workspace list result: {other:?}")),
    }
}

/// Read the inventory through another herdr binary, pinned to the same socket.
fn read_inventory_via(exe: &Path, socket: &Path) -> Result<Inventory, String> {
    let output = Command::new(exe)
        .args(["workspace", "list"])
        .env(crate::api::SOCKET_PATH_ENV_VAR, socket)
        .output()
        .map_err(|err| format!("cannot run {}: {err}", exe.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "{} workspace list did not return JSON ({err}): {}",
            exe.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })?;
    Inventory::from_response(&value)
}

fn count_working_agents(client: &ApiClient) -> Result<usize, String> {
    let response = client
        .request(Request {
            id: "cli:server:swap:agent-list".into(),
            method: Method::AgentList(EmptyParams::default()),
        })
        .map_err(|err| err.to_string())?;
    match response.result {
        ResponseResult::AgentList { agents } => Ok(agents
            .iter()
            .filter(|agent| agent.agent_status == AgentStatus::Working)
            .count()),
        other => Err(format!("unexpected agent list result: {other:?}")),
    }
}

fn request_handoff(client: &ApiClient, new_exe: &Path, target: &BinaryFacts) -> Result<(), String> {
    let request = Request {
        id: "cli:server:swap:live-handoff".into(),
        // Reuse the existing landing guard: the import server refuses the
        // manifest unless it really is this version and protocol.
        method: Method::ServerLiveHandoff(ServerLiveHandoffParams {
            import_exe: Some(new_exe.display().to_string()),
            expected_protocol: Some(target.protocol),
            expected_version: Some(target.version.clone()),
        }),
    };
    let response = client
        .request_value_with_timeout(&request, HANDOFF_REQUEST_TIMEOUT)
        .map_err(|err| err.to_string())?;
    match response.get("error") {
        Some(error) => Err(error.to_string()),
        None => Ok(()),
    }
}

fn wait_for_target_server(socket: &Path, target: &BinaryFacts) -> Result<(), String> {
    let deadline = Instant::now() + HANDOFF_CONFIRM_TIMEOUT;
    loop {
        if let Ok(Some(status)) = crate::api::read_runtime_status_at(socket, STATUS_POLL_TIMEOUT) {
            if status.protocol == Some(target.protocol)
                && status.version.as_deref() == Some(target.version.as_str())
            {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no server answering as v{} protocol {} appeared on {} within {} seconds",
                target.version,
                target.protocol,
                socket.display(),
                HANDOFF_CONFIRM_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(HANDOFF_POLL_INTERVAL);
    }
}

fn prompt_yes_no(question: &str) -> io::Result<bool> {
    loop {
        eprint!("{question} [y/N] ");
        io::stderr().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            return Ok(false);
        }
        match input.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => return Ok(true),
            "" | "n" | "no" => return Ok(false),
            _ => eprintln!("please answer y or n"),
        }
    }
}

/// Resolve the client path to overwrite, if any.
pub(super) fn promote_destination(
    choice: &PromoteChoice,
    driving_client: Option<PathBuf>,
) -> Option<PathBuf> {
    match choice {
        PromoteChoice::Skip => None,
        PromoteChoice::Path(path) => Some(path.clone()),
        PromoteChoice::DrivingClient => driving_client,
    }
}

enum Promotion {
    Installed(PathBuf),
    AlreadyTheTarget(PathBuf),
    Skipped,
}

fn driving_client_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok())
}

fn promote_client(new_exe: &Path, choice: &PromoteChoice) -> Result<Promotion, String> {
    let Some(destination) = promote_destination(choice, driving_client_path()) else {
        return Ok(Promotion::Skipped);
    };
    if std::fs::canonicalize(&destination).is_ok_and(|resolved| resolved == new_exe) {
        return Ok(Promotion::AlreadyTheTarget(destination));
    }
    install_atomically(new_exe, &destination)?;
    Ok(Promotion::Installed(destination))
}

/// Copy into the destination directory and rename over the target, so no
/// observer ever sees a partially written client on PATH.
fn install_atomically(source: &Path, destination: &Path) -> Result<(), String> {
    let directory = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", destination.display()))?;
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", destination.display()))?;
    let staged = directory.join(format!(".{file_name}.swap-{}", std::process::id()));

    let result = (|| -> Result<(), String> {
        std::fs::copy(source, &staged)
            .map_err(|err| format!("failed to stage {}: {err}", staged.display()))?;
        set_executable(&staged)?;
        std::fs::rename(&staged, destination).map_err(|err| {
            format!(
                "failed to move {} into place at {}: {err}",
                staged.display(),
                destination.display()
            )
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .map_err(|err| format!("failed to mark {} executable: {err}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn manual_promote_hint(new_exe: &Path, choice: &PromoteChoice) -> String {
    // Resolve the driving client the same way promotion would, so the hint
    // names the path the command would have written to.
    match promote_destination(choice, driving_client_path()) {
        Some(destination) => format!(
            "the client at {} still speaks the old protocol; install the new one yourself with `cp {} {}` once you are satisfied.",
            destination.display(),
            new_exe.display(),
            destination.display()
        ),
        None => format!(
            "make sure the herdr client you use next is {}.",
            new_exe.display()
        ),
    }
}

fn start_command_for(new_exe: &Path) -> String {
    match crate::session::active_name() {
        Some(name) => format!("{} --session {name}", new_exe.display()),
        None => new_exe.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(version: &str, protocol: u32) -> BinaryFacts {
        BinaryFacts {
            version: version.to_string(),
            protocol,
        }
    }

    fn server(version: &str, protocol: u32) -> ServerFacts {
        ServerFacts {
            version: Some(version.to_string()),
            protocol: Some(protocol),
            live_handoff: true,
        }
    }

    fn input(
        server: Option<ServerFacts>,
        client: BinaryFacts,
        target: BinaryFacts,
    ) -> PreflightInput {
        PreflightInput {
            client,
            server,
            target,
            allow_downgrade: false,
        }
    }

    #[test]
    fn parses_the_documented_flag_set() {
        let args = parse_swap_args(&[
            "--exe".to_string(),
            "/tmp/herdr".to_string(),
            "--dry-run".to_string(),
            "--allow-downgrade".to_string(),
            "--yes".to_string(),
            "--promote-client=/home/me/.local/bin/herdr".to_string(),
        ])
        .expect("args");

        assert_eq!(args.exe, PathBuf::from("/tmp/herdr"));
        assert!(args.dry_run);
        assert!(args.allow_downgrade);
        assert!(args.assume_yes);
        assert_eq!(
            args.promote,
            PromoteChoice::Path(PathBuf::from("/home/me/.local/bin/herdr"))
        );
    }

    #[test]
    fn exe_is_required_and_unknown_flags_are_rejected() {
        assert!(parse_swap_args(&["--dry-run".to_string()]).is_err());
        assert!(parse_swap_args(&["--exe".to_string()]).is_err());
        assert!(parse_swap_args(&[
            "--exe".to_string(),
            "/tmp/herdr".to_string(),
            "--force".to_string()
        ])
        .is_err());
        assert_eq!(
            parse_swap_args(&["--exe".to_string(), "/tmp/herdr".to_string()])
                .expect("args")
                .promote,
            PromoteChoice::DrivingClient
        );
    }

    #[test]
    fn no_promotion_when_explicitly_declined() {
        let args = parse_swap_args(&[
            "--exe".to_string(),
            "/tmp/herdr".to_string(),
            "--no-promote-client".to_string(),
        ])
        .expect("args");
        assert_eq!(args.promote, PromoteChoice::Skip);
        assert_eq!(
            promote_destination(&args.promote, Some(PathBuf::from("/usr/bin/herdr"))),
            None
        );
    }

    #[test]
    fn refuses_when_no_server_is_running() {
        let refusal = evaluate_preflight(&input(None, facts("0.7.5", 18), facts("0.7.6", 18)))
            .expect_err("refusal");
        assert_eq!(refusal.code, "server_not_running");
    }

    #[test]
    fn refuses_when_the_server_cannot_hand_off() {
        let mut running = server("0.6.0", 18);
        running.live_handoff = false;
        let refusal = evaluate_preflight(&input(
            Some(running),
            facts("0.7.5", 18),
            facts("0.7.6", 18),
        ))
        .expect_err("refusal");
        assert_eq!(refusal.code, "live_handoff_unsupported");
        assert!(refusal.message.contains("live_handoff"));
    }

    #[test]
    fn refuses_when_the_driving_client_is_on_another_protocol() {
        // The prototype's hard-won lesson: driving from the new binary breaks
        // every call, so it must be refused before anything is touched.
        let refusal = evaluate_preflight(&input(
            Some(server("0.7.5", 17)),
            facts("0.7.6", 18),
            facts("0.7.6", 18),
        ))
        .expect_err("refusal");
        assert_eq!(refusal.code, "protocol_mismatch");
        assert!(refusal.message.contains("protocol 17"));
        assert!(
            refusal.message.contains("0.7.5"),
            "the refusal must name the fix: {}",
            refusal.message
        );
    }

    #[test]
    fn refuses_a_protocol_downgrade_unless_allowed() {
        let downgrade = input(
            Some(server("0.7.6", 18)),
            facts("0.7.6", 18),
            facts("0.7.5", 17),
        );
        let refusal = evaluate_preflight(&downgrade).expect_err("refusal");
        assert_eq!(refusal.code, "downgrade_refused");
        assert!(refusal.message.contains("--allow-downgrade"));

        let allowed = PreflightInput {
            allow_downgrade: true,
            ..downgrade
        };
        assert_eq!(
            evaluate_preflight(&allowed).expect("plan").kind,
            SwapKind::Downgrade
        );
    }

    #[test]
    fn classifies_swap_direction_from_protocol_then_version() {
        assert_eq!(
            swap_kind(18, Some("0.7.5"), &facts("0.7.5", 18)),
            SwapKind::Refresh
        );
        assert_eq!(
            swap_kind(18, Some("0.7.5"), &facts("0.7.6", 18)),
            SwapKind::Upgrade
        );
        assert_eq!(
            swap_kind(18, Some("0.7.6"), &facts("0.7.5", 18)),
            SwapKind::Downgrade
        );
        assert_eq!(
            swap_kind(18, Some("0.7.5"), &facts("0.7.5", 19)),
            SwapKind::Upgrade
        );
        assert_eq!(
            swap_kind(19, Some("0.7.9"), &facts("0.8.0", 18)),
            SwapKind::Downgrade
        );
        // Unparsable versions must not be guessed into a downgrade refusal.
        assert_eq!(
            swap_kind(18, Some("0.7.5-dev"), &facts("0.7.5-dirty", 18)),
            SwapKind::Refresh
        );
        assert_eq!(swap_kind(18, None, &facts("0.7.5", 18)), SwapKind::Refresh);
    }

    #[test]
    fn same_protocol_same_version_is_a_refresh_and_is_allowed() {
        let plan = evaluate_preflight(&input(
            Some(server("0.7.5", 18)),
            facts("0.7.5", 18),
            facts("0.7.5", 18),
        ))
        .expect("plan");
        assert_eq!(plan.kind, SwapKind::Refresh);
        assert_eq!(plan.server_protocol, 18);
    }

    #[test]
    fn working_agents_gate_only_bites_without_a_terminal_or_yes() {
        assert_eq!(confirm_decision(0, false, false), ConfirmDecision::Proceed);
        assert_eq!(confirm_decision(3, true, false), ConfirmDecision::Proceed);
        assert_eq!(confirm_decision(3, false, true), ConfirmDecision::Prompt);
        assert_eq!(
            confirm_decision(3, false, false),
            ConfirmDecision::RefuseNonInteractive
        );
    }

    fn entry(id: &str, tabs: usize, panes: usize) -> InventoryEntry {
        InventoryEntry {
            workspace_id: id.to_string(),
            label: id.to_string(),
            tab_count: tabs,
            pane_count: panes,
        }
    }

    #[test]
    fn inventory_is_order_independent_and_reports_totals() {
        let before = Inventory::from_entries(vec![entry("b", 2, 5), entry("a", 1, 3)]);
        let after = Inventory::from_entries(vec![entry("a", 1, 3), entry("b", 2, 5)]);

        assert_eq!(before.entries[0].workspace_id, "a");
        assert_eq!(before.total_tabs(), 3);
        assert_eq!(before.total_panes(), 8);
        assert!(before.diff(&after).is_empty());
        assert!(!before.is_empty());
        assert!(Inventory::default().is_empty());
    }

    #[test]
    fn inventory_diff_names_lost_gained_and_resized_workspaces() {
        let before = Inventory::from_entries(vec![entry("a", 1, 3), entry("b", 2, 5)]);

        let lost = Inventory::from_entries(vec![entry("a", 1, 3)]);
        assert_eq!(before.diff(&lost).len(), 1);
        assert!(before.diff(&lost)[0].contains("is gone"));

        let resized = Inventory::from_entries(vec![entry("a", 1, 3), entry("b", 2, 4)]);
        assert!(before.diff(&resized)[0].contains("pane count changed: 5 -> 4"));

        let retabbed = Inventory::from_entries(vec![entry("a", 1, 3), entry("b", 3, 5)]);
        assert!(before.diff(&retabbed)[0].contains("tab count changed: 2 -> 3"));

        let gained =
            Inventory::from_entries(vec![entry("a", 1, 3), entry("b", 2, 5), entry("c", 1, 1)]);
        assert!(before.diff(&gained)[0].contains("appeared"));
    }

    #[test]
    fn inventory_parses_a_workspace_list_response_and_ignores_unknown_fields() {
        let inventory = Inventory::from_response(&serde_json::json!({
            "id": "cli:workspace:list",
            "result": {
                "type": "workspace_list",
                "workspaces": [
                    {
                        "workspace_id": "w2",
                        "label": "two",
                        "tab_count": 2,
                        "pane_count": 4,
                        "something_new_in_a_later_build": true
                    },
                    { "workspace_id": "w1", "label": "one", "tab_count": 1, "pane_count": 1 }
                ]
            }
        }))
        .expect("inventory");

        assert_eq!(inventory.entries.len(), 2);
        assert_eq!(inventory.entries[0].workspace_id, "w1");
        assert_eq!(inventory.total_panes(), 5);
    }

    #[test]
    fn inventory_refuses_error_and_malformed_responses() {
        let error = Inventory::from_response(&serde_json::json!({
            "id": "cli:workspace:list",
            "error": { "code": "protocol_mismatch", "message": "nope" }
        }))
        .expect_err("error");
        assert!(error.contains("protocol_mismatch"));

        assert!(Inventory::from_response(&serde_json::json!({ "result": {} })).is_err());
    }

    #[test]
    fn failure_classification_distinguishes_old_new_and_missing_servers() {
        let target = facts("0.7.6", 18);
        let status = |version: &str, protocol: u32| {
            Ok(Some(crate::api::RuntimeStatus {
                version: Some(version.to_string()),
                protocol: Some(protocol),
                capabilities: None,
            }))
        };

        assert_eq!(
            classify_post_handoff(&status("0.7.6", 18), &target, false),
            PostHandoffState::NewServerRunning
        );
        assert_eq!(
            classify_post_handoff(&status("0.7.5", 17), &target, false),
            PostHandoffState::OldServerRunning {
                version: Some("0.7.5".to_string()),
                protocol: Some(17),
            }
        );
        assert_eq!(
            classify_post_handoff(&Ok(None), &target, false),
            PostHandoffState::NoServerResponding
        );
        assert_eq!(
            classify_post_handoff(&Err("broken pipe".to_string()), &target, false),
            PostHandoffState::Unknown("broken pipe".to_string())
        );
    }

    #[test]
    fn a_refresh_never_claims_the_handoff_landed_from_a_ping_alone() {
        let target = facts("0.7.6", 18);
        let status = Ok(Some(crate::api::RuntimeStatus {
            version: Some("0.7.6".to_string()),
            protocol: Some(18),
            capabilities: None,
        }));

        assert_eq!(
            classify_post_handoff(&status, &target, true),
            PostHandoffState::TargetLikeServerRunning {
                version: Some("0.7.6".to_string()),
                protocol: Some(18),
            }
        );

        let report = failure_report(
            &classify_post_handoff(&status, &target, true),
            &FailureContext {
                new_exe: "/build/herdr".to_string(),
                socket: "/run/herdr.sock".to_string(),
                start_command: "/build/herdr".to_string(),
            },
        )
        .join("\n");
        assert!(report.contains("panes are intact"), "{report}");
        assert!(report.contains("cannot tell them apart"), "{report}");
        assert!(report.contains("no client was promoted"), "{report}");
    }

    #[test]
    fn failure_report_says_what_survived_and_how_to_recover() {
        let context = FailureContext {
            new_exe: "/build/herdr".to_string(),
            socket: "/run/herdr.sock".to_string(),
            start_command: "/build/herdr --session lab".to_string(),
        };

        let kept = failure_report(
            &PostHandoffState::OldServerRunning {
                version: Some("0.7.5".to_string()),
                protocol: Some(17),
            },
            &context,
        )
        .join("\n");
        assert!(kept.contains("still running"));
        assert!(kept.contains("nothing was changed"));

        let gone = failure_report(&PostHandoffState::NoServerResponding, &context).join("\n");
        assert!(
            gone.contains("/build/herdr --session lab"),
            "recovery must print the exact command: {gone}"
        );

        let unknown =
            failure_report(&PostHandoffState::Unknown("timeout".to_string()), &context).join("\n");
        assert!(unknown.contains("could not determine"));
    }

    #[test]
    fn install_atomically_replaces_the_destination() {
        let dir = std::env::temp_dir().join(format!("herdr-swap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let source = dir.join("new-herdr");
        let destination = dir.join("herdr");
        std::fs::write(&source, b"new").expect("source");
        std::fs::write(&destination, b"old").expect("destination");

        install_atomically(&source, &destination).expect("install");

        assert_eq!(std::fs::read(&destination).expect("read"), b"new");
        let staged: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".herdr.swap-")
            })
            .collect();
        assert!(staged.is_empty(), "staging file must not be left behind");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn install_atomically_reports_a_missing_source_without_touching_the_destination() {
        let dir = std::env::temp_dir().join(format!("herdr-swap-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let destination = dir.join("herdr");
        std::fs::write(&destination, b"old").expect("destination");

        let err = install_atomically(&dir.join("absent"), &destination).expect_err("error");

        assert!(err.contains("failed to stage"));
        assert_eq!(std::fs::read(&destination).expect("read"), b"old");
        std::fs::remove_dir_all(&dir).ok();
    }
}
