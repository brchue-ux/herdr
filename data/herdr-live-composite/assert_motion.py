#!/usr/bin/env python3
"""Assert that a region of a real terminal actually changes over time.

This is the #94/#97 regression class. Both shipped with the unit suite fully
green, and both were found by watching a real Kitty client over several seconds:
#97's tray drew every badge correctly and then never moved again, because the
frame that would have carried the next set of bytes was suppressed as identical.

A frozen surface is invisible to any single-frame check, and invisible to a
byte-level check too when the bytes that never arrive are the evidence.

Passing needs *sustained* motion, not one twitch: a minimum number of
consecutive frame pairs must each clear a pixel floor. One large blip — a modal
closing, a scroll — cannot carry a frozen animation over the line.

`--tail-pairs` exists because an animation can *fade in* rather than switch on.
The tray's badges pulse on an amplitude envelope that opens from nothing, so its
first seconds are genuinely below any pixel floor worth setting — measured at a
per-badge swing of 0.4 luma against the 7.5 it settles at. Judging those pairs
asks the surface to be at full amplitude the instant it is looked at. They are
still measured and still printed; they are just not what the verdict is taken
from.
"""

from __future__ import annotations

import argparse
import os
import sys

import composite_lib as lib


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("frames", nargs="+", help="screenshots in capture order")
    ap.add_argument(
        "--region",
        default="0.0,0.0,1.0,1.0",
        help="fractional x0,y0,x1,y1 to measure inside (default: whole frame)",
    )
    ap.add_argument("--label", default="region")
    ap.add_argument("--level", type=int, default=24, help="per-channel difference floor")
    ap.add_argument(
        "--min-changed-px",
        type=int,
        default=500,
        help="a pair counts as moving when this many pixels differ",
    )
    ap.add_argument(
        "--min-active-pairs",
        type=int,
        default=3,
        help="how many pairs must move for the surface to count as animating",
    )
    ap.add_argument(
        "--tail-pairs",
        type=int,
        default=0,
        help="judge only the last N pairs (0: all of them). Every pair is still "
        "measured and printed; this decides which ones the verdict is taken "
        "from. For a surface whose animation fades in rather than switching on, "
        "the opening pairs measure the fade, not the animation.",
    )
    ap.add_argument(
        "--expect-fail",
        action="store_true",
        help="invert the exit code; used by the detector self-test so a known-static "
        "scene proves the assertion can fail at all",
    )
    args = ap.parse_args()

    if len(args.frames) < 2:
        print("FAIL: need at least two frames", file=sys.stderr)
        return 1

    frames = [lib.load(p) for p in args.frames]
    sizes = {f.size for f in frames}
    if len(sizes) != 1:
        print(f"FAIL: frames differ in geometry: {sizes}", file=sys.stderr)
        return 1

    box = lib.frac_box(frames[0].size, args.region)
    crops = [f.crop(box) for f in frames]
    area = (box[2] - box[0]) * (box[3] - box[1])

    total = len(crops) - 1
    judged_from = max(0, total - args.tail_pairs) if args.tail_pairs > 0 else 0

    print(f"{args.label}: {lib.describe_box(box)}  ({area} px)")
    active = 0
    for i in range(total):
        changed = lib.mask_count(lib.changed_mask(crops[i], crops[i + 1], args.level))
        moving = changed >= args.min_changed_px
        counted = i >= judged_from
        if moving and counted:
            active += 1
        print(
            f"  {os.path.basename(args.frames[i])} -> "
            f"{os.path.basename(args.frames[i + 1])}: {changed:>8} px "
            f"({changed / area:.4f} of region)  {'move' if moving else 'STILL'}"
            f"{'' if counted else '  (warm-up, not judged)'}"
        )

    judged = total - judged_from
    print(f"  moving pairs: {active}/{judged} (need {args.min_active_pairs})")
    if active < args.min_active_pairs:
        print(
            f"FAIL: {args.label} moved in only {active} of {judged} judged pairs — "
            "a surface that declares an animation is not animating",
            file=sys.stderr,
        )
        return 0 if args.expect_fail else 1

    print(f"PASS: {args.label} animates")
    if args.expect_fail:
        print("FAIL: this static scene was supposed to be caught", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
