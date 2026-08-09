#!/usr/bin/env bash
# Drive a real herdr fleet with every runtime flag turned on and capture what a
# real client actually receives.
#
# Nothing here is compiled in or out: this repo has no Cargo features and no
# cfg gates on any of these, so a stock binary already contains all of it and
# "all flags on" is purely this config file.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="${HERDR_BIN:?set HERDR_BIN to the herdr binary under test}"
ROOT="${CAPTURE_ROOT:-/tmp/hallflags}"
OUT="$HERE/proof"
COLS=${COLS:-90}
ROWS=${ROWS:-32}
CELL_W=${CELL_W:-9}
CELL_H=${CELL_H:-18}

# A debug build reads herdr-dev, a release build reads herdr (config::app_dir_name),
# so write the config into whichever namespace this binary will look in.
NS="${HERDR_NS:-herdr-dev}"

rm -rf "$ROOT"
mkdir -p "$ROOT/.config/$NS" "$OUT"

cp "$HERE/config.toml" "$ROOT/.config/$NS/config.toml"

E=(env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH
   HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$BIN")

echo "--- config in use ---"
cat "$ROOT/.config/$NS/config.toml"

# This rig's name is a claim about completeness, so check it against the struct
# instead of trusting the file. `persistent_background` was missing for this
# check's first four runs: a shipped flag, drawing a whole-terminal surface,
# outside a job called "all flags on". Nothing could have noticed, because the
# only thing that knew the full list was the Rust source.
#
# Booleans only. `sidebar_card_font` is a path whose empty value already means
# "search the system", and the two `cjk_ime_*` keys are a list and an enum with
# no all-on value; a flag is what this is about.
echo "--- checking the flag list against ExperimentalConfig ---"
python3 - "$HERE/config.toml" "$HERE/../../src/config/model.rs" <<'PY'
import re, sys

cfg_path, model_path = sys.argv[1], sys.argv[2]
model = open(model_path, encoding="utf-8").read()
m = re.search(r"pub struct ExperimentalConfig \{(.*?)\n\}", model, re.S)
if not m:
    raise SystemExit("could not find ExperimentalConfig in " + model_path)
declared = set(re.findall(r"\n    pub (\w+): bool,", m.group(1)))

cfg = open(cfg_path, encoding="utf-8").read()
section = cfg.split("[experimental]", 1)
if len(section) != 2:
    raise SystemExit("config.toml has no [experimental] section")
body = re.split(r"\n\[", section[1], 1)[0]
present = set(re.findall(r"(?m)^(\w+)\s*=", body))

missing = sorted(declared - present)
unknown = sorted(present - declared)
print(f"ExperimentalConfig booleans: {len(declared)}; set here: {len(present & declared)}")
if missing:
    print("FLAGS MISSING FROM THE ALL-FLAGS CONFIG: " + ", ".join(missing), file=sys.stderr)
    print("Add them to data/herdr-all-flags-live/config.toml, or this job's name lies.", file=sys.stderr)
    raise SystemExit(1)
if unknown:
    print("KEYS HERE THAT ARE NOT BOOLEAN FLAGS ON ExperimentalConfig: " + ", ".join(unknown), file=sys.stderr)
    print("A renamed or removed flag is silently inert; fix the config file.", file=sys.stderr)
    raise SystemExit(1)
print("every boolean experimental flag is turned on")
PY

# Which terminal the SERVER believes it is under, which is what decides the
# pixel format — not the client's. host_terminal_kind() reads this process's
# own environment, and its own doc comment says so: "for the split server, this
# is the server process's environment, which only agrees with the terminal's own
# when server and client are co-located." Setting TERM on the client's PTY and
# expecting a format upgrade is testing the wrong process, which is exactly the
# mistake that made an earlier run report f=100 everywhere and call it a
# limitation of synthetic PTYs.
#
#   SERVER_TERM=kitty  -> HostTerminalKind::Kitty, prefers RGB24, but a card is
#                         translucent by design so it correctly stays PNG
#   SERVER_TERM=rio    -> HostTerminalKind::Rio, prefers RGBA32, which applies
#                         to translucent cards, so f=32 must appear on the wire
SERVER_TERM=${SERVER_TERM:-rio}
case "$SERVER_TERM" in
  kitty) SERVER_ENV=(TERM=xterm-kitty) ;;
  rio)   SERVER_ENV=(TERM_PROGRAM=rio TERM=xterm-256color) ;;
  other) SERVER_ENV=(TERM=xterm-256color) ;;
  *) echo "unknown SERVER_TERM: $SERVER_TERM" >&2; exit 2 ;;
esac
echo "--- starting server as SERVER_TERM=$SERVER_TERM (${SERVER_ENV[*]}) ---"
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH \
    HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "${SERVER_ENV[@]}" \
    "$BIN" server >"$ROOT/server.log" 2>&1 &
SRV=$!
trap 'kill $SRV 2>/dev/null || true' EXIT

# Wait on a real API call, and fail if it never answers. This loop used to
# probe `api status` and merely `break` on success — so when the probe never
# succeeded it silently fell through and the script carried on, which is how a
# broken readiness check survived several green runs. A capture taken before
# the server is up is exactly the empty capture the guards below exist to catch,
# so it is better to fail here with the server log than to proceed and blame
# the render.
READY=0
for _ in $(seq 1 80); do
  sleep 0.25
  if "${E[@]}" api snapshot >/dev/null 2>&1; then READY=1; break; fi
done
if [ "$READY" != "1" ]; then
  echo "server never answered api snapshot; not capturing" >&2
  tail -40 "$ROOT/server.log" >&2 || true
  exit 1
fi
echo "server ready"

echo "--- building the fleet ---"
# A first mate, two second mates under it, workers under those: the shape every
# other tree proof in data/ uses, so the captures are comparable.
"${E[@]}" workspace create --label firstmate --cwd /tmp
"${E[@]}" workspace create --label 2ndmate-left --cwd /tmp
"${E[@]}" workspace create --label 2ndmate-right --cwd /tmp
"${E[@]}" workspace report-metadata w2 --source proof --token owner=firstmate
"${E[@]}" workspace report-metadata w3 --source proof --token owner=firstmate
"${E[@]}" pane split w2:p1 --direction down
"${E[@]}" pane split w3:p1 --direction down
"${E[@]}" pane report-agent w2:p1 --source proof --agent left-worker-1 --state working
"${E[@]}" pane report-agent w2:p2 --source proof --agent left-worker-2 --state idle
"${E[@]}" pane report-agent w3:p1 --source proof --agent right-worker --state idle
"${E[@]}" pane report-metadata w2:p1 --source proof --token owner=2ndmate-left
"${E[@]}" pane report-metadata w2:p2 --source proof --token owner=2ndmate-left
"${E[@]}" pane report-metadata w3:p1 --source proof --token owner=2ndmate-right

cap() {
  local name="$1" settle="$2"
  python3 "$HERE/capture.py" "$COLS" "$ROWS" "$CELL_W" "$CELL_H" "$settle" \
    "$OUT/$name.raw" -- env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH \
    HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$BIN"
  python3 "$HERE/decode_frame.py" "$COLS" "$ROWS" "$OUT/$name.raw" > "$OUT/$name.txt" || true
}

echo "--- capture: steady state, all flags on ---"
cap steady 3000

echo "--- capture: a failing worker (failure spider + severity ink) ---"
"${E[@]}" pane report-metadata w2:p1 --source proof --token lifecycle=failed
"${E[@]}" pane report-metadata w2:p1 --source proof --token severity=serious
cap failing 3000

echo "--- capture: failure cleared (spider retreat) ---"
"${E[@]}" pane report-metadata w2:p1 --source proof --clear-token lifecycle
cap cleared 2000

# Capture the cleared state a second time, changing nothing between the two.
# This is the precondition a golden baseline needs and nobody had checked: a
# digest that does not reproduce against itself inside one run can never be
# committed, and a baseline seeded from it would fail every run afterwards for
# reasons that are not regressions. Cheap to ask, and it answers the question
# before a drift failure has to be triaged as a possible flake.
cap cleared-again 2000

echo "--- server log (last 40 lines) ---"
tail -40 "$ROOT/server.log" || true

echo "--- decoded steady-state grid ---"
cat "$OUT/steady.txt" || true

echo "--- decoded failing grid ---"
cat "$OUT/failing.txt" || true

# A crash or a panic anywhere in this configuration is the single most valuable
# signal here: these flags have never been exercised together.
if grep -qiE "panicked at|fatal|thread .* panicked" "$ROOT/server.log"; then
  echo "PANIC DETECTED IN SERVER LOG" >&2
  grep -iE -A5 "panicked at|fatal" "$ROOT/server.log" >&2
  exit 1
fi
echo "no panic in server log"

# A per-frame graphics payload over protocol::MAX_GRAPHICS_FRAME_SIZE is dropped
# *whole*, taking every pixel surface on that pass with it and putting nothing on
# screen to say so — it reads exactly like the capability handshake failing. The
# only evidence is this log line. The guards below would catch it on the steady
# capture, but not on a later one, and this configuration is now the largest it
# has ever been: `persistent_background` puts a whole-terminal image beside the
# sidebar's particle wash and its cards.
if grep -qi "dropping oversized graphics payload" "$ROOT/server.log"; then
  echo "OVERSIZED GRAPHICS PAYLOAD DROPPED — a pass rendered no pixels at all" >&2
  grep -i -B2 -A2 "dropping oversized graphics payload" "$ROOT/server.log" >&2
  echo "Every pixel surface is missing from that frame. Raise nothing to hide it:" >&2
  echo "either the surface must encode smaller (PNG, not raw RGBA) or the geometry" >&2
  echo "here is past what one frame can carry." >&2
  exit 1
fi
echo "no oversized graphics payload dropped"

# "No panic" is not evidence of a render. A capture that is empty because the
# client never drew also never panics, which is exactly how run 2 went green on
# 2,568 bytes against run 1's 5,696,056 from the same six captures. Assert the
# capture is actually substantial, and that the pixel path in particular
# reached the wire, or the whole job is a tick with nothing behind it.
STEADY_BYTES=$(wc -c < "$OUT/steady.raw")
APC_BLOCKS=$(python3 - "$OUT/steady.raw" <<'PY'
import re, sys
blob = open(sys.argv[1], "rb").read()
print(len(re.findall(rb"\x1b_G", blob)))
PY
)
echo "steady capture: ${STEADY_BYTES} bytes, ${APC_BLOCKS} kitty APC blocks"

MIN_BYTES=${MIN_BYTES:-100000}
MIN_APC=${MIN_APC:-10}
if [ "$STEADY_BYTES" -lt "$MIN_BYTES" ]; then
  echo "CAPTURE TOO SMALL: ${STEADY_BYTES} < ${MIN_BYTES} bytes — the client barely drew," >&2
  echo "so nothing below this line proves anything. Check the binary is a release" >&2
  echo "build and that the settles are long enough for it." >&2
  exit 1
fi
if [ "$APC_BLOCKS" -lt "$MIN_APC" ]; then
  echo "NO PIXEL PATH: only ${APC_BLOCKS} APC blocks (< ${MIN_APC})." >&2
  echo "Either the PTY reported no pixel size, or no proportional font was found," >&2
  echo "or kitty_graphics is off — every card flag is inert in that state." >&2
  exit 1
fi

# Assert the transport and the format independently. They are two halves of the
# same change (#77) and only the transport half was ever confirmed before this.
FORMATS=$(python3 - "$OUT/steady.raw" <<'PY'
import re, sys
blob = open(sys.argv[1], "rb").read()
fmts = set()
transports = set()
for m in re.finditer(rb"\x1b_G([^;\x1b]*)", blob):
    for pair in m.group(1).split(b","):
        if pair.startswith(b"f="):
            fmts.add(pair[2:].decode())
        if pair.startswith(b"t="):
            transports.add(pair[2:].decode())
print("f:" + ",".join(sorted(fmts)) + " t:" + ",".join(sorted(transports)))
PY
)
echo "wire formats/transports: $FORMATS"

# Set membership, not substring. Both sets are sorted comma-joined lists, and
# `*"t:f"*` only ever matched when `t=f` was the *sole* transport — which stopped
# being true the moment the capture started working, because the capability
# probe the client emits at startup is itself `t=d,f=24`. A real stream carries
# `t:d,f`, and the substring test read that as "no local transport at all".
FMT_SET=${FORMATS#f:}
FMT_SET=${FMT_SET%% t:*}
TRANSPORT_SET=${FORMATS##* t:}

has_member() { # <needle> <comma-joined set>
  case ",$2," in *",$1,"*) return 0 ;; esac
  return 1
}

if has_member f "$TRANSPORT_SET"; then
  echo "local transport (t=f) confirmed on the wire (transports: $TRANSPORT_SET)"
else
  echo "EXPECTED t=f LOCAL TRANSPORT, got transports: $TRANSPORT_SET" >&2
  exit 1
fi

if [ "$SERVER_TERM" = "rio" ]; then
  if has_member 32 "$FMT_SET"; then
    echo "terminal-aware format confirmed: Rio got RGBA32 (formats: $FMT_SET)"
  else
    echo "FORMAT PICKING DID NOT ENGAGE: server believed it was Rio but no f=32" >&2
    echo "reached the wire (formats: $FMT_SET). Either host_terminal_kind() did not" >&2
    echo "see TERM_PROGRAM, or local transport/locality gating refused." >&2
    exit 1
  fi
fi

# The digest keeps only the sidebar's own columns of the decoded grid, and that
# bound is not tidiness — it is what makes a baseline possible at all. The panes
# to the right of the divider run real shells, and a default prompt carries the
# machine's hostname, which on a hosted runner is freshly generated per run
# (`fv-az1425-773`, `iad20-fj918-…`). A digest containing one can never match a
# committed baseline, so the golden half of this rig would have failed every run
# for a reason that is not a regression. Placement geometry is still taken from
# the whole capture; it is only the character grid that is cropped.
SIDEBAR_COLS=$(awk -F'= *' '/^sidebar_width/ {print $2; exit}' "$HERE/config.toml")
SIDEBAR_COLS=${SIDEBAR_COLS:-42}
export DIGEST_GRID_COLS="$SIDEBAR_COLS"
echo "--- digest: grid cropped to the sidebar's $SIDEBAR_COLS columns ---"

# Reproducibility before comparison. Two captures of one unchanged state must
# digest identically, or a baseline seeded from either is worthless. Checking it
# here means a future drift failure is a real difference rather than a coin toss,
# and it is the assertion that would have to fail before anyone spent a run
# triaging a phantom.
python3 "$HERE/digest.py" write "$OUT/cleared.raw" "$OUT/cleared.txt" \
  "$OUT/cleared-a.digest" >/dev/null
python3 "$HERE/digest.py" write "$OUT/cleared-again.raw" "$OUT/cleared-again.txt" \
  "$OUT/cleared-b.digest" >/dev/null
if diff -u "$OUT/cleared-a.digest" "$OUT/cleared-b.digest" > "$OUT/reproducibility.diff"; then
  echo "digest reproduces across two captures of the same state"
else
  echo "DIGEST DOES NOT REPRODUCE against itself within one run." >&2
  echo "Two captures of an unchanged fleet digested differently, so no baseline" >&2
  echo "committed from this rig could ever match. Something volatile is still in" >&2
  echo "the digest — widen the crop or exclude the field, do not seed a baseline." >&2
  head -60 "$OUT/reproducibility.diff" >&2
  exit 1
fi

# Golden comparison. The assertions above check that the right mechanisms fired;
# this checks that the tree still looks the same. Baselines are per SERVER_TERM,
# because the format branch legitimately differs between them.
BASELINE="$HERE/baseline/steady-$SERVER_TERM.digest"
mkdir -p "$HERE/baseline"
if [ "${BASELINE_WRITE:-0}" = "1" ]; then
  python3 "$HERE/digest.py" write "$OUT/steady.raw" "$OUT/steady.txt" "$BASELINE"
  cp "$BASELINE" "$OUT/steady-$SERVER_TERM.digest"
else
  python3 "$HERE/digest.py" write "$OUT/steady.raw" "$OUT/steady.txt" \
    "$OUT/steady-$SERVER_TERM.digest" >/dev/null
  # digest.py exits 3, not 1, when there is no baseline to compare against. That
  # case used to exit 0 with a printed hint, and no baseline was ever committed,
  # so the golden half of this rig passed vacuously on every run it has had. It
  # is still not fatal by default — a fork with no baselines committed should not
  # be red — but BASELINE_REQUIRED=1 makes it fatal, and CI sets that once the
  # digests are in the tree.
  set +e
  python3 "$HERE/digest.py" check "$OUT/steady.raw" "$OUT/steady.txt" "$BASELINE"
  DIGEST_STATUS=$?
  set -e
  case "$DIGEST_STATUS" in
    0) ;;
    3)
      if [ "${BASELINE_REQUIRED:-0}" = "1" ]; then
        echo "NO BASELINE COMMITTED for SERVER_TERM=$SERVER_TERM, and this run" >&2
        echo "requires one. Take the digest printed above and commit it to" >&2
        echo "data/herdr-all-flags-live/baseline/." >&2
        exit 1
      fi
      echo "!!! DRIFT CHECK INERT: no baseline for SERVER_TERM=$SERVER_TERM."
      echo "!!! Everything above still held, but nothing checked what is drawn."
      ;;
    *) exit "$DIGEST_STATUS" ;;
  esac
fi

# The analysis goes LAST on purpose. A job's log is read from the end, and the
# grids above are thousands of lines: printed before this, the summary is the
# first thing truncation removes, which is exactly what happened on run 1.
echo
echo "=================== SUMMARY ==================="
python3 "$HERE/analyse.py" "$OUT"/steady.raw "$OUT"/failing.raw "$OUT"/cleared.raw
echo "==============================================="
