#!/usr/bin/env bash
# Two workers in one Space, each its own git worktree, each carrying a
# modification, an untracked create and a delete. Focus each in turn and
# screenshot the Changes zone.
#
# Answers two things at once: does the diff target follow the focused pane
# rather than the Space, and does the rendered diff include creates and
# deletes (git diff alone reports neither an untracked file nor, without
# HEAD, a removal).
set -uo pipefail

BIN=$1
TAG=$2
SP=$3
OUT="$SP/shots"
mkdir -p "$OUT"

DISPLAY_NUM=${DISPLAY_NUM:-:99}
SCREEN_W=1900
SCREEN_H=1000

unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_SESSION
unset HERDR_PANE_ID HERDR_TAB_ID HERDR_WORKSPACE_ID HERDR_ENV
export HERDR_CONFIG_PATH="$SP/lab/herdr-diff-config.toml"

# --- the repo the two workers live in -------------------------------------
REPO="$SP/repo"
rm -rf "$REPO"; mkdir -p "$REPO"
git -C "$REPO" init -q -b main
printf 'alpha\nbeta\ngamma\n' > "$REPO/edit.txt"
printf 'this file is about to be deleted\n' > "$REPO/doomed.txt"
printf 'untouched\n' > "$REPO/keep.txt"
git -C "$REPO" add -A
git -C "$REPO" -c user.email=lab@herdr -c user.name=lab commit -qm "seed"

for w in A B; do
  git -C "$REPO" worktree add -q -b "worker$w" "$SP/worker$w" >/dev/null 2>&1
  printf 'alpha\nCHANGED BY WORKER %s\ngamma\n' "$w" > "$SP/worker$w/edit.txt"
  printf 'brand new file from worker %s\n' "$w" > "$SP/worker$w/created-by-$w.txt"
  rm -f "$SP/worker$w/doomed.txt"
done
echo "[lab] worker A: $(git -C "$SP/workerA" status --short | tr '\n' ' ')"
echo "[lab] worker B: $(git -C "$SP/workerB" status --short | tr '\n' ' ')"

SHIM=$(mktemp -d "$SP/shim-XXXXXX")
ln -sf "$BIN" "$SHIM/herdr"
export PATH="$SHIM:$PATH"
echo "[lab] sha256: $(sha256sum "$(command -v herdr)" | cut -d' ' -f1)"

export HERDR_LAB_HELPER='/home/bchue/.treehouse/firstmate-7bab20/12/firstmate/bin/fm-herdr-lab.sh'
HERDR_LAB_SESSION=$("$HERDR_LAB_HELPER" name "df-$TAG") || exit 1
echo "[lab] session: $HERDR_LAB_SESSION"

XVFB_PID=""; KITTY_PID=""
cleanup() {
  local rc=$?
  [ -n "$KITTY_PID" ] && kill "$KITTY_PID" 2>/dev/null
  sleep 0.5
  "$HERDR_LAB_HELPER" teardown "$HERDR_LAB_SESSION"
  local trc=$?
  echo "[lab] TEARDOWN_STATUS=$trc"
  [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  rm -rf "$SHIM"
  [ "$trc" -eq 0 ] || echo "[lab] !!! TEARDOWN FAILED"
  exit "$rc"
}
trap cleanup EXIT

"$HERDR_LAB_HELPER" provision "$HERDR_LAB_SESSION" || exit 1
lab() { "$HERDR_LAB_HELPER" run "$HERDR_LAB_SESSION" "$@"; }

WS=$(lab workspace create --cwd "$SP/workerA" --label workers --focus | jq -r '.result.workspace.workspace_id // .result.workspace.id // empty')
echo "[lab] workspace=$WS"
sleep 2
TABA=$(lab tab list | jq -r '.result.tabs[0].tab_id // empty')
lab tab rename "$TABA" --label workerA >/dev/null 2>&1
TABB=$(lab tab create --cwd "$SP/workerB" --label workerB | jq -r '.result.tab.tab_id // .result.tab.id // empty')
echo "[lab] tabA=$TABA tabB=$TABB"

Xvfb "$DISPLAY_NUM" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" -nolisten tcp >/dev/null 2>&1 &
XVFB_PID=$!
sleep 2
KSOCK="/tmp/kd-$TAG.sock"; rm -f "$KSOCK"
DISPLAY="$DISPLAY_NUM" kitty --config NONE -o font_size=11 -o remember_window_size=no \
  -o initial_window_width="$SCREEN_W" -o initial_window_height="$SCREEN_H" \
  -o allow_remote_control=yes -o background=#101018 --listen-on "unix:$KSOCK" \
  -- bash -lc "exec herdr --session '$HERDR_LAB_SESSION'" >/dev/null 2>&1 &
KITTY_PID=$!
for _ in $(seq 1 60); do [ -S "$KSOCK" ] && break; sleep 0.5; done
sleep 5

shot() { DISPLAY="$DISPLAY_NUM" import -window root "$OUT/$TAG-$1.png" 2>/dev/null; echo "[lab] shot $1"; }

lab tab focus "$TABA" >/dev/null; sleep 6; shot workerA
lab tab focus "$TABB" >/dev/null; sleep 6; shot workerB
lab tab focus "$TABA" >/dev/null; sleep 6; shot workerA-again

lab api snapshot --json > "$OUT/$TAG-snapshot.json" 2>&1 || true
echo "[lab] done"
