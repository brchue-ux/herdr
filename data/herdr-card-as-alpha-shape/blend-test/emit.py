"""Emit Kitty graphics escapes byte-for-byte the way herdr's encoder does.

Mirrors `src/kitty_graphics.rs`:
  upload   \x1b_Ga=t,t=d,f=100,s=W,v=H,i=ID,q=2,m=N;<b64>\x1b\\
  display  \x1b[row;colH  then  \x1b_Ga=p,i=ID,p=PID,c=C,r=R,z=Z,C=1,q=2;\x1b\\

Same chunk size (3072 raw bytes), same q=2, same C=1. If the real encoder's
behaviour differed from this the measurement would not transfer.
"""

import base64
import sys

KITTY_CHUNK_BYTES = 3072


def upload(image_id, path):
    data = open(path, "rb").read()
    from PIL import Image

    w, h = Image.open(path).size
    control = f"a=t,t=d,f=100,s={w},v={h},i={image_id},q=2"
    out = []
    chunks = [
        data[i : i + KITTY_CHUNK_BYTES] for i in range(0, len(data), KITTY_CHUNK_BYTES)
    ]
    for n, chunk in enumerate(chunks):
        more = 1 if n < len(chunks) - 1 else 0
        b64 = base64.standard_b64encode(chunk).decode()
        if n == 0:
            out.append(f"\x1b_G{control},m={more};{b64}\x1b\\")
        else:
            out.append(f"\x1b_Gm={more};{b64}\x1b\\")
    return "".join(out)


def display(image_id, placement_id, row, col, cols, rows, z):
    """row/col are 0-based; herdr writes CUP as y+1;x+1."""
    cup = f"\x1b[{row + 1};{col + 1}H"
    ctl = f"a=p,i={image_id},p={placement_id},c={cols},r={rows},z={z},C=1,q=2"
    return f"{cup}\x1b_G{ctl};\x1b\\"


if __name__ == "__main__":
    # argv: outfile  then triples of  png:row:col:cols:rows:z
    out_path = sys.argv[1]
    # Clear, and hide the cursor: it blinks, so leaving it on puts a lit block
    # in some screenshots and not others and corrupts the measured bounding box.
    buf = ["\x1b[2J\x1b[H\x1b[?25l"]
    for n, spec in enumerate(sys.argv[2:], start=1):
        png, row, col, cols, rows, z = spec.split(":")
        buf.append(upload(n, png))
        buf.append(
            display(n, n, int(row), int(col), int(cols), int(rows), int(z))
        )
    buf.append("\x1b[40;1H")  # park the cursor far from the images
    open(out_path, "w").write("".join(buf))
    print(f"wrote {out_path}")
