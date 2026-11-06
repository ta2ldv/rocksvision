#!/bin/bash
# usage: bash tests/tools/run-themes-0030.sh <pid> ; six themes on the open map (own PID), shots to final-d3/themes
set -u
pid="$1"
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/ai-collab/reports/0030/gui/final-d3/themes"
for t in "wooden 474 340" "autistic 472 229" "matrix 469 252" "amigawb 505 274" "amigatool 500 296" "xcopy 470 318"; do
  set -- $t
  env SHOT_DIR="$out" python3 -I "$root/tests/tools/gui-drive.py" --pid "$pid" --set N="$1" --set TX="$2" --set TY="$3" script "$root/tests/tools/scripts/d2_theme_one.txt" 2>&1 | grep -i "abort\|guard"
done
echo done
