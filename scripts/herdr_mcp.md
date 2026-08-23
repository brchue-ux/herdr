# Herdr MCP connector

`scripts/herdr_mcp.py` lets an assistant ask a running Herdr what is actually
happening — which workspaces, tabs and panes exist, which agents are working or
idle, what each pane's cwd and git worktree are — instead of inferring it from a
description or a stale screenshot.

It is an [MCP](https://modelcontextprotocol.io) stdio server on one side and a
client of Herdr's JSON API socket on the other.

## Why it is a script and not part of `herdr`

Nothing here links against the crate and no new server capability is involved.
It speaks the published newline-delimited JSON protocol to whichever `herdr` is
already running, which has two consequences worth keeping:

* Installing or updating the connector never requires building or replacing a
  `herdr` binary.
* It keeps working across Herdr versions. Field additions flow through
  untouched, and `herdr_query` reaches any read-only method by name, so a new
  API method is usable the day it ships without editing this file.

## Install

Nothing to install — Python 3 standard library only. The one exception is
`herdr_pane_screenshot`, which shells out to ImageMagick (`import`/`convert`);
every other tool has no dependency beyond Python itself.

Claude Code:

```bash
claude mcp add herdr -- python3 /path/to/herdr/scripts/herdr_mcp.py
```

Or, in an MCP client config file:

```json
{
  "mcpServers": {
    "herdr": {
      "command": "python3",
      "args": ["/path/to/herdr/scripts/herdr_mcp.py"]
    }
  }
}
```

Append `--session <name>` to the args to pin one session as the default; every
tool still takes a `session` argument that overrides it.

Check it against a live session without an MCP client at all:

```bash
python3 scripts/herdr_mcp.py --selftest              # default session
python3 scripts/herdr_mcp.py --selftest --session my-lab
python3 scripts/herdr_mcp.py --policy                # what it may and may not send
```

## Tools

| Tool | What it answers |
| --- | --- |
| `herdr_sessions` | Which Herdr sessions exist on this machine and which are running. |
| `herdr_overview` | The whole session as a workspace → tab → pane tree: labels, agent status, focus, cwd, worktree, ownership, metadata tokens. One call, and usually the only one needed. |
| `herdr_pane` | One pane in full, including shell pid, tty and foreground processes. |
| `herdr_pane_read` | The text currently on a pane's screen. |
| `herdr_pane_screenshot` | A real PNG of what Herdr is actually drawing: one pane cropped, or the whole window. |
| `herdr_query` | Any read-only API method by name, raw result. The escape hatch when a shaped tool does not carry the field you need. |

`herdr_overview` output looks like this:

```text
herdr 0.8.0 (protocol 24) - 2 workspace(s), 2 tab(s), 3 pane(s)

* workspace 1 'connector-lab' <w1> [focused]
    linked worktree: ~/src/herdr-wt/connector (herdr)
    tokens: lane=connector
  tab 1 '1' <w1:t1> [focused] - 2 pane(s)
    * pane <w1:p1> 'connector build' agent=claude status=working focused
        cwd=~/src/herdr-wt/connector  owner=w1  relation=worker  held_for=95s
        tokens: owner=w1, summary=wiring the MCP connector
    . pane <w1:p2> 'bchue@host: ~/src/herdr-wt/connector' agent=codex status=idle
```

The leading mark is the agent status: `*` working, `!` blocked, `+` done,
`.` idle, `?` unknown.

## Read-only, and how that is enforced

Every request is checked against `READ_ONLY_METHODS` in `herdr_mcp.py` before a
byte reaches the socket. A method outside that set is refused locally — naming
it explicitly through `herdr_query` does not get it sent.

Two read-shaped methods are deliberately excluded, and they are the interesting
ones:

* `pane.read` with `source=recent` or `recent_unwrapped` (`format=text`)
* `agent.read` with the same sources

Both hand the request to the alt-screen harvest in
`src/server/alt_screen_read.rs`, which scrolls the pane's own full-screen
application by injecting real mouse wheel events for up to 15 seconds, then
scrolls it back. It holds the presented frame while it runs, so the pane looks
frozen rather than visibly moving — but it is synthetic input delivered to a
live process, which is a mutation, not a read.

`herdr_pane_read` therefore offers `visible` (default), `detection` and
`transcript`, none of which enter that path. `pane.read` is not reachable from
`herdr_query` at all, so its `source` can only ever come from that constrained
list.

Session liveness in `herdr_sessions` is probed with a connect-and-close, which
is exactly what `herdr session list` itself does
(`src/session.rs::is_running_at`). Nothing is written to that socket.

## `herdr_pane_screenshot`, and what it can and cannot reach

This tool is the one thing in this file that is not a socket call at all. Its
pixels come from an external screenshot of the *local* X display Herdr's own
terminal happens to be rendered on — the same `import -window root` technique
`data/herdr-worker-card-nested-tiered/` and `data/herdr-live-composite/` use
for their own live verification captures. Cropping to one pane additionally
reads two already-allowlisted methods (`pane.layout` for the pane's cell rect,
`pane.graphics.info` for the cell's pixel size), but the pixels themselves
never pass through Herdr's socket, so there is no method name here that needed
enforcing — nothing is sent to Herdr to get an image back.

That external-capture design is also its limit: it can only see whatever that
local display shows. In a lab (Xvfb + a real terminal sized to fill it, no
window manager) that is exactly Herdr's own window. On an ordinary desktop
with a window manager, the same command captures the whole screen, which may
include other windows — point `HERDR_MCP_DISPLAY` at an isolated display for a
clean capture. Either way, it is the *local* display only. There is no path
from here to a remote client's own screen — e.g. a Windows/Rio client attached
over `--remote` renders on a different machine entirely, and this connector
has no way to reach it.

Cropping to one pane also requires `experimental.kitty_graphics` enabled and a
kitty-graphics-capable client to have attached, since that is what makes
Herdr's cell pixel size (`pane.graphics.info`) known. Whole-window capture
(`workspace_id`/`tab_id`) needs neither.

See `data/herdr-mcp-visual-access-20260823/README.md` for real captures and the
exact live verification performed.

## Environment

| Variable | Effect |
| --- | --- |
| `HERDR_MCP_SOCKET` | Use this socket path directly, ignoring session name resolution. |
| `HERDR_MCP_APP_DIR` | App directory name under the config dir. Set to `herdr-dev` to talk to a debug build's server. |
| `HERDR_MCP_TIMEOUT_SECONDS` | Socket timeout, default 10. |
| `HERDR_MCP_DISPLAY` | X display `herdr_pane_screenshot` screenshots. Falls back to `DISPLAY`. |
| `HERDR_MCP_SCREENSHOT_BIN` | Screenshot binary, default `import` (ImageMagick). Must write PNG bytes to stdout. |
| `HERDR_MCP_CROP_BIN` | Crop binary, default `convert` (ImageMagick). Must read PNG bytes from stdin and write PNG bytes to stdout. |
| `HERDR_MCP_SCREENSHOT_TIMEOUT_SECONDS` | Timeout for the screenshot/crop subprocess, default 15. |

Socket paths otherwise resolve exactly as `src/session.rs` resolves them:
`$XDG_CONFIG_HOME/herdr/herdr.sock` (or `~/.config/herdr/herdr.sock`) for the
default session, and `.../herdr/sessions/<name>/herdr.sock` for a named one.

## Tests

```bash
python3 -m unittest scripts.test_herdr_mcp
```

They cover the read-only gate, socket path resolution, the wire round trip
against a fake Herdr socket, the digest rendering, the screenshot tool's target
resolution and crop math (with the actual pixel capture mocked out), and a full
MCP stdio handshake. `just test` and `just check` run them.

Live capture itself — the real `import`/`convert` subprocess against a real
display — is not part of this suite (it needs an X server and ImageMagick).
It was verified once, live, against a real `kitty` under `Xvfb`; see
`data/herdr-mcp-visual-access-20260823/README.md` for that evidence.
