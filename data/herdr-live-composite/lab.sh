#!/usr/bin/env bash
# Shared Xvfb + real-kitty + screenshot plumbing.
#
# Sourced by controls.sh (synthetic detector self-test) and run.sh (the live
# herdr capture) so both drive the terminal identically and a control genuinely
# controls for the real run.
#
# The window is sized in *pixels* to exactly the Xvfb screen, with no window
# manager running, so the X root window and the kitty window are the same
# rectangle. That is what lets every assertion address regions as fractions of
# the frame: the alternative is hunting the window's geometry at capture time,
# and a fractional region measured against the wrong rectangle fails silently
# rather than loudly.

LAB_W=${LAB_W:-1600}
LAB_H=${LAB_H:-1000}
LAB_FONT_SIZE=${LAB_FONT_SIZE:-12}

lab_require() {
  local missing=0 tool
  for tool in "$@"; do
    command -v "$tool" >/dev/null 2>&1 || { echo "missing required tool: $tool" >&2; missing=1; }
  done
  [ "$missing" = 0 ] || exit 1
}

# lab_start_xvfb <display>
lab_start_xvfb() {
  local disp="$1"
  Xvfb "$disp" -screen 0 "${LAB_W}x${LAB_H}x24" -nolisten tcp >"${LAB_TMP:-/tmp}/xvfb$disp.log" 2>&1 &
  LAB_XVFB_PID=$!
  local n
  for n in $(seq 1 40); do
    sleep 0.25
    if DISPLAY="$disp" xdpyinfo >/dev/null 2>&1; then
      echo "Xvfb up on $disp (${LAB_W}x${LAB_H})"
      return 0
    fi
  done
  echo "Xvfb never came up on $disp" >&2
  return 1
}

# lab_kitty_conf <path>
lab_kitty_conf() {
  cat > "$1" <<CONF
font_family monospace
font_size ${LAB_FONT_SIZE}
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
}

# lab_start_kitty <display> <conf> <logfile> -- <command...>
lab_start_kitty() {
  local disp="$1" conf="$2" log="$3"
  shift 3
  [ "${1:-}" = "--" ] && shift
  DISPLAY="$disp" kitty --config "$conf" \
    --override 'shell_integration=disabled' \
    -- "$@" >"$log" 2>&1 &
  LAB_KITTY_PID=$!
}

# lab_shoot <display> <out.png>
lab_shoot() {
  DISPLAY="$1" import -window root "$2"
}

# lab_wait_for_mean <display> <scratch.png> <min-mean> [attempts]
#
# Waits until the captured frame is at least this bright on average, then
# reports how long it took.
#
# Readiness is content-based rather than a sleep, and deliberately so: measured
# on this rig, glyphs appear about 1 s after the window maps but an image
# placement only lands at about 3 s, because a terminal decodes and scales
# graphics off the parse thread and Xvfb has no GPU to do it on. A fixed sleep
# tuned against text therefore screenshots a scene with the image layer missing
# — which is indistinguishable from a terminal that refused to draw it, and
# would quietly turn the whole compositing check into a tautology.
#
#   text only          mean luma ~0.02
#   text + full wash   mean luma ~0.32
#
# so a threshold between them is a positive assertion that the layer under test
# is actually on screen.
lab_wait_for_mean() {
  local disp="$1" scratch="$2" want="$3" attempts="${4:-60}" n mean
  for n in $(seq 1 "$attempts"); do
    sleep 0.5
    DISPLAY="$disp" import -window root "$scratch" 2>/dev/null || continue
    mean=$(convert "$scratch" -colorspace Gray -format '%[fx:mean]' info: 2>/dev/null || echo 0)
    if awk "BEGIN{exit !($mean >= $want)}"; then
      echo "reached mean luma $mean (>= $want) after ${n} polls"
      return 0
    fi
  done
  echo "never reached mean luma $want on $disp (last: ${mean:-none})" >&2
  return 1
}

# lab_wait_for_paint <display> <scratch.png> [attempts]
lab_wait_for_paint() {
  lab_wait_for_mean "$1" "$2" 0.004 "${3:-60}"
}

# lab_wait_for_settle <display> <scratch-prefix> [attempts] [tolerance-px]
#
# Waits until two consecutive captures agree, i.e. the terminal has finished
# drawing. A fixed post-paint sleep is not good enough: text appears as soon as
# the window maps, while an image placement lands a beat later once the terminal
# has decoded it — so a sleep tuned on one machine screenshots a scene that is
# missing exactly the layer under test on another. This waits for the answer
# instead of guessing how long it takes.
#
# Only for scenes that are supposed to reach a steady state. An animating herdr
# UI never settles, and never should.
lab_wait_for_settle() {
  local disp="$1" prefix="$2" attempts="${3:-30}" tol="${4:-200}" n diff
  DISPLAY="$disp" import -window root "${prefix}-a.png" 2>/dev/null || return 1
  for n in $(seq 1 "$attempts"); do
    sleep 0.6
    DISPLAY="$disp" import -window root "${prefix}-b.png" 2>/dev/null || continue
    # `compare -metric AE` prints "<count> (<normalised>)" on stderr, so take the
    # first token and drop any fractional part before comparing it as an integer.
    diff=$(compare -metric AE "${prefix}-a.png" "${prefix}-b.png" null: 2>&1 || true)
    diff=${diff%% *}
    diff=${diff%%.*}
    [ -n "$diff" ] || diff=999999
    case "$diff" in ''|*[!0-9]*) diff=999999 ;; esac
    if [ "$diff" -le "$tol" ] 2>/dev/null; then
      echo "settled after ${n} polls (${diff} px differing)"
      return 0
    fi
    mv "${prefix}-b.png" "${prefix}-a.png"
  done
  echo "never settled on $disp" >&2
  return 1
}

# lab_shoot_series <display> <dir> <prefix> <count> <interval-seconds>
lab_shoot_series() {
  local disp="$1" dir="$2" prefix="$3" count="$4" interval="$5" n
  mkdir -p "$dir"
  for n in $(seq 1 "$count"); do
    lab_shoot "$disp" "$(printf '%s/%s-%02d.png' "$dir" "$prefix" "$n")"
    sleep "$interval"
  done
  echo "captured $count frames into $dir/$prefix-*.png"
}

lab_stop() {
  [ -n "${LAB_KITTY_PID:-}" ] && kill "$LAB_KITTY_PID" 2>/dev/null
  [ -n "${LAB_XVFB_PID:-}" ] && kill "$LAB_XVFB_PID" 2>/dev/null
  wait "${LAB_KITTY_PID:-}" 2>/dev/null
  wait "${LAB_XVFB_PID:-}" 2>/dev/null
  LAB_KITTY_PID=""
  LAB_XVFB_PID=""
  return 0
}
