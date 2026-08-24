#!/usr/bin/env bash
# Live A/B rig for "a held frame must keep the triview split it drew".
#
# Usage: run-lab.sh <herdr-binary> <tag> <scratch-dir>
#
# Drives a real herdr client inside a real kitty under Xvfb, paints a Claude
# Code-shaped screen into one pane until the triview's eight-row command-log
# zone engages, then opens a DEC 2026 synchronized update in that pane and
# leaves it open. A second pane keeps producing output so the window keeps
# being recomposited while the first pane's frame is held.
#
# What it captures is what the *terminal* is showing, via `kitty @ get-text`,
# not what herdr's own grid holds — the whole defect is a disagreement between
# the two.
set -uo pipefail

BIN=$(readlink -f "$1")
TAG=$2
SP=$(readlink -f "$3")
LAB=$(cd "$(dirname "$0")" && pwd)
OUT="$SP/shots"
mkdir -p "$OUT"

DISPLAY_NUM=${DISPLAY_NUM:-:96}
SCREEN_W=1400
SCREEN_H=900

unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_ENV
export HERDR_CONFIG_PATH="$LAB/herdr-lab-config.toml"
export HERDR_LAB_MARKERS="${HERDR_LAB_MARKERS:-3}"
export HERDR_LAB_MARKER_DELAY="${HERDR_LAB_MARKER_DELAY:-8}"

SHIM=$(mktemp -d "$SP/shim-XXXXXX")
ln -sf "$BIN" "$SHIM/herdr"
export PATH="$SHIM:$PATH"
echo "[lab] herdr on PATH: $(command -v herdr) -> $(readlink -f "$(command -v herdr)")"
echo "[lab] sha256: $(sha256sum "$(command -v herdr)" | cut -d' ' -f1)"

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION=$("$HERDR_LAB_HELPER" name "hf-$TAG") || exit 1
echo "[lab] session: $HERDR_LAB_SESSION"

XVFB_PID=""
KITTY_PID=""
cleanup() {
  local rc=$?
  [ -n "$KITTY_PID" ] && kill "$KITTY_PID" 2>/dev/null
  sleep 0.5
  "$HERDR_LAB_HELPER" teardown "$HERDR_LAB_SESSION"
  local trc=$?
  echo "[lab] TEARDOWN_STATUS=$trc"
  [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  rm -rf "$SHIM"
  [ "$trc" -eq 0 ] || echo "[lab] !!! TEARDOWN FAILED"
  exit "$rc"
}
trap cleanup EXIT

"$HERDR_LAB_HELPER" provision "$HERDR_LAB_SESSION" || exit 1
echo "[lab] provisioned"

lab() { "$HERDR_LAB_HELPER" run "$HERDR_LAB_SESSION" "$@"; }

lab workspace create --cwd /tmp --label lab --focus >/dev/null
sleep 2
PANE=$(lab pane list | jq -r '.result.panes[0].pane_id // empty')
[ -n "$PANE" ] || { echo "[lab] no pane id"; exit 1; }
echo "[lab] claude pane=$PANE"

# A second pane, only so the window keeps being recomposited while the first
# pane's own frame is held and asks for nothing.
NOISE=$(lab pane split "$PANE" --direction down --ratio 0.15 | jq -r '.result.pane.pane_id // .result.pane_id // empty')
echo "[lab] noise pane=$NOISE"
lab pane focus "$PANE" >/dev/null
lab pane declare-agent "$PANE" --agent claude >/dev/null

Xvfb "$DISPLAY_NUM" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp >/dev/null 2>&1 &
XVFB_PID=$!
sleep 2

KSOCK="/tmp/hfk-$TAG.sock"
rm -f "$KSOCK"
DISPLAY="$DISPLAY_NUM" kitty \
  --config NONE \
  -o font_size=13 \
  -o remember_window_size=no \
  -o initial_window_width="$SCREEN_W" \
  -o initial_window_height="$SCREEN_H" \
  -o allow_remote_control=yes \
  -o background=#101018 \
  --listen-on "unix:$KSOCK" \
  -- bash -lc "exec herdr --session '$HERDR_LAB_SESSION'" >/dev/null 2>&1 &
KITTY_PID=$!
for _ in $(seq 1 60); do [ -S "$KSOCK" ] && break; sleep 0.5; done
sleep 4
echo "[lab] kitty up (socket $( [ -S "$KSOCK" ] && echo yes || echo NO ))"

TRIGGER="$SP/trigger-$TAG"
rm -f "$TRIGGER"
export HERDR_LAB_TRIGGER="$TRIGGER"

lab pane run "$PANE" "HERDR_LAB_TRIGGER=$TRIGGER bash $LAB/paint-claude-hold.sh" >/dev/null
[ -n "$NOISE" ] && lab pane run "$NOISE" "while true; do date +%s.%N; sleep 0.25; done" >/dev/null
sleep 22

gettext() { DISPLAY="$DISPLAY_NUM" kitty @ --to "unix:$KSOCK" get-text --match all --extent screen; }
shot() { DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null; }

gettext > "$OUT/$TAG-before.txt"
shot before
echo "[lab] before: composer marker on screen row $(grep -n 'COMPOSER MARKER' "$OUT/$TAG-before.txt" | head -1 | cut -d: -f1)"
echo "[lab] before: log-zone bullets = $(grep -c 'zone_' "$OUT/$TAG-before.txt")"

# Open the synchronized update and leave it open.
touch "$TRIGGER"
sleep 8

gettext > "$OUT/$TAG-during.txt"
shot during
echo "[lab] during: composer marker on screen row $(grep -n 'COMPOSER MARKER' "$OUT/$TAG-during.txt" | head -1 | cut -d: -f1)"
echo "[lab] during: log-zone bullets = $(grep -c 'zone_' "$OUT/$TAG-during.txt")"
echo "[lab] during: 'partial repaint' visible = $(grep -c 'partial repaint' "$OUT/$TAG-during.txt")"

BEFORE_ROW=$(grep -n 'COMPOSER MARKER' "$OUT/$TAG-before.txt" | head -1 | cut -d: -f1)
DURING_ROW=$(grep -n 'COMPOSER MARKER' "$OUT/$TAG-during.txt" | head -1 | cut -d: -f1)
echo "[lab] RESULT tag=$TAG before_row=${BEFORE_ROW:-none} during_row=${DURING_ROW:-none}"
if [ "${BEFORE_ROW:-x}" = "${DURING_ROW:-y}" ]; then
  echo "[lab] RESULT tag=$TAG VERDICT=held (the split survived the batch)"
else
  echo "[lab] RESULT tag=$TAG VERDICT=MOVED (the split changed while the frame was held)"
fi
echo "[lab] done"
