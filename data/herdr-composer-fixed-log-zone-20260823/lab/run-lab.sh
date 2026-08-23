#!/usr/bin/env bash
# Live repro rig for the composer/log-zone bug + layout change.
#
# Usage: run-lab.sh <herdr-binary> <tag> <scratch-dir> <mode>
#   mode: idle  - paints transcript + 3 markers, drags-selects a known
#                 transcript row, then reads the clipboard to see which row
#                 text actually got copied.
#         busy  - paints the real live/expanding "thinking" shape captured
#                 from a real Claude Code v2.1.241 session, to check whether
#                 the command-log zone also picks up the live marker.
set -uo pipefail

BIN=$(readlink -f "$1")
TAG=$2
SP=$(readlink -f "$3")
MODE=${4:-idle}
OUT="$SP/shots"
mkdir -p "$OUT"

DISPLAY_NUM=${DISPLAY_NUM:-:97}
SCREEN_W=1400
SCREEN_H=900

unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_ENV
export HERDR_CONFIG_PATH="$SP/lab/herdr-lab-config.toml"
export HERDR_LAB_MARKERS="${HERDR_LAB_MARKERS:-3}"
export HERDR_LAB_MARKER_DELAY="${HERDR_LAB_MARKER_DELAY:-8}"

SHIM=$(mktemp -d "$SP/shim-XXXXXX")
ln -sf "$BIN" "$SHIM/herdr"
export PATH="$SHIM:$PATH:/tmp/claude-1000/-mnt-data-treehouse-herdr-94dd9b-6-herdr/91221ae1-b623-4fc7-ab64-3f23f77012d6/scratchpad/xclip-extract/usr/bin"
echo "[lab] herdr on PATH: $(command -v herdr) -> $(readlink -f "$(command -v herdr)")"
echo "[lab] sha256: $(sha256sum "$(command -v herdr)" | cut -d' ' -f1)"

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION=$("$HERDR_LAB_HELPER" name "cw-$TAG") || exit 1
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

WSRAW=$(lab workspace create --cwd /tmp --label lab --focus)
echo "[lab] workspace raw: $WSRAW"
sleep 2
PANERAW=$(lab pane list)
PANE=$(printf '%s' "$PANERAW" | jq -r '.result.panes[0].pane_id // empty')
echo "[lab] pane=$PANE"
[ -n "$PANE" ] || { echo "[lab] no pane id"; exit 1; }
lab pane declare-agent "$PANE" --agent claude | head -c 300; echo

Xvfb "$DISPLAY_NUM" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp >/dev/null 2>&1 &
XVFB_PID=$!
sleep 2

KSOCK="/tmp/kl-$TAG.sock"
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

for _ in $(seq 1 60); do
  [ -S "$KSOCK" ] && break
  sleep 0.5
done
sleep 4
echo "[lab] kitty up (socket $( [ -S "$KSOCK" ] && echo yes || echo NO ))"

lab pane run "$PANE" "bash $SP/lab/paint-claude-repro.sh $MODE" >/dev/null
sleep 20

shot() {
  DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null
  echo "[lab] shot $OUT/$TAG-$1.png"
}

lab pane read "$PANE" --source visible --format text > "$OUT/$TAG-terminal.txt" 2>&1
shot terminal

LAYOUT=$(lab pane layout --pane "$PANE")
echo "[lab] layout: $LAYOUT" | tee "$OUT/$TAG-layout.json"

if [ "$MODE" = "idle" ]; then
  X=$(printf '%s' "$LAYOUT" | jq -r '.result.layout.panes[0].rect.x')
  Y=$(printf '%s' "$LAYOUT" | jq -r '.result.layout.panes[0].rect.y')
  echo "[lab] inner_rect origin: x=$X y=$Y"

  kitty_send() { DISPLAY="$DISPLAY_NUM" kitty @ --to "unix:$KSOCK" send-text --match all "$1"; }

  # Drag-select pane row 0 (columns 3..25), the row this task expects
  # ClaudeTriviewLayout::pane_row_for_grid_row's inverse to land on.
  ROW=$((Y + 0))
  SCOL=$((X + 3))
  ECOL=$((X + 26))
  echo "[lab] dragging row=$ROW cols=$SCOL..$ECOL"
  kitty_send "$(printf '\033[<0;%d;%dM' "$SCOL" "$ROW")"
  sleep 0.3
  kitty_send "$(printf '\033[<32;%d;%dM' "$ECOL" "$ROW")"
  sleep 0.3
  kitty_send "$(printf '\033[<0;%d;%dm' "$ECOL" "$ROW")"
  sleep 1.5
  shot after-drag-row0
  DISPLAY="$DISPLAY_NUM" xclip -selection clipboard -o > "$OUT/$TAG-clipboard-row0.txt" 2>&1
  echo "[lab] clipboard after row0 drag: $(cat "$OUT/$TAG-clipboard-row0.txt")"

  # Also drag-select pane row 19 (the composer's own row in this layout).
  ROW2=$((Y + 19))
  echo "[lab] dragging row=$ROW2 cols=$SCOL..$ECOL"
  kitty_send "$(printf '\033[<0;%d;%dM' "$SCOL" "$ROW2")"
  sleep 0.3
  kitty_send "$(printf '\033[<32;%d;%dM' "$ECOL" "$ROW2")"
  sleep 0.3
  kitty_send "$(printf '\033[<0;%d;%dm' "$ECOL" "$ROW2")"
  sleep 1.5
  shot after-drag-row19

  DISPLAY="$DISPLAY_NUM" xclip -selection clipboard -o > "$OUT/$TAG-clipboard-row19.txt" 2>&1
  echo "[lab] clipboard after row19 drag: $(cat "$OUT/$TAG-clipboard-row19.txt")"
fi

lab api snapshot --json > "$OUT/$TAG-snapshot.json" 2>&1 || true
echo "[lab] done"
