#!/bin/bash
# Build tests/fixture-gen (own target dir) and run it. Run from the project root:
#   bash tests/tools/large-fixtures.sh large-sst /private/tmp/rocksvision-large --mb 100
#   bash tests/tools/large-fixtures.sh large-wal /private/tmp/rocksvision-large --mb 100
# macOS: the Command Line Tools libclang is used when present (needed by librocksdb-sys bindgen);
# elsewhere the environment's own LIBCLANG_PATH / library path applies unchanged.
set -euo pipefail
L=/Library/Developer/CommandLineTools/usr/lib
if [ -d "$L" ]; then
  export LIBCLANG_PATH="$L" DYLD_FALLBACK_LIBRARY_PATH="$L"
fi
cargo build --release --manifest-path tests/fixture-gen/Cargo.toml --target-dir tests/fixture-gen/target 2>&1 | tail -3
exec tests/fixture-gen/target/release/fixture-gen "$@"
