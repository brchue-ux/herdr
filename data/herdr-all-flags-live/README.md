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

## The pixel format is decided by the SERVER's environment, not the client's

An early run saw `f=100` (PNG) on every transmit and concluded the format half of #77 could not be
observed without a real terminal emulator. That was wrong, and the mistake is worth keeping written
down because it is easy to repeat: `host_terminal_kind()` reads *this process's* environment, and
its own doc comment says "for the split server, this is the server process's environment, which
only agrees with the terminal's own when server and client are co-located." `TERM` was being set on
the client's PTY while the encoding happens in the server.

`SERVER_TERM` now sets it on the right process, and the job runs the capture twice because one pass
can only confirm one branch:

| `SERVER_TERM` | `HostTerminalKind` | expected on the wire |
|---|---|---|
| `rio` | `Rio` | `f=32` — RGBA32 applies to translucent cards, so the upgrade must appear |
| `kitty` | `Kitty` | `f=100` — RGB24 is refused for a card because it is never opaque |

Both are asserted. A real-world consequence falls out of this: a server that did not inherit the
terminal's environment — started from a unit file, a cron job, a detached shell — silently keeps
PNG while still getting the `t=f` transport benefit.

## What this rig still cannot confirm

How the terminal *composites* what it is sent — glow bleed between cards, `z` ordering — is out of
reach here: kitty composites in linear light while `Canvas::blend` works in sRGB, so software
compositing of these bytes is not what the terminal would draw. That needs the Xvfb + kitty rig in
`data/herdr-card-as-alpha-shape/blend-test/`.

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

## Deliberately not here yet: the binary swap

What this checks is that a binary *draws*. It does not check that a **swap** works — stop herdr,
put a different binary in place, start it again, and expect the fleet to still be there. That risk
lives in the on-disk session snapshot written by the old version, not in the render, and it is the
thing a person actually does.

A `swap.sh` for it exists on `claude/live-swap-wip` and is deliberately held back: it has not yet
completed a run. It builds a fleet on the base branch's binary, stops it, restarts on the musl
binary this job builds, and compares workspace labels, pane ids and published metadata tokens
across the boundary. It currently fails looking for `session.json` after a clean shutdown, and
whether that is a wrong path or a snapshot that is never written is unresolved — so it is not in
front of real changes until it can tell those apart.

## Reproducing locally

Release, for the reason above — a debug binary will trip the `MIN_BYTES` guard rather than produce
a capture worth reading.

```bash
cargo build --release
HERDR_BIN=./target/release/herdr HERDR_NS=herdr ./data/herdr-all-flags-live/run.sh
```
