# Live compositing check — what a real terminal actually draws

A real headless herdr server, a real `kitty` client on a real X server, and
assertions made on **screenshots of the pixels the terminal put on screen**.

This exists to close the gap `data/herdr-all-flags-live/` names about itself:

> **What it does not cover** — How the terminal composites what it is sent —
> glow bleed, `z` ordering. Kitty composites in linear light while
> `Canvas::blend` works in sRGB, so software compositing of these bytes is not
> what the terminal draws.

The two checks are complementary and neither replaces the other:

| | `data/herdr-all-flags-live/` | this rig |
|---|---|---|
| evidence | bytes a client receives, decoded to a grid | pixels a terminal drew, screenshotted |
| needs | a PTY | Xvfb + a real kitty binary |
| catches | wrong bytes, wrong format, wrong transport, no bytes at all | wrong compositing, wrong stacking order, frozen surfaces |
| blind to | how the terminal interprets correct bytes | which protocol key produced a pixel |

## Why this is not optional tooling

Three regressions shipped in this area, all with the unit suite fully green, and
each one was found by hand-building a throwaway Xvfb + kitty lab from scratch:

- **#94** — animation timing and flicker. Found by measuring the actual render
  rate on a real Kitty client; without live graphics the wasted frames come out
  byte-identical and get dropped, so a PTY-based capture shows nothing wrong.
- **#96** — background z-order. herdr emitted a correct `z=-2`; a host that
  ignores the negative band drew the opaque wash over every glyph on screen. The
  bytes were right, so no byte-level check could have caught it. It was isolated
  by replaying one real byte stream into kitty twice with `z=-2` rewritten to
  `z=0`: the entire UI disappeared.
- **#97** — signal tray freeze. The artwork was pixel-correct and never moved
  again, because the frame that would have carried the next bytes was suppressed
  as identical.

## What it asserts

**Text over a drawn background stays legible** (`assert_legible.py`, the #96
class). Two passes over one build differing in exactly one config key —
`persistent_background` — give a reference and a candidate. The reference
supplies the ink/paper masks; asking the candidate for its own would be
circular, since a wash that has covered the text produces perfectly good masks
of *itself*. Three things must hold: the background genuinely drew (or the check
is vacuous), the two clusters stay separable by WCAG contrast, and the candidate
reproduces the reference's mask per pixel. The *direction* of the contrast is
not constrained — herdr's per-cell legibility pass may invert text over a bright
scene, and dark ink on a light wash is still readable.

**Surfaces that declare an animation demonstrably move** (`assert_motion.py`,
the #94/#97 class). The signal tray — the #97 surface exactly — must differ
across a minimum number of consecutive frame pairs. A live pane running a real
process is captured as a control: if that is still too, the client is not
receiving frames at all and the tray verdict means nothing.

Floors are calibrated against a real run rather than guessed. The tray moves
**145-237 px** per 0.6 s pair on 7 of 7 pairs; the floor is 25 — the same floor
also clears the 74-75 px the tray produced when it was still drawing as
characters, so it holds across both. The live pane moves 1,547-1,909 px; its
floor is 400.

An all-idle signal tray is engraved marks that never move, so the rig builds a
throwaway git repo one commit ahead of *and* behind its upstream, which lights
Push=Active and Sync=Attention. Without it a frozen tray would sail through.

That same fact is why capture does not start when the terminal first paints.
The badges go live only once the state behind them arrives — a git remote-status
refresh, on a 1.5 s cadence and off-thread, landing on a client that is already
up and drawing — so "something is on screen" is a readiness signal for painting
and says nothing about whether the surface under test has begun animating. A
capture started there can spend its first pairs on a genuinely static warm-up,
out of a budget of seven of which five must move.

Measured: over eight runs against one stable base, one failed 4/7 with three
0 px pairs followed by four moving ones (run 31272863267) — a working tray,
reported red. So `lab_wait_for_motion` waits for the assertion's own subject,
using the assertion's own instrument, region and floor, and needs **three
consecutive moving pairs** before capture begins. Three because a blip lasting
one frame already yields two moving pairs, one arriving and one leaving.

The gate cannot hide the defect it sits in front of. On timeout it warns and
capture proceeds anyway, so a tray that never moves is still reported by the
assertion with its per-pair numbers; and a tray that moves and *then* freezes —
the #97 shape exactly — now has the whole seven-pair budget pointed at the
period after warm-up rather than partly spent before it.

## The detectors prove they can fail, first

`controls.sh` runs before the expensive job and needs no Rust build. It drives
five synthetic scenes through the same real kitty and checks the verdict of each:

| scene | assertion | must |
|---|---|---|
| wash at `z=-2` behind text | legibility | **pass** |
| the same wash at `z=0` | legibility | **be caught** |
| a block stepping across the screen | motion | **pass** |
| the same block, drawn once | motion | **be caught** |

Measured on this rig: `z=-2` gives contrast 6.74:1 and per-pixel agreement
0.995; `z=0` gives 1.05:1 and 0.538. Those are far enough apart that no
judgement call is involved.

This is not ceremony. The byte-level check's own history contains a run that
went green on a 2,568-byte capture because the only assertion was "no panic". A
check that has never been shown to fail is a tick with nothing behind it, and
the self-test makes the real job's green mean the detectors *were able* to go
red that minute, on that runner, with that kitty build.

## Two things that decide whether this measures anything

**Readiness is content-based, never a sleep.** Measured here, glyphs appear
about 1 s after the window maps but an image placement only lands at about 3 s —
a terminal decodes and scales graphics off its parse thread and Xvfb has no GPU
to do it on. A sleep tuned against text screenshots a frame with the pixel layer
missing, which is indistinguishable from a terminal that refused to draw it.
`lab_wait_for_mean` waits for the frame to actually get brighter instead.

**The window is the whole screen.** kitty is sized in pixels to exactly the Xvfb
screen with no window manager, so the X root window and the kitty window are the
same rectangle and every assertion can address regions as fractions of the
frame. Hunting the window geometry at capture time fails silently — a fractional
region measured against the wrong rectangle still returns a number.

For the same reason the probe **paints its own cell background** (rgb 0,0,160,
padded to a solid rectangle) and is located by that colour. Bounding a bright
region instead would mean knowing where the sidebar ends in pixels, which is
`sidebar_width × cell_width`, and the cell width depends on whichever font and
DPI the runner resolves. Detection runs on the reference pass only: with the
scene on, that backdrop is legitimately covered — the wash sits above the cell
background and below text by design — and mistaking that for a detection failure
would hide the very thing being measured.

## Files

| file | what it does |
|---|---|
| `lab.sh` | Xvfb + kitty + screenshot plumbing, shared by both entry points |
| `controls.sh` | the detector self-test — no herdr, no Rust |
| `controls_scene.py` | synthetic scenes, drawn from inside a real kitty window |
| `run.sh` | one live pass: server, fleet, kitty client, captures, motion asserts |
| `compare.sh` | the legibility assertion across the two passes |
| `config.toml.in` | the lab config; `@PERSISTENT_BACKGROUND@` is the only substitution |
| `assert_legible.py` | text-over-background legibility |
| `assert_motion.py` | sustained inter-frame motion in a region |
| `composite_lib.py` | measurement helpers — Pillow only, no numpy |

## Reproducing locally

Needs `Xvfb`, `kitty`, ImageMagick (`import`, `convert`, `compare`),
`xdpyinfo`, and python3 with Pillow.

```bash
# detectors only — about 35 s, no build
bash data/herdr-live-composite/controls.sh

# the live passes, against a release build
cargo build --release
HERDR_BIN=$PWD/target/release/herdr HERDR_NS=herdr BACKGROUND=off \
  COMPOSITE_DISPLAY=:97 bash data/herdr-live-composite/run.sh
HERDR_BIN=$PWD/target/release/herdr HERDR_NS=herdr BACKGROUND=on \
  COMPOSITE_DISPLAY=:98 bash data/herdr-live-composite/run.sh
bash data/herdr-live-composite/compare.sh
```

`run.sh` starts its **own** server under an isolated `HOME`/`XDG_CONFIG_HOME`
(`config_dir()` derives the socket path from `XDG_CONFIG_HOME`, so the socket
moves with it). On a workstation that already has a live herdr fleet, use a
named session and the fleet's own lab tooling rather than pointing this at the
default session.

## What the first live runs found

The rig earned its keep before it was green.

**No pixel surface drew at all**, while every character surface was fine: no
sidebar cards (max luminance 42/255 over the row area), no whole-terminal
background (0.000 coverage against the reference pass) — but pane text, borders,
tree connectors and the signal tray all rendered, and the tray animated. A UI
that looks plausible and is merely empty.

The cause was in the server's own log, once the rig kept it as an artifact:

```
WARN herdr::server::headless: dropping oversized graphics payload for client
frame client_id=2 graphics_bytes=52124609 max=33554432
```

**52 MB per frame against a 32 MiB `MAX_GRAPHICS_FRAME_SIZE`, dropped ten times
a second**, taking every pixel surface with it. 57 MB with the background scene
on — the scene itself adds only ~4.8 MB, so the overrun is the wash.
`sidebar_particle_field` costs (sidebar area x RGBA x animation loop frames),
and at a 42-column sidebar on a 1600x1000 terminal that is over the cap on its
own.

With the wash off — which is also the captain's own configuration (#96) — the
whole pixel path works. Same commit, same runner, one flag:

| | wash on | wash off |
|---|---|---|
| graphics payload | 52 MB, **dropped** | under the cap, delivered |
| sidebar cards | nothing | drawn: labels, stage chips, glow, tree |
| background coverage | **0.000** | **0.995** |
| text contrast over the scene | n/a — nothing to be over | **19.11:1**, mask agreement 0.992 |

Why nothing else caught it:

- `just check` never renders to a terminal.
- The byte-level check drives a **90x32** PTY. Scale the sidebar down that far
  and the payload fits, so the drop cannot happen there. That is the boundary of
  what a small synthetic PTY can see, not a fault in that check.
- On screen it is not an error, it is an *absence*. Turn the flag on, get a
  sidebar with no cards, and nothing tells you why.

`run.sh` now **asserts** the drop never happens, with the measured bytes in the
failure, so it can never again be a warning in a log nobody reads.
`PARTICLE_FIELD=true` re-runs against the wash.

Ruled out along the way: **#77's local transport is not implicated.** Payload
size was identical with `t=f` on and off (52,124,609 against 52,124,643 bytes),
because the drop happens before a transport is chosen. It stays on, and with the
payload no longer dropped whole this is the first check to exercise `t=f` end to
end — the byte-level check can only ever confirm the path reaches the wire.

## Known next tightening

Two things are measured and printed but not asserted, pending a run's worth of
numbers — the same seed-then-enforce discipline `digest.py` uses next door.

**Whole-frame motion.** In the `BACKGROUND=on` pass it includes the scene's own
orbiting bodies, which is the #96 symptom-3 class (`a=f` refused => root frame
only => frozen planets).

**The sidebar row area.** With the payload delivered, cards draw in full — and
then hold still: 0 changed pixels across all 7 pairs in the reference pass, where
#96 measured card pulse at 18,000-39,000 px per 210 ms pair in a comparable
sidebar. The tray on the same client moves 145-237 px per pair. That gap is worth
someone's attention; it is a printed number here rather than a red build, because
a check that goes red for a reason nobody has diagnosed is one people learn to
re-run past.

## Prior art

`data/herdr-card-as-alpha-shape/blend-test/` is the manual ancestor of this rig:
the same real-kitty-under-Xvfb technique, used once to answer whether the
terminal composites two overlapping transparent images and in which colour space
(it does, in linear light, to the byte). It stays as it is — a recorded
experiment with its result. This directory is the standing version.
