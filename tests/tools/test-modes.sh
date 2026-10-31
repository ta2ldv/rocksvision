#!/usr/bin/env bash
# Runs the test suite in release, debug, release+harness and debug+harness and prints the
# summed passed/failed/ignored per mode (failing test names, if any).
# Usage: bash tests/tools/test-modes.sh [target-dir]   (default target/developer)
set -uo pipefail
td="${1:-target/developer}"
run_mode() {
  local name="$1"; shift
  local out rc=0
  out="$(cargo test --locked --target-dir "$td" "$@" 2>&1)" || rc=$?
  printf '%s\n' "$out" | awk -v n="$name" -v rc="$rc" '
    /^test result:/ { for (i = 1; i <= NF; i++) { if ($(i+1) ~ /^passed/) p += $i; if ($(i+1) ~ /^failed/) f += $i; if ($(i+1) ~ /^ignored/) g += $i } }
    / FAILED$/ { print "  FAILED: " $0 }
    END { printf "%-16s passed=%d failed=%d ignored=%d exit=%s\n", n, p, f, g, rc }'
}
run_mode release --release
run_mode debug
run_mode release+harness --release --features harness
run_mode debug+harness --features harness
