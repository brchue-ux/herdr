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

usage: capture.py <cols> <rows> <cell_w_px> <cell_h_px> <settle_ms> <out> -- <cmd...>
"""
import fcntl
import os
import pty
import selectors
import struct
import sys
import termios
import time


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

    sel.close()
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
