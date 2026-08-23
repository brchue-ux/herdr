#!/usr/bin/env bash
# Drive one herdr binary through a live lab: synthetic Claude pane, real kitty
# client under Xvfb, screenshot in Terminal mode and again with the pane's
# right-click context menu open.
set -uo pipefail

BIN=$1
TAG=$2
SP=$3
OUT="$SP/shots"
mkdir -p "$OUT"

DISPLAY_NUM=${DISPLAY_NUM:-:97}
SCREEN_W=1500
SCREEN_H=900

# Never inherit the captain's default-session wiring.
unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_ENV
export HERDR_CONFIG_PATH="$SP/lab/herdr-lab-config.toml"
# Reaches the pane's own shell: the lab server inherits this shell's env.
export HERDR_LAB_MARKERS="${HERDR_LAB_MARKERS:-3}"
export HERDR_LAB_MARKER_DELAY="${HERDR_LAB_MARKER_DELAY:-10}"

SHIM=$(mktemp -d "$SP/shim-XXXXXX")
ln -sf "$BIN" "$SHIM/herdr"
export PATH="$SHIM:$PATH"
echo "[lab] herdr on PATH: $(command -v herdr) -> $(readlink -f "$(command -v herdr)")"
echo "[lab] sha256: $(sha256sum "$(command -v herdr)" | cut -d' ' -f1)"

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION=$("$HERDR_LAB_HELPER" name "sb-$TAG") || exit 1
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
echo "[lab] pane list raw: $(printf '%s' "$PANERAW" | head -c 600)"
PANE=$(printf '%s' "$PANERAW" | jq -r '.result.panes[0].pane_id // empty')
echo "[lab] pane=$PANE"
[ -n "$PANE" ] || { echo "[lab] no pane id"; exit 1; }
lab pane declare-agent "$PANE" --agent claude | head -c 300; echo

# Real X server + real kitty, so the screenshots are of a real terminal.
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

# Paint the Claude-shaped screen into the pane.
lab pane run "$PANE" "bash $SP/lab/paint-claude.sh" >/dev/null
# Phase 1 paints, herdr's marker scan seeds on it, then phase 2 adds the
# `⏺ Bash(...)` lines that populate the pane's command log.
sleep 22

shot() {
  DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null
  echo "[lab] shot $OUT/$TAG-$1.png"
}

lab pane read "$PANE" --source visible --format text > "$OUT/$TAG-terminal.txt" 2>&1
shot terminal

# Real right-click inside the pane: SGR mouse press/release, button 2.
kitty_send() { DISPLAY="$DISPLAY_NUM" kitty @ --to "unix:$KSOCK" send-text --match all "$1"; }
COL=60
ROW=12
kitty_send "$(printf '\033[<2;%d;%dM' "$COL" "$ROW")"
sleep 0.4
kitty_send "$(printf '\033[<2;%d;%dm' "$COL" "$ROW")"
sleep 3
lab pane read "$PANE" --source visible --format text > "$OUT/$TAG-contextmenu.txt" 2>&1
shot contextmenu

lab api snapshot --json > "$OUT/$TAG-snapshot.json" 2>&1 || true
echo "[lab] done"
