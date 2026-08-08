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

for _ in $(seq 1 60); do
  sleep 0.25
  if "${E[@]}" api status >/dev/null 2>&1; then break; fi
done

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

case "$FORMATS" in
  *"t:f"*) echo "local transport (t=f) confirmed on the wire" ;;
  *) echo "EXPECTED t=f LOCAL TRANSPORT, got: $FORMATS" >&2; exit 1 ;;
esac

if [ "$SERVER_TERM" = "rio" ]; then
  case "$FORMATS" in
    *"f:"*32*) echo "terminal-aware format confirmed: Rio got RGBA32" ;;
    *)
      echo "FORMAT PICKING DID NOT ENGAGE: server believed it was Rio but no f=32" >&2
      echo "reached the wire (got: $FORMATS). Either host_terminal_kind() did not" >&2
      echo "see TERM_PROGRAM, or local transport/locality gating refused." >&2
      exit 1
      ;;
  esac
fi

# The analysis goes LAST on purpose. A job's log is read from the end, and the
# grids above are thousands of lines: printed before this, the summary is the
# first thing truncation removes, which is exactly what happened on run 1.
echo
echo "=================== SUMMARY ==================="
python3 "$HERE/analyse.py" "$OUT"/steady.raw "$OUT"/failing.raw "$OUT"/cleared.raw
echo "==============================================="
