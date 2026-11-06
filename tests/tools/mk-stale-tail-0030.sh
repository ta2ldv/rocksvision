#!/bin/bash
# Builds fixture-gen (target/tester) and generates the SYNTHETIC scratch fixture /private/tmp/rocksvision-0030/stale-tail (not shipped).
# usage: bash tests/tools/mk-stale-tail-0030.sh
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
export DYLD_FALLBACK_LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib
cargo build --manifest-path tests/fixture-gen/Cargo.toml --target-dir target/tester 2>&1 | tail -3
out=/private/tmp/rocksvision-0030/stale-tail
target/tester/debug/fixture-gen wal-stale-tail "$out" --flush-default | tail -2
bin=dist/macos-arm64/RocksVision.app/Contents/MacOS/rocksvision
log=$(ls "$out"/*.log | tail -1)
echo "--- dump $log"
"$bin" --dump "$log" --rows 60 2>&1 | grep -i "stale\|old\|unknown\|engine\|recycl\|summary" | cut -c1-260 | head -12
echo "--- dump CURRENT"
"$bin" --dump "$out/CURRENT" --rows 40 2>&1 | grep -i "WAL\|old" | cut -c1-260 | head
