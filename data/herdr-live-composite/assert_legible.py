#!/usr/bin/env python3
"""Assert that text stays readable once a background is drawn behind it.

This is the #96 regression class, and it is the one PR #98's own body names as
out of reach for a byte-level check: the bytes were always right — `z=-2` and
all — and the failure was entirely in what the terminal did with them.

Two screenshots of the same scene, differing only in whether the background is
on:

    reference   background off — text on the theme's own backdrop
    candidate   background on  — the same text, with the wash behind it

The reference supplies the ink/paper masks. Asking the candidate for its own
masks would be circular: an image that has covered the text produces perfectly
good-looking masks of *itself*.

Three things are checked, and all three have to hold:

1. **The background actually drew.** Without this the check is vacuous — a
   feature that is silently off passes a legibility test trivially, which is
   how a green tick ends up meaning nothing.
2. **Ink and paper are still separable**, by WCAG contrast ratio between the two
   clusters' median luminance *in the candidate*. A wash drawn on top makes both
   clusters the same pixels, so the ratio collapses toward 1.0.
3. **The separation is per-pixel, not just on average.** A ratio can survive by
   accident when a scene happens to be bright exactly where the glyphs are;
   requiring the candidate to re-derive the reference's own mask closes that.

The *direction* of the contrast is deliberately not constrained. herdr's
per-cell legibility pass may invert text over a bright part of the scene, and
dark ink on a light wash is still legible — what must never happen is ink and
paper becoming indistinguishable.
"""

from __future__ import annotations

import argparse
import sys

from PIL import ImageChops

import composite_lib as lib


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("reference", help="screenshot with the background OFF")
    ap.add_argument("candidate", help="screenshot with the background ON")
    ap.add_argument(
        "--search",
        default="0.30,0.04,1.0,0.45",
        help="fractional x0,y0,x1,y1 to hunt the probe text in (default: the "
        "upper pane area, right of the sidebar)",
    )
    ap.add_argument(
        "--block-color",
        default=None,
        help="R,G,B of the cell background the probe paints. Given, the block is "
        "found by that colour in the reference and nothing has to be assumed "
        "about where the sidebar ends — which depends on a cell size that "
        "depends on the runner's font and DPI. Omitted, brightness is used",
    )
    ap.add_argument("--block-tolerance", type=int, default=20)
    ap.add_argument("--ink-luma", type=int, default=115, help="0..255 on the luma image")
    ap.add_argument("--paper-luma", type=int, default=50)
    ap.add_argument("--min-ink-px", type=int, default=3000)
    ap.add_argument("--min-paper-px", type=int, default=3000)
    ap.add_argument(
        "--coverage-region",
        default="0.0,0.0,1.0,1.0",
        help="where to look for proof the background drew. Narrow this to an "
        "area whose only difference between the two passes IS the background: "
        "a sidebar that animates in both passes would otherwise supply "
        "'coverage' that has nothing to do with the feature under test",
    )
    ap.add_argument(
        "--min-bg-coverage",
        type=float,
        default=0.15,
        help="fraction of the coverage region outside the text block that must "
        "differ between the two shots, i.e. proof the background is really drawn",
    )
    ap.add_argument("--min-contrast", type=float, default=3.0)
    ap.add_argument("--min-agreement", type=float, default=0.80)
    ap.add_argument(
        "--expect-fail",
        action="store_true",
        help="invert the exit code; used by the detector self-test so a known-bad "
        "scene proves the assertion can fail at all",
    )
    args = ap.parse_args()

    ref = lib.load(args.reference)
    cand = lib.load(args.candidate)
    if ref.size != cand.size:
        print(f"FAIL: geometry differs, {ref.size} vs {cand.size}", file=sys.stderr)
        return 1

    search = lib.frac_box(ref.size, args.search)
    try:
        if args.block_color:
            rgb = tuple(int(v) for v in args.block_color.split(","))
            if len(rgb) != 3:
                raise ValueError(f"--block-color wants R,G,B, got {args.block_color!r}")
            box = lib.find_block_by_color(ref, search, rgb, tol=args.block_tolerance)
        else:
            box = lib.find_text_block(ref, search, ink_luma=args.ink_luma)
    except ValueError as err:
        print(f"FAIL: {err}", file=sys.stderr)
        return 0 if args.expect_fail else 1

    ref_l = lib.luma(ref.crop(box))
    cand_l = lib.luma(cand.crop(box))
    ink = lib.threshold(ref_l, args.ink_luma)
    paper = lib.threshold(ref_l, -1, args.paper_luma)
    ink_px = lib.mask_count(ink)
    paper_px = lib.mask_count(paper)

    print(f"text block: {lib.describe_box(box)}")
    print(f"  ink pixels   {ink_px}")
    print(f"  paper pixels {paper_px}")
    if ink_px < args.min_ink_px or paper_px < args.min_paper_px:
        print(
            f"FAIL: the probe text is not on screen in the shape this check expects "
            f"({ink_px} ink / {paper_px} paper), so nothing below would mean anything",
            file=sys.stderr,
        )
        return 0 if args.expect_fail else 1

    failures: list[str] = []

    # 1. Did the background actually draw?
    cov_region = lib.frac_box(ref.size, args.coverage_region)
    changed = lib.clear_box(lib.changed_mask(ref, cand, level=16), box)
    outside = changed.crop(cov_region)
    x0, y0, x1, y1 = box
    cx0, cy0, cx1, cy1 = cov_region
    overlap_w = max(0, min(x1, cx1) - max(x0, cx0))
    overlap_h = max(0, min(y1, cy1) - max(y0, cy0))
    total_outside = (cx1 - cx0) * (cy1 - cy0) - overlap_w * overlap_h
    if total_outside <= 0:
        print("FAIL: the coverage region is entirely inside the text block", file=sys.stderr)
        return 0 if args.expect_fail else 1
    coverage = lib.mask_count(outside) / float(total_outside)
    print(f"  coverage region: {lib.describe_box(cov_region)}")
    print(f"  background coverage outside the text block: {coverage:.3f}")
    if coverage < args.min_bg_coverage:
        failures.append(
            f"background covers only {coverage:.3f} of the frame "
            f"(< {args.min_bg_coverage}) — it is not drawn, so this check proves nothing"
        )

    # 2. Are the two clusters still separable in the candidate?
    l_ink = lib.median_under(cand_l, ink)
    l_paper = lib.median_under(cand_l, paper)
    ratio = lib.contrast_ratio(l_ink, l_paper)
    print(f"  candidate median luma: ink {l_ink:.4f}  paper {l_paper:.4f}")
    print(f"  contrast ratio: {ratio:.2f}:1")
    if ratio < args.min_contrast:
        failures.append(
            f"contrast collapsed to {ratio:.2f}:1 (< {args.min_contrast}:1) — ink and "
            "paper are the same pixels, i.e. the background is drawn over the text"
        )

    # 3. Does the candidate reproduce the reference's own mask, per pixel?
    mid = int(round((l_ink + l_paper) / 2.0 * 255))
    cand_ink = (
        lib.threshold(cand_l, mid)
        if l_ink > l_paper
        else lib.threshold(cand_l, -1, mid)
    )
    considered = ImageChops.lighter(ink, paper)
    disagreed = ImageChops.logical_xor(
        cand_ink.convert("1"), ink.convert("1")
    ).convert("L")
    disagreed = ImageChops.multiply(disagreed, considered)
    considered_px = lib.mask_count(considered)
    agreement = 1.0 - lib.mask_count(disagreed) / float(considered_px)
    print(f"  per-pixel mask agreement: {agreement:.3f}")
    if agreement < args.min_agreement:
        failures.append(
            f"only {agreement:.3f} of pixels keep their ink/paper identity "
            f"(< {args.min_agreement}) — the glyph shapes are gone"
        )

    if failures:
        print("", file=sys.stderr)
        for f in failures:
            print(f"FAIL: {f}", file=sys.stderr)
        return 0 if args.expect_fail else 1

    print("PASS: text stays legible over the drawn background")
    if args.expect_fail:
        print("FAIL: this scene was supposed to be caught and was not", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
