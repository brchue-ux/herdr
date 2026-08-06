"""Measure where the probe's rules landed, per shot.

The probe's top 4 px are magenta and its bottom 4 px are yellow, so the first
magenta scanline says where the image's top edge went and the last yellow one
says where its bottom edge went. A true sub-cell translation moves both by the
offset and keeps the height; a clip moves the top and holds the bottom; a scale
moves the top and changes the height.
"""
import glob
import sys

from PIL import Image

MAGENTA = (255, 0, 255)
YELLOW = (255, 255, 0)


def near(px, want, tol=40):
    return all(abs(a - b) <= tol for a, b in zip(px[:3], want))


def scan(path):
    img = Image.open(path).convert("RGB")
    w, h = img.size
    px = img.load()
    top = bottom = None
    top_rows = 0
    bottom_rows = 0
    for y in range(h):
        row_mag = sum(1 for x in range(w) if near(px[x, y], MAGENTA))
        row_yel = sum(1 for x in range(w) if near(px[x, y], YELLOW))
        if row_mag > 100:
            if top is None:
                top = y
            top_rows += 1
        if row_yel > 100:
            if bottom is None:
                bottom = y
            bottom_rows += 1
    return top, top_rows, bottom, bottom_rows


if __name__ == "__main__":
    pattern = sys.argv[1] if len(sys.argv) > 1 else "shots/*.png"
    base = None
    print(f"{'case':10} {'top_y':>6} {'top_px':>7} {'bot_y':>6} {'bot_px':>7} {'height':>7}")
    for path in sorted(glob.glob(pattern)):
        name = path.rsplit("/", 1)[-1].removesuffix(".png")
        top, top_rows, bottom, bottom_rows = scan(path)
        height = (bottom + bottom_rows - top) if (top is not None and bottom is not None) else None
        print(f"{name:10} {str(top):>6} {top_rows:>7} {str(bottom):>6} {bottom_rows:>7} {str(height):>7}")
        if name == "y0":
            base = (top, height)
    if base:
        print(f"\nbaseline y0: top={base[0]} height={base[1]}")
