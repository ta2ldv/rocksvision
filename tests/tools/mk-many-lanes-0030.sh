#!/bin/bash
# Builds fixture-gen (target/tester) and generates the 0030 scratch fixture many-lanes (40 overlapping L0 files);
# keeps many-sst (600) / many-wal from /private/tmp/rocksvision-wal0026 (regenerates them only if missing).
# usage: bash tests/tools/mk-many-lanes-0030.sh
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
export DYLD_FALLBACK_LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib
cargo build --manifest-path tests/fixture-gen/Cargo.toml --target-dir target/tester 2>&1 | tail -3
fg="$root/target/tester/debug/fixture-gen"
out=/private/tmp/rocksvision-0030/many-lanes
"$fg" many-sst "$out" --files 40 --overlap
echo "many-lanes: $(ls "$out" | grep -c '\.sst$') sst, $(ls "$out" | grep -c '\.log$') wal"
w=/private/tmp/rocksvision-wal0026
if [ -d "$w/many-sst" ]; then echo "many-sst kept: $(ls "$w/many-sst" | grep -c '\.sst$') sst"; else "$fg" many-sst "$w/many-sst" --files 600; fi
if [ -d "$w/many-wal" ]; then echo "many-wal kept: $(ls "$w/many-wal" | grep -c '\.log$') wal, $(ls "$w/many-wal" | grep -c '\.sst$') sst"; else echo "many-wal MISSING: run tests/tools/mk-wal-fixtures-0026.sh"; fi
ls "$out" | head -5
