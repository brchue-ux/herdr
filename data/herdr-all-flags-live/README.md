# A live render with every runtime flag on

A real headless herdr server, driven over its socket API by a real client attached through an
explicitly sized PTY, with every runtime flag this fork ships turned on at once. No X server and
no terminal emulator: the evidence is the bytes the client actually receives, decoded back into a
grid (`decode_frame.py`) and summarised by control key (`analyse.py`).

This exists because the bugs that shipped in this area were all invisible to the unit suite —
`U+FE0F` trailing-cell clears, a severity ramp washing an ink pale, two kitty protocol traps. Each
was found by decoding real emitted bytes, and each passed every existing test.

## Nothing is compiled in or out

This repo has no Cargo `[features]` and no `#[cfg(feature = ...)]` anywhere in `src/`, so a stock
binary already contains all of this and "every flag on" is purely `run.sh`'s config file. There is
no all-flags *build*; there is only an all-flags *config*.

## Two things that make the difference between a real capture and a convincing blank

**The PTY sets `ws_xpixel`/`ws_ypixel`, not just rows and columns.** The client resolves the host
cell size from the pty's pixel fields first (`current_terminal_geometry`, `src/client/mod.rs`), and
a PTY opened without them reports no pixels, so `cell_size.is_known()` is false and the server
sends no graphics at all. Every pixel-card flag is then silently inert and the capture reads as a
plain character sidebar — a pass that proves nothing while looking exactly like a real read.

**A proportional font has to exist on the machine.** `image_card::is_available` requires a face
found at runtime and herdr ships none, so a bare runner leaves the pixel path off for a second,
independent reason. The workflow installs `fonts-dejavu`.

## What run 1 confirmed, off the wire

- **`t=f` local transport is live** (#77). A transmit block carried
  `Ga=t,t=f,f=100,...` whose payload base64-decodes to `/tmp/herdr-kitty-graphics-6393/597605.kitty`
  — a path, not pixels, under the pid-scoped directory the commit describes.
- **Cell geometry is exactly what the PTY declared.** Placements read
  `c=29,r=6,w=261,h=108` — 261/29 = 9 px per column, 108/6 = 18 px per row, matching `CELL_W`/`CELL_H`.
- **Card height is uniform across ranks, width is not** (#67). Placements at `c=29`, `c=33` and
  `c=35` all carry `r=6`: rank is carried by width alone, which is exactly what retiring `TIER_SCALE`
  was meant to do, observed here rather than asserted.
- **Native animation is armed** (#70). `Ga=a,i=194906,r=1,z=100,q=2` control blocks appear, and
  frames carry `s=`/`v=`.
- **No panic** with all eleven flags on simultaneously — the single most valuable signal, since this
  combination had never been exercised.

## What this rig cannot confirm

**The terminal-aware pixel format did not engage.** Every transmit came through as `f=100` (PNG),
which #77 documents as the fallback for a terminal herdr does not positively identify — the RGB24 /
RGBA32 fast path never activated, despite `TERM=xterm-kitty`. Identification evidently needs more
than `TERM`, so a synthetic PTY gets the *transport* half of that change and not the *format* half.
Confirming the format half needs a real terminal emulator, i.e. the Xvfb + kitty rig in
`data/herdr-card-as-alpha-shape/blend-test/`.

Likewise, anything about how the terminal *composites* what it is sent — glow bleed between cards,
`z` ordering — is out of reach here: kitty composites in linear light while `Canvas::blend` works
in sRGB, so software compositing of these bytes is not what the terminal would draw.

## Two harness bugs found by run 1, neither a Herdr bug

1. `decode_frame.py` stepped over OSC but not **APC**, so every graphics block landed in the text
   grid as literal base64 and buried the rendered rows. Fixed here with an APC branch.
2. The analysis summary printed *before* the grid dumps, so it was the first thing lost to log
   truncation. It now prints last, because a job log is read from the end.

## Cost, and one saving that was not real

The build dominates: ~4m17s of a 5m46s run 1. A debug build was tried to cut that and it was a
false economy — debug card rasterisation and PNG encode are slow enough that almost nothing
renders inside the capture's settles. Run 2 produced **2,568 bytes against run 1's 5,696,056**
from the same six captures, finished in 2m09s, and **passed**, because the only assertion was
"no panic" and an empty capture does not panic either.

So the build stays release, and `run.sh` now asserts the capture is substantial (`MIN_BYTES`) and
that the pixel path actually reached the wire (`MIN_APC`). A green tick on this job should mean
something was drawn. The savings that are real and kept: the path filter, and warm Swatinem/Zig
caches.

## Reproducing locally

Release, for the reason above — a debug binary will trip the `MIN_BYTES` guard rather than produce
a capture worth reading.

```bash
cargo build --release
HERDR_BIN=./target/release/herdr HERDR_NS=herdr ./data/herdr-all-flags-live/run.sh
```
