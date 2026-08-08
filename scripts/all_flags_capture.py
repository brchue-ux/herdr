#!/usr/bin/env python3
"""Run a real herdr with every capability on, and capture what it actually emits.

The unit suite never sees this combination. Every flag in `tests/fixtures/all-flags.toml`
is off by default, so a stock test run exercises the quiet binary; the surfaces
that have produced shipped bugs -- pixel cards, the particle wash, the signal
tray, the background scene -- are all behind them.

This spawns a real server, attaches a real client through a PTY, and keeps the
bytes the client actually receives. It asserts on those bytes, not on a buffer.

Two details decide whether this is a real capture or a convincing blank, and
both are silent when wrong:

  The PTY must report PIXELS, not just rows/columns. The client resolves the
  host cell size from the pty's ws_xpixel/ws_ypixel first; without them
  `cell_size.is_known()` is false and the server sends NO graphics at all. The
  capture then looks like a plain character sidebar and every graphics flag is
  inert while appearing enabled.

  A proportional font must exist on the machine. `image_card::is_available`
  needs a face found at runtime and herdr ships none, so a bare runner leaves
  the pixel-card path off for a second, independent reason.

Usage: all_flags_capture.py <herdr-binary> <workdir> [--seconds N]
Exits non-zero with a diagnosis if the capture is empty or the server died.
"""

import fcntl
import os
import pty
import struct
import subprocess
import sys
import termios
import time

SESSION = "afcap"
# Big enough that the sidebar is not degenerate; the panel folds to bare lines
# below card::MIN_FOLD_WIDTH and we want the card shell under test.
COLS, ROWS = 200, 50
CELL_W, CELL_H = 9, 18


def set_winsize(fd, cols, rows, xpix, ypix):
    """TIOCSWINSZ including the pixel fields -- see the module docstring."""
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, xpix, ypix))


def run(binary, args, env, timeout=30):
    return subprocess.run(
        [binary, *args], env=env, capture_output=True, text=True, timeout=timeout
    )


def main(argv):
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    binary = os.path.abspath(argv[1])
    workdir = os.path.abspath(argv[2])
    seconds = 8
    if "--seconds" in argv:
        seconds = int(argv[argv.index("--seconds") + 1])

    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    config = os.path.join(repo, "tests", "fixtures", "all-flags.toml")
    if not os.path.exists(config):
        print(f"missing all-flags config at {config}", file=sys.stderr)
        return 2

    os.makedirs(workdir, exist_ok=True)
    env = dict(os.environ)
    env["HOME"] = workdir
    env["XDG_CONFIG_HOME"] = workdir
    env["XDG_RUNTIME_DIR"] = workdir
    # Read by the process that runs `herdr server`, not by an attaching client.
    env["HERDR_CONFIG_PATH"] = config
    env["TERM"] = "xterm-kitty"
    # Identify as kitty: the ambient wash and background scene are refused on a
    # terminal herdr has not positively identified (draws_ambient_wash).
    env["TERM_PROGRAM"] = "kitty"
    env.pop("HERDR_ENV", None)
    env.pop("HERDR_SOCKET_PATH", None)
    env.pop("HERDR_CLIENT_SOCKET_PATH", None)

    print(f"config : {config}")
    print(f"workdir: {workdir}")

    server = subprocess.Popen(
        [binary, "server", "--session", SESSION],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )

    try:
        # Wait for the socket to answer rather than sleeping a fixed amount.
        ready = False
        for _ in range(60):
            if server.poll() is not None:
                break
            probe = run(binary, ["status", "--session", SESSION], env, timeout=15)
            if probe.returncode == 0:
                ready = True
                break
            time.sleep(0.5)

        if not ready:
            out = ""
            if server.poll() is not None:
                out = server.stdout.read() if server.stdout else ""
            print("server never became ready", file=sys.stderr)
            if out:
                print(out[-3000:], file=sys.stderr)
            return 1
        print("server ready")

        # A fleet with some shape to it, so the tree has rows to draw.
        run(binary, ["workspace", "create", "--session", SESSION, "--label", "alpha"], env)
        run(binary, ["workspace", "create", "--session", SESSION, "--label", "beta"], env)

        # Attach a real client on a PTY that reports pixels.
        master, slave = pty.openpty()
        set_winsize(slave, COLS, ROWS, COLS * CELL_W, ROWS * CELL_H)
        set_winsize(master, COLS, ROWS, COLS * CELL_W, ROWS * CELL_H)

        client = subprocess.Popen(
            [binary, "--session", SESSION],
            env=env,
            stdin=slave,
            stdout=slave,
            stderr=subprocess.DEVNULL,
            close_fds=True,
        )
        os.close(slave)

        captured = bytearray()
        deadline = time.time() + seconds
        os.set_blocking(master, False)
        while time.time() < deadline:
            try:
                chunk = os.read(master, 65536)
                if chunk:
                    captured.extend(chunk)
            except BlockingIOError:
                time.sleep(0.02)
            except OSError:
                break

        client.terminate()
        try:
            client.wait(timeout=10)
        except subprocess.TimeoutExpired:
            client.kill()
        os.close(master)

        return report(bytes(captured), server)
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()


def report(data, server):
    """Classify the capture. Empty and 'drew nothing' are different failures."""
    apc = data.count(b"\x1b_G")
    print(f"\ncaptured bytes : {len(data)}")
    print(f"kitty APC (\\e_G): {apc}")

    # A crash shows up as a dead server, not as an empty capture.
    if server.poll() is not None and server.returncode not in (0, -15):
        out = server.stdout.read() if server.stdout else ""
        print(f"FAIL server exited with {server.returncode}", file=sys.stderr)
        if out:
            print(out[-3000:], file=sys.stderr)
        return 1

    # Thresholds are deliberately loose: this guards "the capture is real",
    # not a pixel-exact baseline. An empty or near-empty capture is the failure
    # mode that reads as a pass.
    if len(data) < 2000:
        print(
            f"FAIL capture is {len(data)} bytes -- the client drew essentially "
            f"nothing. Check the PTY reported pixel dimensions and that the "
            f"binary is the all-flags one.",
            file=sys.stderr,
        )
        return 1

    if apc == 0:
        print(
            "FAIL no kitty graphics escapes in the capture. Every graphics flag "
            "is on, so this means the pixel path is inert -- most likely the "
            "host cell size was unknown (PTY pixel fields) or no proportional "
            "font was found on the machine.",
            file=sys.stderr,
        )
        return 1

    print("\nPASS all-flags binary rendered, with graphics on the wire")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
