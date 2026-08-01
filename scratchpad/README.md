# Water pane-creation spike — capture harnesses

Scratch tooling for the `fm/spike-water-creation` branch. **Nothing here is production code.**

The primitive itself is `src/ui/water.rs`; this directory only exists to prove it runs and to
produce the frames in the scout report.

## Watch it

The spike animates every pane creation. It must run in a **private fleet**, never the live one.
The debug build already resolves its app dir to `herdr-dev`, and the `HOME`/`XDG_CONFIG_HOME`
override moves the socket entirely.

```bash
RUSTFLAGS="-Clink-arg=-fuse-ld=lld" cargo build -j 2

env -u HERDR_ENV -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH \
    -u HERDR_PANE_ID -u HERDR_TAB_ID -u HERDR_WORKSPACE_ID \
    HOME=/tmp/hwater XDG_CONFIG_HOME=/tmp/hwater/c \
    HERDR_WATER=pour HERDR_WATER_MS=600 \
    ./target/debug/herdr
```

Then split a pane. `HERDR_WATER` accepts `fill`, `pour`, `pour-right`, `slosh`, `droplets`;
`off` or unset disables it. `HERDR_WATER_MS` defaults to 600 — use 1600–2500 to study the shape.

Stripping `HERDR_ENV` is required: herdr refuses to nest (`src/main.rs:439`), so launching this
from inside an existing herdr session fails silently-looking unless that variable is cleared.

## Files

| File | Purpose |
|---|---|
| `live_capture.py` | Runs the spike under a PTY in a private fleet, splits a pane over the private socket, records the client byte stream. `live_capture.py <behaviour> <duration_ms> <out_prefix>` |
| `reparse.py` | Rebuilds frames from a saved `.raw` stream, split on herdr's own `ESC[?2026h`/`ESC[?2026l` synchronised-output brackets, so every frame is complete rather than torn. Also extracts the truecolor values emitted next to water cells. `reparse.py <behaviour> [fractions]` |
| `run_lab.sh` | Lab-contract wrapper: installs the teardown trap, provisions an isolated named session through `fm-herdr-lab.sh`, runs the captures, tears down. `run_lab.sh <out-dir> [behaviours...]` |
| `live/*.raw` | Raw client byte streams from the live runs — the primary evidence, not regenerable identically |
| `report_frames.txt` | Curated text frames from the in-process harness |

`live/*.frames.txt` are deliberately not committed: they were produced by `live_capture.py`'s own
inline parser, which mangles escape sequences straddling a read boundary. `reparse.py` supersedes
them and regenerates clean frames from the `.raw` files.

## Text frames without running herdr

The in-process harness renders through the real `paint()` into a real ratatui `Buffer`:

```bash
HERDR_WATER_FRAMES_OUT=/tmp/frames.txt \
  cargo test --bin herdr water::tests::capture_frames -- --ignored

cargo test --bin herdr water::tests::measure_cost -- --ignored --nocapture
```
