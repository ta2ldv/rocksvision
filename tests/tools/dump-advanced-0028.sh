#!/bin/bash
# Task 0013 / handoff 0028: read-only `rocksvision --dump` of every file of examples/data/multilevel-advanced-db
# with the packaged binary; writes one dump per file into OUT_DIR and prints the exit code per file.
# Usage: bash tests/tools/dump-advanced-0028.sh <OUT_DIR> [BINARY]
set -u
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:?usage: dump-advanced-0028.sh <OUT_DIR> [BINARY]}"
bin="${2:-$root/dist/macos-arm64/RocksVision.app/Contents/MacOS/rocksvision}"
dir="$root/examples/data/multilevel-advanced-db"
mkdir -p "$out"
fail=0
for f in "$dir"/* "$dir"; do
  name="$(basename "$f")"
  [ "$f" = "$dir" ] && name="DIR"
  timeout 60 "$bin" --dump "$f" --limit 5000 --rows 300 > "$out/dump-$name.txt" 2>&1
  rc=$?
  echo "$rc $name $(wc -l < "$out/dump-$name.txt") lines"
  [ "$rc" -ne 0 ] && fail=1
  grep -q -i "panic" "$out/dump-$name.txt" && { echo "PANIC in $name"; fail=1; }
done
exit $fail
