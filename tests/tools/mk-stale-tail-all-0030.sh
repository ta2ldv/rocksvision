#!/bin/bash
# SYNTHETIC scratch fixture (not shipped): stale tail on an EMPTY newest WAL, every CF flushed.
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
export DYLD_FALLBACK_LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib
cargo build --manifest-path tests/fixture-gen/Cargo.toml --target-dir target/tester 2>&1 | tail -2
out=/private/tmp/rocksvision-0030/stale-tail-all
target/tester/debug/fixture-gen wal-stale-tail "$out" --flush-all | tail -1
ls "$out"
bin=dist/macos-arm64/RocksVision.app/Contents/MacOS/rocksvision
"$bin" --dump "$out/CURRENT" --rows 40 2>&1 | grep -i "Layout\|CF |\|default\|cf_" | cut -c1-200 | head -12
for l in "$out"/*.log; do echo "--- $l"; "$bin" --dump "$l" --rows 30 2>&1 | grep -i "stale\|WriteBatch #" | cut -c1-200 | head -3; done
