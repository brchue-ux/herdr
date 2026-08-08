"""Measurement helpers shared by the live compositing assertions.

Everything here reads a *screenshot of a real terminal window*, so it answers
"what did the terminal draw" rather than "what did herdr send". That is the
whole point of this rig: the byte-level check in `data/herdr-all-flags-live/`
already proves the right bytes reach the wire, and its own README names the gap
it cannot close — how the terminal composites what it is sent.

Two properties are measured, one per regression class that shipped:

* **Legibility over a drawn background.** Text sitting in the negative `z`
  band's shadow must stay separable from the pixels immediately around it. When
  a host ignores the band the wash lands on top and ink and paper become the
  same image, which is exactly what #96 fixed.
* **Motion.** A surface that declares an animation must actually differ between
  consecutive frames. #94 and #97 were both invisible to `cargo test` and both
  were found by watching a real Kitty client over several seconds.

Pillow only — no numpy, no OpenCV. Every operation below is a whole-image
Pillow call rather than a Python loop, so the CI job installs one Debian package
and a 1600x1000 frame still measures in milliseconds.
"""

from __future__ import annotations

from PIL import Image, ImageChops, ImageDraw, ImageStat

# sRGB -> linear-light, as a 256-entry lookup table scaled back to 0..255.
#
# Undoing gamma before weighting matters: the judgement here is a contrast
# threshold, and blending or averaging in sRGB space understates dark text and
# overstates bright text by enough to move a verdict.
_LINEAR_LUT = [
    round(255.0 * ((c / 255.0 / 12.92) if c / 255.0 <= 0.04045 else (((c / 255.0 + 0.055) / 1.055) ** 2.4)))
    for c in range(256)
]

# Rec. 709 / WCAG relative-luminance weights, as Pillow's RGB->L matrix.
_LUMA_MATRIX = (0.2126, 0.7152, 0.0722, 0.0)


def load(path: str) -> Image.Image:
    """Load a screenshot as an RGB image."""
    return Image.open(path).convert("RGB")


def luma(img: Image.Image) -> Image.Image:
    """Relative luminance as an "L" image, 0..255 standing for 0..1."""
    return img.point(_LINEAR_LUT * 3).convert("L", _LUMA_MATRIX)


def contrast_ratio(l1: float, l2: float) -> float:
    """WCAG contrast ratio between two 0..1 relative luminances, order-free."""
    hi, lo = (l1, l2) if l1 >= l2 else (l2, l1)
    return (hi + 0.05) / (lo + 0.05)


def frac_box(size: tuple[int, int], spec: str) -> tuple[int, int, int, int]:
    """Parse `x0,y0,x1,y1` in 0..1 fractions into pixel coordinates.

    Fractions rather than pixels because the window size is a property of the
    runner, and an assertion that hardcodes pixel offsets silently starts
    measuring the wrong rectangle the first time the geometry moves.
    """
    w, h = size
    parts = [float(p) for p in spec.split(",")]
    if len(parts) != 4:
        raise ValueError(f"expected x0,y0,x1,y1 fractions, got {spec!r}")
    x0, y0, x1, y1 = parts
    box = (
        max(0, min(w, int(round(x0 * w)))),
        max(0, min(h, int(round(y0 * h)))),
        max(0, min(w, int(round(x1 * w)))),
        max(0, min(h, int(round(y1 * h)))),
    )
    if box[2] <= box[0] or box[3] <= box[1]:
        raise ValueError(f"empty box {box} from {spec!r} on {w}x{h}")
    return box


def threshold(gray: Image.Image, lo: int, hi: int = 255) -> Image.Image:
    """Binary "L" mask (0 or 255) of pixels with `lo` < value <= `hi`."""
    return gray.point(lambda v: 255 if lo < v <= hi else 0)


def mask_count(mask: Image.Image) -> int:
    """How many pixels a 0/255 "L" mask has set."""
    return mask.histogram()[255]


def changed_mask(a: Image.Image, b: Image.Image, level: int = 24) -> Image.Image:
    """Per-pixel "this moved" mask: max absolute channel difference over a floor.

    Max-channel rather than luminance, because a hue-only change — a badge going
    from Active blue to Attention peach at the same brightness — is real motion
    that a luminance difference would miss.
    """
    diff = ImageChops.difference(a, b)
    r, g, bl = diff.split()
    peak = ImageChops.lighter(ImageChops.lighter(r, g), bl)
    return threshold(peak, level)


def clear_box(mask: Image.Image, box: tuple[int, int, int, int]) -> Image.Image:
    """Zero a rectangle out of a mask, in place, and hand it back."""
    ImageDraw.Draw(mask).rectangle(box, fill=0)
    return mask


def median_under(gray: Image.Image, mask: Image.Image) -> float:
    """Median of an "L" image under a mask, returned as 0..1."""
    return ImageStat.Stat(gray, mask).median[0] / 255.0


def _profile(mask: Image.Image, axis: str) -> list[float]:
    """Mean mask coverage per row (`axis="y"`) or per column (`axis="x"`).

    A box-filtered resize to a one-pixel-wide strip is exactly a per-row mean,
    computed in Pillow's C rather than in a Python loop over a megapixel.
    """
    w, h = mask.size
    if axis == "y":
        strip = mask.resize((1, h), Image.BOX)
        return [strip.getpixel((0, y)) for y in range(h)]
    strip = mask.resize((w, 1), Image.BOX)
    return [strip.getpixel((x, 0)) for x in range(w)]


def find_text_block(
    ref: Image.Image,
    search: tuple[int, int, int, int],
    ink_luma: int = 115,
    share: float = 0.15,
    inset: int = 2,
) -> tuple[int, int, int, int]:
    """Locate the probe block by brightness alone, inside `search`.

    The fallback for a scene with no distinctive backdrop colour, and what the
    synthetic controls used before they painted one. Prefer
    [`find_block_by_color`] wherever the probe can choose its own background.
    """
    return _block_from_mask(
        threshold(luma(ref.crop(search)), ink_luma),
        search,
        share,
        inset,
        "bright pixels",
    )


def color_mask(img: Image.Image, rgb: tuple[int, int, int], tol: int) -> Image.Image:
    """Binary "L" mask of pixels within `tol` of `rgb` on every channel."""
    bands = img.split()
    hit = None
    for band, want in zip(bands, rgb):
        near = band.point(lambda v, w=want: 255 if abs(v - w) <= tol else 0)
        hit = near if hit is None else ImageChops.multiply(hit, near)
    return hit


def _block_from_mask(
    mask: Image.Image,
    search: tuple[int, int, int, int],
    share: float,
    inset: int,
    what: str,
) -> tuple[int, int, int, int]:
    """Tightest rectangle over the rows and columns a mask actually fills.

    Rows and columns count only when their coverage reaches `share` of the
    busiest row/column. That threshold is what discards pane borders and the tab
    strip: a one-pixel rule spans the whole search area but contributes a single
    pixel to each row it crosses, while a line of the probe contributes hundreds.
    """
    x0, y0, _x1, _y1 = search
    if mask_count(mask) == 0:
        raise ValueError(f"no {what} in the search area — the probe never drew")

    rows = _profile(mask, "y")
    cols = _profile(mask, "x")
    row_floor = max(1.0, max(rows) * share)
    col_floor = max(1.0, max(cols) * share)
    keep_rows = [i for i, v in enumerate(rows) if v >= row_floor]
    keep_cols = [i for i, v in enumerate(cols) if v >= col_floor]
    if not keep_rows or not keep_cols:
        raise ValueError(f"{what} present but no solid rows/columns of it")

    return (
        x0 + keep_cols[0] + inset,
        y0 + keep_rows[0] + inset,
        x0 + keep_cols[-1] + 1 - inset,
        y0 + keep_rows[-1] + 1 - inset,
    )


def find_block_by_color(
    ref: Image.Image,
    search: tuple[int, int, int, int],
    rgb: tuple[int, int, int],
    tol: int = 20,
    share: float = 0.15,
    inset: int = 3,
) -> tuple[int, int, int, int]:
    """Locate the probe block by the exact cell background colour it paints.

    Preferred over the brightness heuristic, because it needs to know nothing
    about where anything is. The alternative is bounding a bright region and
    hoping the sidebar is outside it — and the sidebar's own width in pixels
    depends on the cell size, which depends on whichever font and DPI the runner
    resolves. A search rectangle chosen against the wrong cell width does not
    error; it silently measures the wrong pixels.

    The colour is read from the *reference* pass only. With the scene on, the
    wash sits above the cell background and below the text by design, so the
    probe's own backdrop is legitimately gone in the candidate — which is the
    thing being measured, not a detection failure.
    """
    return _block_from_mask(
        color_mask(ref.crop(search), rgb, tol),
        search,
        share,
        inset,
        f"pixels near rgb{rgb}",
    )


def describe_box(box: tuple[int, int, int, int]) -> str:
    x0, y0, x1, y1 = box
    return f"({x0},{y0})-({x1},{y1})  {x1 - x0}x{y1 - y0}px"
