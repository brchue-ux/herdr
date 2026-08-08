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
# A first mate, two second mates under it, workers under those — the same tree
# shape the byte-level check and the other tree proofs in data/ use, so the
# scene has a real owner hierarchy to draw as sun / planets / moons.
"${E[@]}" workspace create --label firstmate --cwd /tmp
"${E[@]}" workspace create --label 2ndmate-left --cwd /tmp
"${E[@]}" workspace create --label 2ndmate-right --cwd /tmp
"${E[@]}" workspace create --label lit --cwd "$ROOT/lit"
"${E[@]}" workspace report-metadata w3 --source proof --token owner=firstmate
"${E[@]}" workspace report-metadata w4 --source proof --token owner=firstmate
"${E[@]}" workspace report-metadata w5 --source proof --token owner=firstmate
"${E[@]}" pane split w4:p1 --direction down
"${E[@]}" pane report-agent w4:p1 --source proof --agent right-worker-1 --state working
"${E[@]}" pane report-agent w4:p2 --source proof --agent right-worker-2 --state idle
"${E[@]}" pane report-metadata w4:p1 --source proof --token owner=2ndmate-right
"${E[@]}" pane report-metadata w4:p2 --source proof --token owner=2ndmate-right

# The workspace the client will be looking at: a static legibility probe on top,
# live output underneath.
"${E[@]}" pane split w3:p1 --direction down
"${E[@]}" pane report-agent w3:p1 --source proof --agent probe --state working
"${E[@]}" pane report-metadata w3:p1 --source proof --token owner=2ndmate-left
"${E[@]}" pane report-metadata w3:p2 --source proof --token owner=2ndmate-left
"${E[@]}" workspace focus w3

# The probe: a fixed block of bright text that never scrolls and never changes,
# so the ink/paper masks taken from the reference pass address the same pixels in
# the candidate pass. `\e[2J\e[H` wipes the shell prompt and the echoed command
# so the block is the only lit thing in the upper pane.
PROBE_LINE='HERDR LEGIBILITY PROBE 0123456789 abcdefghijklmnopqrstuvwxyz ##'
"${E[@]}" pane run w3:p1 -- bash -c \
  "printf '\\033[2J\\033[H'; for i in \$(seq 1 12); do printf '\\033[1;97m%s\\033[0m\\n' '$PROBE_LINE'; done; sleep 100000"

# Live pane text: a real process writing to a real pty, which is the control for
# "is this client receiving anything at all".
"${E[@]}" pane run w3:p2 -- bash -c \
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
