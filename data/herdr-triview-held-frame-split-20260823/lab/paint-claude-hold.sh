#!/usr/bin/env bash
# Paints a Claude Code-shaped screen in three phases, the third of which is the
# one under test: an *open* DEC 2026 synchronized update with a torn repaint
# inside it.
#
# Phase 1  a plain transcript, so herdr's marker scan seeds itself.
# Phase 2  the same transcript with `⏺ Bash(...)` markers, so the pane's own
#          command log fills and the triview's fixed eight-row zone engages.
# Phase 3  `\033[?2026h`, clear, and one line of a repaint — and then nothing.
#          The batch is never closed, so herdr holds this pane's frame for as
#          long as the run lasts, which is what the fix has to survive.
#
# The batch is opened only once the trigger file appears, so the harness can
# capture what the terminal shows on either side of it.
R=$(tput lines)
C=$(tput cols)
TRIGGER=${HERDR_LAB_TRIGGER:?trigger path}

rule=""
i=0
while [ "$i" -lt "$((C - 2))" ]; do rule="${rule}─"; i=$((i + 1)); done

render_idle() { # <marker-count>
  markers=$1
  printf '\033[2J\033[H'
  total=$((R - 5))
  plain=$((total - markers))
  i=1
  while [ "$i" -le "$plain" ]; do
    printf '  transcript line %02d \xe2\x80\x94 held-frame repro\n' "$i"
    i=$((i + 1))
  done
  m=1
  while [ "$m" -le "$markers" ]; do
    printf '\xe2\x8f\xba Bash(cargo nextest run --lib zone_%02d)\n' "$m"
    m=$((m + 1))
  done
  printf ' %s\n' "$rule"
  printf ' \xe2\x9d\xaf COMPOSER MARKER\n'
  printf ' %s\n' "$rule"
  printf '  ~/lab  main | Opus 4.5 | 42%% context left   <<< STATUS BAR\n'
  printf '  ? for shortcuts'
}

render_idle 0
sleep "${HERDR_LAB_MARKER_DELAY:-10}"
render_idle "${HERDR_LAB_MARKERS:-3}"

while [ ! -e "$TRIGGER" ]; do sleep 0.5; done

# The agent opens a synchronized update and starts repainting its whole screen.
# Mid-batch the grid is torn: cleared, with only the first row rewritten. It
# resolves no Claude shape at all, which is exactly what a split recomputed
# from the live grid would be handed.
printf '\033[?2026h\033[2J\033[H\xe2\x9d\xaf partial repaint in progress'
exec sleep 100000
