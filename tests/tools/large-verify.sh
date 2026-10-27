#!/bin/bash
# Phase-2 large-fixture verification. From the project root: bash tests/tools/large-verify.sh
# Generates the two ~100 MB fixtures if missing, then runs the ignored ASSERTION tests (not the generators) and the CLI measurements.
# Deliberately `set -u` only (no -e / pipefail): the test/build output is filtered through `grep`, which exits 1 when
# nothing matches, and each later step must still run and print its own result even if an earlier one failed.
set -u
D=${ROCKSVISION_LARGE_DIR:-/private/tmp/rocksvision-large}
T=target/tester
echo "src tree hash: $(find src Cargo.toml Cargo.lock -type f | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256 | cut -c1-16)"
[ -f "$D/large-sst.sst" ] || bash tests/tools/large-fixtures.sh large-sst "$D" --mb 100
[ -f "$D/large-wal.log" ] || bash tests/tools/large-fixtures.sh large-wal "$D" --mb 100
cargo build --release --locked --target-dir $T 2>&1 | tail -2
echo "binary: $(shasum -a 256 $T/release/rocksvision | cut -c1-16)"
ROCKSVISION_LARGE_DIR="$D" cargo test --release --locked --target-dir $T --test large_gen -- --ignored --nocapture --test-threads=1 assert_large 2>&1 | grep -E "LARGE-|CANCEL|test result|panicked|FAILED|error"
cargo test --release --locked --target-dir $T --test ui_contract -- --ignored --nocapture rv_time_search 2>&1 | grep -E "RV-SEARCH|test result|panicked|FAILED|error"
bash tests/tools/large-measure.sh $T/release/rocksvision "$D"
