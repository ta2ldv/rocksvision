#!/bin/bash
# Dump every regular file of tests/fixtures and examples (except README/SHA256SUMS/MANIFEST.md) with <binary> into <outdir>.
#   bash tests/tools/dump-all.sh <binary> <outdir>      (run from the project root)
set -u
BIN=$1; OUT=$2
rm -rf "$OUT"; mkdir -p "$OUT"
n=0
for root in tests/fixtures examples; do
  find "$root" -type f ! -name '*.md' ! -name SHA256SUMS ! -name .DS_Store ! -path 'examples/prior-work/*' | LC_ALL=C sort | while read -r f; do
    "$BIN" --dump "$f" --limit 5000 --rows 300 > "$OUT/$(echo "$f" | tr '/' '_').txt" 2>&1
  done
done
ls "$OUT" | wc -l
