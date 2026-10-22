#!/bin/sh
# One command: builds dist/macos-x64/RocksVision.app (x86_64-apple-darwin, macOS 11+),
# plus BUILD-INFO.txt, THIRD-PARTY-LICENSES.md, SHA256SUMS and
# dist/rocksvision-<version>-macos-x64.zip (+ .sha256). See docs/BUILD.md.
# On an Intel Mac this is a native build. On an Apple Silicon Mac it is a cross build: the
# x86_64 standard library is added with rustup (`rustup target add x86_64-apple-darwin`); the
# result is marked "cross-built, not run" in BUILD-INFO.txt.
# Usage: scripts/build-macos-x64.sh
set -eu
RV_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
. "$RV_SCRIPT_DIR/lib/common.sh"
. "$RV_SCRIPT_DIR/lib/macos-bundle.sh"
rv_init
rv_require_host Darwin x86_64 arm64
if [ "$(uname -m)" = arm64 ]; then
  RV_NOTE="note: cross-built on an arm64 host, not run here
"
fi
rv_macos_build macos-x64 x86_64-apple-darwin x86_64-apple-darwin
