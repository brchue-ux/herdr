"""Emit herdr's exact escapes, plus the sub-cell X/Y placement offsets
`kitty_graphics::encode_display_placement` already knows how to write but never
sets for a sidebar card layer.

Same upload and display control as blend-test/emit.py; the only addition is the
optional `,X=` / `,Y=` this probe exists to measure.
"""
import base64
import sys

KITTY_CHUNK_BYTES = 3072


def upload(image_id, path):
    from PIL import Image
    data = open(path, "rb").read()
    w, h = Image.open(path).size
    control = f"a=t,t=d,f=100,s={w},v={h},i={image_id},q=2"
    out = []
    chunks = [data[i:i + KITTY_CHUNK_BYTES] for i in range(0, len(data), KITTY_CHUNK_BYTES)]
    for n, chunk in enumerate(chunks):
        more = 1 if n < len(chunks) - 1 else 0
        b64 = base64.standard_b64encode(chunk).decode()
        head = f"\x1b_G{control},m={more};" if n == 0 else f"\x1b_Gm={more};"
        out.append(f"{head}{b64}\x1b\\")
    return "".join(out)


def display(image_id, placement_id, row, col, cols, rows, z, x_off, y_off):
    """`cols`/`rows` of 0 omit `c`/`r` entirely, which is the "let the image keep
    its native size" case this probe also has to measure."""
    cup = f"\x1b[{row + 1};{col + 1}H"
    ctl = f"a=p,i={image_id},p={placement_id},z={z},C=1,q=2"
    if cols and rows:
        ctl = f"a=p,i={image_id},p={placement_id},c={cols},r={rows},z={z},C=1,q=2"
    if x_off:
        ctl += f",X={x_off}"
    if y_off:
        ctl += f",Y={y_off}"
    return f"{cup}\x1b_G{ctl};\x1b\\"


if __name__ == "__main__":
    out_path = sys.argv[1]
    buf = ["\x1b[2J\x1b[H\x1b[?25l"]
    for n, spec in enumerate(sys.argv[2:], start=1):
        png, row, col, cols, rows, z, x_off, y_off = spec.split(":")
        buf.append(upload(n, png))
        buf.append(display(n, n, int(row), int(col), int(cols), int(rows),
                           int(z), int(x_off), int(y_off)))
    buf.append("\x1b[40;1H")
    open(out_path, "w").write("".join(buf))
    print(f"wrote {out_path}")
