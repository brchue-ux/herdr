#!/usr/bin/env bash
# Real patched Rio 0.5.19 under Xvfb, captain's own config, herdr from $BIN.
set -uo pipefail
BIN=$1; TAG=$2; SP=$3
OUT="$SP/shots"; mkdir -p "$OUT"
RIO=/mnt/data/treehouse/rio-fa4e24/1/rio/target/release/rio
XDO="$SP/prefix/usr/bin/xdotool"
export LD_LIBRARY_PATH="$SP/prefix/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

DISPLAY_NUM=${DISPLAY_NUM:-:97}
SCREEN_W=1500; SCREEN_H=900

unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_ENV
export HERDR_CONFIG_PATH="$SP/lab/herdr-lab-config.toml"
export HERDR_LAB_MARKERS="${HERDR_LAB_MARKERS:-3}"
export HERDR_LAB_MARKER_DELAY="${HERDR_LAB_MARKER_DELAY:-10}"
export HERDR_RENDER_PROF=1

SHIM=$(mktemp -d "$SP/shim-XXXXXX")
ln -sf "$BIN" "$SHIM/herdr"
export PATH="$SHIM:$PATH"
echo "[lab] herdr -> $(readlink -f "$(command -v herdr)")"
echo "[lab] sha256 $(sha256sum "$(command -v herdr)" | cut -d' ' -f1)"

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION=$("$HERDR_LAB_HELPER" name "tvrio-$TAG") || exit 1
echo "[lab] session $HERDR_LAB_SESSION"

XVFB_PID=""; RIO_PID=""
cleanup() {
  local rc=$?
  [ -n "$RIO_PID" ] && kill "$RIO_PID" 2>/dev/null
  sleep 0.6
  "$HERDR_LAB_HELPER" teardown "$HERDR_LAB_SESSION"; local trc=$?
  echo "[lab] TEARDOWN_STATUS=$trc"
  [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  rm -rf "$SHIM"
  [ "$trc" -eq 0 ] || echo "[lab] !!! TEARDOWN FAILED"
  exit "$rc"
}
trap cleanup EXIT

"$HERDR_LAB_HELPER" provision "$HERDR_LAB_SESSION" || exit 1
echo "[lab] provisioned"
lab() { "$HERDR_LAB_HELPER" run "$HERDR_LAB_SESSION" "$@"; }

lab workspace create --cwd /tmp --label lab --focus >/dev/null
sleep 2
PANE=$(lab pane list | jq -r '.result.panes[0].pane_id // empty')
echo "[lab] pane=$PANE"
[ -n "$PANE" ] || { echo "[lab] no pane"; exit 1; }
lab pane declare-agent "$PANE" --agent claude >/dev/null

Xvfb "$DISPLAY_NUM" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp >/dev/null 2>&1 &
XVFB_PID=$!
sleep 2

cat > "$SHIM/launch.sh" <<EOF
#!/usr/bin/env bash
export PATH="$SHIM:\$PATH"
export HERDR_CONFIG_PATH="$HERDR_CONFIG_PATH"
unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION HERDR_ENV
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID
exec "$HERDR_LAB_HELPER" run "$HERDR_LAB_SESSION" client
EOF
chmod +x "$SHIM/launch.sh"

DISPLAY="$DISPLAY_NUM" RIO_CONFIG_HOME="$SP/rioconf" "$RIO" -e "$SHIM/launch.sh" >"$SP/rio-$TAG.log" 2>&1 &
RIO_PID=$!
sleep 8
WIN=$(DISPLAY="$DISPLAY_NUM" "$XDO" search --onlyvisible --class rio 2>/dev/null | head -1)
echo "[lab] rio window=$WIN"

key() { DISPLAY="$DISPLAY_NUM" "$XDO" key --window "$WIN" --clearmodifiers "$@"; }
shot() { DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null; echo "[lab] shot $1"; }

lab pane run "$PANE" "bash $SP/lab/paint-claude-typing.sh" >/dev/null
sleep $((HERDR_LAB_MARKER_DELAY + 8))

# enter Mode::Terminal the way the captain does: press a key into the pane
key Return
sleep 2
shot terminal-before-typing
lab pane read "$PANE" --source visible --format text > "$OUT/$TAG-visible.txt" 2>&1

# the painter is now rewriting only the composer row, once a second
mkdir -p "$OUT/$TAG-frames"
ffmpeg -y -f x11grab -framerate 12 -video_size "${SCREEN_W}x${SCREEN_H}" -i "$DISPLAY_NUM" -t 9 \
  "$OUT/$TAG-frames/%03d.png" >/dev/null 2>&1
echo "[lab] frames: $(ls "$OUT/$TAG-frames" | wc -l)"

lab api snapshot --json > "$OUT/$TAG-snapshot.json" 2>&1 || true
echo "[lab] server log: $(ls -d "$HOME/.config/herdr/sessions/$HERDR_LAB_SESSION" 2>/dev/null)"
cp -r "$HOME/.config/herdr/sessions/$HERDR_LAB_SESSION" "$OUT/session-logs" 2>/dev/null || true
echo "[lab] done"
