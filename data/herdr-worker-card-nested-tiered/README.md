# Workers inside their Space's card — live captures

Every image here is a screenshot of a real `kitty` 0.45.0 under `Xvfb`, attached
to an isolated Herdr lab session running a release build of the branch. Nothing
is a mockup or a composite.

The lab fleet is one Space, `herdr`, running three things: a worker it dispatched
itself (`fm/verve-notes`), a second mate that is a *pane* in the same Space
(`fm/skill-direction`), and that mate's own worker (`fm/solarsystem`). The
`owner` metadata token is the only thing that says so, which is what the tiering
is read from.

| file | what it shows |
| --- | --- |
| `01-both-tiers.png` | The list inside the card: header, dashed rule, three worker rows. `fm/solarsystem` came through a second mate, so it is one fixed step in with a dimmer rail and an unlit dot. |
| `02-spawn-push-then-bloom.png` | A worker arriving, left to right: before; its track fully open with **no ink in it**; settled. The two beats never overlap. |
| `03-despawn-fade-then-close.png` | The same worker leaving: present; **content gone with the slot still open**; the gap closed. The reverse of the arrival, in that order. |
| `04-remote-delegated.png` | The same card drawn by the **client**, over the `--remote` bridge with `HERDR_CLIENT_RASTERIZED_*` on — the route a Windows remote client takes. The list crosses the wire on `CardContentWire::crew`; the bands are resolved at both ends from the same face and cell. |
| `05-dot-halo-before.png` / `06-dot-halo-after.png` | The defect this capture found and its fix, at native pixels: a Gaussian sampled only as far as it is bright is a square, and a boundary pixel that took the disc's fill and skipped the glow left a dark ring. |

## Reproducing

`--remote` is exercised without a second machine per the root `CLAUDE.md`
section *"Exercising the `--remote` client path without a second machine"*: a
unix socket relayed to `herdr remote-client-bridge`'s stdio, with the client
launched under `HERDR_CLIENT_SOCKET_PATH`, `HERDR_RENDER_ENCODING=terminal-ansi`,
`HERDR_REMOTE_KEYBINDINGS` and the three `HERDR_CLIENT_RASTERIZED_*` overrides.

**One rig trap worth writing down.** With a server-rasterising client *and* a
delegating client attached at once, the delegated one draws its own cards over a
character grid the server rendered for the other client — `shape_covers_row`
reads shared `AppState`. The result looks exactly like a card geometry bug and is
not one. Attach one client at a time.
