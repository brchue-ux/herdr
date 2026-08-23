#!/usr/bin/env bash
# Paint a Claude Code-shaped screen: transcript, composer box with plain
# horizontal rules, then the two footer rows Claude pins to the screen floor -
# a custom statusLine and the shortcut hint.
#
# Two phases on purpose. herdr's command-marker scan seeds itself on its first
# look at a pane and reports nothing from it (markers already on screen are
# history, not a command that just ran), so the markers have to arrive after
# that first scan to reach the pane's command log.
R=$(tput lines)
C=$(tput cols)
rule=""
i=0
while [ "$i" -lt "$((C - 2))" ]; do rule="${rule}─"; i=$((i + 1)); done

render() { # <marker-count>
  markers=$1
  printf '\033[2J\033[H'
  total=$((R - 5))
  plain=$((total - markers))
  i=1
  while [ "$i" -le "$plain" ]; do
    printf '  transcript line %02d — herdr triview repro\n' "$i"
    i=$((i + 1))
  done
  m=1
  while [ "$m" -le "$markers" ]; do
    printf '⏺ Bash(cargo nextest run --lib zone_%02d)\n' "$m"
    m=$((m + 1))
  done
  printf ' %s\n' "$rule"
  printf ' ❯ try the fix\n'
  printf ' %s\n' "$rule"
  printf '  ~/lab  main | Opus 4.5 | 42%% context left   <<< STATUS BAR\n'
  printf '  ? for shortcuts'
}

render 0
sleep "${HERDR_LAB_MARKER_DELAY:-10}"
render "${HERDR_LAB_MARKERS:-3}"
exec sleep 100000
