#!/usr/bin/env python3
"""Reduce a capture to a stable structural digest, and compare it to a baseline.

The point of a baseline here is to catch a change in *what is drawn*, which the
volume and control-key assertions in run.sh cannot see: they check that the
right mechanisms fired, not that the tree still looks the same.

Raw captures are not comparable byte for byte. Image ids are content hashes that
move whenever artwork moves, animation phase depends on when the settle landed,
and colours breathe continuously by design. So the digest deliberately keeps
only what a layout change would move and a repaint would not:

  * placement geometry — the (cols, rows, width_px, height_px) of every card
    placement, as a sorted set of the *distinct* geometries. This is the tree's
    layout: a card's rank is carried by its width, its tier by its height. #67
    lives here.

    Distinct, not a multiset, and that was measured rather than chosen. A card is
    re-placed on every frame it changes on, so the number of repeats is a count of
    frames the capture happened to contain, not a fact about the tree: two
    captures of one unchanged fleet came back with 93 and 106 placements of the
    same six geometries. Keeping the repeats made the digest unreproducible
    against itself, which no committed baseline could survive.
  * the decoded character grid, with trailing blanks stripped, cropped to the
    sidebar's own columns (`DIGEST_GRID_COLS`). Under the pixel path most card
    cells are covered by images, so this mostly pins the connectors, the rails
    and any character-shell rows.
  * which formats and transports appeared, as sets.

Image ids, payload bytes, z-order and per-frame animation controls are all
excluded on purpose: they move for reasons that are not regressions.

The crop matters for the same reason. The panes to the right of the divider run
real shells, and a default prompt prints the machine's hostname — freshly
generated per run on a hosted runner. Keeping those columns makes every digest
unique, so no committed baseline could ever match and the whole golden check
would read as permanent drift.

  digest.py write <capture> <grid> <out>      — write a digest
  digest.py check <capture> <grid> <baseline> — compare; exit 1 on drift,
                                               exit 3 when there is no baseline
"""
import os
import re
import sys

APC = re.compile(rb"\x1b_G([^;\x1b]*)")

#: Exit code for "there is nothing to compare against". Distinct from drift (1)
#: and from usage (2) so a caller can decide whether a missing baseline is
#: tolerable; this used to be exit 0, which made every run pass vacuously.
NO_BASELINE = 3


def digest(capture_path: str, grid_path: str) -> str:
    blob = open(capture_path, "rb").read()

    placements = []
    formats, transports = set(), set()
    for m in APC.finditer(blob):
        keys = {}
        for pair in m.group(1).split(b","):
            if b"=" in pair:
                k, _, v = pair.partition(b"=")
                keys[k.decode("ascii", "replace")] = v.decode("ascii", "replace")
        if "f" in keys:
            formats.add(keys["f"])
        if "t" in keys:
            transports.add(keys["t"])
        # A placement carries cell extent and pixel extent together.
        if {"c", "r"} <= keys.keys():
            placements.append(
                (keys.get("c", ""), keys.get("r", ""), keys.get("w", ""), keys.get("h", ""))
            )

    lines = ["# placement geometry (cols,rows,width_px,height_px), distinct, sorted"]
    for p in sorted(set(placements)):
        lines.append("placement " + ",".join(p))
    lines.append(f"distinct_placements {len(set(placements))}")
    lines.append("# wire capability")
    lines.append("formats " + ",".join(sorted(formats)))
    lines.append("transports " + ",".join(sorted(transports)))
    grid_cols = os.environ.get("DIGEST_GRID_COLS", "")
    crop = int(grid_cols) if grid_cols.isdigit() and int(grid_cols) > 0 else None
    lines.append(f"# decoded grid (first {crop} columns)" if crop else "# decoded grid")
    try:
        with open(grid_path, encoding="utf-8", errors="replace") as fh:
            for row in fh.read().splitlines():
                if crop is not None:
                    row = row[:crop]
                lines.append("row " + row.rstrip())
    except FileNotFoundError:
        lines.append("row <no decoded grid>")
    return "\n".join(lines) + "\n"


def main() -> int:
    if len(sys.argv) != 5:
        print(__doc__, file=sys.stderr)
        return 2
    mode, capture, grid, target = sys.argv[1:5]
    got = digest(capture, grid)

    if mode == "write":
        with open(target, "w", encoding="utf-8") as fh:
            fh.write(got)
        print(f"wrote digest -> {target} ({len(got.splitlines())} lines)")
        return 0

    if mode != "check":
        print(f"unknown mode {mode}", file=sys.stderr)
        return 2

    try:
        want = open(target, encoding="utf-8").read()
    except FileNotFoundError:
        print(f"NO BASELINE at {target}.")
        print("Run this job once with BASELINE_WRITE=1, take the digest printed")
        print("below, and commit it. Until then there is nothing to compare against.")
        print("--- digest begins ---")
        print(got, end="")
        print("--- digest ends ---")
        return NO_BASELINE

    if got == want:
        print(f"digest matches baseline {target}")
        return 0

    import difflib

    print(f"DIGEST DRIFT against {target}", file=sys.stderr)
    diff = difflib.unified_diff(
        want.splitlines(), got.splitlines(), "baseline", "captured", lineterm="", n=2
    )
    shown = 0
    for line in diff:
        print(line, file=sys.stderr)
        shown += 1
        if shown > 200:
            print("... (truncated)", file=sys.stderr)
            break
    print(
        "\nIf this change is intended, re-run with BASELINE_WRITE=1 and commit the "
        "new digest along with the change that caused it.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
