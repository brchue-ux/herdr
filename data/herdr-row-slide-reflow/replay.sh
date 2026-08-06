#!/usr/bin/env bash
# Put one captured frame of the sidebar into a real headless kitty and
# screenshot what it draws.
#
#   HERDR_MOTION_CAPTURE_DIR=<dir> cargo nextest run -E 'test(motion_capture)' --no-capture
#   replay.sh <dir> <frame-name> <out.png>
#
# The `.esc` files are the exact bytes `kitty_graphics::encode_local_pane_graphics`
# hands the host, uploads included, so nothing here reconstructs a placement —
# it just writes them out. The terminal is deliberately wider than the 42-column
# panel: everything right of it is where the terminal panes live, and a card
# reaching any of it would be the spill the clip box exists to prevent.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAP="$1"; FRAME="$2"; OUT="$3"
DISP="${MOTION_DISPLAY:-:98}"

cat > "$CAP/.kitty-$FRAME.conf" <<CONF
font_family monospace
font_size 14
background #1e1e2e
background_opacity 1.0
window_padding_width 0
remember_window_size no
initial_window_width 1160
initial_window_height 1180
confirm_os_window_close 0
enable_audio_bell no
cursor_blink_interval 0
CONF

# The grid has to be at least as large as the placements or Kitty drops them
# silently and the screenshot is a blank window that reads exactly like a
# rendering bug. Recorded, not assumed — the blend-test harness lost an hour to
# this once. `.size-*` is the receipt.
DISPLAY="$DISP" kitty --config "$CAP/.kitty-$FRAME.conf" \
  --override 'shell_integration=disabled' \
  -- bash -c "printf 'grid %sx%s\n' \"\$(tput cols)\" \"\$(tput lines)\" > '$CAP/.size-$FRAME'; tput civis; cat '$CAP/$FRAME.esc'; sleep 30" \
  >"$CAP/.kitty-$FRAME.log" 2>&1 &
KPID=$!

# Settle first, then wait for the picture to stop changing. Thresholding the
# first frame is no good: the window's own background is already non-black
# before a single card has been drawn.
sleep 3
prev=""
for _ in $(seq 1 20); do
  DISPLAY="$DISP" import -window root "$OUT" 2>/dev/null || { sleep 0.5; continue; }
  now=$(convert "$OUT" -format '%#' info: 2>/dev/null || echo x)
  [ -n "$prev" ] && [ "$now" = "$prev" ] && break
  prev="$now"
  sleep 0.5
done
kill "$KPID" 2>/dev/null || true
wait "$KPID" 2>/dev/null || true
echo "captured $OUT  ($(cat "$CAP/.size-$FRAME" 2>/dev/null || echo 'grid unknown'))"
