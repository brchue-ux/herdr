# Right-click pastes; `shift`+right-click opens Herdr's menu

Live evidence for `feat(input): paste on right-click and move herdr's context
menu to shift+right-click`.

Reported as: *copy-paste is broken — right-click, the conventional terminal
paste gesture, does not paste.*

## What was actually wrong

Not a regression. Three things compose into it, and all three predate every
recent change:

1. **`ui.mouse_capture` is on by default**, so the host terminal forwards a
   right-click to Herdr instead of acting on it. The terminal's own paste
   action never runs.
2. **Herdr claimed the bare right-click for its own pane context menu**, in
   `3397e1ce` (2026-04-21) — months before the triview (`a4296f10`,
   2026-08-21), the sidebar/card work (`dcd68a62`, 2026-08-23) or the
   kitty-graphics negotiation fix (`5196368f`, 2026-08-22). None of those
   touch right-click routing or the paste path.
3. **That menu has no paste item, and Herdr had no mouse paste gesture at
   all.** `AppState::handle_mouse` never reached a clipboard read on any
   button. The only mouse paste that ever worked was the *outer terminal's*
   `shift`+mouse bypass, which the Windows beta doc named as the paste route —
   a property of the terminal, not of Herdr.

## What the rig proves

`run-lab.sh` drives an isolated `fm-herdr-lab` session whose server is this
branch's release build, with a real `herdr` client inside a real `kitty` under
`Xvfb`, and clicks it with **real X11 button events** (`xdotool`, verified with
`xev` to carry `state 0x1` for the shift chord). Assertions read the terminal's
own screen (`kitty @ get-text`) and the server's own view of the pane
(`herdr pane read`), so neither half is asserted from the other's word.

| case | gesture | expected | `kitty-default` | `kitty-forwards-shift` |
| --- | --- | --- | --- | --- |
| 1 | right-click | pastes, no menu | pass | pass |
| 2 | `shift`+right-click | menu, no paste | **no menu** | pass |
| 3 | `ctrl`+right-click | menu | pass | pass |

The shot after `shift`+right-click on stock kitty is byte-identical to the one
before it (`default-02-after-right-click.png`, md5 `3aa518f5…`), which is the
whole claim: nothing reached Herdr, so it is not kept twice. The shot after
`shift`+right-click on the forwarding pass is byte-identical to the `ctrl` one
(md5 `9ea233e2…`) — the same menu, in the same place, from either chord.

Case 1 pastes the real X CLIPBOARD selection (owned by a real `xclip`) into a
real shell pane: `shots/default-02-pane-read.txt` shows
`herdr-paste-probe-9f3a2b` on the prompt line, wrapped across the pane's own
width — which is why the assertion joins rows before matching, and why a naive
`grep` reports a false negative on a passing run.

## The finding worth carrying forward

**Stock kitty never forwards `shift`+right-click to any application.** Its own
default is

```
mouse_map shift+right press ungrabbed,grabbed mouse_selection extend
```

(`kitty/options/definition.py`, `extend_selection_grabbed`) — bound in
`grabbed` mode too, so an application that has captured the mouse never sees
the chord. That is the same reason `ui.right_click_passthrough_modifier`
rejects `shift`.

So the remap deliberately puts the menu on **any modified right-click**, not on
`shift` alone: `shift` is the documented chord, and `ctrl`/`alt` are the live
fallback in terminals that keep `shift`+mouse for themselves. Case 3 is that
fallback, measured on stock kitty.

`KITTY_FORWARD_SHIFT=1` re-runs the rig with

```
mouse_map shift+right press grabbed no_op
```

which *removes* kitty's grabbed trigger (`kitty/config.py` pops a definition
that parses empty) rather than remapping it, so the chord reaches Herdr. That
pass is what proves Herdr's own `shift` half is correct rather than merely
untested.

## Running it

```bash
cargo build --release
PASS_LABEL=kitty-default                        bash data/herdr-rightclick-paste-remap-20260823/run-lab.sh
PASS_LABEL=kitty-forwards-shift KITTY_FORWARD_SHIFT=1 \
                                                bash data/herdr-rightclick-paste-remap-20260823/run-lab.sh
```

Needs `Xvfb`, `kitty`, `import`/`convert`, `jq`, and local copies of `xclip`
and `xdotool` under the scratch paths the script names (this box has neither
installed; `apt-get download` + `dpkg-deb -x` is enough, no root). The session
lifecycle goes through `fm-herdr-lab.sh` end to end — provision, guarded
teardown, fleet-state tripwire — and the script fails loudly if teardown does
not verify.
