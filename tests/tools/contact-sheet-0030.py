#!/usr/bin/env python3
"""usage: python3 -I tests/tools/contact-sheet-0030.py OUT.png X Y W H COLS IN1.png IN2.png ...
Crops each full-resolution shot to the region (screen points; 2 px per pt), downsizes to 50 %, labels with the file name, tiles COLS per row."""
import sys
from PIL import Image, ImageDraw

out, x, y, w, h, cols = sys.argv[1], *map(int, sys.argv[2:7])
files = sys.argv[7:]
tiles = []
for f in files:
    im = Image.open(f).convert("RGB").crop((x * 2, y * 2, (x + w) * 2, (y + h) * 2))
    im = im.resize((w, h))
    d = ImageDraw.Draw(im)
    d.rectangle((0, 0, w, 14), fill=(0, 0, 0))
    d.text((2, 1), f.split("/")[-1][:60], fill=(255, 255, 0))
    tiles.append(im)
rows = (len(tiles) + cols - 1) // cols
sheet = Image.new("RGB", (cols * (w + 4), rows * (h + 4)), (60, 60, 60))
for i, t in enumerate(tiles):
    sheet.paste(t, ((i % cols) * (w + 4), (i // cols) * (h + 4)))
sheet.save(out)
print(out, sheet.size)
