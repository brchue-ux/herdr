#!/usr/bin/env bash
# Prove the two compositing detectors can fail, before trusting them on herdr.
#
# Needs no herdr binary and no Rust toolchain: five synthetic scenes in a real
# kitty under Xvfb, four assertions with a known expected verdict each.
#
#   behind (z=-2) -> legibility must PASS
#   over   (z=0)  -> legibility must FAIL   <- the #96 shape, isolated to one key
#   moving        -> motion must PASS
#   still         -> motion must FAIL       <- the #94/#97 shape
#
# This runs first in CI and gates the expensive job. Its value is not the
# synthetic scenes; it is that a green tick from the real job then means the
# detectors were *capable* of going red that same minute, on that same runner,
# with that same kitty build. A check that cannot be shown to fail is a tick
# with nothing behind it, which is exactly how a 2,568-byte capture once passed
# the byte-level check.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=data/herdr-live-composite/lab.sh
source "$HERE/lab.sh"

DISP="${COMPOSITE_DISPLAY:-:96}"
OUT="${CONTROLS_OUT:-/tmp/herdr-composite-controls}"
export LAB_TMP="$OUT"

lab_require Xvfb kitty import convert xdpyinfo python3
python3 -c 'import PIL' || { echo "python3 needs Pillow" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"
CONF="$OUT/kitty.conf"
lab_kitty_conf "$CONF"

trap lab_stop EXIT
lab_start_xvfb "$DISP"

# shoot_case <case> <min-mean-luma> <out.png>
shoot_case() {
  local case="$1" want="$2" out="$3"
  lab_start_kitty "$DISP" "$CONF" "$OUT/kitty-$case.log" -- \
    python3 "$HERE/controls_scene.py" "$case"
  lab_wait_for_mean "$DISP" "$OUT/.probe-$case.png" "$want" || {
    echo "--- kitty log ($case) ---" >&2
    cat "$OUT/kitty-$case.log" >&2 || true
    return 1
  }
  lab_wait_for_settle "$DISP" "$OUT/.settle-$case" || return 1
  lab_shoot "$DISP" "$out"
  kill "$LAB_KITTY_PID" 2>/dev/null || true
  wait "$LAB_KITTY_PID" 2>/dev/null || true
  LAB_KITTY_PID=""
  sleep 0.5
}

# series_case <case> <prefix> <count> <interval>
series_case() {
  local case="$1" prefix="$2" count="$3" interval="$4"
  lab_start_kitty "$DISP" "$CONF" "$OUT/kitty-$case.log" -- \
    python3 "$HERE/controls_scene.py" "$case"
  lab_wait_for_paint "$DISP" "$OUT/.probe-$case.png" || {
    echo "--- kitty log ($case) ---" >&2
    cat "$OUT/kitty-$case.log" >&2 || true
    return 1
  }
  lab_shoot_series "$DISP" "$OUT" "$prefix" "$count" "$interval"
  kill "$LAB_KITTY_PID" 2>/dev/null || true
  wait "$LAB_KITTY_PID" 2>/dev/null || true
  LAB_KITTY_PID=""
  sleep 0.5
}

echo "=== control scenes ==="
# The wash makes the frame roughly sixteen times brighter than text alone, so
# 0.15 is comfortably between "text only" and "text plus wash" and asserts the
# image layer is genuinely on screen before anything is measured.
shoot_case text 0.004 "$OUT/ref.png"
shoot_case behind 0.15 "$OUT/behind.png"
shoot_case over 0.15 "$OUT/over.png"
series_case moving moving 5 0.6
series_case still still 5 0.6

FAILED=0
step() {
  local label="$1"
  shift
  echo
  echo "--- $label ---"
  if "$@"; then
    echo "-> as expected"
  else
    echo "-> UNEXPECTED VERDICT: $label" >&2
    FAILED=1
  fi
}

cd "$HERE"
step "legibility: image at z=-2 must be judged legible" \
  python3 assert_legible.py "$OUT/ref.png" "$OUT/behind.png" --block-color 0,0,160
step "legibility: the same image at z=0 must be caught" \
  python3 assert_legible.py "$OUT/ref.png" "$OUT/over.png" --block-color 0,0,160 --expect-fail
step "motion: a moving block must be judged animating" \
  python3 assert_motion.py "$OUT"/moving-*.png --label "moving block"
step "motion: a static block must be caught" \
  python3 assert_motion.py "$OUT"/still-*.png --label "static block" --expect-fail

echo
if [ "$FAILED" != 0 ]; then
  echo "DETECTOR SELF-TEST FAILED — the assertions do not discriminate, so the" >&2
  echo "live job downstream would be measuring nothing." >&2
  exit 1
fi
echo "detector self-test passed: both assertions fire on their own failure shape"
