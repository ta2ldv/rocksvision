#!/bin/bash
# Engine check of single SST files with fixture-gen --verify (which has no single-file mode).
# From the project root:  bash tests/tools/engine-check-sst.sh <WORKDIR> <sst> [<sst> ...]
# Copies tests/fixtures to <WORKDIR>/fx, drops the given SSTs into fx/corrupt/ (named ext-<basename>), runs
# `fixture-gen --verify fx scratch` and prints only the ext-* lines. A "PASS ext-X" means the engine REJECTED X
# (the corrupt/ section expects damage); "FAIL ext-X" means the engine accepted it (valid file / damage undetected).
set -u
W=$1; shift
rm -rf "$W/fx" "$W/scratch"
mkdir -p "$W"
cp -R tests/fixtures "$W/fx"
for f in "$@"; do cp "$f" "$W/fx/corrupt/ext-$(basename "$f")"; done
bash tests/tools/large-fixtures.sh --verify "$W/fx" "$W/scratch" 2>&1 | grep -E "ext-|panicked|Error"
