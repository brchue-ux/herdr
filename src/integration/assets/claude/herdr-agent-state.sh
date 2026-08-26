#!/bin/sh
# installed by herdr
# managed by herdr; reinstalling or updating the integration overwrites this file.
# add custom hooks beside this file instead of editing it.
# HERDR_INTEGRATION_ID=claude
# HERDR_INTEGRATION_VERSION=10

set -eu

action="${1:-}"
hook_input_file="$(mktemp "${TMPDIR:-/tmp}/herdr-claude-hook.XXXXXX")" || exit 0
trap 'rm -f "$hook_input_file"' EXIT HUP INT TERM
cat >"$hook_input_file" 2>/dev/null || true

case "$action" in
  session) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

# Agent-edit baseline snapshots (written by the diff-capture PreToolUse hook)
# are keyed by agent session id and are dead the moment that session ends.
# Nothing else ever reaps them, so sweep stale ones on session start.
#
# Never the current session's own directory, though. A directory's mtime only
# advances when a *new* file lands inside it, so a long-lived session that has
# settled on a stable set of touched files — still editing, just not touching
# anything new — looks stale here after three days and would be reaped by any
# other session's sweep. Its next edit would then diff against /dev/null and
# render as a whole-file addition.
herdr_session_id="$(HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 -c '
import json, os, sys

try:
    with open(os.environ["HERDR_HOOK_INPUT_FILE"], encoding="utf-8") as handle:
        session_id = json.load(handle).get("session_id")
except Exception:
    session_id = None
sys.stdout.write(session_id if isinstance(session_id, str) else "")
' 2>/dev/null)" || herdr_session_id=""
# "." when the id is unknown: -mindepth 1 means no candidate is ever named
# ".", so the guard then excludes nothing rather than everything.
find /tmp/herdr-agent-diff-baseline -maxdepth 1 -mindepth 1 -type d -mtime +3 \
  ! -name "${herdr_session_id:-.}" -exec rm -rf {} + 2>/dev/null || true

HERDR_ACTION="$action" HERDR_HOOK_INPUT_FILE="$hook_input_file" python3 - <<'PY'
import json
import os
import random
import socket
import time

source = "herdr:claude"
action = os.environ.get("HERDR_ACTION", "")
pane_id = os.environ.get("HERDR_PANE_ID")
socket_path = os.environ.get("HERDR_SOCKET_PATH")
hook_input_file = os.environ.get("HERDR_HOOK_INPUT_FILE")

if not pane_id or not socket_path:
    raise SystemExit(0)

hook_input = {}
if hook_input_file:
    try:
        with open(hook_input_file, encoding="utf-8") as handle:
            content = handle.read()
        if content.strip():
            hook_input = json.loads(content)
    except Exception:
        hook_input = {}

hook_event_name = str(hook_input.get("hook_event_name") or "")
is_subagent = bool(hook_input.get("agent_id"))
if is_subagent:
    raise SystemExit(0)
if hook_event_name == "SubagentStop":
    # SubagentStop is a completion event. Older Herdr integrations mapped it
    # to durable working, but Claude recap/away-summary can emit it after the
    # main turn has already stopped. Never let it revive an idle pane.
    raise SystemExit(0)
request_id = f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}"
report_seq = time.time_ns()
session_id = hook_input.get("session_id")
agent_session_id = session_id if isinstance(session_id, str) and session_id else None
transcript_path = hook_input.get("transcript_path")
agent_session_path = transcript_path if isinstance(transcript_path, str) and transcript_path else None
session_start_source = hook_input.get("source") if hook_event_name == "SessionStart" else None
if not isinstance(session_start_source, str) or not session_start_source:
    session_start_source = None
if agent_session_id:
    params = {
        "pane_id": pane_id,
        "source": source,
        "agent": "claude",
        "seq": report_seq,
        "agent_session_id": agent_session_id,
    }
    if agent_session_path:
        params["agent_session_path"] = agent_session_path
    if session_start_source:
        params["session_start_source"] = session_start_source
    request = {
        "id": request_id,
        "method": "pane.report_agent_session",
        "params": params,
    }
else:
    raise SystemExit(0)

try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(0.5)
    client.connect(socket_path)
    client.sendall((json.dumps(request) + "\n").encode())
    try:
        client.recv(4096)
    except Exception:
        pass
    client.close()
except Exception:
    pass

# A fresh agent session inherits the pane, not the previous session's work, so
# the pane's agent edit log has to start empty. Gated on SessionStart because
# this script's "session" action is only installed there, and sent after the
# report above so the pane is already bound to this session. Subagent and
# SubagentStop invocations returned long before this point.
if hook_event_name == "SessionStart":
    clear_request = {
        "id": f"{source}:{int(time.time() * 1000)}:{random.randrange(1_000_000):06d}",
        "method": "pane.report_edit_diff",
        "params": {"pane_id": pane_id, "file": "", "clear_all": True},
    }
    try:
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(0.5)
        client.connect(socket_path)
        client.sendall((json.dumps(clear_request) + "\n").encode())
        try:
            client.recv(4096)
        except Exception:
            pass
        client.close()
    except Exception:
        pass
PY
