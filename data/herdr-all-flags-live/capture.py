#!/usr/bin/env python3
"""Attach a herdr client through a real, explicitly sized PTY and capture its
raw output stream.

The window size is set with TIOCSWINSZ including the *pixel* fields
(ws_xpixel/ws_ypixel), not just rows/cols. That matters: the client resolves
the host cell size from the pty's pixel fields first (see
`current_terminal_geometry` in src/client/mod.rs), and a PTY opened without
them reports no pixels, so `cell_size.is_known()` is false and the server
sends no Kitty graphics at all — the whole pixel-card path silently does
nothing and the capture looks like a plain character sidebar.

A PTY opened with no window size at all reports about four columns, which
wraps every pane and makes any layout evidence worthless while still looking
like a real read.

The parent also plays the terminal for one exchange. Since #82 the client sends
a Kitty Graphics capability probe at startup and paints nothing until something
answers it (`kitty_graphics_capability_confirmed`), so a bare PTY — which has no
terminal on the other end — makes the whole pixel path inert. That is not a
hypothetical: on the base this check was written against there was no probe, and
on current master the same capture drops from 5,696,056 bytes to 60,993 with a
single APC block, while still passing every assertion that is not a volume guard.

The reply is written *when the probe is seen in the stream*, never on a timer.
Blind or repeated writes into a freshly forked PTY race the child's cooked→raw
transition and get silently swallowed — measured as working twice and then
failing five times in a row with identical code. Waiting for the probe removes
the race by construction: the client cannot have emitted it before entering raw
mode.

This stubs the handshake, it does not test it. What a real terminal does with
the bytes that follow is `data/herdr-live-composite/`'s job.

usage: capture.py <cols> <rows> <cell_w_px> <cell_h_px> <settle_ms> <out> -- <cmd...>
"""
import fcntl
import os
import pty
import re
import selectors
import struct
import sys
import termios
import time

# `\x1b_Gi=1,a=q,t=d,f=24,s=1,v=1;AAAA\x1b\\` — src/terminal_theme.rs.
# Matched loosely on the key fields so a reordering of the control block does
# not silently stop the reply and take the pixel path down with it.
CAPABILITY_PROBE = re.compile(rb"\x1b_G[^;\x1b]*\ba=q\b[^;\x1b]*;")
CAPABILITY_REPLY = b"\x1b_Gi=1;OK\x1b\\"
# How far back to re-scan on each read, so a probe straddling two reads is still
# matched whole. Comfortably more than the probe's own length.
PROBE_OVERLAP = 128


def main() -> int:
    if "--" not in sys.argv:
        print(__doc__, file=sys.stderr)
        return 2
    split = sys.argv.index("--")
    cols, rows, cell_w, cell_h, settle_ms = (int(x) for x in sys.argv[1:split][:5])
    out_path = sys.argv[1:split][5]
    cmd = sys.argv[split + 1 :]
    if not cmd:
        print("no command given", file=sys.stderr)
        return 2

    pid, fd = pty.fork()
    if pid == 0:
        # Child: exec the client. TERM has to advertise something the client
        # will drive normally; the size is set by the parent below.
        os.environ["TERM"] = os.environ.get("CAPTURE_TERM", "xterm-kitty")
        os.execvp(cmd[0], cmd)
        os._exit(127)

    # ws_row, ws_col, ws_xpixel, ws_ypixel
    winsz = struct.pack("HHHH", rows, cols, cols * cell_w, rows * cell_h)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, winsz)

    chunks = bytearray()
    sel = selectors.DefaultSelector()
    sel.register(fd, selectors.EVENT_READ)
    answered = False
    scanned = 0
    deadline = time.monotonic() + settle_ms / 1000.0
    while time.monotonic() < deadline:
        for _key, _mask in sel.select(timeout=0.05):
            try:
                data = os.read(fd, 65536)
            except OSError:
                deadline = 0
                break
            if not data:
                deadline = 0
                break
            chunks.extend(data)
            if not answered:
                # Scan from a little behind the last position so a probe split
                # across two reads is still seen whole.
                window = chunks[max(0, scanned - PROBE_OVERLAP) :]
                if CAPABILITY_PROBE.search(bytes(window)):
                    os.write(fd, CAPABILITY_REPLY)
                    answered = True
                    print("answered the kitty graphics capability probe", flush=True)
                scanned = len(chunks)

    sel.close()
    if not answered:
        print(
            "WARNING: never saw a kitty graphics capability probe; the client will "
            "have painted no pixels at all",
            file=sys.stderr,
        )
    try:
        os.kill(pid, 15)
    except ProcessLookupError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    try:
        os.close(fd)
    except OSError:
        pass

    with open(out_path, "wb") as fh:
        fh.write(bytes(chunks))
    print(f"captured {len(chunks)} bytes -> {out_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
