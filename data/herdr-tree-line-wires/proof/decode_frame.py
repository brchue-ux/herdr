#!/usr/bin/env python3
"""Minimal VT100 grid emulator: enough to read a ratatui full-redraw frame
back out as plain text rows. Not a full terminal emulator - CUP, basic
cursor motion, erase, SGR-strip, and printable text is all a ratatui app
needs."""
import sys, re

def render(raw, cols, rows):
    grid = [[" "] * cols for _ in range(rows)]
    r, c = 0, 0
    i = 0
    n = len(raw)
    csi_re = re.compile(r"\x1b\[([0-9;?]*)([A-Za-z])")
    while i < n:
        ch = raw[i]
        if ch == "\x1b":
            if i + 1 < n and raw[i + 1] == "[":
                m = csi_re.match(raw, i)
                if m:
                    params, final = m.group(1), m.group(2)
                    parts = [p for p in params.replace("?", "").split(";") if p != ""]
                    nums = [int(p) for p in parts if p.isdigit()]
                    if final in ("H", "f"):
                        row = (nums[0] - 1) if len(nums) > 0 else 0
                        col = (nums[1] - 1) if len(nums) > 1 else 0
                        r, c = max(0, row), max(0, col)
                    elif final == "A":
                        r = max(0, r - (nums[0] if nums else 1))
                    elif final == "B":
                        r = min(rows - 1, r + (nums[0] if nums else 1))
                    elif final == "C":
                        c = min(cols - 1, c + (nums[0] if nums else 1))
                    elif final == "D":
                        c = max(0, c - (nums[0] if nums else 1))
                    elif final == "J":
                        code = nums[0] if nums else 0
                        if code == 2 or code == 3:
                            grid = [[" "] * cols for _ in range(rows)]
                    elif final == "K":
                        code = nums[0] if nums else 0
                        if code == 0:
                            for cc in range(c, cols):
                                grid[r][cc] = " "
                        elif code == 1:
                            for cc in range(0, c + 1):
                                grid[r][cc] = " "
                        else:
                            for cc in range(cols):
                                grid[r][cc] = " "
                    i = m.end()
                    continue
                else:
                    i += 2
                    continue
            elif i + 1 < n and raw[i + 1] == "]":
                # OSC sequence: skip to BEL or ST
                j = i + 2
                while j < n and raw[j] not in ("\x07",):
                    if raw[j] == "\x1b" and j + 1 < n and raw[j + 1] == "\\":
                        j += 2
                        break
                    j += 1
                i = j + 1
                continue
            else:
                i += 2
                continue
        elif ch == "\r":
            c = 0
            i += 1
            continue
        elif ch == "\n":
            r = min(rows - 1, r + 1)
            i += 1
            continue
        else:
            if c < cols and r < rows:
                grid[r][c] = ch
            c += 1
            if c >= cols:
                c = 0
                r = min(rows - 1, r + 1)
            i += 1
    return ["".join(row).rstrip() for row in grid]


if __name__ == "__main__":
    cols, rows = int(sys.argv[1]), int(sys.argv[2])
    path = sys.argv[3]
    with open(path, "r", errors="replace") as f:
        raw = f.read()
    for line in render(raw, cols, rows):
        print(line)
