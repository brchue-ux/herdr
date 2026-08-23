# Claude triview vs. the agent's own status bar

Live evidence for `fix: seat the claude triview's log zone above the agent's status bar`.

Reported as: *"my status bar is blocked by a black bar, and only appears when I
right click."*

## What to look at

`status-bar-band.png` is the same band of the same screen three times, stacked:

1. **`fork/master`, terminal mode** — the composer's row is drawn, and the two rows
   under it are blank. Those rows hold the pane's status line and its shortcuts hint.
2. **`fork/master`, right-click** — the pane context menu puts the app in
   `Mode::ContextMenu`, `should_attempt_claude_triview` answers `false`, and the pane
   falls back to a plain full-grid render. The status bar is back.
3. **This branch, terminal mode** — the command-log zone sits between the composer
   and the status line, and the status line and hint are on the pane's own floor
   below it. The rows the log occupies came off the top of the transcript.

`fix-terminal.png` is that third state whole: the transcript starts at line 04 rather
than 01, which is the three rows the three-command log was given.

`cap-terminal.png` is the same scenario with nine commands run. The zone stops at
eight rows and shows the newest eight; the transcript starts at line 09; the status
line is still last.

Lit pixels in the status-bar band (`convert -crop 1240x48+260+848 -colorspace Gray
-threshold 25%`):

| build | terminal mode | context menu |
|---|---|---|
| `fork/master` | 210 | 3113 |
| this branch, 3 commands | 3339 | 3113 |
| this branch, 9 commands | 3339 | — |

`herdr pane read --source visible` returns the status line in all cases. The content
was always in the pty; only herdr's render dropped it — that is the positive control
that this is a rendering bug and not a terminal-state one.

## How it was produced

`run-lab.sh <herdr-binary> <tag> <scratch-dir>` provisions an isolated named session
against the given binary, creates one workspace, declares its pane's agent as
`claude` (`herdr pane declare-agent`, so no real Claude Code process is needed), then
runs `paint-claude.sh` in it. `HERDR_LAB_MARKERS` sets how many commands the pane
reports.

`paint-claude.sh` paints a Claude Code-shaped screen sized to the pane: a transcript,
a composer bounded by two plain horizontal rules — the shape
`detect::prompt_box_body_line_range` recognizes — and then the two footer rows Claude
Code pins to the literal bottom of its screen, a `statusLine` and the shortcuts hint.
It `exec`s a long `sleep` so the screen stays put.

It paints **twice**: herdr's command-marker scan seeds itself on its first look at a
pane and deliberately reports nothing from it, so the `⏺ Bash(...)` lines have to
arrive after that first scan to reach the pane's command log.

The client is a real `kitty` under `Xvfb`, screenshotted with `import`; the
right-click is a real SGR mouse report sent through `kitty @ send-text`.

`run-lab.sh` drives an external lab helper (`$HERDR_LAB_HELPER`) for session
provisioning and teardown, so it is not runnable as-is without one; the herdr-side
recipe — `workspace create`, `pane declare-agent`, `pane run`, `pane read` — is the
reusable part.
