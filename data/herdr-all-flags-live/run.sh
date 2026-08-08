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

cat > "$ROOT/.config/$NS/config.toml" <<'CONF'
# Every runtime flag this fork ships, turned on.
[experimental]
allow_nested = true
kitty_graphics = true
sidebar_card_shapes = true
sidebar_particle_field = true
kitty_graphics_local_transport = true
pane_history = true
reveal_hidden_cursor_for_cjk_ime = true

[ui]
sidebar_width = 42

[ui.sidebar.animation]
row_enter = "wipe"
row_enter_ms = 400
row_exit = "wipe"
row_exit_ms = 400
row_motion = "slide"
view_switch = "dissolve"
view_switch_ms = 400
view_switch_particles_per_cell = 2

[ui.sidebar.cards]
pulse = true
wash = true
wash_ms = 400
stage_hue = true

[ui.sidebar.notifications]
enabled = true
CONF

E=(env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH
   HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$BIN")

echo "--- config in use ---"
cat "$ROOT/.config/$NS/config.toml"

echo "--- starting server ---"
"${E[@]}" server >"$ROOT/server.log" 2>&1 &
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

# The analysis goes LAST on purpose. A job's log is read from the end, and the
# grids above are thousands of lines: printed before this, the summary is the
# first thing truncation removes, which is exactly what happened on run 1.
echo
echo "=================== SUMMARY ==================="
python3 "$HERE/analyse.py" "$OUT"/steady.raw "$OUT"/failing.raw "$OUT"/cleared.raw
echo "==============================================="
