#!/bin/bash
# Keeps the full-resolution README screenshots under screenshots/highres/<name>_highres.png and
# writes lighter previews to screenshots/<name>.png (the paths README.md embeds).
# Usage (project root):  bash scripts/make-screenshot-previews.sh [max-width-px]   (default 1728)
# Idempotent: an existing highres original is never overwritten; previews are regenerated from it.
set -euo pipefail
width=${1:-1728}
cd "$(dirname "$0")/.."
mkdir -p screenshots/highres
total_before=0
total_after=0
for png in screenshots/*.png; do
  name=$(basename "$png" .png)
  hi="screenshots/highres/${name}_highres.png"
  if [ ! -f "$hi" ]; then
    cp -p "$png" "$hi"
  fi
  total_before=$((total_before + $(stat -f %z "$hi")))
  sips -Z "$width" "$hi" --out "$png" >/dev/null
  total_after=$((total_after + $(stat -f %z "$png")))
  printf '%-28s %9d -> %8d bytes  %s\n' "$name" "$(stat -f %z "$hi")" "$(stat -f %z "$png")" "$(sips -g pixelWidth -g pixelHeight "$png" | awk '/pixel/ {printf "%s ", $2}')"
done
printf 'total highres %d bytes, previews %d bytes\n' "$total_before" "$total_after"
