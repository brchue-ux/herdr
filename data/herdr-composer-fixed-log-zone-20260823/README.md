# Fixed 8-row command-log zone, and the selection-offset bug it replaces

Live evidence for two changes to the same feature (`feat(terminal): split Claude
Code panes into transcript, composer, and command-log zones`, #173, and its two
follow-ups #209 and #210):

1. A bug fix: mouse selection/copy in a Claude triview pane read the wrong row
   whenever the command-log zone had shifted the transcript and composer up.
2. A layout change: the log zone is now a fixed 8 rows always, never sized to
   how many commands it holds, so the composer's own row can never move again
   — the root-cause class of bug behind both this and #209's cursor fix.

Both verified live: a real `kitty` under `Xvfb`, isolated named session,
binaries hash-verified, real SGR mouse press/drag/release and wheel events
sent through `kitty @ send-text`. `lab/run-lab.sh` and
`lab/verify-fixed-zone.sh` are the reusable rigs; `lab/paint-claude-repro.sh`
paints the synthetic Claude-shaped screens (labeled transcript lines, so a
selection's own copied text proves which row it actually grabbed).

## The bug, reproduced and root-caused

`ClaudeTriviewLayout::transcript_skip` rows are cropped off the top of the
transcript to make room for the log zone, and everything downstream that
draws from the live grid was taught to project through that shift by #209 —
except mouse selection. `Selection::anchor`/`Selection::drag`
(`src/selection.rs`, called from `src/app/input/mouse.rs` and
`src/app/input/selection.rs`) took the pane row the mouse was actually over
and treated it as a live-grid row directly, with no projection at all.

`baseline-idle2-terminal.png` is a 3-command triview pane (`fork/master`
`ff9918e6`, before this fix): the displayed transcript starts at "transcript
line 04" because 3 rows shifted off the top. Dragging over pane row 18 (which
reads "transcript line 22" on screen — see the same screenshot) and reading
the clipboard back (`baseline-idle2-clipboard-row19.txt`) gives:

```
transcript line 19 — her
```

Off by exactly 3 — `transcript_skip` — in the direction of reading the
identity-mapped (unshifted) row instead of the one actually drawn there.
`fixed-idle3-terminal.png` is the identical repro against this branch:
dragging the same on-screen row now copies (`fixed-idle3-clipboard-row19.txt`):

```
transcript line 27 — her
```

— the row actually on screen (this run used a taller pane and 3 recorded
commands, transcript_skip is now fixed at 8, hence line 27 rather than 22;
`src/app/input/mouse.rs`'s own in-process tests
(`mouse_selection_lands_on_the_row_the_triview_split_actually_drew`,
`mouse_selection_on_the_composer_row_lands_on_the_composer`) pin the same fact
without a lab).

The fix: `ClaudeTriviewLayout::grid_row_for_pane_row` (`src/pane/terminal.rs`),
the inverse of the existing `pane_row_for_grid_row` #209 already used for the
cursor and hyperlinks. Applied at every point a screen row turns into a
`Selection` coordinate — `src/app/input/mouse.rs`'s mouse-down handler,
`Selection::drag`'s new `triview` parameter, and `render_selection_highlight`
in `src/ui/panes.rs`, which used to just skip drawing the highlight entirely
whenever the split had shifted (`rows_shifted` guard) rather than projecting
it — a defensible stopgap when the shift was sometimes zero, but with the
shift now *always* present once triview engages, that guard would have
silently disabled selection highlighting on every Claude pane.

## The layout change: log zone fixed at 8 rows, never sized to content

`ClaudeTriviewLayout::transcript_skip`/`log_rows` are now the constant
`CLAUDE_TRIVIEW_LOG_ROWS` (8) whenever the split engages at all, replacing the
old `requested_log_rows.min(...)` — never 0, never partial, never bigger.
`base-m0-terminal.png`/`base-m12-terminal.png` (`composer-line.txt` alongside
each) show the *old* behavior: on `fork/master`, the composer's line number in
the rendered frame is `38` at 0 recorded commands and needs re-measuring at
every different command count because `transcript_skip` scaled with it.
`fix-m0-terminal.png`/`fix-m12-terminal.png` (this branch) show line `38` at
*both* 0 and 12 commands — the composer never moves, whether the zone is
empty (8 blank rows, still reserved) or overflowing.

Retention was raised from 8 to `PANE_COMMAND_LOG_MAX = 500`
(`src/app/pane_command_log.rs`) so there is real history to scroll back
into — the zone's display height and the log's own retention are no longer
the same number.

## Growth direction reversed, and internal scroll added

The captain's clarification (`addendum-live-log-above-composer-20260823.md`):
new commands spawn at the zone's own **top** row, pushing earlier ones down —
opposite of the transcript above it. Compare the log zone in
`base-m12-terminal.png` (`zone_05` through `zone_12`, oldest-to-newest reading
top-to-bottom — the old, bottom-anchored-newest behavior) against
`fix-m0-terminal.png`... no — against `fix-m12c-before-scroll.png` and
`base-m12-terminal.png` directly: the fixed build shows `zone_12` down to
`zone_05`, newest at the top.

Scrolling past 8 reveals older ones rather than only ever showing the most
recent 8: `fix-m12c-before-scroll.png` (zone_12..zone_05, unscrolled) versus
`fix-m12c-after-scroll.png` (zone_08..zone_01, scrolled to the oldest 8 and
clamped there) after three wheel notches over the log zone. Reuses the
existing wheel-routing mechanism (`src/app/input/mouse.rs`'s
`handle_terminal_wheel`) rather than inventing a new one: a wheel event whose
row falls inside the log zone now adjusts a new
`AppState::pane_command_log_scroll` entry instead of the pane's own PTY
scrollback, so reviewing history never disengages the triview split the way
scrolling the pane itself does. `wheel_over_the_log_zone_scrolls_it_without_touching_pane_scrollback`
and `wheel_over_the_transcript_still_scrolls_the_pane`
(`src/app/input/mouse.rs`) cover both sides of that split in-process.

## Whether this shares a root cause with #209's cursor bug

Same category (a drawn position and a separately-tracked logical position
disagreeing), same underlying `ClaudeTriviewLayout::transcript_skip`
projection — but a **different, previously-unfixed code path**. #209 fixed
`tab_surface_cursor`, `visible_hyperlinks`, and the retained dirty-patch
guard; it did not touch `Selection`, `src/app/input/mouse.rs`, or
`src/app/input/selection.rs` at all (verified: `git show ab0a7b0f --stat`
touches none of `src/selection.rs`/`src/app/input/mouse.rs`). So: same root
cause *class*, but the selection bug was never actually fixed by #209 and
needed its own projection applied at its own call sites.

## A separate finding: the addendum's "live block above the composer"

The addendum (`addendum-live-log-above-composer-20260823.md`) asked whether a
real Claude Code pane's live, in-progress tool-call detail rendering above
the composer during an active turn shares a cause with the selection bug, or
is a routing leak where content fails to reach the log zone. Investigated
against a **real** Claude Code v2.1.241 session (`claude` is installed on
this box) driving a genuine multi-step shell command, captured via `tmux
capture-pane`, not a guess from the one screenshot the captain attached:

- The composer's own two rule lines stay present and are recognized
  correctly by `claude_triview_layout` throughout the busy/thinking state —
  confirmed with a direct unit-level check of
  `prompt_box_body_line_range`/`transcript_line_range` against both a
  (wrong) borderless-busy-state guess and the real captured busy-state text;
  only the real captured text resolves a composer.
- `command_marker::shell_echo_regex` matches the live, still-expanding
  `⎿  $ ...` echo line the same as any folded one, so the command **is**
  recorded into `PaneCommandLog` while still live — reproduced with the
  synthetic `busy` paint mode added to `paint-claude-repro.sh`
  (`baseline-busy-terminal.png`): the log zone below the composer correctly
  shows `for i in 1 2 3 4 5 6; do echo step $i...` at the same time the
  transcript above shows the same command still expanded.

So: **not a routing bug**. The log zone is not failing to receive live
commands, and the split's detection does not break during a busy turn. What
the captain is seeing is two *intentionally* redundant views of the same
information — Claude's own live transcript (which naturally still shows what
it just ran) and herdr's persistent summary below the composer (which by
design outlives the transcript scrolling past it, per #173's own commit
message). Making the transcript stop showing content once it is mirrored in
the log zone would mean selectively suppressing live PTY-drawn content based
on marker detection — a materially larger, different-shaped feature (content
filtering keyed to a fragile, version-sensitive detector) than either this
bug fix or the layout change, and not something to guess at without the
live-agent evidence-gathering process this project's `CLAUDE.md` requires for
agent-detection changes. Left out of this PR; called out here as a distinct,
scoped-out finding rather than folded silently into "fixed."
