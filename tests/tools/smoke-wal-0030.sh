#!/bin/bash
# WAL opt-in smoke (entries 4 and 5 of optin-p4.sh), exits and test lines.
cd "$(cd "$(dirname "$0")/../.." && pwd)"
export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib DYLD_FALLBACK_LIBRARY_PATH=/Library/Developer/CommandLineTools/usr/lib
T=target/tester
ROCKSVISION_LARGE_DIR=/private/tmp/rocksvision-large cargo test --release --locked --target-dir $T --test large_gen -- --ignored --nocapture --test-threads=1 assert_large_wal_search_cancel > /private/tmp/rocksvision-0030/smoke4.txt 2>&1
echo "assert_large_wal_search_cancel exit=$?"; grep -E "^test |test result" /private/tmp/rocksvision-0030/smoke4.txt | tail -3
RV_WAL=/private/tmp/rocksvision-large/large-wal.log cargo test --release --locked --target-dir $T --test ui_contract -- --ignored --nocapture rv_time_search > /private/tmp/rocksvision-0030/smoke5.txt 2>&1
echo "rv_time_search exit=$?"; grep -E "^test |test result|rv_time|ms" /private/tmp/rocksvision-0030/smoke5.txt | tail -4 | cut -c1-200
