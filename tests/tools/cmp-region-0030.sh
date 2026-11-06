#!/bin/bash
# usage: bash tests/tools/cmp-region-0030.sh <png-a> <png-b> [x y w h in screen points; default the app window 164 76 1400 900]
# Crops both FULL-resolution shots to the region (factor 2 px per pt) and compares sha256 (excludes the menu-bar clock).
set -u
a="$1"; b="$2"; x="${3:-164}"; y="${4:-76}"; w="${5:-1400}"; h="${6:-900}"
t=/private/tmp/rocksvision-0030/cmp
mkdir -p "$t"
f=2
for n in a b; do
  src=$a; [ "$n" = b ] && src=$b
  sips --cropToHeightWidth $((h*f)) $((w*f)) --cropOffset $((y*f)) $((x*f)) "$src" --out "$t/$n.png" >/dev/null
done
sa=$(shasum -a 256 "$t/a.png" | cut -c1-16); sb=$(shasum -a 256 "$t/b.png" | cut -c1-16)
if [ "$sa" = "$sb" ]; then echo "IDENTICAL $sa"; else echo "DIFFERENT $sa $sb"; fi
