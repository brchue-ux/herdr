#!/usr/bin/env bash
# Replay every captured frame and montage the panel column of each into one
# strip, so a transition can be read left to right.
#
#   strip.sh <dir> enter|leave <out.png>
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CAP="$1"; WHICH="$2"; OUT="$3"
# The panel is 42 of the replay grid's columns. Cropping to a little past it is
# what makes a spill visible rather than cropped away: the extra columns are
# terminal-pane territory and must stay empty.
CROP="${STRIP_CROP:-560x1010+0+0}"

shots=()
for esc in "$CAP/$WHICH"-*.esc; do
  frame="$(basename "$esc" .esc)"
  bash "$HERE/replay.sh" "$CAP" "$frame" "$CAP/$frame.png" >/dev/null
  convert "$CAP/$frame.png" -crop "$CROP" +repage \
    -bordercolor '#585b70' -border 1 "$CAP/$frame.crop.png"
  shots+=("$CAP/$frame.crop.png")
done

montage "${shots[@]}" -tile "${#shots[@]}"x1 -geometry +2+2 -background '#11111b' "$OUT"
echo "wrote $OUT from ${#shots[@]} frames"
