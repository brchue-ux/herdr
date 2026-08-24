#!/usr/bin/env bash
# Paints a Claude Code-shaped screen on the alternate screen, with mouse
# reporting on, in two phases so herdr's marker scan seeds itself before the
# `⏺ Bash(...)` lines that fill the pane's own command log arrive.
#
# Alternate screen + mouse reporting are not decoration: they are two of the
# five gates an alternate-screen `pane read --source recent` harvest requires,
# which is the hold this rig exercises. Claude Code satisfies both.
R=$(tput lines)
C=$(tput cols)

rule=""
i=0
while [ "$i" -lt "$((C - 2))" ]; do rule="${rule}─"; i=$((i + 1)); done

cleanup() { printf '\033[?1000l\033[?1006l\033[?1049l'; stty echo icanon 2>/dev/null; }
trap cleanup EXIT

# A real full-screen app consumes its own input; a shell echoes it, and the
# harvest's injected scroll would land on screen as literal SGR text.
stty -echo -icanon 2>/dev/null
printf '\033[?1049h\033[?1000h\033[?1006h'

render() { # <marker-count>
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

# A wrapped prompt: the same agent, the same screen, one more composer row.
# An ordinary thing for Claude Code to draw, and enough to resolve a different
# split - which is the case that garbles rather than disengages.
render_wrapped() {
  printf '\033[2J\033[H'
  total=$((R - 6))
  markers=${HERDR_LAB_MARKERS:-3}
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
  printf ' \xe2\x9d\xaf COMPOSER MARKER that has grown long enough to\n' 
  printf '   wrap onto a second composer row\n'
  printf ' %s\n' "$rule"
  printf '  ~/lab  main | Opus 4.5 | 42%% context left   <<< STATUS BAR\n'
  printf '  ? for shortcuts'
}

render 0
sleep "${HERDR_LAB_MARKER_DELAY:-8}"
render "${HERDR_LAB_MARKERS:-3}"
sleep "${HERDR_LAB_SETTLE:-6}"

# Phase 3, the one under test. Claude Code repaints its whole screen inside a
# DEC 2026 synchronized update, so for the length of every batch the live grid
# is torn - cleared, with only part of the repaint written - while herdr holds
# the frame it last drew. The batch here is kept just inside herdr's own
# SYNCHRONIZED_UPDATE_HOLD_TIMEOUT (150ms), so the hold is genuinely engaged
# rather than timed out, and the pane spends about half its time in one.
while true; do
  printf '\033[?2026h\033[2J\033[H\xe2\x9d\xaf partial repaint in progress'
  sleep 0.12
  render "${HERDR_LAB_MARKERS:-3}"
  printf '\033[?2026l'
  sleep 0.12
done
