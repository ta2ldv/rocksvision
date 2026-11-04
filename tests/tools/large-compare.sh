#!/bin/bash
# Wall time of `--dump` on a large fixture for several binaries and option sets (5 runs each, warm cache).
#   bash tests/tools/large-compare.sh <file> <binary>...
set -u
F=$1; shift
for b in "$@"; do
  for a in "--limit 1" "--limit 1 --rows 1" "--limit 50" "--limit 400 --rows 400"; do
    t=""
    for run in 1 2 3 4 5; do
      s=$(/usr/bin/time -p "$b" --dump "$F" $a 2>&1 >/dev/null | awk '/real/ {print $2}')
      t="$t $s"
    done
    echo "$b [$a]:$t"
  done
done
