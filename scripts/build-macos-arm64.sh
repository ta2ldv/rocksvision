#!/bin/sh
# One command: builds dist/macos-arm64/RocksVision.app on an Apple Silicon Mac (macOS 11+),
# plus BUILD-INFO.txt, THIRD-PARTY-LICENSES.md, SHA256SUMS and
# dist/rocksvision-<version>-macos-arm64.zip (+ .sha256). See docs/BUILD.md.
# Usage: scripts/build-macos-arm64.sh   (from any directory; paths with spaces are fine)
set -eu
RV_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
. "$RV_SCRIPT_DIR/lib/common.sh"
. "$RV_SCRIPT_DIR/lib/macos-bundle.sh"
rv_init
rv_require_host Darwin arm64
# Native host build (no --target): the binary is target/release/rocksvision.
rv_macos_build macos-arm64 "" aarch64-apple-darwin
