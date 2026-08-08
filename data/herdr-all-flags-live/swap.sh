#!/usr/bin/env bash
# Test the thing a person actually does: stop herdr, put a different binary in
# place, start it again, and expect the fleet to still be there.
#
# The all-flags render capture (run.sh) proves a binary draws. It does not prove
# a *swap* works, because it only ever runs one binary against a session that
# binary created. The risks in a swap live somewhere else entirely: the on-disk
# session snapshot written by the old version has to be readable by the new one,
# and the fleet has to come back.
#
#   swap.sh <old-binary> <new-binary>
#
# Old builds the session; new inherits it. Both run with every flag on.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OLD_BIN="${1:?usage: swap.sh <old-binary> <new-binary>}"
NEW_BIN="${2:?usage: swap.sh <old-binary> <new-binary>}"
ROOT="${SWAP_ROOT:-/tmp/hswap}"
NS="${HERDR_NS:-herdr}"

rm -rf "$ROOT"
mkdir -p "$ROOT/.config/$NS"
cp "$HERE/config.toml" "$ROOT/.config/$NS/config.toml"

run_with() {
  local bin="$1"; shift
  env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH \
      HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$bin" "$@"
}

start_server() {
  local bin="$1"
  run_with "$bin" server >>"$ROOT/server.log" 2>&1 &
  echo $! > "$ROOT/server.pid"
  for _ in $(seq 1 80); do
    sleep 0.25
    if run_with "$bin" api status >/dev/null 2>&1; then return 0; fi
  done
  echo "server built from $bin never became ready" >&2
  tail -40 "$ROOT/server.log" >&2 || true
  return 1
}

echo "=== old binary: $("$OLD_BIN" --version 2>&1 | head -1) ==="
start_server "$OLD_BIN"

# A fleet with structure worth losing: nesting, agent state, and published
# metadata tokens, which is the state most likely to be dropped by a snapshot
# format change.
run_with "$OLD_BIN" workspace create --label firstmate --cwd /tmp
run_with "$OLD_BIN" workspace create --label 2ndmate --cwd /tmp
run_with "$OLD_BIN" workspace report-metadata w2 --source swap --token owner=firstmate
run_with "$OLD_BIN" pane split w2:p1 --direction down
run_with "$OLD_BIN" pane report-agent w2:p2 --source swap --agent worker-1 --state working
run_with "$OLD_BIN" pane report-metadata w2:p2 --source swap --token owner=2ndmate
run_with "$OLD_BIN" pane report-metadata w2:p2 --source swap --token lifecycle=failed

sleep 1
run_with "$OLD_BIN" api snapshot > "$ROOT/before.json"
echo "--- before: $(wc -c < "$ROOT/before.json") bytes of snapshot ---"

echo "=== stopping on the old binary ==="
run_with "$OLD_BIN" session stop >/dev/null 2>&1 || true
sleep 2
kill "$(cat "$ROOT/server.pid")" 2>/dev/null || true
sleep 1

if [ ! -f "$ROOT/.config/$NS/session.json" ]; then
  echo "FAIL: no session.json was written, so there is nothing for a swap to restore" >&2
  exit 1
fi
echo "session.json: $(wc -c < "$ROOT/.config/$NS/session.json") bytes"

echo "=== new binary: $("$NEW_BIN" --version 2>&1 | head -1) ==="
start_server "$NEW_BIN"
sleep 2
run_with "$NEW_BIN" api snapshot > "$ROOT/after.json"
echo "--- after: $(wc -c < "$ROOT/after.json") bytes of snapshot ---"

python3 - "$ROOT/before.json" "$ROOT/after.json" <<'PY'
import json, sys

before = json.load(open(sys.argv[1]))
after = json.load(open(sys.argv[2]))


def labels(doc):
    return sorted(w.get("label") or "" for w in doc.get("workspaces", []))


def panes(doc):
    out = []
    for w in doc.get("workspaces", []):
        for t in w.get("tabs", []):
            for p in t.get("panes", []):
                out.append(p.get("id") or "")
    return sorted(out)


def tokens(doc):
    out = {}
    for w in doc.get("workspaces", []):
        for t in w.get("tabs", []):
            for p in t.get("panes", []):
                for k, v in (p.get("tokens") or {}).items():
                    out[f"{p.get('id')}::{k}"] = v
    return out


fails = []
b_lab, a_lab = labels(before), labels(after)
if b_lab != a_lab:
    fails.append(f"workspace labels changed across the swap:\n  before {b_lab}\n  after  {a_lab}")

b_pan, a_pan = panes(before), panes(after)
if b_pan != a_pan:
    fails.append(f"pane ids changed across the swap:\n  before {b_pan}\n  after  {a_pan}")

b_tok, a_tok = tokens(before), tokens(after)
lost = {k: v for k, v in b_tok.items() if k not in a_tok}
if lost:
    fails.append(f"published metadata tokens lost across the swap: {lost}")

print(f"workspaces restored : {a_lab}")
print(f"panes restored      : {len(a_pan)}")
print(f"tokens restored     : {len(a_tok)} of {len(b_tok)}")

if not a_lab:
    fails.append("the restored session has no workspaces at all")

if fails:
    print("\n=== SWAP FAILURES ===")
    for f in fails:
        print(f"- {f}")
    raise SystemExit(1)
print("\nswap OK: the new binary inherited the old binary's session intact")
PY
STATUS=$?

kill "$(cat "$ROOT/server.pid")" 2>/dev/null || true

if grep -qiE "panicked at|thread .* panicked" "$ROOT/server.log"; then
  echo "PANIC ACROSS THE SWAP" >&2
  grep -iE -A5 "panicked at" "$ROOT/server.log" >&2
  exit 1
fi

exit $STATUS
