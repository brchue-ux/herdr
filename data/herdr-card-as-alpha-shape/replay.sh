#!/usr/bin/env bash
# Put the artwork a real Herdr would send into a real terminal, and screenshot it.
#
#   HERDR_SHAPE_CAPTURE_DIR=<dir> cargo nextest run -E 'test(shape_capture)' --no-capture
#   replay.sh <dir> shapes|sheet <out.png>
#
# Reads <dir>/manifest.tsv and places each image at the cell rect the sidebar
# placed it at, through the same escape sequences `src/kitty_graphics.rs` emits.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAP="$1"; WHICH="$2"; OUT="$3"
DISP="${BLEND_DISPLAY:-:97}"
EMIT="$HERE/blend-test/emit.py"

specs=()
while IFS=$'\t' read -r file x y w h cw ch; do
  [ "$file" = "file" ] && continue
  case "$WHICH:$file" in
    shapes:sheet.png) continue ;;
    sheet:shape-*)    continue ;;
  esac
  # +1 row so the top card's bloom is not clipped by the screen edge.
  specs+=("$CAP/$file:$((y + 1)):$x:$w:$h:0")
done < "$CAP/manifest.tsv"

python3 "$EMIT" "$CAP/.esc-$WHICH" "${specs[@]}" >/dev/null

cat > "$CAP/.kitty-$WHICH.conf" <<CONF
font_family monospace
font_size 20
background #1e1e2e
background_opacity 1.0
window_padding_width 0
remember_window_size no
initial_window_width 720
initial_window_height 1290
confirm_os_window_close 0
enable_audio_bell no
cursor_blink_interval 0
CONF

# The terminal has to be at least as large as the placements, or Kitty drops
# them silently and the screenshot is a blank window that looks like a bug in
# the cards. Recorded rather than assumed: this cost an hour once.
DISPLAY="$DISP" kitty --config "$CAP/.kitty-$WHICH.conf" \
  --override 'shell_integration=disabled' \
  -- bash -c "echo \"grid \$(tput cols)x\$(tput lines)\" > '$CAP/.size-$WHICH'; cat '$CAP/.esc-$WHICH'; sleep 30" \
  >"$CAP/.kitty-$WHICH.log" 2>&1 &
KPID=$!

# Settle, then wait for the picture to stop changing. A threshold on the first
# frame is no good: the window's own background already clears any fixed bar
# before a single card has been drawn, which is how this captured ten blank
# screenshots in a row and reported them as a rendering failure.
sleep 4
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
echo "captured $OUT"
