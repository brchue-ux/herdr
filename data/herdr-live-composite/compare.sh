#!/usr/bin/env bash
# Compare the two live passes and decide whether text survived the background.
#
# Run after `BACKGROUND=off run.sh` and `BACKGROUND=on run.sh`. Both passes
# produce a steady screenshot of the same fleet in the same geometry, differing
# only in `persistent_background`, so this is the same shape of counterfactual
# that isolated #96 to a single protocol key.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OFF="${OFF_SHOT:-$HERE/proof/off/steady.png}"
ON="${ON_SHOT:-$HERE/proof/on/steady.png}"

for f in "$OFF" "$ON"; do
  [ -f "$f" ] || { echo "missing capture: $f" >&2; exit 1; }
done

echo "=================== TEXT OVER THE BACKGROUND ==================="
# The probe block is in the upper pane, right of the 42-column sidebar.
#
# Coverage is the vacuity guard, not the assertion: a feature that is silently
# off passes a legibility test trivially, and a green tick would then mean
# nothing. It is measured over the UPPER pane area, outside the probe block —
# the sidebar animates in both passes and the lower pane is writing live text in
# both, so either would supply "coverage" that has nothing to do with the
# background. What is left is empty pane backdrop in the reference, so anything
# that differs there is the scene.
exec python3 "$HERE/assert_legible.py" "$OFF" "$ON" \
  --block-color "${PROBE_BLOCK_COLOR:-0,0,160}" \
  --search "${PROBE_SEARCH:-0.10,0.02,1.0,0.60}" \
  --coverage-region "${COVERAGE_REGION:-0.30,0.05,1.0,0.45}" \
  --min-bg-coverage "${MIN_BG_COVERAGE:-0.05}" \
  --min-contrast "${MIN_CONTRAST:-3.0}" \
  --min-agreement "${MIN_AGREEMENT:-0.75}"
