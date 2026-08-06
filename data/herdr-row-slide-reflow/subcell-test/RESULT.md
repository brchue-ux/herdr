# Can a sidebar card be placed at sub-cell resolution, and is it worth it?

**Yes it can, and no it is not.** Both halves measured on a real `kitty` 0.45.0 on `Xvfb`,
driven through the same escapes `src/kitty_graphics.rs` emits.

Row motion therefore places cards at **whole cells**. This file is why, so the decision does
not have to be re-derived from scratch the next time someone looks at the strip and wonders
whether it could be smoother.

## What `X=` / `Y=` actually do

Kitty's placement command takes an offset in pixels *within the first cell*. `encode_display_placement`
already knows how to write it; nothing in herdr ever sets it. Two cases were measured, with a
probe image whose top four pixel rows are magenta and whose bottom four are yellow, so a
screenshot says exactly which source rows landed where.

| case | top edge | bottom edge | drawn height |
|---|---|---|---|
| `y0` — `c`/`r` given, `Y=0` | 84 | 179 | 95 |
| `y8` — `c`/`r` given, `Y=8` | 92 | 179 | **87** |
| `y16` — `c`/`r` given, `Y=16` | 100 | 179 | **79** |
| `native_y0` — no `c`/`r`, `Y=0` | 84 | 204 | 120 |
| `native_y8` — no `c`/`r`, `Y=8` | 92 | 212 | **120** |
| `native_y16` — no `c`/`r`, `Y=16` | 100 | 216 | **120** |

Two facts fall out:

1. **`Y=` is a true translation.** The top edge moves down by exactly the offset, every time,
   at both a scaled and a native placement.
2. **With `c`/`r` given it clips, and with them omitted it does not.** Herdr always gives `c`/`r`
   — that is how a card is placed into the cells its row owns, and how the clipper crops a card
   at the panel's edge — so the bottom `Y` pixels of a card's image would be cut off. Those
   pixels are the card's own bloom, and a cut that *moves* is precisely the shearing edge this
   whole line of work exists to remove. Buying sub-cell placement therefore also means giving
   every card an extra cell row of transparent padding for the clip to eat.

## Why it was not bought here — and why that is a boundary, not a verdict

The binding constraint on how smooth motion looks is not the cell. It is the frame.

`crate::anim::behaviour::SMOOTH_FRAME_INTERVAL` is **50 ms**, the finest step any behaviour in the
engine declares. A 320 ms arrival is therefore about **six frames**. A reflow moves a row by one
row's height — about 72 px on a 9x18 px cell, which is four cells.

- At whole cells, six frames resolve to **five distinct positions**.
- At sub-cell, six frames resolve to **six**.

So at the engine's current step, sub-cell placement buys almost nothing, and this change ships at
whole cells.

**That is not the same as saying sub-cell is not worth having.** The measurement above counts
*positions per transition* at a fixed 50 ms step, which quietly assumes stepped motion is
acceptable. It is not: four 18 px jumps read as stepping, not as a glide. Held to a smooth-motion
requirement the same numbers say the opposite, and this is the correction —

- Smoothness needs a **finer frame tier** as well. Roughly 4 px per step over 72 px is ~18 steps,
  so ~18 ms frames; `MIN_RENDER_INTERVAL` is 16 ms, so the loop can do it.
- But extra frames alone buy nothing. Cell-crossing time here is `320 x 18/72` = **80 ms**, so any
  interval below 80 ms already produces duplicate positions under cell quantization. Twenty frames
  still land on five positions without sub-cell placement.
- **Both are needed together**, and then a third thing follows: once a card sits at a fraction of a
  cell, a character connector cannot follow it — a glyph occupies a whole cell row and there is no
  half-row position for `|`, `├` or `─`. So smooth motion also requires the tree's trunk and
  branches to be drawn as pixel artwork.

That chain — sub-cell placement, a finer frame tier, and the line as pixel wires — is the named
next piece of work. The transparent-pad cost below is real and still applies to whoever does it.

## The pad sub-cell would cost

With `c`/`r` given, a `Y` offset clips the bottom `Y` pixels of the image, and those are the card's
own bloom. A clip that *moves* is the shearing edge this line of work exists to remove, so sub-cell
placement also means giving every card image one extra cell row of transparent padding for the clip
to eat.

## Reproducing

```bash
Xvfb :98 -screen 0 900x1300x24 &
python3 make_probe.py
PROBE_DISPLAY=:98 bash run.sh y0 0 && PROBE_DISPLAY=:98 bash run.sh y8 8
PROBE_COLS=0 PROBE_ROWS=0 PROBE_DISPLAY=:98 bash run.sh native_y8 8
python3 analyse.py 'shots/*.png'
```
