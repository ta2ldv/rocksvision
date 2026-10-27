#!/bin/bash
# Wall time / max RSS of `rocksvision --dump` on the large fixtures (warm cache). From the project root:
#   bash tests/tools/large-measure.sh [binary] [dir]
# defaults: target/tester/release/rocksvision  /private/tmp/rocksvision-large
# Deliberately `set -u` only (no -e / pipefail): `grep -c` exits 1 when it counts 0 (bad=0 is the good case)
# and the loop must keep going when a single dump fails; failures show up as the printed numbers.
set -u
BIN=${1:-target/tester/release/rocksvision}
D=${2:-/private/tmp/rocksvision-large}
for f in large-wal.log large-sst.sst; do
  for a in "--limit 50" "--limit 400 --rows 400"; do
    for run in 1 2 3; do
      /usr/bin/time -l "$BIN" --dump "$D/$f" $a > "$D/out.txt" 2> "$D/err.txt"
      echo "$f [$a] run$run: $(grep real "$D/err.txt" | awk '{print $1}')s real, $(grep 'maximum resident' "$D/err.txt" | awk '{printf "%.1f MiB", $1/1048576}') maxRSS, out $(wc -c < "$D/out.txt" | tr -d ' ') B, bad=$(grep -c -E 'Invalid|Truncated' "$D/out.txt")"
    done
    grep -m3 -E "note:|Data blocks|entries" "$D/out.txt" | cut -c1-160
  done
done
