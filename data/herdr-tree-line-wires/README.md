# Trunk segments, addressable and independently animated

This task turns the sidebar's ancestor rail — the `│` beside a row that continues an ancestor's
branch down toward a later sibling — from a character redrawn identically every frame into an
addressable object: `anim::ElementId::TrunkSegment`, one per row with a gap still open beneath it,
its own `Family` in the animation engine. See the `TrunkSegment`/`TrunkSegmentId` doc comments in
`src/anim.rs`, `sidebar_trunk_segment_members` in `src/ui/sidebar.rs`, and
`AppState::sidebar_trunk_lifecycle` in `src/app/state.rs`.

Everything here is a real headless `herdr-dev` server driven over its socket API by a real client
attached through a sized PTY — the same rig `just check`'s testing docs describe, not a
reconstruction. `proof/decode_frame.py` is a small VT100 grid emulator (CUP, basic cursor motion,
erase, SGR-strip) that turns the client's raw ANSI stream back into the plain-text rows below;
color is stripped because what this task changes is *structure*, not hue or glow, which are out of
scope for this task.

## The captures

| file | width/shell | what it shows |
|---|---|---|
| `proof/line-shell-width-26.txt` | 26 cols, bare-line shell | the mechanism below `card::MIN_FOLD_WIDTH` — unchanged from before this task |
| `proof/card-shell-before-new-mate.txt` | 42 cols, card shell | the captain's real width: three mates, two workers under `2ndmate-left`, one under `2ndmate-right` |
| `proof/card-shell-segment-mounting.txt` | 42 cols, card shell | 150ms after a fourth mate (`2ndmate-far-right`) is created |
| `proof/card-shell-after-new-mate.txt` | 42 cols, card shell | the same fleet once the new mate's row has settled |

Read `before` against `mounting`/`after`: in `before`, `2ndmate-right` is the last mate, so its own
column and `right-worker`'s column carry no rail at all — four blank leading columns ahead of
`└──`. Once `2ndmate-far-right` arrives, that ancestor column has something to continue toward
again, and a `│` appears beside `2ndmate-right` and `right-worker` alike — two segments, one per
row, mounted independently of either row's own arrival (neither row is new; both were already
idle). That is the case `a_trunk_segment_mounts_settles_and_retracts_on_its_own_clock` in
`src/app/runtime.rs` pins at the state-machine level; these captures are the same behaviour read
off a real render.

Every capture keeps the frame and rail alignment CLAUDE.md documents for the card shell — a
branch's third cell running into the border it points at, `├──`/`└──` picking the right glyph per
row, cards sized by rank — because the mechanism only changes *which* object decides a rail cell's
paint, never where `agent_row_prefix`/`card_rail_prefix` puts one.

## What is not here

The connector's own three cells (`├──`/`└──`) already animate a travelling charge through
`ConnectorCharge`, unrelated to this task. The vertical rail below a row's *own* connector, toward
its next sibling — as opposed to an ancestor's column running past it — is still a plain glyph;
giving it a segment of its own is named as follow-up work in the `TrunkSegment` doc comment, not
built here. No spider, and no new signal/charge content beyond `row_enter`/`row_exit = "wipe"`,
which is only what proves the mount/dismount clock reaches the screen.

## Reproducing

```bash
# a short-prefix private fleet, same as the testing docs describe
mkdir -p /tmp/hwp/.config/herdr-dev
cat > /tmp/hwp/.config/herdr-dev/config.toml <<'EOF'
[experimental]
allow_nested = true
[ui]
sidebar_width = 42
[ui.sidebar.animation]
row_enter = "wipe"
row_enter_ms = 400
row_exit = "wipe"
row_exit_ms = 400
EOF
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH HOME=/tmp/hwp XDG_CONFIG_HOME=/tmp/hwp/.config \
  ./target/debug/herdr server &

E="env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH HOME=/tmp/hwp XDG_CONFIG_HOME=/tmp/hwp/.config ./target/debug/herdr"
$E workspace create --label firstmate --cwd /tmp
$E workspace create --label 2ndmate-left --cwd /tmp
$E workspace create --label 2ndmate-right --cwd /tmp
$E workspace report-metadata w2 --source proof --token owner=firstmate
$E workspace report-metadata w3 --source proof --token owner=firstmate
$E pane split w2:p1 --direction down
$E pane split w3:p1 --direction down
$E pane report-agent w2:p1 --source proof --agent left-worker-1 --state working
$E pane report-agent w2:p2 --source proof --agent left-worker-2 --state idle
$E pane report-agent w3:p1 --source proof --agent right-worker --state idle
$E pane report-metadata w2:p1 --source proof --token owner=2ndmate-left
$E pane report-metadata w2:p2 --source proof --token owner=2ndmate-left
$E pane report-metadata w3:p1 --source proof --token owner=2ndmate-right

# then attach a client through a sized PTY (proof/decode_frame.py <cols> <rows> <raw-capture>
# turns the captured ANSI stream into the text rows above)
```
