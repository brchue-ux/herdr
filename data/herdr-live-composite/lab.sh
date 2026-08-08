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

# Where the measurement scripts live, so `lab_wait_for_motion` can call the same
# one the assertions do rather than re-implementing the measurement.
LAB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

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

# lab_wait_for_motion <display> <scratch-prefix> <region> <min-px> [runs] [attempts] [interval]
#
# The exact inverse of `lab_wait_for_settle`: waits until a region has *started*
# moving, for scenes that are supposed to animate and never settle.
#
# This is a readiness gate, not an assertion. An animated surface does not
# necessarily begin animating at the moment the terminal first paints — the
# signal tray only stops being engraved marks once the state behind its badges
# has arrived, which on this rig is a git remote-status refresh landing after
# the window is already up. A capture started on "something is on screen" can
# therefore spend its first frames on a genuinely static warm-up, and a fixed
# budget of frame pairs is spent on a period that was never under test. That is
# a measured failure mode, not a hypothetical: run 31272863267 opened with three
# 0 px pairs and then moved on all four remaining ones, and failed 4/7.
#
# `runs` consecutive moving pairs are required, so a transient — one card
# settling, one row wiping in — does not open the gate on a tray that is still
# frozen. Three, not two: a blip lasting a single frame already produces two
# moving pairs, one as it appears and one as it goes, so two would admit exactly
# the thing this is meant to exclude.
#
# The measurement is `assert_motion.py`, called with the region and floor the
# caller will later assert on, because a gate that measured something slightly
# different from the assertion would be worse than no gate at all.
#
# Returns non-zero on timeout rather than exiting: a surface that never moves is
# exactly the #97 defect, and the assertion — not the gate — is what has to
# report it, with its per-pair numbers attached.
lab_wait_for_motion() {
  local disp="$1" prefix="$2" region="$3" minpx="$4"
  local want="${5:-3}" attempts="${6:-30}" interval="${7:-0.6}"
  local n run=0
  DISPLAY="$disp" import -window root "${prefix}-a.png" 2>/dev/null || return 1
  for n in $(seq 1 "$attempts"); do
    sleep "$interval"
    DISPLAY="$disp" import -window root "${prefix}-b.png" 2>/dev/null || continue
    if python3 "$LAB_DIR/assert_motion.py" "${prefix}-a.png" "${prefix}-b.png" \
        --region "$region" --min-changed-px "$minpx" --min-active-pairs 1 \
        --label "warm-up" >"${prefix}.log" 2>&1; then
      run=$((run + 1))
    else
      run=0
    fi
    # Every poll's measurement goes in the job log. A gate that only ever says
    # "opened after N polls" cannot be told apart from one that opened on the
    # wrong thing, which is exactly the question run 31274414469 raised.
    printf '  poll %2d: %s (run %d/%d)\n' "$n" \
      "$(sed -n 's/^  .*png: *\([0-9]*\) px .*(\(move\|STILL\)).*$/\1 px \2/p;
                 s/^  .*png: *\([0-9]*\) px .*  \(move\|STILL\)$/\1 px \2/p' \
           "${prefix}.log" | tail -1)" "$run" "$want"
    mv -f "${prefix}-b.png" "${prefix}-a.png"
    if [ "$run" -ge "$want" ]; then
      echo "region $region moved on $want consecutive pairs after ${n} polls" \
           "($(awk "BEGIN{printf \"%.1f\", $n * $interval}")s of warm-up)"
      return 0
    fi
  done
  echo "region $region never moved on $want consecutive pairs within $attempts polls" >&2
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

# Every branch is guarded and the function always succeeds. Callers install this
# as an EXIT trap under `set -e`, where a bare `[ -n "$x" ] && kill` that finds
# nothing to kill aborts the rest of the handler — so the X server survives the
# script that started it, and in run.sh the server would never be asked to stop.
lab_stop() {
  if [ -n "${LAB_KITTY_PID:-}" ]; then
    kill "$LAB_KITTY_PID" 2>/dev/null || true
    wait "$LAB_KITTY_PID" 2>/dev/null || true
  fi
  if [ -n "${LAB_XVFB_PID:-}" ]; then
    kill "$LAB_XVFB_PID" 2>/dev/null || true
    wait "$LAB_XVFB_PID" 2>/dev/null || true
  fi
  LAB_KITTY_PID=""
  LAB_XVFB_PID=""
  return 0
}
