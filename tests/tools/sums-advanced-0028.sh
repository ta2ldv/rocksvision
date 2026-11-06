#!/bin/bash
# Task 0013 / handoff 0028: append SHA-256 lines of examples/data/multilevel-advanced-db/* to
# examples/SHA256SUMS (format "<hex>  data/<scenario>/<file>", relative to examples/), unless already
# listed, then run `shasum -a 256 -c SHA256SUMS` from examples/.
# Usage: bash tests/tools/sums-advanced-0028.sh [--check-only]
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root/examples"
if [ "${1:-}" != "--check-only" ]; then
  if grep -q "  data/multilevel-advanced-db/" SHA256SUMS; then
    echo "already listed; not appending"
  else
    LC_ALL=C ls data/multilevel-advanced-db | while read -r f; do
      shasum -a 256 "data/multilevel-advanced-db/$f"
    done >> SHA256SUMS
    echo "appended $(grep -c '  data/multilevel-advanced-db/' SHA256SUMS) lines"
  fi
fi
total=$(wc -l < SHA256SUMS | tr -d ' ')
ok=$(shasum -a 256 -c SHA256SUMS | grep -c ': OK$' || true)
echo "shasum -c: $ok/$total OK"
[ "$ok" = "$total" ]
