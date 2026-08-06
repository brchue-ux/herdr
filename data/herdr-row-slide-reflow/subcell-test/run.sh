#!/usr/bin/env bash
# Drive a real headless kitty and screenshot one sub-cell placement case.
#   run.sh <case-name> <Y-offset-px> [X-offset-px]
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CASE="$1"; YOFF="$2"; XOFF="${3:-0}"
DISP="${PROBE_DISPLAY:-:98}"
OUT="$HERE/shots"; mkdir -p "$OUT"
# 0/0 omits c and r, so the image keeps its native pixel size.
COLS="${PROBE_COLS:-20}"; ROWS="${PROBE_ROWS:-5}"

python3 "$HERE/emit.py" "$HERE/.esc-$CASE" \
  "$HERE/probe.png:4:4:$COLS:$ROWS:0:$XOFF:$YOFF" >/dev/null

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

DISPLAY="$DISP" kitty --config "$HERE/.kitty-$CASE.conf" \
  --override 'shell_integration=disabled' \
  -- bash -c "printf '%s' \"\$(cat '$HERE/.esc-$CASE')\"; sleep 30" \
  >"$HERE/.kitty-$CASE.log" 2>&1 &
KPID=$!
for _ in $(seq 1 40); do
  sleep 0.5
  DISPLAY="$DISP" import -window root "$OUT/$CASE.png" 2>/dev/null || continue
  n=$(convert "$OUT/$CASE.png" -colorspace Gray -format '%[fx:mean]' info: 2>/dev/null || echo 0)
  if awk "BEGIN{exit !($n > 0.001)}"; then break; fi
done
DISPLAY="$DISP" import -window root "$OUT/$CASE.png"
kill "$KPID" 2>/dev/null || true
wait "$KPID" 2>/dev/null || true
echo "captured $OUT/$CASE.png"
