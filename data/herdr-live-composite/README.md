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
out of a judged window of which most must move.

Measured: over eight runs against one stable base, one failed 4/7 with three
0 px pairs followed by four moving ones (run 31272863267) — a working tray,
reported red. So `lab_wait_for_motion` waits for the assertion's own subject,
using the assertion's own instrument, region and floor, and needs **three
consecutive moving pairs** before capture begins. Three because a blip lasting
one frame already yields two moving pairs, one arriving and one leaving.

That gate is half of it, and run 31274414469 — a failure *with* the gate in
place, same 4/7 shape — showed which half. What a readiness gate can see is the
badges arriving: engraved marks becoming lit ones, one large change. What it
cannot see is that the pulse then opens on an amplitude envelope. Measured per
badge across that run's frames, the swing grows

    0.4  ->  3.9  ->  5.7  ->  7.4  ->  7.5   luma

so for about two seconds the animation is genuinely running and every pair still
measures **0 px** against a per-channel floor of 24. There is nothing left to
wait for — the fade-in *is* the animation — so no readiness signal can fix it.

Hence the other half: capture runs past the fade and `--tail-pairs` takes the
verdict from a trailing window, printing the earlier pairs marked
`(warm-up, not judged)`.

Neither half can hide the defect it sits in front of. The gate warns on timeout
and captures anyway, so a tray that never moves is still reported by the
assertion with its numbers. And the tail window is where a freeze is *most*
visible: a tray that animates and then stops — the #97 shape exactly — freezes
inside the judged pairs, which is checked against real frames rather than
asserted.

### The window is sized for the beat, not for the fade

Both mitigations above answer a *transient*: something that happens once, at the
start, and is over. The failures that kept arriving after them were not that
shape, and the reason took 154 measured pairs per pass to see.

A lit badge snaps and settles on `BADGE_CHARGE_PERIOD` — 2,660 ms — and a
resting one breathes over 5,880 ms. Sampling a 2.66 s cycle every 0.6 s advances
0.226 of a turn per frame, so the samples walk around the cycle rather than
landing on it, and re-enter the slow part of the waveform every four to five
frames. There the whole badge layer moves by less than the 24-per-channel floor:
the pair measures 10-12 px against a 25 px floor and reads STILL while the
surface is animating perfectly.

That it is a beat and not a warm-up is what the pair positions say. Over the
fourteen most recent runs, the `on` pass put its still pairs at positions 1, 6
and 11 — 10, 9 and 7 times respectively — with a gap of five in 11 of 27
intervals. A transient loads the head; this repeats forever.

| | moving | still | beat period | worst 7-pair window |
|---|---|---|---|---|
| `off` | 85.7% | 14.3% | 4 frames | 5 of 7 |
| `on` | 73.4% | 26.6% | 5 frames | **3 of 7** |

The `on` pass is the compressed one: the badge swing composites over a lit
background, so the same animation clears the floor on fewer pixels. Its
structural rate is 73.4%, and the threshold it was being judged against was 5 of
7 — **71.4%**. A threshold sitting on a surface's own duty cycle is not
strictness, it is a coin toss, and the tail windows say so. Those fourteen runs
scored 3, 5, 5, 5, 5, 5, 5, 5, 5, 5, 6, 6, 6, 6 out of 7 on the `on` pass: nine
landed exactly on the line with no margin at all, and the first one to land a
trough in the judged window twice went red on a build whose tray was fine.

That build is the control, because it was re-run unchanged. Run 32616990497 on
commit `c189d70b` scored **3 of 7** and went red; re-run on the same commit,
same runner image, same kitty, it scored **5 of 7** and went green — straight
back onto the line. Nothing between the two runs but which phase of the badge
cycle the capture happened to start on.

Repositioning a seven-pair window inside an eleven-pair capture cannot escape a
trough that returns every five pairs. Lengthening it can, because a beat with a
fixed duty cycle averages out over a window longer than its period. Measured
worst case over *every* window of each length on the `on` pass:

    3/7  ->  4/8  ->  5/9  ->  6/10  ->  7/11

So capture runs to **18 frames** and `--tail-pairs 12` takes the verdict from
the last twelve — past two full beats on either pass.

The threshold on that window comes from a stated margin rule rather than from
taste: **two pairs below the worst window a healthy tray has ever produced.**
The worst measured is 63.6% — 7 of 11, `on` pass — which over twelve pairs is
7.6, so the line is **6 of 12**. Every recorded window of nine pairs or more, on
either pass, clears it: the worst are 5/9, 6/10 and 7/11 against a 50% line.

Six of twelve is not weak where it matters. A frozen tray scores 0. A tray that
animates and then stops reaches six only if every pair before the freeze moved,
and on a surface whose own duty cycle is 73.4% six clean pairs in a row is a
one-in-seven event — the other six times in seven it goes red. A tray frozen for
the last two thirds of the window cannot pass at all. The old 5-of-7 rule was no
better on that axis, and much worse on the one that was actually firing.

The tempting fix is the wrong one. Dropping `--level` so the compressed swing
clears it would work on the numbers and destroy the assertion: on a still pair
the background scene alone already contributes 10-12 px inside the tray region,
so a lower floor buys a tray assertion that a moving *background* satisfies on
its own. Window length is the axis that cannot go vacuous.

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
