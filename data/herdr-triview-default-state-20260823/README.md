# The triview split as the pane's default state

Live evidence for `fix: make the claude triview the pane's default state and
let its log fill`.

Reported as: *the three-region split "silently falls back instead of showing"
— a real focused Claude Code pane still renders as one unbroken block of text.*

## What was actually wrong

Two independent things, both confirmed against a real `claude` CLI (v2.1.241)
driven in a plain PTY at 120x34 before any herdr code was touched.

1. **The split was gated on `Mode::Terminal`.** Herdr starts in
   `Mode::Navigate`, and every overlay leaves `Terminal` too, so the
   unmodified full-pane render was the steady state.
2. **The command log could never fill.** `detect::command_marker` looked for
   `⏺ Bash(...)` — U+23FA. Across every state captured from v2.1.241, U+23FA
   appears **zero** times and `Bash(` appears zero times in the default view.
   The bullet is now U+25CF, and in the default non-verbose view a shell call
   prints no `Bash(...)` line at all:

   ```
   ● Sleeping 20s in Python then printing ok
     ⎿  $ python3 -c "import time; time.sleep(20); print('ok')"
   ```

   Only `ctrl+o`'s detailed transcript shows `● Bash(...)`. Once the call
   finishes, the whole block folds to `Ran 1 shell command` and the `⎿  $ `
   echo leaves the screen — which is why the log keeps its own copy.

   Non-command `⎿` results put U+00A0 where a shell command puts `$ `, which
   is what keeps a `Read`'s output from being logged as a command.

Together: with an always-empty log, `log_rows` is 0, and the split's only
remaining output is two dim `─` dividers drawn exactly where Claude already
drew `─`. An engaged split was pixel-indistinguishable from the fallback.

## What to look at

`split-band.png` is the same band of the same screen three times, stacked:

1. **`fork/master`, navigate mode** — no third zone. This is the mode herdr
   starts in and no key was pressed to reach it.
2. **`fork/master`, terminal mode** — the split engages here, and the band is
   *the same*: no command-log zone, because the log is empty.
3. **This branch, navigate mode** — the three logged commands sit between the
   composer and the agent's own status bar, which is still on the pane floor.

`fix-navigate.png` is that third state whole. `fix-contextmenu.png` is the
same pane with the right-click menu open: the split stays engaged, where
before a context menu dropped the pane to the plain render.

`diff-workerA.png` / `diff-workerB.png` are two tabs in one Space, each its
own git worktree, each carrying a modification, an untracked create and a
delete. Focusing each shows exactly that worker's own three changes. Every
`+`/`-` line is inside the Changes zone; the terminal zone has none.

## How it was produced

`run-lab.sh <herdr-binary> <tag> <scratch-dir>` provisions an isolated named
session against the given binary, creates one workspace, declares its pane's
agent as `claude` (`herdr pane declare-agent`, so no real Claude Code process
is needed), then runs `paint-claude-v2.sh` in it and screenshots **navigate
mode first**. That ordering is the point: the older harness in
`data/herdr-triview-status-bar-20260822/` only ever shot `Mode::Terminal`,
which is why a split that drew nowhere else still looked correct.

`paint-claude-v2.sh` paints the v2.1.241 shape above — U+25CF bullets and
`⎿  $ ` echoes, a composer bounded by two plain rules, and the two footer
rows Claude pins to the screen floor. It paints twice, because herdr's marker
scan seeds itself on its first look at a pane and reports nothing from it.

`run-lab-diff.sh` builds a repo with two `git worktree` workers and drives
`herdr tab focus` between them. It lowers `diff_zone_width_threshold` in the
lab config, since the fixed three-zone layout otherwise wants 300 content
columns and the lab window is narrower.

Both drive an external lab helper (`$HERDR_LAB_HELPER`) for provisioning and
teardown, so neither is runnable as-is without one.

## Residual risk

The client here is a real `kitty` under `Xvfb`. The captain's own client, per
his live server log on the day this was written, is **local Rio 0.5.19**
(`is_local=true classified_kind=Rio`) — not the same terminal. Everything
this change touches is plain ratatui cell rendering, not the Kitty Graphics
pixel path where local-vs-remote clients have actually diverged before, so
the terminal should not matter here; but it was not verified against Rio
itself, or against a Windows remote client.
