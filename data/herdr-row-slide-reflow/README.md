# Rows that make room, and give it back

The tree stopped being one opaque image in
[`data/herdr-card-as-alpha-shape/`](../herdr-card-as-alpha-shape/): each card is its own
transparent shape at its own placement, and two overlapping shapes are composited by the
terminal. This is what that bought — a row arriving is *seen* to push its siblings apart, and a
row leaving is seen to let them close.

Everything in here is a real headless `kitty` 0.45.0 on `Xvfb`, fed the exact bytes
`kitty_graphics::encode_local_pane_graphics` hands the host. Nothing is a reconstruction.

## The pictures

| file | what it shows |
|---|---|
| `proof/enter-strip.png` | ten frames of one worker arriving, left to right |
| `proof/leave-strip.png` | nine frames of the same worker leaving |
| `proof/overlap-zoom.png` | two cards crossing mid-motion: the glows blend, nothing shears |
| `proof/clip-at-panel-edge.png` | the arriving card cut exactly at the panel's edge, with the terminal-pane area beside it untouched |
| `fallback.txt` | the same panel below 34 columns, with motion on and with it off — byte-identical |
| `cost.tsv` | what each frame costs on the wire |

Read the enter strip left to right: the tree stands still, a card appears at the panel's right
edge and travels left, and as it travels the two second-mate groups below it pan down to open
its slot. The leave strip is the same thing backwards, because the engine plays a dismount as
its mount reversed — that is not a second implementation, it is the same numbers read the other
way.

## What it costs

`cost.tsv` has two columns. `standalone_bytes` is a whole frame encoded against an empty cache
— every card uploaded — which is what a client that has just attached receives. `incremental_bytes`
is the same frame encoded against a cache that persists across the transition, which is what the
running client actually pays.

| frame | standalone | incremental |
|---|---|---|
| `enter-00-before` | 343 KB | 343 KB (first frame, everything uploaded) |
| `enter-01` | 343 KB | **208 KB** |
| `enter-03` | 374 KB | 31 KB (the new card's own image, once) |
| `enter-04` … `enter-08` | 374 KB | **66–470 bytes** |
| `leave-02` … `leave-07` | 374 KB | **66–470 bytes** |
| `leave-08` | 343 KB | **208 KB** |

**A frame of motion costs a few hundred bytes and no rasterisation at all.** That is the whole
affordability argument: a card's signature is a hash of what the card *is* — its content and its
size — and deliberately not of where it sits, so moving one is a clone of a few kilobytes of PNG
plus a new placement escape, and never the ~16 ms of drawing a card, its bloom and its type.
`rows_make_room_for_each_other::a_row_appearing_re_places_its_siblings_without_redrawing_one_of_them`
is that property as an assertion.

The two 208 KB frames are the frames where the tree's *membership* changes — the row appearing
and the row finally going — and they are **not** the motion. They are the placement pipeline
keying a host image on the card's slot (`HostSurfaceId::SidebarCards(slot)`), so a row inserted
or removed in the middle shifts every slot below it and those images are re-uploaded under new
ids. That is bytes on a socket, not milliseconds of drawing: the rasterisation is already
avoided, and the cards are carried over rather than redrawn. Fixing the upload too means giving
a card a stable identity in the graphics surface namespace, which is a change to how placements
are keyed and belongs in its own piece of work.

## Sub-cell placement was measured and declined

See [`subcell-test/RESULT.md`](subcell-test/RESULT.md). Short version: Kitty's `X`/`Y` really do
translate a placement by a fraction of a cell, but the engine's own 50 ms frame step is coarser
than a cell is tall over any arrival short enough to read as one — so sub-cell placement buys
about one extra position per transition, in exchange for a pixel-offset path and a transparent
pad on every card image. Cards are placed at whole cells.

## Reproducing

```bash
Xvfb :99 -screen 0 1920x1080x24 &
CAP=$(mktemp -d)
HERDR_MOTION_CAPTURE_DIR=$CAP cargo nextest run -E 'test(motion_capture)' --no-capture
MOTION_DISPLAY=:99 bash strip.sh "$CAP" enter "$CAP/enter-strip.png"
MOTION_DISPLAY=:99 bash strip.sh "$CAP" leave "$CAP/leave-strip.png"
```

`replay.sh` does one frame on its own if you want to look at a single moment.

Two traps the harness already handles, both of which cost time once:

- **Kitty silently drops a placement larger than the grid.** A terminal one row short screenshots
  as a blank window that reads exactly like a rendering bug. `replay.sh` writes the grid it got
  to `.size-<frame>` so the receipt is there.
- **The panel is clamped by `ui.sidebar_max_width`, which defaults to 36.** Asking for 42 columns
  without lifting the ceiling silently gives you 36, and every measurement is then of a different
  panel. The capture lifts it.
