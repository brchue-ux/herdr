#!/usr/bin/env python3
"""Capture real frames of the water materialise animation from a running herdr.

Runs the spiked debug binary under a PTY in a fully private fleet (its own HOME and
XDG_CONFIG_HOME; the debug build additionally uses the `herdr-dev` app dir, so its
socket can never be the live fleet's). Splits a pane over the private socket, then
reconstructs the frames the *client terminal* actually received.

Usage: live_capture.py <behaviour> <duration_ms> <out_prefix>
"""
import os
import pty
import re
import select
import shutil
import signal
import subprocess
import sys
import time
import fcntl
import termios
import struct

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
BIN = os.path.join(REPO, "target", "debug", "herdr")
PRIVATE_HOME = "/tmp/hwater"
COLS, ROWS = 100, 30

WATER_GLYPHS = "▁▂▃▄▅▆▇█"


def private_env(behaviour, duration_ms):
    env = dict(os.environ)
    # Strip every inherited Herdr marker: the socket overrides (so we can never
    # reach the live fleet) and HERDR_ENV/pane ids (which trip the nested-herdr
    # guard at src/main.rs:439).
    for k in list(env):
        if k.startswith("HERDR_"):
            env.pop(k, None)
    env["HOME"] = PRIVATE_HOME
    env["XDG_CONFIG_HOME"] = PRIVATE_HOME + "/c"
    env["XDG_DATA_HOME"] = PRIVATE_HOME + "/d"
    env["XDG_STATE_HOME"] = PRIVATE_HOME + "/s"
    env["TERM"] = "xterm-256color"
    env["COLORTERM"] = "truecolor"
    env["HERDR_WATER"] = behaviour
    env["HERDR_WATER_MS"] = str(duration_ms)
    return env


class Screen:
    """Minimal VT reconstructor: enough of the CSI set for herdr's output."""

    def __init__(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.reset()

    def reset(self):
        self.grid = [[" "] * self.cols for _ in range(self.rows)]
        self.fg = [[None] * self.cols for _ in range(self.rows)]
        self.x = self.y = 0

    def snapshot(self):
        return "\n".join("".join(row).rstrip() for row in self.grid)

    def put(self, ch, fg):
        if 0 <= self.y < self.rows and 0 <= self.x < self.cols:
            self.grid[self.y][self.x] = ch
            self.fg[self.y][self.x] = fg
        self.x += 1
        if self.x >= self.cols:
            self.x = self.cols - 1

    def feed(self, data):
        i, n = 0, len(data)
        cur_fg = None
        while i < n:
            ch = data[i]
            if ch == "\x1b":
                m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", data[i:])
                if m:
                    params, cmd = m.group(1), m.group(2)
                    nums = [int(p) for p in params.split(";") if p.isdigit()]
                    if cmd == "H" or cmd == "f":
                        self.y = (nums[0] - 1) if nums else 0
                        self.x = (nums[1] - 1) if len(nums) > 1 else 0
                    elif cmd == "J":
                        mode = nums[0] if nums else 0
                        if mode == 2:
                            self.grid = [[" "] * self.cols for _ in range(self.rows)]
                    elif cmd == "K":
                        mode = nums[0] if nums else 0
                        if mode == 0 and 0 <= self.y < self.rows:
                            for xx in range(self.x, self.cols):
                                self.grid[self.y][xx] = " "
                    elif cmd == "m":
                        if params in ("", "0"):
                            cur_fg = None
                        else:
                            fgm = re.match(r"(?:.*;)?38;2;(\d+);(\d+);(\d+)", params)
                            cur_fg = tuple(map(int, fgm.groups())) if fgm else cur_fg
                    elif cmd == "C":
                        self.x += nums[0] if nums else 1
                    elif cmd == "A":
                        self.y -= nums[0] if nums else 1
                    elif cmd == "B":
                        self.y += nums[0] if nums else 1
                    elif cmd == "d":
                        self.y = (nums[0] - 1) if nums else 0
                    elif cmd == "G":
                        self.x = (nums[0] - 1) if nums else 0
                    i += m.end()
                    continue
                m = re.match(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", data[i:])
                if m:
                    i += m.end()
                    continue
                m = re.match(r"\x1bP[^\x1b]*\x1b\\", data[i:])
                if m:
                    i += m.end()
                    continue
                i += 2
                continue
            if ch == "\r":
                self.x = 0
            elif ch == "\n":
                self.y += 1
            elif ch == "\b":
                self.x = max(0, self.x - 1)
            elif ch >= " ":
                self.put(ch, cur_fg)
            i += 1


def main():
    behaviour, duration_ms, prefix = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    env = private_env(behaviour, duration_ms)
    shutil.rmtree(PRIVATE_HOME, ignore_errors=True)
    os.makedirs(PRIVATE_HOME + "/c", exist_ok=True)

    pid, fd = pty.fork()
    if pid == 0:
        os.execve(BIN, [BIN], env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    log = []  # (t, bytes)
    t0 = time.time()

    def pump(until):
        while time.time() < until:
            r, _, _ = select.select([fd], [], [], 0.01)
            if fd in r:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    return False
                if not chunk:
                    return False
                log.append((time.time() - t0, chunk))
        return True

    # Boot, then declare focus so the client stays a *viewer* — the server
    # suppresses scheduled work when has_app_viewers() is false
    # (src/server/headless.rs, handle_scheduled_tasks_headless).
    sock = PRIVATE_HOME + "/c/herdr-dev/herdr.sock"
    deadline = time.time() + 25
    while time.time() < deadline and not os.path.exists(sock):
        if not pump(time.time() + 0.2):
            break
    pump(time.time() + 1.5)
    os.write(fd, b"\x1b[I")  # focus-in
    pump(time.time() + 0.5)

    def cli(*args):
        return subprocess.run(
            [BIN, *args], env=env, capture_output=True, text=True, timeout=20
        )

    print("socket:", sock, "exists" if os.path.exists(sock) else "MISSING")
    print("status:", cli("status").stdout.strip().replace("\n", " | ")[:300])

    split_at = time.time() - t0
    out = cli("pane", "split", "--direction", "down")
    print("split:", (out.stdout or out.stderr).strip()[:200])

    pump(time.time() + max(3.0, duration_ms / 1000.0 + 2.0))

    os.kill(pid, signal.SIGTERM)
    time.sleep(0.4)
    try:
        os.close(fd)
    except OSError:
        pass

    # Reconstruct frames from the byte stream the client actually received.
    screen = Screen(COLS, ROWS)
    frames = []
    for t, chunk in log:
        screen.feed(chunk.decode("utf-8", "replace"))
        snap = screen.snapshot()
        if not frames or frames[-1][1] != snap:
            frames.append((t, snap))

    water_frames = [
        (t, s) for t, s in frames if t >= split_at and any(g in s for g in WATER_GLYPHS)
    ]

    with open(prefix + ".frames.txt", "w") as f:
        f.write(f"behaviour={behaviour} duration_ms={duration_ms} "
                f"split_at={split_at:.3f}s screen={COLS}x{ROWS}\n")
        f.write(f"total distinct screens={len(frames)} "
                f"screens containing water glyphs={len(water_frames)}\n\n")
        for t, s in water_frames:
            f.write(f"--- t={t:.3f}s (t-split={t - split_at:+.3f}s)\n{s}\n\n")

    with open(prefix + ".raw", "wb") as f:
        for _, chunk in log:
            f.write(chunk)

    deltas = [round((water_frames[i + 1][0] - water_frames[i + 1 - 1][0]) * 1000)
              for i in range(len(water_frames) - 1)]
    print(f"distinct screens: {len(frames)}")
    print(f"screens with water glyphs after split: {len(water_frames)}")
    if water_frames:
        print(f"water frame span: {water_frames[0][0] - split_at:+.3f}s .. "
              f"{water_frames[-1][0] - split_at:+.3f}s")
        print(f"inter-frame ms: {deltas}")


if __name__ == "__main__":
    main()
