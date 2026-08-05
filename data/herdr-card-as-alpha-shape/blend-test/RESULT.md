# Does the terminal composite two overlapping transparent images, or does the top one win?

**It composites them. Exactly, and in linear light.** Measured, not reasoned about.

That answer selects **Build B: one placement per card**.

## How it was measured

A real `kitty` 0.45.0 driven headless on `Xvfb :97`, fed the *same escape sequences
`src/kitty_graphics.rs` emits* — same `a=t,t=d,f=100,s=,v=,i=,q=2` upload, same 3072-byte
chunking, same `a=p,i=,p=,c=,r=,z=,C=1,q=2` display after a `CUP` — then screenshotted with
`import -window root` and the overlap pixel compared against four hypotheses arithmetically.

Two RGBA PNGs, each a rounded rect built the way the card is: transparent outside, a soft
bloom falling off outward, a hard stroke on the boundary, a dim `a=0.30` fill inside. They are
placed to overlap by about a third.

| file | what it does |
|---|---|
| `make_images.py` | builds the two soft-edged RGBA cards |
| `emit.py` | emits herdr's exact upload + placement escapes |
| `run.sh` | drives headless kitty for one case and screenshots it |
| `analyse.py` | measures geometry off the shot and classifies the overlap pixel |
| `RESULTS.txt` | full numeric output |
| `shots/*.png` | the screenshots |

Reproduce with `Xvfb :97 -screen 0 900x1300x24 &` — the same display the proof
screenshots use — then
`BLEND_DISPLAY=:97 bash run.sh same_z0 0 0 && python3 analyse.py 'shots/*.png'`.

## The result

Every case blends, and the blend is an exact source-over composite:

| case | z (lower card, upper card) | verdict | max channel error |
|---|---|---|---|
| `same_z0` | 0, 0 — **the sidebar's own band** | blends, amber over cyan | **0** |
| `same_zneg` | -1, -1 — neutral band, under text | blends, amber over cyan | **0** |
| `diff_z` | 0, 1 | blends, amber over cyan | **0** |
| `diff_z_far` | 0, 100 | blends, amber over cyan | **0** |
| `diff_zneg` | -2, -1 | blends, amber over cyan | **0** |
| `rev_z` | 1, 0 — cyan on top | blends, **cyan over amber** | 1 |

Two things are settled by that table:

1. **Transparency composites.** In the overlap, `a=0.30` amber over `a=0.30` cyan over black
   produced `(151,129,130)` — nowhere near amber alone `(148,72,33)` or cyan alone
   `(33,130,148)`. A terminal where the top placement simply won would have given amber alone.
2. **`z` is honoured, and it picks which is source and which is destination.** `rev_z` is the
   same two images with the z order swapped, and it produced `(130,141,151)` — the *other*
   order's composite, predicted to the byte. Nothing about the sidebar's `z = 0` band is
   special; it behaves the same as the neutral negative band.

## The one surprise: it blends in linear light, not in sRGB

The predicted sRGB-space composite is `(100,129,123)`; the measured pixel is `(151,129,130)`,
which is the *linear-light* composite to within zero. So the terminal un-gammas, blends, and
re-gammas.

This matters beyond bookkeeping, and it is an argument for Build B on its own:

- `Canvas::blend` in `src/ui/sidebar/image_card/canvas.rs` composites in **sRGB space**. So
  herdr compositing two cards itself (Build C) would *not* reproduce what the terminal does —
  overlapping glows would come out visibly darker and less luminous than the terminal's own
  answer for the same two shapes.
- Letting the terminal blend gets the physically-correct additive-looking glow overlap for
  free, which is exactly the look wanted where two card glows cross.

## What this means for the build

**Build B.** Each card is its own RGBA image with its own placement id, its own position, and
its own alpha — transparent outside its own glow. There is no sheet, so there is no rectangle
to clip, and the terminal blends the overlaps for us in the right colour space.

The opaque `fill_row_backdrop` in `src/ui/sidebar/image_card.rs` — which paints the theme
background across every cell of every row, and is the sharp edge the captain can see — goes
away entirely rather than being made transparent.
