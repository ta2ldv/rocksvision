#!/bin/bash
# Scratch folders for the 0024 acceptance run (all under /private/tmp/rocksvision-0024/scratch).
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
s=/private/tmp/rocksvision-0024/scratch
mkdir -p "$s"
rm -rf "$s/lone" "$s/nocurrent" "$s/nomanifest" "$s/missing-sst"
mkdir "$s/lone" "$s/nocurrent"
cp "$root/tests/fixtures/db-checkpoint/000009.sst" "$s/lone/"
cp "$root/tests/fixtures/db-checkpoint/000016.log" "$s/nocurrent/"
cp "$root/tests/fixtures/db-checkpoint/OPTIONS-000007" "$s/nocurrent/"
cp -R "$root/tests/fixtures/db-checkpoint" "$s/nomanifest"
rm "$s/nomanifest/MANIFEST-000005"
cp -R "$root/tests/fixtures/db-checkpoint" "$s/missing-sst"
rm "$s/missing-sst/000013.sst"
ls -R "$s"
