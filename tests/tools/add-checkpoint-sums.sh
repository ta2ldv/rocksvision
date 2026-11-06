#!/bin/bash
# Appends the db-checkpoint/ files to tests/fixtures/SHA256SUMS (same format as the existing lines:
# `shasum -a 256` of paths relative to tests/fixtures), then verifies the whole file.
# usage: bash tests/tools/add-checkpoint-sums.sh
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root/tests/fixtures"
grep -q ' db-checkpoint/' SHA256SUMS && { echo "db-checkpoint already listed"; exit 1; }
for f in $(ls db-checkpoint | sort); do
  shasum -a 256 "db-checkpoint/$f" >> SHA256SUMS
done
wc -l SHA256SUMS
shasum -a 256 -c SHA256SUMS | grep -v ': OK$' || echo "all OK"
ls -l db-checkpoint
du -sk db-checkpoint
