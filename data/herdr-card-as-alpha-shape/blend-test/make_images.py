"""Two soft-edged glowing shapes with straight alpha, shaped like a herdr card.

Each is a rounded rect: transparent outside, a bright stroke on the boundary,
a soft bloom falling off outside it, and a dim fill inside. Exactly the shape
model the card is meant to become, so the blend answer transfers directly.
"""
import math, sys
from PIL import Image

def rr_distance(px, py, x, y, w, h, r):
    hx, hy = w / 2.0, h / 2.0
    r = max(0.0, min(r, hx, hy))
    dx = abs(px - (x + hx)) - (hx - r)
    dy = abs(py - (y + hy)) - (hy - r)
    outside = math.hypot(max(dx, 0.0), max(dy, 0.0))
    return outside + min(max(dx, dy), 0.0) - r

def card(w, h, rgb, bloom_reach=14.0, path="out.png"):
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    px = img.load()
    inset = bloom_reach
    rx, ry = inset, inset
    rw, rh = w - 2 * inset, h - 2 * inset
    rad = 12.0
    for yy in range(h):
        for xx in range(w):
            d = rr_distance(xx + 0.5, yy + 0.5, rx, ry, rw, rh, rad)
            a = 0.0
            if d > 0.0:                      # bloom outside the shape
                a = max(0.0, 1.0 - d / bloom_reach) ** 2 * 0.85
            elif d > -2.5:                   # the stroke: the shape's own edge
                a = 1.0
            else:                            # dim fill inside
                a = 0.30
            if a <= 0.0:
                continue
            px[xx, yy] = (rgb[0], rgb[1], rgb[2], int(round(min(1.0, a) * 255)))
    img.save(path)
    return path

if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    card(240, 120, (64, 224, 255), path=f"{out}/card_cyan.png")
    card(240, 120, (255, 128, 64), path=f"{out}/card_amber.png")
    print("wrote card_cyan.png card_amber.png")
