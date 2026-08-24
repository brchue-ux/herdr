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
# pane's own frame is held and asks for nothing. The ratio is the *first*
# pane's share, and the split leaves the new pane focused - the Claude pane has
# to be focused for the triview to engage at all, so focus walks back up.
NOISE=$(lab pane split "$PANE" --direction down --ratio 0.85 | jq -r '.result.pane.pane_id // .result.pane_id // empty')
echo "[lab] noise pane=$NOISE"
lab pane focus --direction up >/dev/null
echo "[lab] focused pane: $(lab pane current | jq -r '.result.pane.pane_id // .result.pane_id // "?"')"
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

lab pane run "$PANE" "bash $LAB/paint-claude-hold.sh" >/dev/null
[ -n "$NOISE" ] && lab pane run "$NOISE" "while true; do date +%s.%N; sleep 0.05; done" >/dev/null
sleep 12

gettext() { DISPLAY="$DISPLAY_NUM" kitty @ --to "unix:$KSOCK" get-text --match all --extent screen; }
shot() { DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null; }
composer_row() { grep -n 'COMPOSER MARKER' "$1" | head -1 | cut -d: -f1; }

# Taken while the pane is still in phase 2 - before it starts opening batches -
# so this is the split with nothing held, and every later sample is measured
# against it.
gettext > "$OUT/$TAG-settled.txt"
shot settled
SETTLED_ROW=$(composer_row "$OUT/$TAG-settled.txt")
SETTLED_BULLETS=$(grep -c 'zone_' "$OUT/$TAG-settled.txt")
echo "[lab] settled: composer row=${SETTLED_ROW:-none} log-zone bullets=$SETTLED_BULLETS"
sleep 8

# Phase 3 is already running by now: the pane opens and closes a DEC 2026 batch
# about four times a second. Sample what the terminal is showing as fast as
# `kitty @ get-text` will answer and count how many samples caught the split
# somewhere other than where it settled.
MOVED=0
GONE=0
SAMPLES=${HERDR_LAB_SAMPLES:-40}
for index in $(seq 1 "$SAMPLES"); do
  gettext > "$OUT/$TAG-sample.txt"
  row=$(composer_row "$OUT/$TAG-sample.txt")
  bullets=$(grep -c 'zone_' "$OUT/$TAG-sample.txt")
  if [ "${row:-none}" != "${SETTLED_ROW:-none}" ]; then
    MOVED=$((MOVED + 1))
    cp "$OUT/$TAG-sample.txt" "$OUT/$TAG-moved-$index.txt"
  fi
  if [ "$bullets" -lt "$SETTLED_BULLETS" ]; then
    GONE=$((GONE + 1))
  fi
done
shot sampling

echo "[lab] RESULT tag=$TAG settled_row=${SETTLED_ROW:-none} settled_bullets=$SETTLED_BULLETS samples=$SAMPLES moved=$MOVED log_zone_lost=$GONE"
if [ "$MOVED" -eq 0 ] && [ "$GONE" -eq 0 ]; then
  echo "[lab] RESULT tag=$TAG VERDICT=held (every sample showed the split where it settled)"
else
  echo "[lab] RESULT tag=$TAG VERDICT=MOVED ($MOVED/$SAMPLES samples caught the split elsewhere)"
fi
echo "[lab] done"
