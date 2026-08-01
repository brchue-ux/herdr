#!/usr/bin/env bash
# Lab-contract wrapper. Provisions an isolated named session with the helper,
# installs the teardown trap BEFORE provisioning, runs the water captures, and
# tears down through the helper only.
#
# The captures themselves drive a *separate private fleet*: the debug binary with
# its own HOME/XDG_CONFIG_HOME. That fleet's socket is never the live fleet's, and
# no command in this script is ever scoped by ambient HERDR_SESSION.
set -uo pipefail

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION="$("$HERDR_LAB_HELPER" name herdr-water-creation)" || exit 1
export HERDR_LAB_SESSION
echo "lab session: $HERDR_LAB_SESSION"

trap '"$HERDR_LAB_HELPER" teardown "$HERDR_LAB_SESSION"' EXIT

"$HERDR_LAB_HELPER" provision "$HERDR_LAB_SESSION" || exit 1
echo "provisioned OK"

OUT="${1:?usage: run_lab.sh <out-dir>}"
mkdir -p "$OUT"

shift
for b in "${@:-fill pour slosh droplets}"; do
  echo "=== capture $b ==="
  python3 "$(dirname "$0")/live_capture.py" "$b" 1600 "$OUT/live-$b" 2>&1 | sed 's/^/    /'
done
