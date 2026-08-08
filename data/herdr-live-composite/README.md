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
the #94/#97 class). The sidebar — cards, signal tray, particle wash — must
differ across a minimum number of consecutive frame pairs. A live pane running a
real process is captured as a control: if that is still too, the client is not
receiving frames at all and the sidebar verdict means nothing.

An all-idle signal tray is engraved marks that never move, so the rig builds a
throwaway git repo one commit ahead of *and* behind its upstream, which lights
Push=Active and Sync=Attention. Without it a frozen tray would sail through.

## The detectors prove they can fail, first

`controls.sh` runs before the expensive job and needs no Rust build. It drives
five synthetic scenes through the same real kitty and checks the verdict of each:

| scene | assertion | must |
|---|---|---|
| wash at `z=-2` behind text | legibility | **pass** |
| the same wash at `z=0` | legibility | **be caught** |
| a block stepping across the screen | motion | **pass** |
| the same block, drawn once | motion | **be caught** |

Measured on this rig: `z=-2` gives contrast 6.26:1 and per-pixel agreement
0.993; `z=0` gives 1.00:1 and 0.492. Those are far enough apart that no
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

## Known next tightening

Whole-frame motion is measured and printed but not asserted. In the
`BACKGROUND=on` pass it includes the scene's own orbiting bodies, which is the
#96 symptom-3 class (`a=f` refused ⇒ root frame only ⇒ frozen planets). It needs
one real run's numbers before a floor can be set that is neither vacuous nor
flaky — the same seed-then-enforce discipline `digest.py` uses next door.

## Prior art

`data/herdr-card-as-alpha-shape/blend-test/` is the manual ancestor of this rig:
the same real-kitty-under-Xvfb technique, used once to answer whether the
terminal composites two overlapping transparent images and in which colour space
(it does, in linear light, to the byte). It stays as it is — a recorded
experiment with its result. This directory is the standing version.
