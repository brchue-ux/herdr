# Live evidence — the triview caret landed in the command-log zone

Real **patched Rio 0.5.19** under `Xvfb`, driving the captain's own
`~/.config/herdr/config.toml` verbatim (`kitty_graphics = true`,
`sidebar_card_shapes = true`, `sidebar_width = 42`), isolated named lab
session, binaries hash-verified.

Not `kitty`: PR #194 shipped with "not verified against Rio itself" as its
stated residual risk, and this is the follow-up that closes it. The bug turned
out **not** to be terminal-specific — see "It is not Rio" below — but the
verification was still done on the client the captain actually uses.

## What the captain saw

> it's like there are two terminal input lines now and one hides behind the new
> command output that exists under the input line

> I see the pink cursor move as I type

## Reproduce

```bash
bash run-rio-lab.sh <herdr-binary> <tag> <scratch-dir>
```

`paint-claude-typing.sh` paints Claude Code v2.1.241's real screen shape (a
full-width rule pair around a `❯ ` composer, two footer rows on the floor,
three `⎿  $ ` shell-command echoes so the pane's command log fills), then
rewrites **only** the composer row once every 250 ms — what typing into the
agent looks like on the wire. The lab presses `Return` into the Rio window
first, so the app is in `Mode::Terminal`, the mode the captain types in.

`ffmpeg -f x11grab` at 12 fps, not `import`: a still every second lands on the
frame the split is correct on about half the time.

## `shots/caret-band.png`

The same band of the same screen, twice — base on top, this branch below:

| build | where the caret is |
|---|---|
| `fork/master` (`61246691`) | on `● cargo nextest run --lib zone_02`, **three rows below the composer**, inside the command-log zone |
| this branch | at the end of `❯ tththithisis a testing now a b c d`, on the composer |

Three is `ClaudeTriviewLayout::transcript_skip` — the rows the log took off the
top of the transcript, which shifted the transcript and the composer up by
exactly that much and which nothing downstream knew about.

`shots/caret-band-three-frames.png` is the same pair at three separate frames of
the typing run, so the base row is not a single unlucky capture.

## It is not Rio

The counters say so. With graphics on — the captain's configuration — the
retained dirty-patch fast path never runs at all:

```
$ cat shots/base-retained-counters.txt
…  retained_fallback.graphics_cache_active=1
```

Every retained attempt in the captain's configuration falls back on the cached
sidebar cards, so the *literal* duplicate composer that path would paint is not
what he is seeing. What he is seeing is the **frame cursor**, which
`crate::ui::tab_surface::tab_surface_cursor` reported on the composer's *grid*
row rather than the row it was drawn on. A caret blinking and moving inside a
row of command output is a second input line hiding behind the log.

The retained path is still fixed here, because it has the same defect and it
*does* run for any client without active graphics — which the first pass of
this lab, run before the config was valid, measured directly
(`retained_success.sent=1`, six times, while typing).

`shots/fix-retained-counters.txt` is the same run on this branch.
