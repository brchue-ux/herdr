# The failure spider

A persistent, red, pulsing marker that climbs a failing card's own trunk/branch to rest at its
top-centre border, and stays there until the card clears. See the `ElementId::FailureSpider` doc
comment in `src/anim.rs`, `render_failure_spiders`/`failure_spider_position` in `src/ui/sidebar.rs`,
and `App::failing_card_rows`/`failure_spider_lifecycle` in `src/app/runtime.rs`.

## How it climbs

This builds directly on `herdr-tree-line-wires`'s addressable ancestor-rail segments
(`anim::ElementId::TrunkSegment`), whose own doc comment names the spider as the reason a segment's
gap is addressable at all — "the addressing a future signal or the failure spider needs to sit at
one gap rather than 'somewhere on this column.'" `TrunkSegment` was left with no way to express a
sub-position within itself (`TrunkRailPaint::cell` asks the engine at a fixed `1×1` extent on
purpose, see that struct's doc comment), so a marker that actually travels needed its own
continuous `progress`. The failure spider is that: a new `ElementId::FailureSpider(CardRow)`, its
own `Family`, with a `Lifecycle` of `mount` (the climb, `Curve::SnapArrival`) → `idle` (the resting
pulse) → `dismount` (the retreat, the climb reversed by the engine's own "leaving is arriving played
backwards" rule) — the same three-phase shape `CardWash`/`TrunkSegment` already use, generalised
with a dismount (see `Lifecycle::with_dismount`, previously dead code outside tests, now load-bearing
here).

The path itself (`failure_spider_position` in `src/ui/sidebar.rs`) is four waypoints, each leg a
single cell-grid axis move, never a diagonal, because that is how the tree's own lines are drawn:
up the row's trunk column to its own branch, along the branch to the card's left border (which
CLAUDE.md's own bullet records shares the same column as the connector rail), up that border to the
top, then across the top border to centre. It needs no ancestor topology to be well-defined — the
trunk column (`card.rect.x`) exists for every card regardless of nesting depth — which is why the
mechanism works identically on a top-level mate and a deeply nested worker.

## Why it's gated the way it is, not the way `row_enter`/`row_exit` are

The failure spider is a core failure signal, not a decorative animation toggle — the captain's spec
says it has to persist "until the failure clears," which means it has to mount and dismount on an
otherwise completely unconfigured Herdr, unlike `TrunkSegment`/row motion which only exist once
`[ui.sidebar.animation]` is set. `App::advance_animations` reads `failing_card_rows` eagerly, ahead
of its own cheap "nothing to animate, forget everything" exit, and `Animator::has_any` (new) is what
keeps that exit from firing while a spider is still mid-retreat after its card has already cleared —
the same problem the tree view switch's own comment names for a singleton, generalised here to a
membership set. `app::runtime::tests::a_failing_card_mounts_a_spider_with_nothing_else_configured`
pins this at the state-machine level.

## Character shell only, for now

A pixel card's sheet is opaque and drawn over the same cells the spider would rest on
(`image_card::shape_covers_row`), so `render_failure_spiders` returns early under that path rather
than draw something invisible. Rasterising the spider into `image_card::build_cards` so it survives
the pixel path is a named follow-up, not built here — the same "two renderers, one row model" split
CLAUDE.md already documents for everything else in the tree.

## `herdr-pane-signal-carrier` overlap

Track D also names a not-yet-built "a pane carries a signal along a connector" item. The failure
spider's own travelling-marker shape — a dedicated `ElementId` with its own continuous `progress`,
read against externally-computed waypoint geometry rather than against `TrunkSegment`'s fixed `1×1`
extent — is very likely the right shared primitive for that item too, once it exists: the same
"walk waypoints derived from `WorkspaceCardArea`/the tree's own layout, using an element's bounded
mount `progress` as the position parameter" shape should serve a general signal carrier without
change. Nothing here blocks on that work; it isn't built yet. If it lands, it should reuse
`failure_spider_position`'s waypoint-and-lerp approach (generalised past four fixed legs) rather than
grow a second one.

## The captures

Real, headless `herdr-dev` server driven over its socket API by a real client attached through a
sized PTY, at `sidebar_width = 42` — the captain's own persisted width, read directly out of the
real `~/.config/herdr/session.json` on this machine, and the same value every other card/tree proof
in `data/` already uses. `proof/decode_frame.py` is `herdr-tree-line-wires`'s own VT100 grid
emulator, copied here with two fixes documented in its own header — neither is a Herdr bug, see
below.

| file | what it shows |
|---|---|
| `proof/0-before.txt` | a healthy fleet: `firstmate`, `2ndmate`, and `2ndmate`'s `worker-1` (`working`), no spider |
| `proof/1-mount-early.txt` | ~180ms after `worker-1` publishes `lifecycle=failed`: the spider low on the trunk, mid-climb |
| `proof/2-mount-late.txt` | further into the 650ms climb, now on the card's own top border, moving right |
| `proof/3-idle-a.txt` | past the climb: resting dead-centre on the top border (column 20 of a 27-wide card, `left + (right-left)/2` exactly) |
| `proof/4-idle-b.txt` | ~400ms later, same position — confirms it *rests*, not a mount that never finished — with a different pulsed colour than `3-idle-a` at the same spot (`221;163;191` vs `213;188;217`, read straight off the raw SGR bytes) |
| `proof/5-dismount-early.txt` | ~180ms after `worker-1`'s `lifecycle` token is cleared: retreating left along the top border |
| `proof/6-dismount-late.txt` | further back down, near where the climb started |
| `proof/7-gone.txt` | the retreat has finished; the spider is gone, matching `Animator::frame` returning `None` |

## Two things this live render actually caught, that no unit test did

**The glyph needed `U+FE0F` (emoji presentation selector).** Without it, `ratatui::buffer::Buffer::diff`
does not emit an explicit clear for a wide grapheme's trailing cell — that behaviour is
`ratatui`'s own documented workaround for terminals that do not reliably clear a wide emoji's
trailing cell otherwise, gated specifically on the grapheme containing `U+FE0F`. See the
`FAILURE_SPIDER_GLYPH` doc comment in `src/ui/sidebar.rs`.

**`Severity::Critical` was the wrong severity to resolve the ink at.** `Severity`'s light-reach ramp
moves an ink *toward the light bound* as severity escalates — the same direction a card's own alert
breath moves, because worse trouble is meant to read as a brighter, more forward light — so
`Critical` on this machine's dark panel washed the spider to a pale rose (`227;202;223`, close to
white) rather than reading as red. `Severity::Serious` stays close enough to the panel to read as an
actual red (`221;163;191`); see the `render_failure_spiders` comment where the ink is resolved.

Both were found by decoding the real captured bytes, not assumed from the code — the second one
specifically by comparing the SGR sequence immediately preceding the spider glyph across two
captures at severities before and after the change.

## Reproducing

```bash
mkdir -p /tmp/hspider/home/.config/herdr-dev
cat > /tmp/hspider/home/.config/herdr-dev/config.toml <<'EOF'
[experimental]
allow_nested = true
[ui]
sidebar_width = 42
EOF

E="env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH HOME=/tmp/hspider/home XDG_CONFIG_HOME=/tmp/hspider/home/.config ./target/debug/herdr"
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH HOME=/tmp/hspider/home XDG_CONFIG_HOME=/tmp/hspider/home/.config \
  ./target/debug/herdr server &

$E workspace create --label firstmate --cwd /tmp
$E workspace create --label 2ndmate --cwd /tmp
$E workspace report-metadata w2 --source proof --token owner=firstmate
$E pane split w2:p1 --direction down
$E pane report-agent w2:p2 --source proof --agent worker-1 --state working
$E pane report-metadata w2:p2 --source proof --token owner=2ndmate

# attach a client through a 90x32 PTY, then:
$E pane report-metadata w2:p2 --source proof --token lifecycle=failed   # mount
# ...wait past 650ms for it to settle, then:
$E pane report-metadata w2:p2 --source proof --clear-token lifecycle   # dismount

# proof/decode_frame.py <cols> <rows> <raw-capture-file> decodes a captured
# raw ANSI stream back into the text rows above.
```
