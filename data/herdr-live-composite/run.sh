#!/usr/bin/env bash
# One live compositing pass: a real headless herdr server, a real kitty client
# under Xvfb, real screenshots of what the terminal actually put on screen.
#
#   BACKGROUND=off  the reference pass — no whole-terminal scene
#   BACKGROUND=on   the same fleet with `persistent_background` on
#
# The two passes differ in exactly one config key, which is what makes the
# legibility comparison a counterfactual rather than a vibe. #96 was isolated
# the same way: one real byte stream, replayed twice, `z=-2` against `z=0`.
#
# This is the gap `data/herdr-all-flags-live/` names and cannot close. That
# check proves the right bytes reach the wire; this one proves the terminal
# draws them where herdr assumed it would.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=data/herdr-live-composite/lab.sh
source "$HERE/lab.sh"

BIN="${HERDR_BIN:?set HERDR_BIN to the herdr binary under test}"
BG="${BACKGROUND:-off}"
case "$BG" in on|off) ;; *) echo "BACKGROUND must be on or off, got: $BG" >&2; exit 2 ;; esac

# A debug build reads herdr-dev, a release build reads herdr (config::app_dir_name),
# so write the config into whichever namespace this binary will look in.
NS="${HERDR_NS:-herdr-dev}"
ROOT="${CAPTURE_ROOT:-/tmp/herdr-composite-$BG}"
OUT="${COMPOSITE_OUT:-$HERE/proof/$BG}"
DISP="${COMPOSITE_DISPLAY:-:97}"
FRAMES="${FRAMES:-8}"
FRAME_INTERVAL="${FRAME_INTERVAL:-0.6}"
export LAB_TMP="$ROOT"

lab_require Xvfb kitty import convert xdpyinfo compare git python3
python3 -c 'import PIL' || { echo "python3 needs Pillow" >&2; exit 1; }

rm -rf "$ROOT" "$OUT"
mkdir -p "$ROOT/.config/$NS" "$OUT"

case "$BG" in on) BG_TOML=true ;; off) BG_TOML=false ;; esac
sed "s/@PERSISTENT_BACKGROUND@/$BG_TOML/" "$HERE/config.toml.in" \
  > "$ROOT/.config/$NS/config.toml"
echo "--- config in use (BACKGROUND=$BG) ---"
cat "$ROOT/.config/$NS/config.toml"

# Every herdr invocation goes through this, so nothing can accidentally reach a
# socket outside the isolated config dir.
E=(env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH -u HERDR_ENV
   -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID
   HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$BIN")

cleanup() {
  # The client is the thing holding the session open; stop it before the server
  # so the server sees a normal detach rather than a socket dying under it.
  lab_stop
  if [ -n "${SRV:-}" ]; then
    "${E[@]}" server stop >/dev/null 2>&1 || kill "$SRV" 2>/dev/null || true
    wait "$SRV" 2>/dev/null || true
  fi
}
trap cleanup EXIT

lab_start_xvfb "$DISP"

# TERM on the *server* decides the pixel format it picks, because
# `host_terminal_kind()` reads this process's own environment — the trap PR #98
# documents. The wash gate reads the client's own probe instead
# (`ClientMessage::Hello.host_terminal`), and the client here is a real kitty,
# so both halves are genuinely kitty rather than assumed to be.
echo "--- starting server ---"
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH -u HERDR_ENV \
    HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" \
    TERM=xterm-kitty HERDR_RENDER_PROF=1 \
    "$BIN" server >"$ROOT/server.log" 2>&1 &
SRV=$!

READY=0
for _ in $(seq 1 80); do
  sleep 0.25
  if "${E[@]}" api snapshot >/dev/null 2>&1; then READY=1; break; fi
done
if [ "$READY" != 1 ]; then
  echo "server never answered api snapshot; not capturing" >&2
  tail -40 "$ROOT/server.log" >&2 || true
  exit 1
fi
echo "server ready"

# A repo one commit ahead of and one behind its upstream. An all-idle signal tray
# is engraved marks that never move, so without this the sidebar animation
# assertion would be measuring card pulse alone and a frozen tray — the exact
# #97 defect — would sail through.
echo "--- building a repo that lights the tray (Push=Active, Sync=Attention) ---"
lit_repo() {
  local up="$ROOT/lit-upstream.git" a="$ROOT/lit" b="$ROOT/lit-other"
  local g=(git -c user.email=lab@herdr.invalid -c user.name=lab -c init.defaultBranch=main
           -c commit.gpgsign=false)
  "${g[@]}" init -q --bare "$up"
  "${g[@]}" clone -q "$up" "$a" 2>/dev/null
  ( cd "$a" && echo one > a.txt && "${g[@]}" add -A && "${g[@]}" commit -qm one \
      && "${g[@]}" push -q -u origin main )
  "${g[@]}" clone -q "$up" "$b" 2>/dev/null
  # Upstream moves on: the first clone is now one behind.
  ( cd "$b" && echo two > b.txt && "${g[@]}" add -A && "${g[@]}" commit -qm two \
      && "${g[@]}" push -q origin main )
  # ...and gains a local commit it has not pushed: one ahead as well.
  ( cd "$a" && echo three > c.txt && "${g[@]}" add -A && "${g[@]}" commit -qm three \
      && "${g[@]}" fetch -q origin )
}
if lit_repo >"$ROOT/git.log" 2>&1; then
  echo "lit repo ready at $ROOT/lit"
else
  echo "WARNING: could not build the ahead/behind repo; the tray will be idle" >&2
  tail -20 "$ROOT/git.log" >&2 || true
fi

echo "--- building the fleet ---"
# Ids are read back from the API rather than assumed. A fresh server's first
# workspace is `w1`, not `w2` — assuming otherwise annotates a workspace that
# does not exist, and the only reason that fails loudly here is that
# `workspace_not_found` is an error; the same mistake made against a workspace
# that *does* exist silently decorates the wrong row.
ws_create() { # <label> <cwd> -> workspace id
  "${E[@]}" workspace create --label "$1" --cwd "$2" | python3 -c '
import json, sys
print(json.load(sys.stdin)["result"]["workspace"]["workspace_id"], end="")'
}

split_down() { # <pane id> -> the new pane id
  local parent="$1" new
  new=$("${E[@]}" pane split "$parent" --direction down | python3 -c '
import re, sys
ids = sorted(set(re.findall(r"\"pane_id\":\"([^\"]+)\"", sys.stdin.read())))
print(ids[-1] if ids else "", end="")')
  if [ -z "$new" ] || [ "$new" = "$parent" ]; then
    echo "pane split of $parent did not report a new pane id (got: '${new}')" >&2
    return 1
  fi
  printf '%s' "$new"
}

# A first mate, two second mates under it, workers under those — the same tree
# shape the other tree proofs in data/ use, so the scene has a real owner
# hierarchy to draw as sun / planets / moons.
FM=$(ws_create firstmate /tmp)
LEFT=$(ws_create 2ndmate-left /tmp)
RIGHT=$(ws_create 2ndmate-right /tmp)
LIT=$(ws_create lit "$ROOT/lit")
echo "workspaces: firstmate=$FM left=$LEFT right=$RIGHT lit=$LIT"

for ws in "$LEFT" "$RIGHT" "$LIT"; do
  "${E[@]}" workspace report-metadata "$ws" --source proof --token owner=firstmate
done

RIGHT_P2=$(split_down "$RIGHT:p1")
"${E[@]}" pane report-agent "$RIGHT:p1" --source proof --agent right-worker-1 --state working
"${E[@]}" pane report-agent "$RIGHT_P2" --source proof --agent right-worker-2 --state idle
"${E[@]}" pane report-metadata "$RIGHT:p1" --source proof --token owner=2ndmate-right
"${E[@]}" pane report-metadata "$RIGHT_P2" --source proof --token owner=2ndmate-right

# The workspace the client will be looking at: a static legibility probe on top,
# live output underneath.
PROBE_PANE="$LEFT:p1"
LIVE_PANE=$(split_down "$PROBE_PANE")
"${E[@]}" pane report-agent "$PROBE_PANE" --source proof --agent probe --state working
"${E[@]}" pane report-metadata "$PROBE_PANE" --source proof --token owner=2ndmate-left
"${E[@]}" pane report-metadata "$LIVE_PANE" --source proof --token owner=2ndmate-left
"${E[@]}" workspace focus "$LEFT"
echo "probe pane: $PROBE_PANE   live pane: $LIVE_PANE"

# The probe: a fixed block of bright text that never scrolls and never changes,
# so the ink/paper masks taken from the reference pass address the same pixels in
# the candidate pass. `\033[2J\033[H` wipes the shell prompt and the echoed
# command, leaving the block as the only lit thing in the upper pane.
#
# `pane run` types its argument into the pane's shell and presses Enter
# (`Method::PaneSendInput`), joining argv with single spaces first — so the whole
# command line has to arrive as ONE argument. Passing it as `-- bash -c '...'`
# gets the `--` typed literally and the join flattens the quoting, which yields a
# shell line that runs but draws nothing like the intended block.
#
# It paints its own cell background (rgb 0,0,160, padded to a solid rectangle)
# so the block can be located by colour instead of by assuming where the sidebar
# ends in pixels — that boundary moves with the cell size, which moves with
# whichever font and DPI the runner resolves.
PROBE_LINE='HERDR LEGIBILITY PROBE 0123456789 abcdefghijklmnopqrstuvwxyz ##'
PROBE_CMD="printf '\\033[2J\\033[H'; for i in \$(seq 1 12); do printf '\\033[1;97;48;2;0;0;160m%-70s\\033[0m\\n' '$PROBE_LINE'; done; sleep 100000"
"${E[@]}" pane run "$PROBE_PANE" "$PROBE_CMD"

# Live pane text: a real process writing to a real pty, which is the control for
# "is this client receiving anything at all".
"${E[@]}" pane run "$LIVE_PANE" \
  'n=0; while :; do n=$((n+1)); printf "live pane output line %s\n" "$n"; sleep 0.3; done'

sleep 2

echo "--- attaching a real kitty client on $DISP ---"
CONF="$ROOT/kitty.conf"
lab_kitty_conf "$CONF"
lab_start_kitty "$DISP" "$CONF" "$ROOT/kitty.log" -- \
  env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH -u HERDR_ENV \
      -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
      HOME="$ROOT" XDG_CONFIG_HOME="$ROOT/.config" "$BIN"

if ! lab_wait_for_paint "$DISP" "$ROOT/.probe.png" 80; then
  echo "the client never drew anything" >&2
  echo "--- kitty log ---" >&2; tail -40 "$ROOT/kitty.log" >&2 || true
  echo "--- server log ---" >&2; tail -40 "$ROOT/server.log" >&2 || true
  exit 1
fi

# Graphics land well after glyphs do — measured at about two seconds behind on
# this rig, because a terminal decodes and scales images off the parse thread and
# Xvfb has no GPU. Screenshotting as soon as text appears captures a frame with
# every pixel layer missing, which looks exactly like a terminal that refused to
# draw them.
sleep "${GRAPHICS_SETTLE:-6}"

echo "--- capturing ---"
lab_shoot "$DISP" "$OUT/steady.png"
lab_shoot_series "$DISP" "$OUT" frame "$FRAMES" "$FRAME_INTERVAL"

echo "--- server log (last 40 lines) ---"
tail -40 "$ROOT/server.log" || true
echo "--- render profile ---"
grep -h "render.prof" "$ROOT/.config/$NS"/herdr-server.log 2>/dev/null | tail -5 || true

if grep -qiE "panicked at|thread .* panicked" "$ROOT/server.log"; then
  echo "PANIC DETECTED IN SERVER LOG" >&2
  grep -iE -A5 "panicked at" "$ROOT/server.log" >&2
  exit 1
fi
echo "no panic in server log"

echo
echo "=================== ASSERTIONS (BACKGROUND=$BG) ==================="

# The sidebar is 42 columns wide. At any plausible cell width on a 1600px window
# that is at least 21% of the frame, so the leftmost 18% is inside it whatever
# font the runner resolves — no cell-size arithmetic, no way to drift onto the
# pane area by accident.
python3 "$HERE/assert_motion.py" "$OUT"/frame-*.png \
  --region "${SIDEBAR_REGION:-0.0,0.05,0.18,0.95}" \
  --label "sidebar (cards + signal tray + particle wash)" \
  --min-changed-px "${SIDEBAR_MIN_PX:-800}" \
  --min-active-pairs "${SIDEBAR_MIN_PAIRS:-4}"

# The control: a real process is writing to a real pty in the lower pane. If this
# is still, the client is not receiving frames at all and the sidebar verdict
# above says nothing about animation.
python3 "$HERE/assert_motion.py" "$OUT"/frame-*.png \
  --region "${LIVE_REGION:-0.30,0.55,1.0,0.95}" \
  --label "live pane output (control)" \
  --min-changed-px 200 \
  --min-active-pairs "${SIDEBAR_MIN_PAIRS:-4}"

# Reported, not asserted, until a real run has fixed what the numbers look like:
# whole-frame motion, which in the BACKGROUND=on pass includes the scene's own
# orbiting bodies.
echo
echo "--- whole-frame motion (reported) ---"
python3 "$HERE/assert_motion.py" "$OUT"/frame-*.png \
  --label "whole frame" --min-changed-px 1 --min-active-pairs 0 || true

echo
echo "capture complete: $OUT"
