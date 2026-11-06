#!/bin/bash
# usage: bash tests/tools/run-themes-0026.sh <pid> <outdir>; six-theme captures on multilevel-db cf_users (own PID)
set -u
pid="$1"; out="$2"
root="$(cd "$(dirname "$0")/../.." && pwd)"
d="$root/tests/tools/gui-drive.py"
env SHOT_DIR="$out" python3 -I "$d" --pid "$pid" script "$root/tests/tools/scripts/accept_0026_d2_theme_open.txt" 2>&1 | grep -i "abort\|guard"
for t in "wooden 474 340" "autistic 472 229" "matrix 469 252" "amigawb 505 274" "amigatool 500 296" "xcopy 470 318"; do
  set -- $t
  env SHOT_DIR="$out" python3 -I "$d" --pid "$pid" --set N="$1" --set TX="$2" --set TY="$3" script "$root/tests/tools/scripts/accept_0026_d2_theme_one.txt" 2>&1 | grep -i "abort\|guard"
done
echo done
