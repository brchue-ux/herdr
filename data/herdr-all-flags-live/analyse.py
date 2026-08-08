#!/usr/bin/env python3
"""Summarise what a captured herdr client stream actually contains.

This is the assertion half of the live capture: rather than eyeballing a
screenshot, it reads the raw bytes and reports which mechanisms actually
reached the wire. Kitty graphics travel as APC blocks (`ESC _ G <keys> ; <payload> ESC \\`),
so the control keys are the evidence for which of the pixel-path flags are live:

  a=t/a=T  transmit (T = transmit and display)
  a=p      put/display an already-transmitted image
  a=f      transmit an animation frame
  a=a      control animation (arm/play)
  t=d      payload inline in the escape stream (base64)
  t=f      payload is a local file path  -> kitty_graphics_local_transport
  f=24/32  raw RGB / RGBA pixels         -> terminal-aware pixel format
  f=100    PNG

usage: analyse.py <capture-file> [<capture-file>...]
"""
import re
import sys

APC = re.compile(rb"\x1b_G([^;\x1b]*)(?:;([^\x1b]*))?\x1b\\", re.S)


def controls(blob: bytes):
    out = []
    for m in APC.finditer(blob):
        keys = {}
        for pair in m.group(1).split(b","):
            if b"=" in pair:
                k, _, v = pair.partition(b"=")
                keys[k.decode("ascii", "replace")] = v.decode("ascii", "replace")
        out.append((keys, len(m.group(2) or b"")))
    return out


def summarise(path: str) -> None:
    blob = open(path, "rb").read()
    blocks = controls(blob)
    print(f"\n=== {path} ===")
    print(f"bytes on the wire      : {len(blob)}")
    print(f"kitty APC blocks       : {len(blocks)}")
    if not blocks:
        print("  (no graphics reached the wire — pixel path inactive)")
    else:
        def tally(key):
            counts = {}
            for keys, _ in blocks:
                if key in keys:
                    counts[keys[key]] = counts.get(keys[key], 0) + 1
            return counts

        print(f"  actions   a=  : {tally('a') or '(default t)'}")
        print(f"  medium    t=  : {tally('t') or '(default d)'}")
        print(f"  format    f=  : {tally('f') or '(default 32)'}")
        print(f"  image ids I/i : {len(tally('i')) + len(tally('I'))} distinct")
        payload = sum(n for _, n in blocks)
        print(f"  payload bytes : {payload}")
        anim = sum(1 for k, _ in blocks if k.get("a") in ("f", "a"))
        print(f"  animation frames/control blocks : {anim}")

    # Non-graphics evidence.
    sgr = len(re.findall(rb"\x1b\[[0-9;]*m", blob))
    print(f"SGR colour sequences   : {sgr}")
    truecolor = len(re.findall(rb"\x1b\[[34]8;2;", blob))
    print(f"  of which truecolor   : {truecolor}")


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    for path in sys.argv[1:]:
        summarise(path)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
