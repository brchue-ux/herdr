#!/usr/bin/env bash
# Paint a Claude Code-shaped screen for the composer/log-zone repro:
# transcript lines labeled by number (so a selection's own text proves which
# row it actually grabbed), a composer bounded by two plain horizontal rules,
# then the two footer rows Claude Code pins to the screen floor. Three
# phases so the pane's command log fills after herdr's marker scan seeds
# itself on the first look (same two-phase trick as
# data/herdr-triview-status-bar-20260822/paint-claude.sh).
R=$(tput lines)
C=$(tput cols)
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
    printf '  transcript line %02d \xe2\x80\x94 herdr triview repro\n' "$i"
    i=$((i + 1))
  done
  m=1
  while [ "$m" -le "$markers" ]; do
    printf '\xe2\x8f\xba Bash(cargo nextest run --lib zone_%02d)\n' "$m"
    m=$((m + 1))
  done
  printf ' %s\n' "$rule"
  printf ' \xe2\x9d\xaf try the fix\n'
  printf ' %s\n' "$rule"
  printf '  ~/lab  main | Opus 4.5 | 42%% context left   <<< STATUS BAR\n'
  printf '  ? for shortcuts'
}

# The real busy/"thinking" shape, captured live from a real Claude Code
# v2.1.241 session on this box (tmux, 70x20) while it ran a multi-step shell
# script: the composer's own two rules stay put and empty, and the
# live/expanding tool-call detail (a multi-line `⎿  $ ...` echo plus a
# "Kneading… (Ns · …)" progress line) sits directly above them, in the
# transcript. Recognized markers: the multi-line command's *first* physical
# line matches `shell_echo_regex` (`⎿  $ `), same as any other collapsed
# marker.
render_busy() {
  printf '\033[2J\033[H'
  total=$((R - 5))
  plain=$((total - 8))
  i=1
  while [ "$i" -le "$plain" ]; do
    printf '  transcript line %02d \xe2\x80\x94 herdr triview repro\n' "$i"
    i=$((i + 1))
  done
  printf '\xe2\x97\x8f Running the script now.\n'
  printf '\n'
  printf '\xe2\x97\x8f Running multi-step loop, midpoint check, whoami, pwd with sleeps \xc2\xb7 11s\n'
  printf '  \xe2\x8e\xbf  $ for i in 1 2 3 4 5 6; do echo step $i; sleep 1.5; done; echo\n'
  printf '     midpoint; ps -eo pid,cmd | head -5; sleep 1.5; whoami; sleep 1.5;\n'
  printf '     pwd (9s \xc2\xb7 7 lines)\n'
  printf '     (ctrl+b ctrl+b (twice) to run in background)\n'
  printf '\n'
  printf '\xe2\x9c\xb6 Kneading\xe2\x80\xa6 (14s \xc2\xb7 \xe2\x86\x93 211 tokens)\n'
  printf '  \xe2\x8e\xbf  Tip: You can control how big a workflow is just by prompting.\n'
  printf ' %s\n' "$rule"
  printf ' \xe2\x9d\xaf \n'
  printf ' %s\n' "$rule"
  printf '  \xe2\xa0\x8b Sonnet 5            5%% \xe2\x96\x81\xe2\x96\x81\xe2\x96\x81\xe2\x96\x81\xe2\x96\x81\xe2\x96\x81\xe2\x96\x81\xe2\x96\x81 $0.10\n'
  printf '  \xe2\x8f\xb5\xe2\x8f\xb5 auto mode on (shift+tab to cycle) \xc2\xb7 \xe2\x86\x90 for agents'
}

case "${1:-idle}" in
  idle)
    render_idle 0
    sleep "${HERDR_LAB_MARKER_DELAY:-10}"
    render_idle "${HERDR_LAB_MARKERS:-3}"
    ;;
  busy)
    render_idle 0
    sleep "${HERDR_LAB_MARKER_DELAY:-10}"
    render_busy
    ;;
esac
exec sleep 100000
