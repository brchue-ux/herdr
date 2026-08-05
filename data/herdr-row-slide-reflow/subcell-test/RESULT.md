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

## Why it is not worth buying

The binding constraint on how smooth motion looks is not the cell — it is the frame.

`crate::anim::behaviour::SMOOTH_FRAME_INTERVAL` is **50 ms**, the finest step any behaviour in
the engine declares. A 320 ms arrival is therefore about **six frames**. A reflow moves a row by
one row's height — around 76 px at the 10×21 px cell the cards are measured against, which is
four cells.

- At whole cells, six frames resolve to **five distinct positions**.
- At sub-cell, six frames resolve to **six**.

One extra position, in exchange for a pixel-offset path through the placement pipeline and a
transparent pad on every card image. The frame step is coarser than the cell is tall, so the
quantization that is actually visible is the engine's, not the grid's.

This flips if `row_enter_ms` is raised a long way: at 1500 ms an arrival is thirty frames, the
per-frame travel drops to about 2.5 px, and whole cells would then be the thing you see. If the
captain wants arrivals that slow, this is the file to come back to.

## Reproducing

```bash
Xvfb :98 -screen 0 900x1300x24 &
python3 make_probe.py
PROBE_DISPLAY=:98 bash run.sh y0 0 && PROBE_DISPLAY=:98 bash run.sh y8 8
PROBE_COLS=0 PROBE_ROWS=0 PROBE_DISPLAY=:98 bash run.sh native_y8 8
python3 analyse.py 'shots/*.png'
```
