# The summary badge and the group chevron, on the card

Both controls are drawn *over* a character row. A pixel card covered them while
their click targets stayed live underneath — raised on #48, again in #52's
review. This is the pixels showing that they are card elements now.

## The rig

A real `kitty` under `Xvfb`, attached to an isolated lab session, on the
server-rasterised path — the captain's own route. The pixel-card flags live on
`AppState`, which is server-owned, so `HERDR_CONFIG_PATH` has to be exported
before the session is provisioned; a client-side override cannot turn the pixel
path on.

```bash
# lab.toml: onboarding = false, [ui] sidebar_width = 42,
#           [experimental] kitty_graphics = true, sidebar_card_shapes = true
export HERDR_CONFIG_PATH=<lab.toml>
herdr session ...                       # through the lab helper, named session
Xvfb :91 -screen 0 1200x760x24 &
DISPLAY=:91 kitty -o initial_window_width=110c -o initial_window_height=30c \
  -o font_size=13 -- env HERDR_CONFIG_PATH=<lab.toml> <herdr> --session <lab>
DISPLAY=:91 import -window root shot.png
```

The fleet is one worktree group — a main checkout labelled `mate` and a linked
worktree labelled `issue` — where `mate` also owns a worker pane that published
a `summary` token. So the first card is the one carrying *both* controls, which
is the case the rail is sized for.

`before.png` and `after.png` are the same fleet, the same terminal and the same
geometry, drawn by a `fork/master` server and by this branch's server. Only the
sidebar is cropped; the panes either side are unchanged.

## What the pixels show

### `before.png` — the defect

The mate's card heads a worktree group *and* owns a worker that reported back,
and carries neither control. Both cells are still clickable.

### `after.png` — both controls, on the card

`▤1` and `▾` on the card's right rail, in the band above the state chip, right
aligned to the margin the chip is. The title, the tidbit and the chip are
untouched: at this geometry the chip is wider than the rail, so the rail costs
the title nothing.

### `collapsed.png`, `chevron-both-ways.png` — the chevron really turns

The same card with the group folded. Neither `▸` nor `▾` can be *set* — the
proportional faces a card is drawn in do not carry U+25B8/U+25BE — so the
chevron is a `canvas::Triangle`, and `chevron-both-ways.png` is the two
directions at 8× against each other. The badge beside it is identical in both,
which is [`CHEVRON_NOSE`]: the reserved box does not change when a group opens
or closes, so nothing beside it reflows on a click.

Note that a collapsed group keeps its *active* child on screen — see
`visible_group_idx` in `src/ui/sidebar.rs` — so the `issue` row does not vanish
in `collapsed.png`. That is unchanged behaviour, not the chevron disagreeing
with itself.
