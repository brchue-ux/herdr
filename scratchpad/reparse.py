#!/usr/bin/env python3
"""Re-parse a captured raw PTY stream into clean frames.

The live capture fed each read() chunk to the reconstructor independently, so an
escape sequence straddling a chunk boundary leaked its parameters into the grid.
That is a capture artifact, not a render artifact — a real terminal sees one
continuous stream. This tool carries the partial tail between chunks.
"""
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from live_capture import Screen, COLS, ROWS  # noqa: E402

G = "▁▂▃▄▅▆▇█"
PARTIAL = re.compile(r"\x1b(\[[0-9;?]*|\]|P|)?$")


END = "\x1b[?2026l"


def frames_from_raw(path, cols=COLS, rows=ROWS):
    """Split on herdr's synchronised-output brackets (ESC[?2026h .. ESC[?2026l).

    Those are exactly the frame boundaries the client terminal honours, so each
    snapshot below is a complete frame as displayed — never a torn mid-write.
    """
    data = open(path, "rb").read().decode("utf-8", "replace")
    screen = Screen(cols, rows)
    out = []
    for part in data.split(END):
        screen.feed(part + END)
        snap = screen.snapshot()
        if not out or out[-1] != snap:
            out.append(snap)
    return out


def pane_crop(frame, x0, x1):
    """Crop to the lower pane's interior."""
    lines = frame.split("\n")
    tops = [k for k, l in enumerate(lines) if "┌" in l]
    if len(tops) < 2:
        return None
    top = tops[1] + 1
    bots = [k for k, l in enumerate(lines) if "└" in l and k > top]
    bot = bots[0] if bots else len(lines)
    return [l[x0:x1].rstrip() for l in lines[top:bot]]


def water_colours(path):
    """Truecolor pairs actually emitted next to a water glyph."""
    data = open(path, "rb").read().decode("utf-8", "replace")
    pairs = re.findall(
        r"\x1b\[[0-9;]*?38;2;(\d+);(\d+);(\d+);48;2;(\d+);(\d+);(\d+)m([^\x1b]{0,3})",
        data,
    )
    fg_seen, bg_seen = {}, {}
    for r, g, b, br, bg_, bb, tail in pairs:
        if not tail:
            continue
        if any(ch in G for ch in tail):
            fg_seen[(int(r), int(g), int(b))] = fg_seen.get((int(r), int(g), int(b)), 0) + 1
        bg_seen[(int(br), int(bg_), int(bb))] = bg_seen.get((int(br), int(bg_), int(bb)), 0) + 1
    return fg_seen, bg_seen


if __name__ == "__main__":
    name = sys.argv[1]
    fracs = [float(x) for x in sys.argv[2].split(",")] if len(sys.argv) > 2 else [
        0.08, 0.25, 0.45, 0.65, 0.85
    ]
    raw = f"scratchpad/live/live-{name}.raw"
    frames = frames_from_raw(raw)
    water = [f for f in frames if any(g in f for g in G)]
    print(f"### live `{name}` — {len(frames)} distinct screens, "
          f"{len(water)} carrying water glyphs")
    for fr in fracs:
        f = water[min(int(len(water) * fr), len(water) - 1)]
        c = pane_crop(f, 27, 99)
        if c is None:
            continue
        print(f"\n-- frame {int(fr * 100)}% through the water-bearing sequence")
        for l in c:
            print("  " + l)
