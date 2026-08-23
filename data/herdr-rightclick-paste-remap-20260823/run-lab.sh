#!/usr/bin/env bash
# Live verification of the right-click paste / shift+right-click menu remap.
#
# Real X11 clicks (xdotool) into a real kitty under Xvfb, driving a real herdr
# client attached to an isolated fm-herdr-lab session whose server is this
# branch's release build.
set -u

SP=/tmp/claude-1000/-mnt-data-treehouse-herdr-94dd9b-8-herdr/edc63f0e-d11c-4247-b1a2-29a1a23053f4/scratchpad
WT=/mnt/data/treehouse/herdr-94dd9b/8/herdr
BIN="${HERDR_BIN:-$WT/target/release/herdr}"
PASS_LABEL="${PASS_LABEL:-kitty-default}"
OUT="${OUT:-$SP/out-$PASS_LABEL}"
DISP=:77
LAB_W=1600
LAB_H=1000
KSOCK=/tmp/hkrc-$UID.sock
CLIP_TEXT='herdr-paste-probe-9f3a2b'

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'

rm -rf "$OUT"; mkdir -p "$OUT"
exec > >(tee "$OUT/run.log") 2>&1

# --- tools ----------------------------------------------------------------
SHIM="$SP/shim"; rm -rf "$SHIM"; mkdir -p "$SHIM"
ln -sf "$BIN" "$SHIM/herdr"
ln -sf "$SP/xclip/root/usr/bin/xclip" "$SHIM/xclip"
export PATH="$SHIM:$PATH"
XDOTOOL="$SP/xdo/root/usr/bin/xdotool"
export LD_LIBRARY_PATH="$SP/xdo/root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

echo "herdr on PATH -> $(readlink -f "$(command -v herdr)")"
sha256sum "$BIN" | head -1

# Agents run inside the captain's own panes; scrub every inherited override so
# nothing can reach a socket outside the lab session.
unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_ENV \
      HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_SESSION

# --- lab config -----------------------------------------------------------
CONF="$SP/lab-config.toml"
cat > "$CONF" <<'TOML'
onboarding = false

[experimental]
allow_nested = true

[ui]
mouse_capture = true
confirm_close = false
prompt_new_tab_name = false
TOML
export HERDR_CONFIG_PATH="$CONF"

# --- X ---------------------------------------------------------------------
Xvfb "$DISP" -screen 0 "${LAB_W}x${LAB_H}x24" -nolisten tcp >"$OUT/xvfb.log" 2>&1 &
XVFB_PID=$!
for _ in $(seq 1 40); do sleep 0.25; DISPLAY=$DISP xdpyinfo >/dev/null 2>&1 && break; done
DISPLAY=$DISP xdpyinfo >/dev/null 2>&1 || { echo "FATAL: Xvfb never came up"; exit 1; }
export DISPLAY=$DISP
echo "Xvfb up on $DISP"

# --- lab session ------------------------------------------------------------
LAB=$("$HERDR_LAB_HELPER" name herdr-rc-paste-v2)
echo "lab session: $LAB"

KITTY_PID=""
CLIP_PID=""
cleanup() {
  local rc=$?
  [ -n "$KITTY_PID" ] && kill "$KITTY_PID" 2>/dev/null
  [ -n "$CLIP_PID" ] && kill "$CLIP_PID" 2>/dev/null
  sleep 1
  "$HERDR_LAB_HELPER" teardown "$LAB"
  local trc=$?
  if [ "$trc" -ne 0 ]; then
    echo "FATAL: lab teardown FAILED (exit $trc)"
    rc=1
  else
    echo "lab teardown ok"
  fi
  herdr session list --json 2>/dev/null | jq -c '[.sessions[]|{name,running}]' || true
  [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  exit "$rc"
}
trap cleanup EXIT

"$HERDR_LAB_HELPER" provision "$LAB" || { echo "FATAL: provision failed"; exit 1; }
echo "provisioned"
lab() { "$HERDR_LAB_HELPER" run "$LAB" "$@"; }

# The server is what renders and what handles the mouse, so prove it is ours.
SRV_PID=$(lab status --json | jq -r '.server.pid // empty')
echo "server pid=$SRV_PID exe=$(readlink -f /proc/$SRV_PID/exe 2>/dev/null)"

# --- a workspace with a shell pane -----------------------------------------
mkdir -p "$SP/labcwd"
lab workspace create --cwd "$SP/labcwd" --label rcpaste --focus >/dev/null
sleep 2
PANE=$(lab pane list | grep -o '"pane_id":"[^"]*"' | head -1 | cut -d'"' -f4)
echo "pane: $PANE"
[ -n "$PANE" ] || { echo "FATAL: no pane"; exit 1; }

# --- clipboard --------------------------------------------------------------
printf '%s' "$CLIP_TEXT" | xclip -selection clipboard -i &
CLIP_PID=$!
sleep 1
echo "clipboard now holds: [$(xclip -selection clipboard -o)]"

# --- kitty + herdr client ---------------------------------------------------
KCONF="$SP/kitty.conf"
cat > "$KCONF" <<CONF
font_family monospace
font_size 12
background #000000
background_opacity 1.0
window_padding_width 0
hide_window_decorations yes
remember_window_size no
initial_window_width ${LAB_W}
initial_window_height ${LAB_H}
confirm_os_window_close 0
enable_audio_bell no
update_check_interval 0
cursor_blink_interval 0
CONF
[ "${KITTY_FORWARD_SHIFT:-0}" = 1 ] && cat >> "$KCONF" <<'CONF'
# kitty's own default is
#   mouse_map shift+right press ungrabbed,grabbed mouse_selection extend
# (kitty/options/definition.py, "extend_selection_grabbed"), so the chord never
# reaches an application that has grabbed the mouse. Mapping the grabbed half to
# no_op *removes* that trigger (kitty/config.py pops a definition that parses
# empty), which is what makes kitty forward it. Only the KITTY_FORWARD_SHIFT=1
# pass does this.
mouse_map shift+right press grabbed no_op
CONF

rm -f "$KSOCK"
kitty --config "$KCONF" -o allow_remote_control=yes --listen-on="unix:$KSOCK" \
  --override 'shell_integration=disabled' \
  -- "$BIN" --session "$LAB" >"$OUT/kitty.log" 2>&1 &
KITTY_PID=$!

# Content-based readiness: wait until the frame is actually painted.
for n in $(seq 1 60); do
  sleep 0.5
  import -window root "$OUT/scratch.png" 2>/dev/null || continue
  mean=$(convert "$OUT/scratch.png" -colorspace Gray -format '%[fx:mean]' info: 2>/dev/null || echo 0)
  awk "BEGIN{exit !($mean >= 0.004)}" && { echo "painted (mean $mean) after $n polls"; break; }
done
sleep 3

kitty @ --to "unix:$KSOCK" ls >/dev/null 2>&1 || echo "WARN: kitty remote control not answering"

screen_text() { kitty @ --to "unix:$KSOCK" get-text --extent screen 2>/dev/null; }

# A pane is `width` columns wide, so a pasted token longer than the space left
# on the prompt line is split across a wrap and a naive grep misses it. Join
# every line, having first dropped the padding a terminal dump pads rows with.
dewrapped_count() { # <file> <needle>
  sed 's/[[:space:]]*$//' "$1" | tr -d '\n\r' | grep -o "$2" | wc -l
}

screen_text > "$OUT/00-initial.txt"
import -window root "$OUT/00-initial.png"
COLS=$(kitty @ --to "unix:$KSOCK" ls | jq -r '.[0].tabs[0].windows[0].columns')
ROWS=$(kitty @ --to "unix:$KSOCK" ls | jq -r '.[0].tabs[0].windows[0].lines')
echo "terminal grid: ${COLS}x${ROWS}"
CW=$(awk "BEGIN{printf \"%.4f\", $LAB_W/$COLS}")
CH=$(awk "BEGIN{printf \"%.4f\", $LAB_H/$ROWS}")
echo "cell approx ${CW}x${CH}px"

# Pane rect in cells, straight from the server's own view.
lab api snapshot > "$OUT/snapshot.json"
read -r PX PY PW PH <<<"$(jq -r --arg p "$PANE" '
  .result.snapshot.layouts[0].panes[] | select(.pane_id==$p) |
  "\(.area.x) \(.area.y) \(.area.width) \(.area.height)"' "$OUT/snapshot.json" 2>/dev/null)"
if [ -z "${PX:-}" ] || [ "$PX" = "null" ]; then
  echo "snapshot pane rect unavailable; falling back to layout area"
  read -r PX PY PW PH <<<"$(jq -r '.result.snapshot.layouts[0].area |
    "\(.x) \(.y) \(.width) \(.height)"' "$OUT/snapshot.json")"
fi
echo "pane rect cells: x=$PX y=$PY w=$PW h=$PH"

# Click the middle of the pane.
CLICK_X=$(awk "BEGIN{printf \"%d\", ($PX + $PW/2) * $CW}")
CLICK_Y=$(awk "BEGIN{printf \"%d\", ($PY + $PH/2) * $CH}")
echo "click at ${CLICK_X},${CLICK_Y}px"

# Positive control: a real LEFT click has to reach herdr at all.
"$XDOTOOL" mousemove "$CLICK_X" "$CLICK_Y" click 1
sleep 1
screen_text > "$OUT/01-after-left.txt"

# ==== CASE 1: bare right-click must paste ==================================
echo "=== CASE 1: bare right-click ==="
"$XDOTOOL" mousemove "$CLICK_X" "$CLICK_Y" click 3
sleep 2.5
screen_text > "$OUT/02-after-right.txt"
import -window root "$OUT/02-after-right.png"
lab pane read "$PANE" --format text > "$OUT/02-pane-read.txt" 2>&1 || true

C1_PASTED=no; C1_MENU=no; C1_PASTED_SCREEN=no
C1_COUNT=$(dewrapped_count "$OUT/02-pane-read.txt" "$CLIP_TEXT")
[ "$C1_COUNT" -ge 1 ] && C1_PASTED=yes
[ "$(dewrapped_count "$OUT/02-after-right.txt" "$CLIP_TEXT")" -ge 1 ] && C1_PASTED_SCREEN=yes
grep -qE 'Close pane|Split right' "$OUT/02-after-right.txt" && C1_MENU=yes
echo "CASE1 pasted(pane read)=$C1_PASTED pasted(screen)=$C1_PASTED_SCREEN menu=$C1_MENU"
echo "CASE1 clipboard-text occurrences on the pane: $C1_COUNT"

# ==== CASE 2: shift+right-click must open herdr's menu =====================
echo "=== CASE 2: shift+right-click ==="
"$XDOTOOL" keydown shift
"$XDOTOOL" mousemove "$CLICK_X" "$CLICK_Y" click 3
"$XDOTOOL" keyup shift
sleep 2
screen_text > "$OUT/03-after-shift-right.txt"
import -window root "$OUT/03-after-shift-right.png"
lab pane read "$PANE" --format text > "$OUT/03-pane-read.txt" 2>&1 || true

C2_MENU=no; C2_PASTED=no
grep -qE 'Close pane|Split right' "$OUT/03-after-shift-right.txt" && C2_MENU=yes
C2_COUNT=$(dewrapped_count "$OUT/03-pane-read.txt" "$CLIP_TEXT")
[ "$C2_COUNT" -gt "$C1_COUNT" ] && C2_PASTED=yes
echo "CASE2 menu=$C2_MENU pasted=$C2_PASTED (occurrences $C1_COUNT -> $C2_COUNT)"

# ==== CASE 3: ctrl+right-click is the documented fallback chord =============
"$XDOTOOL" key Escape; sleep 0.8
echo "=== CASE 3: ctrl+right-click ==="
"$XDOTOOL" keydown ctrl
"$XDOTOOL" mousemove "$CLICK_X" "$CLICK_Y" click 3
"$XDOTOOL" keyup ctrl
sleep 2
screen_text > "$OUT/04-after-ctrl-right.txt"
import -window root "$OUT/04-after-ctrl-right.png"
C3_MENU=no
grep -qE 'Close pane|Split right' "$OUT/04-after-ctrl-right.txt" && C3_MENU=yes
echo "CASE3 menu=$C3_MENU"

echo
echo "============ RESULT ($PASS_LABEL) ============"
echo "case 1  bare right-click pastes .............. $C1_PASTED (screen: $C1_PASTED_SCREEN, menu opened: $C1_MENU)"
echo "case 2  shift+right-click opens menu ......... $C2_MENU (pasted: $C2_PASTED)"
echo "case 3  ctrl+right-click opens menu .......... $C3_MENU"
echo "=================================================="
