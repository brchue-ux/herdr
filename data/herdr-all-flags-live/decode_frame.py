#!/usr/bin/env python3
"""A copy of ../../herdr-tree-line-wires/proof/decode_frame.py with two fixes
needed to read *this* task's captures correctly, neither of which is a Herdr
bug:

1. Grapheme/width handling. The failure spider's glyph is a two-codepoint,
   double-width grapheme (spider + VS16), and the original decoder advances
   the cursor one column per *codepoint*, not per grapheme — it would count
   the VS16 as its own column and desync every later write on the row.
2. An off-by-one in OSC-sequence skipping: the original always advanced one
   past where the inner loop already left `j`, which is correct when an OSC
   is BEL-terminated but eats the leading ESC of whatever follows an
   ST-terminated one (`\\x1b\\\\`) — the form Herdr emits before every
   animated cell update here. That ate the very next escape sequence's ESC,
   turning a real cursor move into literal printed text and leaving the
   position it should have erased still lit — which read as a stuck trail of
   spiders one per animation frame until this was found and fixed. Confirmed
   by inspecting the raw byte deltas directly (see the PR description); the
   Rust side was correct throughout, only this proof tool was not."""
import sys, re, unicodedata

VS16 = "️"


def char_width(ch):
    if ch == VS16:
        return 0
    # crude but sufficient: treat the spider (and any other symbol/emoji
    # codepoint that commonly renders wide) as width 2, everything else the
    # normal East-Asian-width-aware width used elsewhere in this repo's docs.
    if 0x1F300 <= ord(ch) <= 0x1FAFF or 0x2600 <= ord(ch) <= 0x27BF:
        return 2
    if unicodedata.east_asian_width(ch) in ("W", "F"):
        return 2
    return 1


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
                        if code in (2, 3):
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
            elif i + 1 < n and raw[i + 1] == "_":
                # APC, always ST-terminated (ESC \). Kitty graphics ride this:
                # ESC _ G <controls> ; <payload> ESC \. Without this branch the
                # generic two-byte skip below eats only "ESC _" and the whole
                # control string plus its base64 payload lands in the grid as
                # literal text, burying the actual rendered rows. Graphics are
                # analysed separately by analyse.py; here they are structure to
                # step over, not content.
                j = i + 2
                while j < n:
                    if raw[j] == "\x1b" and j + 1 < n and raw[j + 1] == "\\":
                        j += 2
                        break
                    if raw[j] == "\x07":
                        j += 1
                        break
                    j += 1
                i = j
                continue
            elif i + 1 < n and raw[i + 1] == "]":
                # OSC, terminated by BEL (consume it) or ST == ESC \ (already
                # consumed by the inner break) -- the original single-cell
                # decoder this is based on always added one more, which ate
                # the leading ESC of whatever followed an ST-terminated OSC.
                j = i + 2
                terminated_by_st = False
                while j < n and raw[j] != "\x07":
                    if raw[j] == "\x1b" and j + 1 < n and raw[j + 1] == "\\":
                        j += 2
                        terminated_by_st = True
                        break
                    j += 1
                i = j if terminated_by_st else j + 1
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
            w = char_width(ch)
            if w == 0:
                # combining/zero-width: fold into the cell just written
                if c > 0 and r < rows:
                    grid[r][c - 1] += ch
                i += 1
                continue
            if c < cols and r < rows:
                grid[r][c] = ch
                if w == 2 and c + 1 < cols:
                    grid[r][c + 1] = ""
            c += w
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
