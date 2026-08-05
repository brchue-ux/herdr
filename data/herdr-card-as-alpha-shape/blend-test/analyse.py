"""Read the screenshots and decide: did the terminal composite, or did one win?

The test is arithmetic, not eyeballing. Each card's interior is alpha 0.30 ink
over whatever is behind it. In the overlap the amber interior sits over the cyan
interior, so source-over predicts a specific third colour that is nowhere near
either card alone -- and "the top one won" predicts amber's colour exactly. Those
are far enough apart that the pixel decides it with no judgement call.

Two source-over models are checked, because which space the terminal blends in
is itself unknown and changes the predicted numbers:

  sRGB-space   blend the 8-bit values directly
  linear-light un-gamma to linear, blend, re-gamma

Geometry is measured off the screenshot rather than assumed: the placement was
made in cells and the terminal scales the image to them, so the host cell size
decides where the pixels actually landed.
"""

import glob
import os
import sys

from PIL import Image

CYAN = (64, 224, 255)
AMBER = (255, 128, 64)
FILL_ALPHA = 0.30


def to_linear(c8):
    c = c8 / 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def to_srgb8(lin):
    c = 12.92 * lin if lin <= 0.0031308 else 1.055 * (lin ** (1 / 2.4)) - 0.055
    return round(max(0.0, min(1.0, c)) * 255)


def over_srgb(src, dst, a):
    return tuple(round(s * a + d * (1 - a)) for s, d in zip(src, dst))


def over_linear(src, dst, a):
    return tuple(
        to_srgb8(to_linear(s) * a + to_linear(d) * (1 - a)) for s, d in zip(src, dst)
    )


def bbox_of(img, predicate):
    """Tightest box over pixels satisfying predicate, or None."""
    w, h = img.size
    px = img.load()
    xs, ys = [], []
    for y in range(0, h, 2):
        for x in range(0, w, 2):
            if predicate(px[x, y]):
                xs.append(x)
                ys.append(y)
    if not xs:
        return None
    return min(xs), min(ys), max(xs), max(ys)


def classify(px, cyan_only, amber_only):
    """Which hypothesis does this overlap pixel match?

    Both stacking orders are offered, because z decides which card is the source
    and which the destination -- matching one order rather than the other is how
    we tell that z was honoured at all.
    """
    cands = {
        "BLENDS amber-over-cyan (linear)": over_linear(AMBER, cyan_only, FILL_ALPHA),
        "BLENDS cyan-over-amber (linear)": over_linear(CYAN, amber_only, FILL_ALPHA),
        "BLENDS amber-over-cyan (sRGB)": over_srgb(AMBER, cyan_only, FILL_ALPHA),
        "BLENDS cyan-over-amber (sRGB)": over_srgb(CYAN, amber_only, FILL_ALPHA),
        "TOP WINS (amber replaced cyan)": amber_only,
        "BOTTOM WINS (amber never drew)": cyan_only,
    }
    best, bestd = None, 1e9
    for name, ref in cands.items():
        d = max(abs(a - b) for a, b in zip(px[:3], ref))
        if d < bestd:
            best, bestd = name, d
    return best, bestd, cands


def probe(path):
    img = Image.open(path).convert("RGB")
    lit = bbox_of(img, lambda p: sum(p) > 24)
    if lit is None:
        return None
    x0, y0, x1, y1 = lit

    # Cyan is the upper-left card, amber the lower-right; they overlap in the
    # middle. Sample interiors well away from every stroke and bloom.
    cyan_pt = (x0 + 30, y0 + 30)
    amber_pt = (x1 - 30, y1 - 30)
    # Overlap: right/lower part of cyan's box that amber also covers. Amber's
    # own box starts where the lit region's second card begins; take the
    # midpoint of the two card origins, nudged inside both.
    ov_pt = ((x0 + x1) // 2 - 34, (y0 + y1) // 2)

    cyan_only = img.getpixel(cyan_pt)
    amber_only = img.getpixel(amber_pt)
    ov = img.getpixel(ov_pt)
    return lit, cyan_pt, cyan_only, amber_pt, amber_only, ov_pt, ov


if __name__ == "__main__":
    pattern = sys.argv[1] if len(sys.argv) > 1 else "shots/*.png"
    for path in sorted(glob.glob(pattern)):
        r = probe(path)
        name = os.path.basename(path)
        if r is None:
            print(f"{name}: nothing drew")
            continue
        lit, cpt, c, apt, a, opt, ov = r
        verdict, dist, cands = classify(ov, c, a)
        print(f"=== {name}   lit box {lit}")
        print(f"    cyan-only  at {cpt} = {c}")
        print(f"    amber-only at {apt} = {a}")
        print(f"    overlap    at {opt} = {ov}")
        for n, ref in cands.items():
            mark = "<--" if n == verdict else "   "
            d = max(abs(p - q) for p, q in zip(ov[:3], ref))
            print(f"      {mark} predicts {str(ref):>18} for {n}   (max err {d})")
        print(f"    VERDICT: {verdict}  (max channel error {dist})")
        print()
