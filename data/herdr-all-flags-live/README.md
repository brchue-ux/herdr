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
- **No panic** with every flag on simultaneously — the single most valuable signal, since this
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
compositing of these bytes is not what the terminal would draw. That needs `data/herdr-live-composite/`.

Three more gaps, named so they are not mistaken for coverage:

- **The `--remote` client's sidebar is a different render path** and neither standing rig covers it.
  A delegating client is sent `CardScene`/`TrayScene` *tokens* and rasterises the pixels itself, so
  a tap of the server socket sees no graphics at all. That is the population #103's dropped-scene
  bug lived in. `HERDR_CLIENT_RASTERIZED_CARDS=1` plus `HERDR_CLIENT_RASTERIZED_SIGNAL_TRAY=1` put a
  Unix client on it; the escapes exist only between that client and its terminal.
- **Nothing here runs at a real window size.** 90x32 at 9x18 is 810x576 px. `MAX_GRAPHICS_FRAME_SIZE`
  is reachable at a 42-column sidebar on a 1600x1000 terminal, and no PTY this small can get there.
- **`kitty_graphics_capability_confirmed` is still a shared foreground-derived slot**, unlike the
  terminal kind and the cell size, which were folded across every viewer in #101 and #102.

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

## The binary swap (`swap.sh`)

What the captures check is that a binary *draws*. `swap.sh` checks that a **swap** works, which is
a different risk entirely and lives in state on disk rather than in the render. It runs two phases,
because herdr ships two mechanisms carrying two different formats — see AGENTS.md, "Server state
that has to survive a restart":

| | boundary | what moves |
|---|---|---|
| cold restart | `persist::SessionSnapshot` (`session.json`) | stop, swap the binary, start again |
| live handoff | `server::handoff::HandoffManifest` | `herdr server swap --exe`, fleet never stops |

The gnu build creates the fleet; the **musl artifact this job already builds** inherits it, so the
binary under test is the one a release actually ships. Both phases compare workspace labels, pane
ids, and every published metadata token at both levels.

### Three defects it was parked on, and what each turned out to be

It sat unmerged for a run because it failed looking for `session.json` after what looked like a
clean shutdown, and the two candidate explanations — wrong path, or a snapshot never written — were
both wrong.

**The signal went to the wrong process.** `$!` after backgrounding a shell *function* is the pid of
the subshell bash forks to run it, not of the program that function executes; bash does not
exec-optimise a function body away. So `kill -TERM "$(cat server.pid)"` tore down the wrapper and
left the server running, reparented and unsignalled. The wait loop then saw its pid vanish and
printed "server exited cleanly", the server's shutdown path never ran, and no snapshot was written.
`( exec env … ) &` makes `$!` the server's own pid. Reproduced in isolation: recorded pid 20330,
real server 20331.

**Two of its three assertions could not fail.** The comparison walked
`workspaces[].tabs[].panes[]`. `session.snapshot` has no such nesting — it is four flat top-level
arrays, `workspaces`, `tabs`, `panes`, `agents`, with `tokens` an object on workspaces and panes
alike. Both loops therefore yielded nothing and compared two empty collections. Run against real
snapshots with *every token deliberately dropped*, the old comparison prints `panes restored: 0`,
`tokens restored: 0 of 0`, and `swap OK: the new binary inherited the old binary's session intact`.
Every check now asserts its own subject is non-empty first, because a structural mistake in a
comparison is invisible unless the comparison refuses to run on nothing.

**The panic report was skipped exactly when it was needed.** `STATUS=$?` sat after an inline
`python3` heredoc under `set -e`, so a failing comparison aborted the script before the panic grep
it was collecting a status for.

The stop is now `herdr server stop` — the command herdr's own post-update guidance prints — and the
assertion is that **the API stops answering**, not that a pid disappeared. Those are different
claims, and only the first is about the server.

## The drift check was inert, and why a baseline could not have been seeded

`digest.py check` returned **0** when the baseline file was missing, and no baseline was ever
committed, so the golden half of this rig passed vacuously on every run it has ever had. A missing
baseline now exits 3 and `BASELINE_REQUIRED=1` makes it fatal.

Seeding one would not have worked either. The digest included the full decoded grid, and the panes
to the right of the divider run real shells whose default prompt carries the machine's hostname —
freshly generated per run on a hosted runner (`fv-az1425-773`). Any committed baseline would have
mismatched on every run afterwards, as permanent drift with a plausible-looking diff. The digest is
now cropped to the sidebar's own columns (`DIGEST_GRID_COLS`, taken from `sidebar_width`), which is
the surface this rig is about; placement geometry is still read from the whole capture.

To keep that honest rather than assumed, `run.sh` captures one unchanged state **twice** and
requires the two digests to be identical. That is the precondition a baseline needs, it is
checkable on the very first run, and it means a future drift failure is a real difference rather
than something to be triaged as a possible flake.

## The flag list is checked against the struct

This rig's name is a claim about completeness, so `run.sh` diffs `config.toml`'s `[experimental]`
keys against the boolean fields of `ExperimentalConfig` and fails on either a missing flag or a
stale one. `persistent_background` — the whole-terminal background scene — was missing for this
check's first four runs: a shipped flag, drawing a full-surface image, outside a job called "all
flags on". Nothing could have noticed, because the only thing that knew the full list was the Rust
source.

It is also the one flag here that legitimately does nothing in one of the two passes: an opaque
ambient wash is refused on a terminal not measured to draw the below-text band, so it reaches the
wire under `SERVER_TERM=kitty` and is correctly withheld under `rio`. A placement appearing under
`rio` would be the #96 regression.

## One more silent failure, now guarded

A per-frame graphics payload over `protocol::MAX_GRAPHICS_FRAME_SIZE` (32 MiB) is dropped *whole*,
taking every pixel surface on that pass with it and putting nothing on screen to say so — it reads
exactly like the capability handshake failing. The only evidence is `dropping oversized graphics
payload` in the server log, which `run.sh` now greps for. This matters more than it did: with
`persistent_background` on, the configuration is the largest it has ever been. Note the geometry
here (90x32 at 9x18 = 810x576 px) is far too small to reach that cap on its own — a small PTY never
does, which is why this guard is about catching the day something makes it reachable, not proof the
cap is respected at a real window size.

## Reproducing locally

Release, for the reason above — a debug binary will trip the `MIN_BYTES` guard rather than produce
a capture worth reading.

```bash
cargo build --release
HERDR_BIN=./target/release/herdr HERDR_NS=herdr ./data/herdr-all-flags-live/run.sh
```
