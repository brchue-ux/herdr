# The card as a shape with an alpha channel

Evidence for `[experimental] sidebar_card_shapes`. Two things are recorded here:
the measurement that chose the architecture, and the pixels that show it works.

| | |
|---|---|
| `blend-test/` | **Phase 1.** Does a real terminal composite two overlapping transparent images, or does the top one win? Measured, not reasoned about. See [`blend-test/RESULT.md`](blend-test/RESULT.md). |
| `proof/` | **Phase 2.** The artwork a real Herdr sends, put into a real terminal and screenshotted. |
| `replay.sh` | Places captured card artwork into headless Kitty at the cells the sidebar placed it at. |

## The answer that chose the architecture

**Kitty composites overlapping RGBA placements exactly — and in linear light.**
Every case measured (same `z`, different `z`, the sidebar's own `z = 0` band, a
neutral negative band, and both stacking orders) came out an exact source-over
composite, max channel error 0 in five of six cases and 1 in the sixth.

So: **one placement per card.** Each card is its own RGBA image, its own alpha,
its own placement id, its own position. Nothing composites the tree into a sheet,
and moving one card moves one card.

The linear-light finding is not a footnote — `Canvas::blend` composites in sRGB,
so had Herdr blended the cards itself it would *not* have reproduced what the
terminal does. Letting the terminal do it gets the physically correct glow
overlap for free.

## What the pixels show

Reproduce with:

```bash
Xvfb :97 -screen 0 900x1300x24 &
HERDR_SHAPE_CAPTURE_DIR=/tmp/cap cargo nextest run -E 'test(shape_capture)' --no-capture
BLEND_DISPLAY=:97 bash replay.sh /tmp/cap shapes proof/shapes.png
BLEND_DISPLAY=:97 bash replay.sh /tmp/cap sheet  proof/sheet.png
```

Same fleet — ten agents at three depths, `sidebar_width = 42` — drawn both ways.

### `proof/edge-comparison.png` — the thing he saw

The left edge, where a full-width mate card steps in to an indented worker card.

- **Sheet:** a hard vertical rule with square corners, and the glow terminating
  dead against it. This is *"the sharp rectangular edge of the background that
  has not been blended with the glow."*
- **Shapes:** no edge. The glow falls off into the panel, and the neighbouring
  cards' glows blend into one another.

### `proof/overlap-zoom.png` — a card on top of another card

One card's placement moved four columns right and two rows up — nothing about its
artwork changed, only where it is. It lies over its neighbour with no rectangular
seam and nothing clipped, because there is no box to clip. This is a row slide
mid-flight, and it is the case the sheet could not express.

### `proof/shapes.png`, `proof/sheet.png` — the tree at rest

Both read the same: same geometry, same tiers, same two-line titles. The layout
is asserted identical across the flag by
`the_shapes_path_moves_nothing_the_layout_settled`.

## Scope of the visual check, stated plainly

The screenshots are the **real card artwork** and the **real placement rects**
that `build_cards` produces, driven into a real Kitty 0.45 through the same
escape sequences `src/kitty_graphics.rs` emits. What they do *not* exercise is
Herdr's own TUI process end to end — the fleet comes from the test fixture rather
than from live panes, and the escapes are replayed by `replay.sh` rather than
written by the running binary. The emission path itself is unchanged by this work
and is covered by the tests in `src/kitty_graphics.rs`.

The character fallback below 34 columns is covered by test, not by screenshot:
`a_panel_too_narrow_for_a_card_gets_no_shapes` asserts that a narrow panel both
draws no shapes *and* does not suppress its character cards — the pairing that
would otherwise leave a blank row.

## A trap worth keeping

Kitty **silently drops** a placement that does not fit the grid. A window one
column too narrow produces a blank screenshot that looks exactly like a rendering
bug in the cards. `replay.sh` records the grid it actually got into
`.size-<which>` for this reason.
