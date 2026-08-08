#!/usr/bin/env python3
"""Gate CI on what a real terminal actually composited, not on a unit assertion.

`data/herdr-card-as-alpha-shape/blend-test/analyse.py` measures and *reports*;
it always exits 0 because it was written to answer a question, not to guard a
branch. This wraps it in a pass/fail decision so the same evidence can gate a
build.

What is being held: kitty composites two overlapping transparent placements
source-over and *in linear light*. That is a measured property of the terminal
(see the blend-test RESULT.md), and `src/kitty_graphics.rs` emits placements on
the assumption it holds -- `Canvas::blend` composites in sRGB, so Herdr blending
two images itself does NOT reproduce what the terminal does with the same two.
If this gate ever fails, the card path's overlap behaviour is no longer what the
design was measured against.

Two failure modes are reported separately on purpose, because they look
identical in a screenshot and mean opposite things:

  "nothing drew"  -- the placement never landed. kitty silently drops a
                     placement larger than the grid, so a window one column
                     short produces a blank capture that reads exactly like a
                     rendering bug. This is an environment fault.
  wrong verdict   -- pixels landed but composited differently than measured.
                     This is a real regression.

Usage:
  scripts/live_render_gate.py <shot.png> [more.png ...]
  scripts/live_render_gate.py --expect cyan-over-amber <shot.png>

`--expect` selects which card the capture placed on top, so the same gate holds
both stacking orders -- kitty honouring `z` in both directions is itself part of
what the card path relies on.
"""

import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BLEND_TEST = os.path.join(REPO, "data", "herdr-card-as-alpha-shape", "blend-test")
sys.path.insert(0, BLEND_TEST)

import analyse  # noqa: E402  (path must be set first)

# The measured verdicts, keyed by which card the capture put on top. Both are
# linear-light source-over; only the operand order differs.
VERDICTS = {
    "amber-over-cyan": "BLENDS amber-over-cyan (linear)",
    "cyan-over-amber": "BLENDS cyan-over-amber (linear)",
}
DEFAULT_EXPECT = "amber-over-cyan"

# analyse.py reported a max channel error of 1 against the linear-light
# prediction on both kitty 0.45.0 (original measurement) and 0.32.2. Allow a
# little slack for font/scaling differences between runners without letting a
# genuinely different blend model (nearest wrong model is 21 off) sneak past.
MAX_CHANNEL_ERROR = 6


def gate(path, expected_verdict):
    """Return (ok, message) for one screenshot."""
    name = os.path.basename(path)
    probed = analyse.probe(path)
    if probed is None:
        return False, (
            f"{name}: nothing drew -- the capture is blank. This is usually the "
            f"placement exceeding the grid (kitty drops those silently), not a "
            f"compositing regression. Check the terminal is wide enough."
        )

    _lit, _cpt, cyan, _apt, amber, _opt, overlap = probed
    verdict, dist, _cands = analyse.classify(overlap, cyan, amber)

    if verdict != expected_verdict:
        return False, (
            f"{name}: overlap pixel {overlap} classified as {verdict!r}, "
            f"expected {expected_verdict!r}. The terminal is no longer "
            f"compositing the way the card path was measured against."
        )
    if dist > MAX_CHANNEL_ERROR:
        return False, (
            f"{name}: verdict {verdict!r} but max channel error {dist} "
            f"exceeds {MAX_CHANNEL_ERROR}."
        )
    return True, f"{name}: {verdict} (max channel error {dist})"


def main(argv):
    argv = argv[1:]
    expect = DEFAULT_EXPECT
    if argv and argv[0] == "--expect":
        if len(argv) < 2 or argv[1] not in VERDICTS:
            print(
                f"--expect must be one of: {', '.join(sorted(VERDICTS))}",
                file=sys.stderr,
            )
            return 2
        expect = argv[1]
        argv = argv[2:]

    paths = argv
    if not paths:
        print(
            "usage: live_render_gate.py [--expect ORDER] <shot.png> [...]",
            file=sys.stderr,
        )
        return 2

    expected_verdict = VERDICTS[expect]
    failed = 0
    for path in paths:
        if not os.path.exists(path):
            print(f"FAIL {os.path.basename(path)}: no such capture", file=sys.stderr)
            failed += 1
            continue
        ok, message = gate(path, expected_verdict)
        print(("PASS " if ok else "FAIL ") + message, file=sys.stdout if ok else sys.stderr)
        failed += 0 if ok else 1

    if failed:
        print(f"\n{failed} live-render check(s) failed", file=sys.stderr)
        return 1
    print(f"\nall {len(paths)} live-render check(s) passed")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
