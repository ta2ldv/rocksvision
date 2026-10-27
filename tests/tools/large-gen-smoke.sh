#!/bin/bash
# Smoke-run the two synthetic-WAL GENERATORS at 2 MiB into a scratch dir (never the 100 MB fixture dir). From the project root.
# pipefail: a failing cargo test aborts the script even though its output is filtered through grep.
set -euo pipefail
S=$(mktemp -d /private/tmp/rocksvision-gensmoke.XXXXXX)
trap 'rm -rf "$S"' EXIT
ROCKSVISION_LARGE_DIR="$S" ROCKSVISION_LARGE_MB=2 cargo test --release --locked --target-dir target/tester --test large_gen -- --ignored --nocapture gen_large_fixtures 2>&1 | grep -E "wrote|test result|panicked"
RV_WAL="$S/rv.log" RV_WAL_MB=2 cargo test --release --locked --target-dir target/tester --test ui_contract -- --ignored --nocapture rv_gen_wal 2>&1 | grep -E "RV-GEN|test result|panicked"
RV_WAL="$S/rv.log" RV_NEEDLE=ZZZ cargo test --release --locked --target-dir target/tester --test ui_contract -- --ignored --nocapture rv_time_search 2>&1 | grep -E "RV-SEARCH|test result|panicked"
ls -l "$S"
