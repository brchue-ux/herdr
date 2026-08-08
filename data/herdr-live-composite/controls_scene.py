#!/usr/bin/env python3
"""Draw a synthetic scene *inside* a real kitty window, for the detector self-test.

Runs as kitty's own command, so it reads the real terminal size instead of
guessing one, and it emits the same Kitty graphics protocol shape
`src/kitty_graphics.rs` emits — `a=t,t=d,f=100,s=,v=,i=,q=2` uploads chunked at
3072 raw bytes, then `a=p,i=,p=,c=,r=,z=,C=1,q=2` after a CUP. If this diverged
from the real encoder the self-test would be proving something about a different
protocol than the one herdr speaks.

Cases:

    text            probe text only — the legibility reference
    behind          the same text with a full-surface opaque image at z=-2
    over            the same text with that image at z=0, the #96 failure
    moving          a block that steps across the screen
    still           the same block, drawn once and left alone

`behind` must pass the legibility assertion and `over` must fail it; `moving`
must pass the motion assertion and `still` must fail it. A detector that cannot
be made to fail is not evidence, and that is the specific way the byte-level
check nearly shipped green on an empty capture.
"""

from __future__ import annotations

import base64
import io
import shutil
import sys
import time

from PIL import Image

KITTY_CHUNK_BYTES = 3072
PROBE_LINE = "HERDR LEGIBILITY PROBE 0123456789 abcdefghijklm ##"
PROBE_LINES = 12
PROBE_ROW = 4
PROBE_COL_FRAC = 0.36


def upload(image_id: int, png: bytes, width: int, height: int) -> str:
    control = f"a=t,t=d,f=100,s={width},v={height},i={image_id},q=2"
    chunks = [
        png[i : i + KITTY_CHUNK_BYTES] for i in range(0, len(png), KITTY_CHUNK_BYTES)
    ]
    out = []
    for n, chunk in enumerate(chunks):
        more = 1 if n < len(chunks) - 1 else 0
        b64 = base64.standard_b64encode(chunk).decode()
        if n == 0:
            out.append(f"\x1b_G{control},m={more};{b64}\x1b\\")
        else:
            out.append(f"\x1b_Gm={more};{b64}\x1b\\")
    return "".join(out)


def place(image_id: int, row: int, col: int, cols: int, rows: int, z: int) -> str:
    cup = f"\x1b[{row + 1};{col + 1}H"
    ctl = f"a=p,i={image_id},p={image_id},c={cols},r={rows},z={z},C=1,q=2"
    return f"{cup}\x1b_G{ctl};\x1b\\"


SCENE_W = 240
SCENE_H = 150


def scene_png() -> bytes:
    """A fully opaque wash with enough structure to be obviously *something*.

    Opaque on purpose: that is what makes the z band load-bearing, and it is
    literally what `persistent_background` puts on screen (`pack_rgba8` with
    `force_opaque = true`).

    Small, and left for the terminal to scale up to the placement's cell
    rectangle — the same thing that happens to the real scene. Rasterising it at
    full window resolution in Python took long enough that the screenshot landed
    before the upload did, and an image that has not arrived yet looks exactly
    like an image the terminal refused.
    """
    img = Image.new("RGB", (SCENE_W, SCENE_H))
    px = img.load()
    cx, cy = SCENE_W * 0.55, SCENE_H * 0.45
    for y in range(SCENE_H):
        for x in range(SCENE_W):
            dx, dy = (x - cx) / SCENE_W, (y - cy) / SCENE_H
            r = (dx * dx + dy * dy) ** 0.5
            v = max(0.0, 1.0 - r * 1.6)
            px[x, y] = (
                int(40 + 150 * v),
                int(50 + 120 * v * v),
                int(70 + 90 * v),
            )
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def probe_text(cols: int) -> str:
    col = int(cols * PROBE_COL_FRAC)
    out = []
    for n in range(PROBE_LINES):
        out.append(f"\x1b[{PROBE_ROW + n};{col + 1}H\x1b[1;97m{PROBE_LINE}\x1b[0m")
    return "".join(out)


def main() -> int:
    case = sys.argv[1] if len(sys.argv) > 1 else "text"
    size = shutil.get_terminal_size(fallback=(120, 40))
    cols, rows = size.columns, size.lines

    w = sys.stdout.write
    # Hide the cursor: it blinks, so leaving it on puts a lit block into some
    # screenshots and not others and corrupts every measurement below.
    w("\x1b[2J\x1b[H\x1b[?25l")

    if case in ("text", "behind", "over"):
        if case in ("behind", "over"):
            # Cell size is not knowable from inside, and it does not need to be:
            # the placement is made in cells and the terminal scales the image to
            # them, which is exactly how herdr places the real scene.
            #
            # Emitted *before* the text so that seeing the text on screen is
            # proof the upload has already been consumed — the stream is
            # processed in order, so the readiness poll cannot catch a half-drawn
            # scene.
            png = scene_png()
            w(upload(1, png, SCENE_W, SCENE_H))
            w(place(1, 0, 0, cols, rows, -2 if case == "behind" else 0))
        w(probe_text(cols))
        w(f"\x1b[{rows};1H")
        sys.stdout.flush()
        time.sleep(120)
        return 0

    if case in ("moving", "still"):
        block = "█" * 12
        for step in range(400):
            col = 4 + (step * 7) % max(1, cols - 20) if case == "moving" else 4
            w("\x1b[2J")
            for n in range(6):
                w(f"\x1b[{PROBE_ROW + n * 2};{col + 1}H\x1b[1;97m{block}\x1b[0m")
            w(f"\x1b[{rows};1H")
            sys.stdout.flush()
            if case == "still":
                time.sleep(120)
                return 0
            time.sleep(0.25)
        return 0

    print(f"unknown case: {case}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
