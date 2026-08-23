#!/usr/bin/env bash
# Paint the screen Claude Code v2.1.241 actually draws, captured live at
# 120x34 from the real CLI on 2026-08-22 (see README.md).
#
# Differs from data/herdr-triview-status-bar-20260822/paint-claude.sh in the
# two facts that fix depends on: the tool bullet is U+25CF, not U+23FA, and in
# the default non-verbose view the command itself appears only as a `⎿  $ `
# echo under a prose description line — there is no `Bash(...)` text at all.
#
# Two phases, for the same reason the older script has two: herdr's marker
# scan seeds itself on its first look at a pane and reports nothing from it.
R=$(tput lines)
C=$(tput cols)
rule=""
i=0
while [ "$i" -lt "$((C - 1))" ]; do rule="${rule}─"; i=$((i + 1)); done

render() { # <marker-count>
  markers=$1
  printf '\033[2J\033[H'
  # Claude pins its composer and footer to the screen floor; the transcript
  # floats above with blank filler between. Bottom five rows are fixed.
  body=$((R - 5))
  printf '❯ run the checks\n'
  used=1
  m=1
  while [ "$m" -le "$markers" ]; do
    printf '\n● Running check %02d\n' "$m"
    printf '  ⎿  $ cargo nextest run --lib zone_%02d\n' "$m"
    used=$((used + 3))
    m=$((m + 1))
  done
  printf '\n● All green.\n'
  used=$((used + 2))
  while [ "$used" -lt "$body" ]; do printf '\n'; used=$((used + 1)); done
  printf '%s\n' "$rule"
  printf '❯ \n'
  printf '%s\n' "$rule"
  printf '  ⠹ Sonnet 5 ⎇master            5%% ▁▁▁ $0.09 ❯   <<< STATUS BAR\n'
  printf '  ⏵⏵ accept edits on (shift+tab to cycle) · ← for agents'
}

render 0
sleep "${HERDR_LAB_MARKER_DELAY:-10}"
render "${HERDR_LAB_MARKERS:-3}"
exec sleep 100000
