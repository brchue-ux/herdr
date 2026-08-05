#!/usr/bin/env bash
# Drive a real headless kitty and screenshot what it actually composites.
#
# One case per invocation:  run.sh <case-name> <z-a> <z-b>
# Places card_cyan at (row 2,col 2) and card_amber overlapping it at (row 4,col 8),
# each z given by the arguments, then screenshots the X root window.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CASE="$1"; ZA="$2"; ZB="$3"
DISP="${BLEND_DISPLAY:-:97}"
OUT="$HERE/shots"
mkdir -p "$OUT"

# Cell geometry is fixed so the pixel maths in the analysis is exact.
# 240x120 px images; at 12x24 px cells that is 20 cols x 5 rows each.
COLS=20; ROWS=5

python3 "$HERE/emit.py" "$HERE/.esc-$CASE" \
  "$HERE/card_cyan.png:2:2:$COLS:$ROWS:$ZA" \
  "$HERE/card_amber.png:4:12:$COLS:$ROWS:$ZB" >/dev/null

cat > "$HERE/.kitty-$CASE.conf" <<CONF
font_family monospace
font_size 12
background #000000
background_opacity 1.0
window_padding_width 0
remember_window_size no
initial_window_width 900
initial_window_height 700
confirm_os_window_close 0
enable_audio_bell no
CONF

DISPLAY="$DISP" kitty \
  --config "$HERE/.kitty-$CASE.conf" \
  --override 'shell_integration=disabled' \
  -- bash -c "printf '%s' \"\$(cat '$HERE/.esc-$CASE')\"; sleep 30" \
  >"$HERE/.kitty-$CASE.log" 2>&1 &
KPID=$!

# Wait for the window to map and the images to land.
for _ in $(seq 1 40); do
  sleep 0.5
  if DISPLAY="$DISP" xdpyinfo >/dev/null 2>&1; then :; fi
  DISPLAY="$DISP" import -window root "$OUT/$CASE.png" 2>/dev/null || continue
  # Non-black pixel count tells us the images actually drew.
  n=$(convert "$OUT/$CASE.png" -colorspace Gray -format '%[fx:mean]' info: 2>/dev/null || echo 0)
  if awk "BEGIN{exit !($n > 0.001)}"; then break; fi
done

DISPLAY="$DISP" import -window root "$OUT/$CASE.png"
kill "$KPID" 2>/dev/null || true
wait "$KPID" 2>/dev/null || true
echo "captured $OUT/$CASE.png"
