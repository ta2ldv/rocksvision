#!/usr/bin/env python3
"""Shrinks the README preview PNGs in place (Pillow). Usage (project root):
    python3 -I scripts/png-optimize.py [--quantize N] screenshots/*.png
Lossless by default (max zlib level, optimized filters). --quantize N converts to an N-colour
palette (lossy; fine for flat UI captures, check textured themes by eye). Prints bytes before/after.
"""
import sys
from PIL import Image

args = sys.argv[1:]
colors = 0
if args and args[0] == "--quantize":
    colors = int(args[1])
    args = args[2:]
total_before = total_after = 0
for path in args:
    before = len(open(path, "rb").read())
    im = Image.open(path)
    im.load()
    if colors:
        im = im.convert("RGB").quantize(colors=colors, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    im.save(path, format="PNG", optimize=True, compress_level=9)
    after = len(open(path, "rb").read())
    total_before += before
    total_after += after
    print(f"{path:40s} {before:9d} -> {after:8d}")
print(f"total {total_before} -> {total_after} bytes")
