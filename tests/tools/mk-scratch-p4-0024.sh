#!/bin/bash
# Scratch dirs for the 0024 phase-4 GUI re-check (under /private/tmp/rocksvision-0024/scratch).
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
s=/private/tmp/rocksvision-0024/scratch
mkdir -p "$s"
rm -rf "$s/badcurrent" "$s/levels-copy" "$s/missing-sst"
mkdir "$s/badcurrent"
cp "$root/tests/fixtures/db-levels/MANIFEST-000005" "$s/badcurrent/"
printf 'xx' > "$s/badcurrent/CURRENT"
cp -R "$root/tests/fixtures/db-levels" "$s/levels-copy"
cp -R "$root/tests/fixtures/db-checkpoint" "$s/missing-sst"
rm "$s/missing-sst/000013.sst"
ls -R "$s/badcurrent" "$s/levels-copy"
ls "$s/missing-sst"
