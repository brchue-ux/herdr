#!/usr/bin/env bash
# Test the two things a person actually does to change which binary is running,
# and expect the fleet to still be there afterwards.
#
# The all-flags render capture (run.sh) proves a binary draws. It cannot prove a
# *swap* works, because it only ever runs one binary against a session that same
# binary created. The risks in a swap live somewhere else entirely: state written
# by the old version has to be readable by the new one, and the fleet has to come
# back.
#
#   swap.sh <old-binary> <new-binary>
#
# Two phases, because herdr ships two mechanisms and they carry different state
# (see AGENTS.md, "Server state that has to survive a restart"):
#
#   1. cold restart   — stop, put a different binary in place, start again. The
#                       boundary is `persist::SessionSnapshot` in session.json,
#                       which deliberately holds nothing with a deadline on it.
#   2. live handoff   — `herdr server swap --exe`, which replaces the process
#                       while the fleet keeps running. The boundary is
#                       `server::handoff::HandoffManifest`: that snapshot plus
#                       per-pane runtime plus TTL-bearing metadata.
#
# Both run with every flag on, from the same config.toml run.sh uses, so both
# mean the same thing by "all on".
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Where the inline python blocks find swap_snapshot.py. Exported rather than
# passed, because each block is a heredoc with its own argv.
export SWAP_HELPERS="$HERE"
OLD_BIN="${1:?usage: swap.sh <old-binary> <new-binary>}"
NEW_BIN="${2:?usage: swap.sh <old-binary> <new-binary>}"
ROOT="${SWAP_ROOT:-/tmp/hswap}"
NS="${HERDR_NS:-herdr}"

rm -rf "$ROOT"
mkdir -p "$ROOT/.config/$NS"
cp "$HERE/config.toml" "$ROOT/.config/$NS/config.toml"

# A release build reads `herdr`, a debug build reads `herdr-dev`
# (config::app_dir_name), and session.json lives in that same directory for an
# unnamed session — `session::data_dir()` is `config_dir()` itself until a
# `--session` name is given. So this is the path the snapshot must appear at, and
# it is worth stating rather than deriving twice.
SESSION_JSON="$ROOT/.config/$NS/session.json"

run_with() {
  local bin="$1"; shift
  env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH \
      HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$bin" "$@"
}

SRV_PID=""
# Two ways out, because after a live handoff there is no pid to hold: the process
# this script started has been replaced by one it never forked. Asking the socket
# to stop is the only handle on that one, and without it the check leaves a live
# server behind — the first real run did exactly that, and the runner reaped a
# stray `herdr-swap-target` on its way out.
cleanup() {
  if [ -n "$SRV_PID" ]; then
    kill "$SRV_PID" 2>/dev/null || true
  fi
  run_with "$NEW_BIN" server stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

# `$!` after backgrounding a shell *function* is the pid of the subshell bash
# forks to run it, not of the program the function ends up executing — bash does
# not exec-optimise a function body away, so there is always an extra process in
# between. Signalling that pid tears down the wrapper and leaves the server
# running, reparented and unsignalled.
#
# That is the whole of the failure this check was parked on: the SIGTERM went to
# the subshell, the wait loop saw its pid disappear and reported a clean exit,
# the real server never ran its shutdown path, and no session.json was ever
# written. It read as "the snapshot is not where we think" or "the snapshot is
# never written" — it was neither. `exec` inside an explicit subshell makes `$!`
# the server's own pid, and `env` execs the binary in that same pid.
start_server() {
  local bin="$1" label="$2"
  echo "=== starting server: $label ($(run_with "$bin" --version 2>&1 | head -1)) ==="
  ( exec env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH \
         HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$bin" server \
         >>"$ROOT/server.log" 2>&1 ) &
  SRV_PID=$!

  # Readiness is "the api socket answers a real call", not `api status` — that
  # subcommand takes get/set/clear and exits non-zero when called bare, so it can
  # never succeed and a loop probing it silently falls through. `api snapshot` is
  # used because this script compares snapshots: if it cannot answer, there is
  # nothing to compare and waiting longer is pointless.
  local i
  for i in $(seq 1 80); do
    sleep 0.25
    if run_with "$bin" api snapshot >/dev/null 2>&1; then
      echo "server ready (pid $SRV_PID)"
      return 0
    fi
    if ! kill -0 "$SRV_PID" 2>/dev/null; then
      echo "FAIL: server exited during startup" >&2
      tail -40 "$ROOT/server.log" >&2 || true
      return 1
    fi
  done
  echo "FAIL: server built from $bin never answered api snapshot" >&2
  tail -40 "$ROOT/server.log" >&2 || true
  ls -la "$ROOT/.config/$NS/" >&2 || true
  return 1
}

# Stop it the documented way. `herdr server stop` is the command herdr's own
# post-update guidance prints; `herdr session stop` is the session *manager* and
# requires a name, so calling it bare is a usage error that an earlier version of
# this script swallowed with `|| true`, making a signal-less "stop" look like it
# had worked.
#
# The API no longer answering is the assertion, not the pid disappearing. Those
# are different claims, and only the first one is about the server: a stop that
# reaches the wrong process leaves a live server behind a dead wrapper, and
# nothing downstream can tell the difference except by asking.
stop_server() {
  local bin="$1"
  echo "=== stopping the server ==="
  run_with "$bin" server stop || echo "(server stop returned non-zero; falling back to SIGTERM)"

  local i
  for i in $(seq 1 60); do
    if ! run_with "$bin" api snapshot >/dev/null 2>&1; then break; fi
    if [ "$i" = 20 ]; then
      echo "(still answering after 10s; sending SIGTERM to $SRV_PID)"
      kill -TERM "$SRV_PID" 2>/dev/null || true
    fi
    sleep 0.5
  done

  if run_with "$bin" api snapshot >/dev/null 2>&1; then
    echo "FAIL: the server is still answering api snapshot after the stop." >&2
    echo "The stop did not reach the server process. If the pid this script holds" >&2
    echo "($SRV_PID) has exited, then it was never the server's pid — see the" >&2
    echo "comment on start_server." >&2
    exit 1
  fi
  echo "server is down (api no longer answers)"
  SRV_PID=""
}

snapshot_to() {
  local bin="$1" dest="$2"
  run_with "$bin" api snapshot > "$dest"
  echo "  $(basename "$dest"): $(wc -c < "$dest") bytes"
}

# Compare two snapshots for everything that must survive. Runs against the real
# shape of `session.snapshot`, which is four flat top-level arrays — `workspaces`,
# `tabs`, `panes`, `agents` — not workspaces nested inside each other.
#
# The version of this that never ran walked `workspaces[].tabs[].panes[]`, a
# shape the API does not have. Both loops therefore yielded nothing, and the pane
# and token assertions compared two empty collections and passed. That is why
# every check below also asserts its own subject is non-empty: a structural
# mistake in a comparison is invisible unless the comparison refuses to run on
# nothing.
compare_snapshots() {
  local before="$1" after="$2" phase="$3"
  python3 - "$before" "$after" "$phase" <<'PY'
import json, os, sys

sys.path.insert(0, os.environ["SWAP_HELPERS"])
from swap_snapshot import looks_like_snapshot, snapshot_of

phase = sys.argv[3]
before = snapshot_of(json.load(open(sys.argv[1])))
after = snapshot_of(json.load(open(sys.argv[2])))

# Fail at the read rather than downstream. An unwrap that returned the wrong
# level yields a dict with none of these keys, and every comparison below then
# comes back equal-and-empty — which is exactly how the previous version of this
# script reported `swap OK` while every token had been dropped.
for name, doc in (("before", before), ("after", after)):
    if not looks_like_snapshot(doc):
        print(
            f"{phase}: the {name} snapshot does not have the shape of a "
            f"SessionSnapshot (top-level keys: {sorted(doc)[:12] if isinstance(doc, dict) else type(doc).__name__}).",
            file=sys.stderr,
        )
        print(
            "`herdr api snapshot` prints the whole socket response and the snapshot "
            "sits at .result.snapshot — see swap_snapshot.py.",
            file=sys.stderr,
        )
        raise SystemExit(1)


def labels(doc):
    return sorted(w.get("label") or "" for w in doc.get("workspaces", []))


def pane_ids(doc):
    return sorted(p.get("pane_id") or "" for p in doc.get("panes", []))


def tokens(doc):
    """Every published token in the session, keyed by owner and name.

    Both workspaces and panes carry a `tokens` object; the old comparison looked
    at neither, and would not have looked at workspace tokens even if its pane
    walk had worked.
    """
    out = {}
    for w in doc.get("workspaces", []):
        for k, v in (w.get("tokens") or {}).items():
            out[f"workspace {w.get('workspace_id')}::{k}"] = v
    for p in doc.get("panes", []):
        for k, v in (p.get("tokens") or {}).items():
            out[f"pane {p.get('pane_id')}::{k}"] = v
    return out


fails = []
b_lab, a_lab = labels(before), labels(after)
b_pan, a_pan = pane_ids(before), pane_ids(after)
b_tok, a_tok = tokens(before), tokens(after)

print(f"--- {phase} ---")
print(f"workspaces : {len(b_lab)} -> {len(a_lab)}  {a_lab}")
print(f"panes      : {len(b_pan)} -> {len(a_pan)}")
print(f"tokens     : {len(b_tok)} -> {len(a_tok)}")

# Guards against the comparison itself being vacuous. These are facts about the
# fleet swap.sh builds, so they hold by construction unless the walk is wrong.
if not b_lab:
    fails.append("the BEFORE snapshot has no workspaces — the fleet was never built")
if not b_pan:
    fails.append(
        "the BEFORE snapshot has no panes. Every pane assertion here is vacuous; "
        "the snapshot shape this walk expects is wrong."
    )
if not b_tok:
    fails.append(
        "the BEFORE snapshot has no published tokens. Every token assertion here "
        "is vacuous; the snapshot shape this walk expects is wrong."
    )

if b_lab != a_lab:
    fails.append(f"workspace labels changed:\n  before {b_lab}\n  after  {a_lab}")
if b_pan != a_pan:
    fails.append(f"pane ids changed:\n  before {b_pan}\n  after  {a_pan}")

lost = {k: v for k, v in b_tok.items() if k not in a_tok}
changed = {k: (v, a_tok[k]) for k, v in b_tok.items() if k in a_tok and a_tok[k] != v}
if lost:
    fails.append(f"published tokens lost: {lost}")
if changed:
    fails.append(f"published token values changed: {changed}")

if fails:
    print(f"\n=== {phase}: FAILURES ===")
    for f in fails:
        print(f"- {f}")
    raise SystemExit(1)
print(f"{phase}: intact — labels, pane ids and every published token survived")
PY
}

fail_on_panic() {
  if grep -qiE "panicked at|thread .* panicked" "$ROOT/server.log"; then
    echo "PANIC IN SERVER LOG" >&2
    grep -iE -A5 "panicked at" "$ROOT/server.log" >&2
    return 1
  fi
  return 0
}

# ---------------------------------------------------------------- phase 1
# Cold restart: the old binary builds the fleet, the new binary inherits it
# through session.json.

start_server "$OLD_BIN" "old binary"

# A fleet with structure worth losing: nesting by published owner, agent state,
# and metadata tokens at both levels — the state most likely to be dropped by a
# snapshot format change.
echo "=== building the fleet on the old binary ==="
run_with "$OLD_BIN" workspace create --label firstmate --cwd /tmp
run_with "$OLD_BIN" workspace create --label 2ndmate --cwd /tmp

# Resolve ids from the snapshot rather than assuming the numbering. `w1`/`w2` are
# right today, but a check that hardcodes them fails confusingly the first time
# anything creates a workspace at startup.
# A plain assignment, not `read` from a heredoc: `set -e` and `pipefail` abort on
# a failing command substitution, but a command substitution *inside a heredoc*
# has its exit status discarded, so a resolver that failed would hand the rest of
# the script empty ids and fail later somewhere unrelated.
MATE_INFO=$(run_with "$OLD_BIN" api snapshot | python3 -c '
import json, sys
sys.path.insert(0, __import__("os").environ["SWAP_HELPERS"])
from swap_snapshot import snapshot_of
doc = snapshot_of(json.load(sys.stdin))
ws = next((w for w in doc.get("workspaces", []) if w.get("label") == "2ndmate"), None)
if ws is None:
    raise SystemExit("no workspace labelled 2ndmate in the snapshot")
wid = ws["workspace_id"]
pane = next((p for p in doc.get("panes", []) if p.get("workspace_id") == wid), None)
if pane is None:
    raise SystemExit(f"workspace {wid} has no panes in the snapshot")
print(wid, pane["pane_id"])
')
MATE_WS=${MATE_INFO%% *}
MATE_PANE=${MATE_INFO##* }
echo "resolved: 2ndmate workspace=$MATE_WS first pane=$MATE_PANE"

run_with "$OLD_BIN" workspace report-metadata "$MATE_WS" --source swap --token owner=firstmate
run_with "$OLD_BIN" pane split "$MATE_PANE" --direction down
WORKER_PANE=$(run_with "$OLD_BIN" api snapshot | python3 -c '
import json, sys
sys.path.insert(0, __import__("os").environ["SWAP_HELPERS"])
from swap_snapshot import snapshot_of
wid = sys.argv[1] if len(sys.argv) > 1 else None
doc = snapshot_of(json.load(sys.stdin))
ids = sorted(p["pane_id"] for p in doc.get("panes", []) if p.get("workspace_id") == wid)
if len(ids) < 2:
    raise SystemExit(f"expected a second pane in {wid}, got {ids}")
print(ids[-1])
' "$MATE_WS")
echo "resolved: worker pane=$WORKER_PANE"

run_with "$OLD_BIN" pane report-agent "$WORKER_PANE" --source swap --agent worker-1 --state working
run_with "$OLD_BIN" pane report-metadata "$WORKER_PANE" --source swap --token owner=2ndmate
run_with "$OLD_BIN" pane report-metadata "$WORKER_PANE" --source swap --token lifecycle=failed

# Let the fleet settle before snapshotting it; panes are real processes.
sleep 3
snapshot_to "$OLD_BIN" "$ROOT/cold-before.json"

stop_server "$OLD_BIN"

# The snapshot is written by save_session_now() as the server loop exits — there
# is no periodic write on this path — so its absence here means the shutdown path
# did not run, which is a different bug from the snapshot being unreadable.
if [ ! -f "$SESSION_JSON" ]; then
  echo "FAIL: the server is down but no session.json was written." >&2
  echo "Expected: $SESSION_JSON" >&2
  echo "--- anything named session*.json under \$ROOT ---" >&2
  find "$ROOT" -name 'session*.json' -print >&2 2>/dev/null || true
  echo "--- config dir ---" >&2
  ls -la "$ROOT/.config/$NS/" >&2 || true
  echo "--- server log (tail) ---" >&2
  tail -60 "$ROOT/server.log" >&2 || true
  exit 1
fi
echo "session.json: $(wc -c < "$SESSION_JSON") bytes"

start_server "$NEW_BIN" "new binary (cold restart)"
sleep 2
snapshot_to "$NEW_BIN" "$ROOT/cold-after.json"

COLD_STATUS=0
compare_snapshots "$ROOT/cold-before.json" "$ROOT/cold-after.json" \
  "cold restart (session.json)" || COLD_STATUS=$?

# ---------------------------------------------------------------- phase 2
# Live handoff: `herdr server swap --exe` replaces the process under a fleet that
# never stops. Carries the handoff manifest, not session.json.
#
# The target is a copy at a distinct path, because that is what a swap onto a
# freshly built binary looks like and it keeps this from being a no-op on the
# filesystem. Same build in and out: a person re-running `herdr server swap`
# after a rebuild with no version change is doing exactly this, so it has to work.
HANDOFF_STATUS=0
if [ "${SWAP_HANDOFF:-1}" = "1" ] && [ "$COLD_STATUS" = "0" ]; then
  SWAP_TARGET="$ROOT/herdr-swap-target"
  cp "$NEW_BIN" "$SWAP_TARGET"
  chmod +x "$SWAP_TARGET"

  snapshot_to "$NEW_BIN" "$ROOT/handoff-before.json"

  echo "=== live handoff: server swap --exe ==="
  # --dry-run first: it runs every preflight check and stops before the handoff,
  # so a refusal here is a preflight problem and a refusal below is the handoff
  # itself. Separating them is the difference between a diagnosis and a red tick.
  if ! run_with "$NEW_BIN" server swap --exe "$SWAP_TARGET" --dry-run --yes \
        --allow-downgrade --no-promote-client; then
    echo "FAIL: server swap --dry-run refused; the handoff preflight does not pass" >&2
    HANDOFF_STATUS=1
  elif ! run_with "$NEW_BIN" server swap --exe "$SWAP_TARGET" --yes \
        --allow-downgrade --no-promote-client; then
    echo "FAIL: server swap refused after its own preflight passed" >&2
    HANDOFF_STATUS=1
  else
    # The handoff replaces the process, so the pid this script holds is stale and
    # the socket goes away and comes back. Wait for the *new* process to answer.
    SRV_PID=""
    READY=0
    for _ in $(seq 1 80); do
      sleep 0.25
      if run_with "$NEW_BIN" api snapshot >/dev/null 2>&1; then READY=1; break; fi
    done
    if [ "$READY" != "1" ]; then
      echo "FAIL: nothing answered api snapshot after the handoff" >&2
      tail -60 "$ROOT/server.log" >&2 || true
      HANDOFF_STATUS=1
    else
      snapshot_to "$NEW_BIN" "$ROOT/handoff-after.json"
      compare_snapshots "$ROOT/handoff-before.json" "$ROOT/handoff-after.json" \
        "live handoff (server swap --exe)" || HANDOFF_STATUS=$?
    fi
  fi
else
  echo "=== live handoff skipped ==="
fi

# The panic check runs whatever the comparisons said. Under `set -e` an inline
# failure used to abort the script before this, so the one run that most needed
# a panic report was the one that never printed it.
PANIC_STATUS=0
fail_on_panic || PANIC_STATUS=1

echo
echo "=================== SWAP SUMMARY ==================="
echo "cold restart : $([ "$COLD_STATUS" = 0 ] && echo OK || echo FAILED)"
if [ "${SWAP_HANDOFF:-1}" = "1" ] && [ "$COLD_STATUS" = "0" ]; then
  echo "live handoff : $([ "$HANDOFF_STATUS" = 0 ] && echo OK || echo FAILED)"
else
  echo "live handoff : skipped"
fi
echo "server panics: $([ "$PANIC_STATUS" = 0 ] && echo none || echo PRESENT)"
echo "===================================================="

[ "$COLD_STATUS" = 0 ] && [ "$HANDOFF_STATUS" = 0 ] && [ "$PANIC_STATUS" = 0 ]
