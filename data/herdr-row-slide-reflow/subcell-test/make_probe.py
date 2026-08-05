"""A probe image whose every pixel row is identifiable, so a screenshot says
exactly which source rows landed where.

Opaque so the measurement is about geometry and nothing else: 240x120 px, which
is 20 cols x 5 rows at the 12x24 px cell the harness fixes. A 4 px magenta rule
on the very top row and a 4 px yellow rule on the very bottom, with a green
band every 24 px (one per cell row) in between.
"""

from PIL import Image

W, H = 240, 120
img = Image.new("RGBA", (W, H), (20, 20, 60, 255))
px = img.load()

for y in range(H):
    for x in range(W):
        if y < 4:
            px[x, y] = (255, 0, 255, 255)      # top rule
        elif y >= H - 4:
            px[x, y] = (255, 255, 0, 255)      # bottom rule
        elif y % 24 < 2:
            px[x, y] = (0, 255, 0, 255)        # one rule per cell boundary

img.save("probe.png")
print("wrote probe.png", img.size)
