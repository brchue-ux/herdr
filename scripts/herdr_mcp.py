#!/usr/bin/env python3
"""Read-only MCP server exposing live Herdr session state.

Herdr's JSON API is a newline-delimited request/response protocol on a Unix
socket (`src/api/`, `src/server/socket_paths.rs`). This server is a plain MCP
stdio client of that socket, so an assistant can ask what panes, tabs,
workspaces and agents actually exist instead of inferring them from a stale
screenshot.

Two properties are deliberate:

* **No Herdr build is involved.** Nothing here links against the crate, and no
  new server capability is required. It speaks the published wire protocol to
  whichever `herdr` is already running, so it keeps working across Herdr
  upgrades and never gates on installing a new binary.
* **Read-only is enforced here, not merely intended.** Every request is checked
  against `READ_ONLY_METHODS` before a byte reaches the socket, so a method that
  changes session state cannot be sent even if a caller names it explicitly.
  See `SIDE_EFFECT_NOTES` for the two read-shaped methods that are excluded
  because they are not actually side-effect free.

Run it with no arguments to serve MCP over stdio:

    python3 scripts/herdr_mcp.py

`--selftest` prints the tool list and a live overview to stdout instead, which
is a useful smoke check against a running session:

    python3 scripts/herdr_mcp.py --selftest --session my-lab
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
from pathlib import Path
from typing import Any, Callable, Iterable

SERVER_NAME = "herdr"
SERVER_VERSION = "0.1.0"

# MCP revisions this server understands. The newest is used when a client asks
# for something outside the set, which is what the spec prescribes.
SUPPORTED_PROTOCOL_VERSIONS = ("2025-06-18", "2025-03-26", "2024-11-05")
DEFAULT_PROTOCOL_VERSION = SUPPORTED_PROTOCOL_VERSIONS[0]

DEFAULT_SESSION_NAME = "default"
MAX_SESSION_NAME_LEN = 64
DEFAULT_TIMEOUT_SECONDS = 10.0

# Env overrides, all optional. `HERDR_MCP_SOCKET` is the escape hatch for a
# session whose socket does not live where `src/session.rs` would put it.
SOCKET_OVERRIDE_ENV_VAR = "HERDR_MCP_SOCKET"
APP_DIR_ENV_VAR = "HERDR_MCP_APP_DIR"
TIMEOUT_ENV_VAR = "HERDR_MCP_TIMEOUT_SECONDS"


class HerdrApiError(RuntimeError):
    """A Herdr API call failed, or was refused before it was sent."""


# ---------------------------------------------------------------------------
# Read-only policy
# ---------------------------------------------------------------------------

# Methods this connector may send, each with the one-line summary shown to the
# model in the `herdr_query` tool schema. Anything absent is refused.
#
# Membership is a claim that the method does not change session state. It is not
# derived from Herdr's own `request_changes_ui`: that predicate answers "should
# the TUI redraw", which is a narrower question than "did this perturb the
# session", and two methods it calls read-only are excluded below.
READ_ONLY_METHODS: dict[str, str] = {
    "ping": "Server version, wire protocol number and capabilities.",
    "session.snapshot": (
        "Everything at once: workspaces, tabs, panes, agents, layouts, focus, "
        "machine register. The cheapest way to answer a broad question."
    ),
    "workspace.list": "All workspaces with label, pane/tab counts, agent status, worktree.",
    "workspace.get": 'One workspace. Params: {"workspace_id": "..."}.',
    "tab.list": 'Tabs, optionally one workspace. Params: {"workspace_id": "..."} (optional).',
    "tab.get": 'One tab. Params: {"tab_id": "..."}.',
    "pane.list": 'Panes, optionally one workspace. Params: {"workspace_id": "..."} (optional).',
    "pane.get": 'One pane. Params: {"pane_id": "..."}.',
    "pane.current": "The pane the calling process is running in, when there is one.",
    "pane.layout": 'Split layout tree for a pane\'s tab. Params: {"pane_id": "..."} (optional).',
    "pane.process_info": (
        'Shell pid, tty and foreground processes for a pane. '
        'Params: {"pane_id": "..."} (optional).'
    ),
    "pane.neighbor": 'Pane adjacent in a direction. Params: {"pane_id": "...", "direction": "left"}.',
    "pane.edges": 'Which edges of its tab a pane touches. Params: {"pane_id": "..."}.',
    "layout.export": (
        'Serialisable layout for a tab or pane. '
        'Params: {"tab_id": "..."} or {"pane_id": "..."} (both optional).'
    ),
    "agent.list": "Every detected agent: status, relation, owner, tokens, pane and workspace.",
    "agent.get": 'One agent. Params: {"target": "<pane id, agent name or number>"}.',
    "agent.explain": (
        'Why detection assigned a pane its agent status. '
        'Params: {"target": "<pane id, agent name or number>"}.'
    ),
    "worktree.list": (
        'Git worktrees Herdr knows about and which workspace has each open. '
        'Params: {"workspace_id": "..."} or {"cwd": "..."} (both optional).'
    ),
    "server.agent_manifests": "The agent detection manifests currently loaded.",
    "plugin.list": 'Installed plugins. Params: {"plugin_id": "..."} (optional).',
    "plugin.action.list": 'Actions a plugin declares. Params: {"plugin_id": "..."} (optional).',
    "plugin.log.list": 'Recent plugin command log entries. Params: {"plugin_id": "..."} (optional).',
    "surface.graphics.info": 'Graphics on a named surface. Params: {"surface": "..."}.',
    "pane.graphics.info": 'Graphics on a pane. Params: {"pane_id": "..."}.',
}

# `pane.read` is allowed, but only through `herdr_pane_read`, which constrains
# `source`. It is kept out of `READ_ONLY_METHODS` so `herdr_query` cannot reach
# it with an unconstrained `source`.
PANE_READ_METHOD = "pane.read"

# Pane content sources that never drive the alt-screen harvest. See
# `SIDE_EFFECT_NOTES`.
SAFE_READ_SOURCES = ("visible", "detection", "transcript")

SIDE_EFFECT_NOTES = """\
Excluded despite looking read-only:

  pane.read with source=recent / recent_unwrapped (format=text)
  agent.read with source=recent / recent_unwrapped (format=text)

Both hand the request to the alt-screen harvest (src/server/alt_screen_read.rs),
which scrolls the pane's own full-screen application by injecting real mouse
wheel events for up to 15 seconds and then scrolls it back. It holds the
presented frame while it runs, so the pane is frozen rather than visibly moving,
but it is still synthetic input delivered to a live process. That is a mutation,
so this connector never sends it. `herdr_pane_read` offers source=visible
(default), detection and transcript, none of which enter that path."""


def is_read_only_method(method: str) -> bool:
    return method in READ_ONLY_METHODS


# ---------------------------------------------------------------------------
# Socket resolution — mirrors src/session.rs and src/config/io.rs
# ---------------------------------------------------------------------------


def app_dir_name() -> str:
    return os.environ.get(APP_DIR_ENV_VAR) or "herdr"


def config_dir() -> Path:
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg:
        return Path(xdg) / app_dir_name()
    home = os.environ.get("HOME")
    if home:
        return Path(home) / ".config" / app_dir_name()
    return Path("/tmp") / app_dir_name()


def validate_session_name(name: str) -> None:
    """Reject names `src/session.rs::validate_name` would reject."""
    if not name:
        raise HerdrApiError("session name cannot be empty")
    if len(name.encode("utf-8")) > MAX_SESSION_NAME_LEN:
        raise HerdrApiError(f"session name cannot be longer than {MAX_SESSION_NAME_LEN} bytes")
    if name in (".", ".."):
        raise HerdrApiError("session name cannot be . or ..")
    for char in name:
        if not (char.isascii() and (char.isalnum() or char in "._-")):
            raise HerdrApiError(
                "session name may only contain ASCII letters, numbers, '.', '_' and '-'"
            )


def session_data_dir(session: str | None) -> Path:
    if session is None or session == DEFAULT_SESSION_NAME:
        return config_dir()
    validate_session_name(session)
    return config_dir() / "sessions" / session


def api_socket_path(session: str | None) -> Path:
    override = os.environ.get(SOCKET_OVERRIDE_ENV_VAR)
    if override:
        return Path(override)
    return session_data_dir(session) / "herdr.sock"


def socket_is_live(path: Path) -> bool:
    """True when something is accepting connections on `path` right now."""
    if not path.exists():
        return False
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(0.5)
            sock.connect(str(path))
        return True
    except OSError:
        return False


def list_sessions() -> list[dict[str, Any]]:
    """Every session Herdr would list, default first, each with liveness."""
    sessions: list[dict[str, Any]] = []

    def entry(name: str, is_default: bool) -> dict[str, Any]:
        path = api_socket_path(None if is_default else name)
        return {
            "name": name,
            "default": is_default,
            "running": socket_is_live(path),
            "socket_path": str(path),
            "session_dir": str(session_data_dir(None if is_default else name)),
        }

    sessions.append(entry(DEFAULT_SESSION_NAME, True))

    sessions_dir = config_dir() / "sessions"
    names: list[str] = []
    try:
        for child in sessions_dir.iterdir():
            if not child.is_dir():
                continue
            name = child.name
            if name == DEFAULT_SESSION_NAME:
                continue
            try:
                validate_session_name(name)
            except HerdrApiError:
                continue
            names.append(name)
    except (FileNotFoundError, NotADirectoryError, PermissionError):
        names = []

    sessions.extend(entry(name, False) for name in sorted(names))
    return sessions


# ---------------------------------------------------------------------------
# Herdr API client
# ---------------------------------------------------------------------------


class HerdrClient:
    """Newline-delimited JSON client for one Herdr session's API socket."""

    def __init__(self, session: str | None = None, timeout: float | None = None) -> None:
        self.session = session
        if timeout is None:
            try:
                timeout = float(os.environ.get(TIMEOUT_ENV_VAR, DEFAULT_TIMEOUT_SECONDS))
            except ValueError:
                timeout = DEFAULT_TIMEOUT_SECONDS
        self.timeout = timeout
        self._request_seq = 0

    def socket_path(self) -> Path:
        return api_socket_path(self.session)

    def call(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """Send one allowlisted request and return its `result` payload.

        Refuses, without connecting, any method outside the read-only set.
        """
        if method != PANE_READ_METHOD and not is_read_only_method(method):
            raise HerdrApiError(
                f"refused: '{method}' is not in this connector's read-only allowlist. "
                "This connector never sends a method that can change session state."
            )
        self._request_seq += 1
        request = {
            "id": f"mcp-{os.getpid()}-{self._request_seq}",
            "method": method,
            "params": params or {},
        }
        response = self._roundtrip(request)

        if "error" in response:
            error = response.get("error") or {}
            code = error.get("code", "error")
            message = error.get("message", "unknown error")
            raise HerdrApiError(f"herdr {method} failed [{code}]: {message}")
        if "result" not in response:
            raise HerdrApiError(f"herdr {method} returned no result: {response!r}")
        return response["result"]

    def _roundtrip(self, request: dict[str, Any]) -> dict[str, Any]:
        path = self.socket_path()
        payload = (json.dumps(request) + "\n").encode("utf-8")
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.settimeout(self.timeout)
                sock.connect(str(path))
                sock.sendall(payload)
                line = self._read_line(sock)
        except FileNotFoundError as err:
            raise HerdrApiError(
                f"no herdr session at {path}. "
                "Use herdr_sessions to see which sessions are running."
            ) from err
        except ConnectionRefusedError as err:
            raise HerdrApiError(
                f"herdr socket at {path} exists but nothing is listening "
                "(the session is stopped; the socket file is stale)."
            ) from err
        except socket.timeout as err:
            raise HerdrApiError(
                f"herdr did not answer {request['method']} within {self.timeout:g}s"
            ) from err
        except OSError as err:
            raise HerdrApiError(f"herdr socket error at {path}: {err}") from err

        if not line:
            raise HerdrApiError("herdr closed the connection without answering")
        try:
            return json.loads(line)
        except json.JSONDecodeError as err:
            raise HerdrApiError(f"herdr sent malformed JSON: {err}") from err

    def _read_line(self, sock: socket.socket) -> bytes:
        chunks: list[bytes] = []
        while True:
            chunk = sock.recv(65536)
            if not chunk:
                break
            newline = chunk.find(b"\n")
            if newline >= 0:
                chunks.append(chunk[:newline])
                break
            chunks.append(chunk)
        return b"".join(chunks)


# ---------------------------------------------------------------------------
# Digest rendering
# ---------------------------------------------------------------------------

STATUS_MARK = {
    "working": "*",
    "blocked": "!",
    "done": "+",
    "idle": ".",
    "unknown": "?",
}


def _short(value: Any, limit: int = 72) -> str:
    text = " ".join(str(value).split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def _home_relative(path: str) -> str:
    home = os.environ.get("HOME")
    if home and (path == home or path.startswith(home + "/")):
        return "~" + path[len(home) :]
    return path


def render_overview(snapshot: dict[str, Any], workspace_filter: str | None = None) -> str:
    """Render `session.snapshot` as a compact workspace -> tab -> pane tree."""
    workspaces = snapshot.get("workspaces") or []
    tabs = snapshot.get("tabs") or []
    panes = snapshot.get("panes") or []
    agents_by_pane = {
        agent.get("pane_id"): agent for agent in (snapshot.get("agents") or []) if agent.get("pane_id")
    }

    if workspace_filter:
        wanted = {
            workspace.get("workspace_id")
            for workspace in workspaces
            if workspace_filter in (workspace.get("workspace_id"), workspace.get("label"))
            or str(workspace.get("number")) == workspace_filter
        }
        if not wanted:
            return f"no workspace matching {workspace_filter!r}"
        workspaces = [w for w in workspaces if w.get("workspace_id") in wanted]
        tabs = [t for t in tabs if t.get("workspace_id") in wanted]
        panes = [p for p in panes if p.get("workspace_id") in wanted]

    lines: list[str] = []
    header = f"herdr {snapshot.get('version', '?')} (protocol {snapshot.get('protocol', '?')})"
    counts = f"{len(workspaces)} workspace(s), {len(tabs)} tab(s), {len(panes)} pane(s)"
    lines.append(f"{header} - {counts}")
    if snapshot.get("status"):
        lines.append(f"session status: {_short(snapshot['status'])}")
    lines.append("")

    panes_by_tab: dict[str, list[dict[str, Any]]] = {}
    for pane in panes:
        panes_by_tab.setdefault(pane.get("tab_id", ""), []).append(pane)
    tabs_by_workspace: dict[str, list[dict[str, Any]]] = {}
    for tab in tabs:
        tabs_by_workspace.setdefault(tab.get("workspace_id", ""), []).append(tab)

    for workspace in workspaces:
        workspace_id = workspace.get("workspace_id", "")
        mark = STATUS_MARK.get(workspace.get("agent_status", "unknown"), "?")
        focus = " [focused]" if workspace.get("focused") else ""
        lines.append(
            f"{mark} workspace {workspace.get('number')} {workspace.get('label', '')!r} "
            f"<{workspace_id}>{focus}"
        )
        worktree = workspace.get("worktree")
        if worktree:
            kind = "linked worktree" if worktree.get("is_linked_worktree") else "checkout"
            lines.append(
                f"    {kind}: {_home_relative(worktree.get('checkout_path', ''))} "
                f"({worktree.get('repo_name', '')})"
            )
        tokens = workspace.get("tokens") or {}
        if tokens:
            lines.append("    tokens: " + _short(", ".join(f"{k}={v}" for k, v in sorted(tokens.items())), 100))
        if workspace.get("absorbed"):
            lines.append(f"    absorbed: {workspace['absorbed']}")

        for tab in tabs_by_workspace.get(workspace_id, []):
            tab_id = tab.get("tab_id", "")
            tab_focus = " [focused]" if tab.get("focused") else ""
            lines.append(
                f"  tab {tab.get('number')} {tab.get('label', '')!r} "
                f"<{tab_id}>{tab_focus} - {tab.get('pane_count', 0)} pane(s)"
            )
            for pane in panes_by_tab.get(tab_id, []):
                lines.extend(_render_pane(pane, agents_by_pane.get(pane.get("pane_id"))))
        lines.append("")

    machine = snapshot.get("machine_register") or {}
    if machine.get("reading"):
        summary = ", ".join(
            f"{quantity.get('name')}={quantity['value']:.0%}"
            for quantity in machine.get("quantities") or []
            if quantity.get("value") is not None
        )
        if summary:
            lines.append(f"machine: {summary}")

    return "\n".join(lines).rstrip() + "\n"


def _render_pane(pane: dict[str, Any], agent: dict[str, Any] | None) -> list[str]:
    mark = STATUS_MARK.get(pane.get("agent_status", "unknown"), "?")
    bits: list[str] = []
    name = pane.get("label") or pane.get("title") or pane.get("terminal_title_stripped")
    if name:
        bits.append(repr(_short(name, 48)))
    agent_name = pane.get("display_agent") or pane.get("agent")
    if agent_name:
        bits.append(f"agent={agent_name}")
    bits.append(f"status={pane.get('agent_status', 'unknown')}")
    if pane.get("focused"):
        bits.append("focused")
    if pane.get("unread"):
        bits.append("unread")
    line = f"    {mark} pane <{pane.get('pane_id')}> " + " ".join(bits)
    lines = [line]

    detail: list[str] = []
    cwd = pane.get("foreground_cwd") or pane.get("cwd")
    if cwd:
        detail.append(f"cwd={_home_relative(cwd)}")
    if pane.get("owner"):
        detail.append(f"owner={pane['owner']}")
    if agent and agent.get("relation"):
        detail.append(f"relation={agent['relation']}")
    if agent and agent.get("state_age_ms") is not None:
        detail.append(f"held_for={round(agent['state_age_ms'] / 1000)}s")
    if pane.get("absorbed"):
        detail.append(f"absorbed={pane['absorbed']}")
    if detail:
        lines.append("        " + "  ".join(detail))

    tokens = pane.get("tokens") or {}
    if tokens:
        lines.append(
            "        tokens: "
            + _short(", ".join(f"{k}={v}" for k, v in sorted(tokens.items())), 100)
        )
    return lines


def render_pane_detail(pane: dict[str, Any], process: dict[str, Any] | None) -> str:
    lines = [f"pane <{pane.get('pane_id')}>"]
    for key in (
        "workspace_id",
        "tab_id",
        "terminal_id",
        "label",
        "title",
        "terminal_title_stripped",
        "agent",
        "display_agent",
        "declared_agent",
        "agent_status",
        "focused",
        "unread",
        "cwd",
        "foreground_cwd",
        "owner",
        "absorbed",
        "revision",
    ):
        if pane.get(key) not in (None, "", False):
            lines.append(f"  {key}: {pane[key]}")
    for key in ("tokens", "state_labels", "agent_session", "scroll", "activity", "created_by"):
        if pane.get(key):
            lines.append(f"  {key}: {json.dumps(pane[key], sort_keys=True)}")
    if process:
        lines.append("  process:")
        for key in ("shell_pid", "foreground_process_group_id", "tty"):
            if process.get(key) is not None:
                lines.append(f"    {key}: {process[key]}")
        for entry in process.get("foreground_processes") or []:
            command = entry.get("cmdline") or entry.get("name")
            lines.append(f"    pid {entry.get('pid')}: {_short(command, 100)}")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# Tools
# ---------------------------------------------------------------------------

SESSION_PROPERTY = {
    "type": "string",
    "description": (
        "Herdr session name. Omit for the default session. "
        "Use herdr_sessions to discover names."
    ),
}


def _tool_definitions() -> list[dict[str, Any]]:
    method_lines = "\n".join(f"  {name} - {desc}" for name, desc in READ_ONLY_METHODS.items())
    return [
        {
            "name": "herdr_sessions",
            "description": (
                "List Herdr sessions on this machine and whether each is running. "
                "Start here when you do not know which session to query."
            ),
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
        },
        {
            "name": "herdr_overview",
            "description": (
                "Live state of a Herdr session as a workspace -> tab -> pane tree: labels, "
                "agent busy/idle status, focus, cwd, git worktree association, ownership and "
                "published metadata tokens. One call answers most questions about what is "
                "running right now."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": SESSION_PROPERTY,
                    "workspace": {
                        "type": "string",
                        "description": "Limit to one workspace by id, label or number.",
                    },
                    "format": {
                        "type": "string",
                        "enum": ["text", "json"],
                        "description": (
                            "text (default) is a compact digest; json returns the raw "
                            "session.snapshot payload."
                        ),
                    },
                },
                "additionalProperties": False,
            },
        },
        {
            "name": "herdr_pane",
            "description": (
                "Everything known about one pane, including its shell pid, tty and "
                "foreground processes."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": SESSION_PROPERTY,
                    "pane_id": {"type": "string", "description": "Pane id from herdr_overview."},
                    "format": {"type": "string", "enum": ["text", "json"]},
                },
                "required": ["pane_id"],
                "additionalProperties": False,
            },
        },
        {
            "name": "herdr_pane_read",
            "description": (
                "Text currently on a pane's screen. source=visible (default) is exactly what "
                "the pane shows; detection is the bottom region Herdr matches agent state "
                "against; transcript is agent output with the input composer removed. "
                "Scrollback sources are deliberately unavailable: they make Herdr scroll the "
                "live pane to harvest history, which is not a read."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": SESSION_PROPERTY,
                    "pane_id": {"type": "string"},
                    "source": {"type": "string", "enum": list(SAFE_READ_SOURCES)},
                    "lines": {
                        "type": "integer",
                        "description": "Maximum lines to return (server caps this).",
                    },
                },
                "required": ["pane_id"],
                "additionalProperties": False,
            },
        },
        {
            "name": "herdr_query",
            "description": (
                "Call any read-only Herdr JSON API method directly and return its raw result. "
                "Use when the shaped tools above do not carry the field you need.\n\n"
                "Available methods:\n" + method_lines
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": SESSION_PROPERTY,
                    "method": {"type": "string", "enum": sorted(READ_ONLY_METHODS)},
                    "params": {
                        "type": "object",
                        "description": "Method parameters; omit when the method takes none.",
                    },
                },
                "required": ["method"],
                "additionalProperties": False,
            },
        },
    ]


def tool_herdr_sessions(_client_factory: Callable[[str | None], HerdrClient], _args: dict) -> str:
    sessions = list_sessions()
    lines = []
    for entry in sessions:
        state = "running" if entry["running"] else "stopped"
        default = " (default)" if entry["default"] else ""
        lines.append(f"{entry['name']}{default}: {state} - {entry['socket_path']}")
    if not any(entry["running"] for entry in sessions):
        lines.append("")
        lines.append("No session is running; start herdr, or check HERDR_MCP_SOCKET.")
    return "\n".join(lines) + "\n"


def tool_herdr_overview(client_factory: Callable[[str | None], HerdrClient], args: dict) -> str:
    client = client_factory(args.get("session"))
    result = client.call("session.snapshot")
    snapshot = result.get("snapshot", result)
    if args.get("format") == "json":
        return json.dumps(snapshot, indent=2, sort_keys=True)
    return render_overview(snapshot, args.get("workspace"))


def tool_herdr_pane(client_factory: Callable[[str | None], HerdrClient], args: dict) -> str:
    client = client_factory(args.get("session"))
    pane_id = args["pane_id"]
    pane = client.call("pane.get", {"pane_id": pane_id}).get("pane", {})
    try:
        process = client.call("pane.process_info", {"pane_id": pane_id}).get("process_info")
    except HerdrApiError:
        # Process inspection is best effort: a pane whose shell has exited still
        # has state worth reporting.
        process = None
    if args.get("format") == "json":
        return json.dumps({"pane": pane, "process": process}, indent=2, sort_keys=True)
    return render_pane_detail(pane, process)


def tool_herdr_pane_read(client_factory: Callable[[str | None], HerdrClient], args: dict) -> str:
    source = args.get("source", "visible")
    if source not in SAFE_READ_SOURCES:
        raise HerdrApiError(
            f"refused: source={source!r} is not read-only. "
            f"Allowed: {', '.join(SAFE_READ_SOURCES)}.\n\n{SIDE_EFFECT_NOTES}"
        )
    client = client_factory(args.get("session"))
    params: dict[str, Any] = {
        "pane_id": args["pane_id"],
        "source": source,
        "format": "text",
        "strip_ansi": True,
    }
    if args.get("lines") is not None:
        params["lines"] = int(args["lines"])
    read = client.call(PANE_READ_METHOD, params).get("read", {})
    text = read.get("text", "")
    notes = []
    if read.get("truncated"):
        notes.append("(older rows omitted)")
    if read.get("transcript_applied") is False and source == "transcript":
        notes.append("(no transcript region for this agent; fell back to recent output)")
    return (text + ("\n" + " ".join(notes) if notes else "")).rstrip() + "\n"


def tool_herdr_query(client_factory: Callable[[str | None], HerdrClient], args: dict) -> str:
    method = args["method"]
    if not is_read_only_method(method):
        raise HerdrApiError(
            f"refused: '{method}' is not in this connector's read-only allowlist.\n\n"
            f"{SIDE_EFFECT_NOTES}"
        )
    client = client_factory(args.get("session"))
    result = client.call(method, args.get("params") or {})
    return json.dumps(result, indent=2, sort_keys=True)


TOOLS: dict[str, Callable[[Callable[[str | None], HerdrClient], dict], str]] = {
    "herdr_sessions": tool_herdr_sessions,
    "herdr_overview": tool_herdr_overview,
    "herdr_pane": tool_herdr_pane,
    "herdr_pane_read": tool_herdr_pane_read,
    "herdr_query": tool_herdr_query,
}


# ---------------------------------------------------------------------------
# MCP stdio server
# ---------------------------------------------------------------------------


class McpServer:
    """Minimal MCP stdio server: JSON-RPC 2.0, one message per line.

    Hand-rolled rather than built on the MCP SDK so the connector stays a single
    stdlib-only file with nothing to install, which is what lets it run against
    whichever Herdr is already on the machine.
    """

    def __init__(
        self,
        client_factory: Callable[[str | None], HerdrClient] | None = None,
        default_session: str | None = None,
    ) -> None:
        self.default_session = default_session
        self._client_factory = client_factory or (
            lambda session: HerdrClient(session if session is not None else self.default_session)
        )
        self.protocol_version = DEFAULT_PROTOCOL_VERSION

    def handle(self, message: dict[str, Any]) -> dict[str, Any] | None:
        """Return the response for `message`, or None for a notification."""
        method = message.get("method")
        message_id = message.get("id")
        is_notification = message_id is None

        try:
            if method == "initialize":
                result = self._initialize(message.get("params") or {})
            elif method in ("notifications/initialized", "initialized"):
                return None
            elif method == "ping":
                result = {}
            elif method == "tools/list":
                result = {"tools": _tool_definitions()}
            elif method == "tools/call":
                result = self._call_tool(message.get("params") or {})
            elif method in ("resources/list", "prompts/list"):
                # Declared capabilities do not include these, but some clients
                # probe anyway; an empty list is friendlier than an error.
                key = "resources" if method.startswith("resources") else "prompts"
                result = {key: []}
            elif is_notification:
                return None
            else:
                return _rpc_error(message_id, -32601, f"unknown method: {method}")
        except HerdrApiError as err:
            if is_notification:
                return None
            if method == "tools/call":
                return _rpc_result(message_id, _tool_error(str(err)))
            return _rpc_error(message_id, -32000, str(err))
        except Exception as err:  # noqa: BLE001 - a tool bug must not kill the server
            if is_notification:
                return None
            return _rpc_error(message_id, -32603, f"{type(err).__name__}: {err}")

        if is_notification:
            return None
        return _rpc_result(message_id, result)

    def _initialize(self, params: dict[str, Any]) -> dict[str, Any]:
        requested = params.get("protocolVersion")
        if requested in SUPPORTED_PROTOCOL_VERSIONS:
            self.protocol_version = requested
        return {
            "protocolVersion": self.protocol_version,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "instructions": (
                "Read-only view of live Herdr sessions. Call herdr_overview for what is "
                "running now; herdr_sessions when you do not know the session name. "
                "Nothing here can change session state."
            ),
        }

    def _call_tool(self, params: dict[str, Any]) -> dict[str, Any]:
        name = params.get("name")
        handler = TOOLS.get(name)
        if handler is None:
            raise HerdrApiError(f"unknown tool: {name}")
        arguments = params.get("arguments") or {}
        text = handler(self._client_factory, arguments)
        return {"content": [{"type": "text", "text": text}], "isError": False}

    def serve(self, stdin: Iterable[str], stdout: Any) -> None:
        for line in stdin:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError as err:
                _write(stdout, _rpc_error(None, -32700, f"parse error: {err}"))
                continue
            if isinstance(message, list):
                # Batches were removed in the 2025-06-18 revision; older clients
                # may still send them.
                for response in filter(None, (self.handle(item) for item in message)):
                    _write(stdout, response)
                continue
            if not isinstance(message, dict):
                _write(stdout, _rpc_error(None, -32600, "invalid request"))
                continue
            response = self.handle(message)
            if response is not None:
                _write(stdout, response)


def _rpc_result(message_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": message_id, "result": result}


def _rpc_error(message_id: Any, code: int, message: str) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": message_id, "error": {"code": code, "message": message}}


def _tool_error(message: str) -> dict[str, Any]:
    return {"content": [{"type": "text", "text": message}], "isError": True}


def _write(stream: Any, payload: dict[str, Any]) -> None:
    stream.write(json.dumps(payload) + "\n")
    stream.flush()


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def _selftest(session: str | None) -> int:
    server = McpServer(default_session=session)
    print("# tools")
    for tool in _tool_definitions():
        print(f"- {tool['name']}: {tool['description'].splitlines()[0]}")
    print()
    print("# sessions")
    print(tool_herdr_sessions(server._client_factory, {}), end="")
    print()
    print("# overview")
    try:
        print(tool_herdr_overview(server._client_factory, {"session": session}), end="")
    except HerdrApiError as err:
        print(f"unavailable: {err}")
        return 1
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--session", help="Herdr session name (default: the default session)")
    parser.add_argument(
        "--selftest",
        action="store_true",
        help="Print the tool list and a live overview instead of serving MCP",
    )
    parser.add_argument(
        "--policy",
        action="store_true",
        help="Print the read-only allowlist and the excluded methods, then exit",
    )
    args = parser.parse_args(argv)

    if args.policy:
        print("read-only methods this connector may send:")
        for name, description in READ_ONLY_METHODS.items():
            print(f"  {name} - {description}")
        print(f"  {PANE_READ_METHOD} - only via herdr_pane_read, source in {SAFE_READ_SOURCES}")
        print()
        print(SIDE_EFFECT_NOTES)
        return 0

    if args.selftest:
        return _selftest(args.session)

    McpServer(default_session=args.session).serve(sys.stdin, sys.stdout)
    return 0


if __name__ == "__main__":
    sys.exit(main())
