#!/usr/bin/env bash
# Claude Code v2.1.241's real screen shape, then a typing loop that rewrites
# only the composer row - exactly what the captain is doing when he sees it.
R=$(tput lines); C=$(tput cols)
rule=""; i=0
while [ "$i" -lt "$C" ]; do rule="${rule}─"; i=$((i+1)); done

paint() { # <markers> <composer-text>
  printf '\033[2J\033[H'
  body=$((R-5))
  printf '❯ run the checks\n'; used=1
  m=1
  while [ "$m" -le "$1" ]; do
    printf '\n● Running check %02d\n' "$m"
    printf '  ⎿  $ cargo nextest run --lib zone_%02d\n' "$m"
    used=$((used+3)); m=$((m+1))
  done
  printf '\n● All green.\n'; used=$((used+2))
  while [ "$used" -lt "$body" ]; do printf '\n'; used=$((used+1)); done
  printf '%s\n' "$rule"
  printf '❯ %s\n' "$2"
  printf '%s\n' "$rule"
  printf '  ⠹ Sonnet 5 ⎇master            5%% ▁▁▁ $0.09 ❯\n'
  printf '  ⏵⏵ accept edits on (shift+tab to cycle) · ← for agents'
}

paint 0 ""
sleep "${HERDR_LAB_MARKER_DELAY:-10}"
paint "${HERDR_LAB_MARKERS:-3}" ""
sleep 6
# Now "type": rewrite ONLY the composer row, addressed absolutely, which is
# what a real agent's composer redraw does.
txt=""
for w in t th thi this i s " " a " " t e s t i n g " " n o w " " a " " b " " c " " d; do
  txt="${txt}${w}"
  printf '\033[%d;1H\033[K❯ %s' "$((R-3))" "$txt"
  sleep 0.25
done
exec sleep 100000
