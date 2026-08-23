# `herdr_pane_screenshot` — live captures

Every image here came from `herdr_pane_screenshot` (`scripts/herdr_mcp.py`) called
against a real `herdr` server, with a real `kitty` 0.45.0 client attached under
`Xvfb`, exactly the same technique `data/herdr-worker-card-nested-tiered/`
verified PR #192 with (`import -window root`, no window manager, kitty sized to
fill the Xvfb screen). Nothing here is a mockup or a composite.

The lab session ran two panes side by side in one tab: `w1:p1` printing
`\033[41m PANE ONE RED \033[0m` and `w1:p2` printing `\033[44m PANE TWO BLUE
\033[0m`, so a pane-level crop is visibly verifiable, not just numerically.

| file | what it shows |
| --- | --- |
| `01-whole-window.png` | `tab_id=w1:t1` (whole-window capture): the full `1600x1000` Xvfb screen, both panes, no cropping. |
| `02-pane-crop-red.png` | `pane_id=w1:p1`: cropped to that pane's exact cell rect, `590x969`px. Contains the red-background text; no blue. |
| `03-pane-crop-blue.png` | `pane_id=w1:p2`: same crop math, different pane. Contains the blue-background text (`#0E73CC`); no red. |

## Crop math, checked against the real numbers

`pane.layout` reported `w1:p1` at cell rect `{x:42, y:1, width:59, height:51}`
and `w1:p2` at `{x:101, y:1, width:59, height:51}`; `pane.graphics.info`
reported `cell_width_px:10, cell_height_px:19`. `59*10=590` and `51*19=969` —
exactly the pixel dimensions ImageMagick's `convert -crop` produced above.

## Read-only, checked against the real session

`session.snapshot`'s pane count and every pane's `revision` were identical
before and after three `herdr_pane_screenshot` calls (whole-window, both pane
crops): `[('w1:p1', 1), ('w1:p2', 1)]` unchanged. The capture never sends
anything to Herdr's socket beyond the same read-only calls `herdr_query` could
already make (`session.snapshot`, `pane.layout`, `pane.graphics.info`) — the
pixels themselves come from an external `import`/`convert` subprocess against
the X display, never from Herdr.

A background workspace/tab (`w2`, created unfocused) was confirmed to be
**refused** rather than silently screenshotting the wrong thing: `pane_id=w2:p1`
raised `refused: that target is not the workspace/tab currently shown on the
local display.`

## What this does and does not prove

Verified: a real local kitty terminal under Xvfb, in a lab, with
`experimental.kitty_graphics` enabled. This is exactly the environment the tool's
own description names as the good case.

Not verified, and out of scope here: a Windows/Rio client attached over
`--remote`. That client renders on a different machine the herdr server process
has no access to; `herdr_pane_screenshot` cannot reach it and its description
says so. Tracked separately in firstmate's backlog as
`herdr-windows-lab-options-20260823-decision-windows-testing-capability-choice`.

## Reproducing

1. Provision an isolated named session (never `default`), enable
   `experimental.kitty_graphics = true` in that session's `config.toml`.
2. Attach a real `kitty` client under `Xvfb` per `data/herdr-live-composite/lab.sh`'s
   conventions (no window manager, kitty window == Xvfb screen), pointed at the
   session with `herdr --session <name>` — clear every ambient `HERDR_*` env var
   first (`HERDR_ENV` in particular; herdr refuses a nested attach otherwise).
3. Split a pane, put distinct content in each.
4. Run `HERDR_MCP_DISPLAY=:<N> python3 scripts/herdr_mcp.py --selftest --session <name>`,
   or call `tool_herdr_pane_screenshot` directly for `workspace_id`/`tab_id`/`pane_id`.
