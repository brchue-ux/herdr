#!/usr/bin/env bash
# Live verification of the fixed 8-row command-log zone:
#   (a) the composer's own row never moves across empty/partial/overflow
#       command counts
#   (b) a mouse drag over the transcript still lands on the row actually
#       drawn there in all three states
#   (c) scrolling the log zone past 8 commands reveals older ones
set -uo pipefail

BIN=$(readlink -f "$1")
TAG=$2
SP=$(readlink -f "$3")
MARKERS=$4
OUT="$SP/shots"
mkdir -p "$OUT"

DISPLAY_NUM=${DISPLAY_NUM:-:97}
SCREEN_W=1400
SCREEN_H=900

unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_ENV
export HERDR_CONFIG_PATH="$SP/lab/herdr-lab-config.toml"
export HERDR_LAB_MARKERS="$MARKERS"
export HERDR_LAB_MARKER_DELAY="${HERDR_LAB_MARKER_DELAY:-8}"

SHIM=$(mktemp -d "$SP/shim-XXXXXX")
ln -sf "$BIN" "$SHIM/herdr"
export PATH="$SHIM:$PATH:/tmp/claude-1000/-mnt-data-treehouse-herdr-94dd9b-6-herdr/91221ae1-b623-4fc7-ab64-3f23f77012d6/scratchpad/xclip-extract/usr/bin"
echo "[lab] herdr on PATH: $(command -v herdr) -> $(readlink -f "$(command -v herdr)")"
echo "[lab] sha256: $(sha256sum "$(command -v herdr)" | cut -d' ' -f1)"

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION=$("$HERDR_LAB_HELPER" name "vf-$TAG") || exit 1
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

lab pane run "$PANE" "bash $SP/lab/paint-claude-repro.sh idle" >/dev/null
sleep 20

shot() {
  DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null
  echo "[lab] shot $OUT/$TAG-$1.png"
}

lab pane read "$PANE" --source visible --format text > "$OUT/$TAG-terminal.txt" 2>&1
shot terminal

# Composer row: the line containing the composer prompt, by *text line
# number* in the dump (1-based) -- constant across marker counts iff the
# fixed zone is really fixed.
COMPOSER_LINE=$(grep -n 'try the fix' "$OUT/$TAG-terminal.txt" | head -1 | cut -d: -f1)
echo "[lab] MARKERS=$MARKERS composer_line=$COMPOSER_LINE"
echo "$COMPOSER_LINE" > "$OUT/$TAG-composer-line.txt"

LAYOUT=$(lab pane layout --pane "$PANE")
X=$(printf '%s' "$LAYOUT" | jq -r '.result.layout.panes[0].rect.x')
Y=$(printf '%s' "$LAYOUT" | jq -r '.result.layout.panes[0].rect.y')

kitty_send() { DISPLAY="$DISPLAY_NUM" kitty @ --to "unix:$KSOCK" send-text --match all "$1"; }

# Drag-select pane row 0 (the transcript's own topmost visible row in every
# state -- always "transcript line 01" plus however many markers shifted it),
# to prove selection still lands correctly in this log-zone state.
ROW=$((Y + 0))
SCOL=$((X + 3))
ECOL=$((X + 26))
kitty_send "$(printf '\033[<0;%d;%dM' "$SCOL" "$ROW")"
sleep 0.3
kitty_send "$(printf '\033[<32;%d;%dM' "$ECOL" "$ROW")"
sleep 0.3
kitty_send "$(printf '\033[<0;%d;%dm' "$ECOL" "$ROW")"
sleep 1.5
shot after-drag-row0
DISPLAY="$DISPLAY_NUM" xclip -selection clipboard -o > "$OUT/$TAG-clipboard-row0.txt" 2>&1
echo "[lab] MARKERS=$MARKERS clipboard row0: $(cat "$OUT/$TAG-clipboard-row0.txt")"

if [ "$MARKERS" -gt 8 ]; then
  # Overflow: scroll the log zone and confirm older commands appear.
  # The log zone's pane rows are consumed_rows()..consumed_rows()+8, where
  # consumed_rows() = (transcript_rows_full - 8) + 1 + composer_rows(1) + 1
  # and transcript_rows_full = area_height - 5 (rule/composer/rule/footerx2).
  # area_height here is the pane's own height, from `pane layout` above.
  AREA_HEIGHT=$(printf '%s' "$LAYOUT" | jq -r '.result.layout.panes[0].rect.height')
  TRANSCRIPT_FULL=$((AREA_HEIGHT - 5))
  CONSUMED=$((TRANSCRIPT_FULL - 8 + 1 + 1 + 1))
  # SGR mouse rows are 1-based against the 0-based pane row Y is already in,
  # so targeting pane-relative row R takes screen row Y+R+1 (empirically
  # confirmed against the row0/row19 drag tests above); aim for the middle
  # of the 8-row zone for margin against any further off-by-one.
  LOG_ROW=$((Y + CONSUMED + 1 + 4))
  echo "[lab] area_height=$AREA_HEIGHT consumed_rows=$CONSUMED log_row(screen)=$LOG_ROW"
  shot before-scroll
  kitty_send "$(printf '\033[<65;%d;%dM' "$SCOL" "$LOG_ROW")"
  kitty_send "$(printf '\033[<65;%d;%dM' "$SCOL" "$LOG_ROW")"
  kitty_send "$(printf '\033[<65;%d;%dM' "$SCOL" "$LOG_ROW")"
  sleep 1
  shot after-scroll
fi

lab api snapshot --json > "$OUT/$TAG-snapshot.json" 2>&1 || true
echo "[lab] done"
