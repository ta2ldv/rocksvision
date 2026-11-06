#!/bin/bash
# The five opt-in (ignored) entries, each separately, output to a file (no pipe), Cargo exit recorded.
# Output: /private/tmp/rocksvision-0024/optin/N.txt ; summary on stdout.
set -u
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
o=/private/tmp/rocksvision-0024/optin
mkdir -p "$o"
T=target/tester

env ROCKSVISION_LARGE_DIR=/private/tmp/rocksvision-0024/synth-gen cargo test --release --locked --target-dir $T --test large_gen -- --ignored --nocapture --test-threads=1 gen_large_fixtures > "$o/1.txt" 2>&1
echo "1 large_gen::gen_large_fixtures exit=$?"
env RV_WAL=/private/tmp/rocksvision-0024/synth-rv/rv-synth.log cargo test --release --locked --target-dir $T --test ui_contract -- --ignored --nocapture rv_gen_wal > "$o/2.txt" 2>&1
echo "2 ui_contract::rv_gen_wal exit=$?"
env ROCKSVISION_LARGE_DIR=/private/tmp/rocksvision-large cargo test --release --locked --target-dir $T --test large_gen -- --ignored --nocapture --test-threads=1 assert_large_fixtures_bounded_dump > "$o/3.txt" 2>&1
echo "3 large_gen::assert_large_fixtures_bounded_dump exit=$?"
env ROCKSVISION_LARGE_DIR=/private/tmp/rocksvision-large cargo test --release --locked --target-dir $T --test large_gen -- --ignored --nocapture --test-threads=1 assert_large_wal_search_cancel > "$o/4.txt" 2>&1
echo "4 large_gen::assert_large_wal_search_cancel exit=$?"
env RV_WAL=/private/tmp/rocksvision-large/large-wal.log cargo test --release --locked --target-dir $T --test ui_contract -- --ignored --nocapture rv_time_search > "$o/5.txt" 2>&1
echo "5 ui_contract::rv_time_search exit=$?"
