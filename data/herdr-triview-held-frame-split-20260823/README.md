# A held frame must keep the triview split it drew

Live evidence for `fix: hold the claude triview's split while the pane's frame
is held`, captured against real release binaries in a real `kitty` under
`Xvfb`.

## The defect

`GhosttyPaneTerminal::render_claude_triview` blits its three zones — the
transcript, the composer body, and the agent's own footer — out of
`render_state`. A frame hold freezes `render_state`; it does not freeze the
live ghostty grid, and `claude_triview_layout` reads the live grid. So every
paint that landed inside a hold split frozen pixels at a *live* screen's
boundaries.

Two holds reach this, both routine:

* **A DEC 2026 synchronized update.** Claude Code repaints its whole screen
  inside one, so mid-batch the live grid is a torn screen — cleared, with only
  part of the repaint written. It resolves no Claude shape at all, so the split
  disengaged, the pane fell back to the plain full-pane render, and the
  transcript and composer jumped down by `transcript_skip` (8) rows for that
  frame and back on the next. Bounded by
  `SYNCHRONIZED_UPDATE_HOLD_TIMEOUT` (150 ms), so each instance is brief and
  constant while the agent is drawing.
* **An alternate-screen `pane read --source recent` harvest.** Bounded by
  `ALT_SCREEN_READ_HOLD_TIMEOUT` (30 s). It drives the pane's own app with real
  scroll input on purpose, so the live grid moves under a frozen frame by
  design.

A live grid that is still Claude-shaped but a *different* shape — a composer
that wrapped onto a second row — is the case that garbles rather than
disengages: the split moves, the pixels do not, and herdr's dividers land a row
away from the agent's own rules that are still on screen.

It also made a selection defect intermittent. A selection stores a grid row and
the highlight is drawn back through whatever split the frame resolved, so a
frame that dropped the split drew the same anchor eight rows away from the text
it was taken from, and back on the next frame. A fixture that never engages a
hold cannot see it — which is why PR #214's own repro could not reproduce the
captain's report.

## The rig

`lab/run-lab.sh <herdr-binary> <tag> <scratch-dir>` provisions an isolated
named lab session through the firstmate herdr-lab helper, paints a Claude Code
shaped screen into the focused pane until the triview's fixed eight-row
command-log zone engages, then has that pane open and close a DEC 2026 batch
about four times a second with a torn repaint inside each one. A second pane
prints a timestamp every 50 ms so the window keeps being recomposited while the
first pane's frame is held and asks for nothing.

It samples what the **terminal** is showing via `kitty @ get-text` — not what
herdr's own grid holds, since the whole defect is a disagreement between the
two — and counts how many samples caught the split somewhere other than where
it settled with nothing held.

The paint script puts the pane on the alternate screen with mouse reporting on,
because that is what Claude Code does and what the harvest gate requires.

## Result

| build | sha256 (first 12) | samples | split moved | log zone lost |
| --- | --- | --- | --- | --- |
| `fork/master` (aae7c649) | `567fb8d08dd8` | 40 | **16** | **16** |
| this branch | `1c09b7d5bc5c` | 40 | 0 | 0 |

Repeat run, same rig, fresh sessions and fresh displays
(`shots/repeat.txt`):

| build | samples | split moved | log zone lost |
| --- | --- | --- | --- |
| `fork/master` | 40 | **15** | **15** |
| this branch | 40 | 0 | 0 |

`shots/base-settled.txt` is the split with nothing held.
`shots/base-moved-sample.txt` is one of the sixteen baseline samples: the
command-log zone's `●` lines are gone, eight transcript rows are back at the
top, and the composer has moved down by exactly `transcript_skip` rows.
`shots/fixed-last-sample.txt` is the same moment on this branch.

## In-process coverage

* `pane::terminal::tests::a_synchronized_update_holds_the_triview_split_it_drew`
* `pane::terminal::tests::an_alt_screen_read_hold_holds_the_triview_split_it_drew`
* `pane::terminal::tests::a_held_triview_split_is_refused_once_the_pane_no_longer_fits_it`
* `ui::panes::tests::a_held_frame_keeps_the_selection_highlight_on_the_row_it_was_dragged_over`

The first two fail on `fork/master` and pass here.
