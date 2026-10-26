#!/bin/sh
# One command: builds dist/macos-arm64/RocksVision.app on an Apple Silicon Mac (macOS 11+),
# plus BUILD-INFO.txt, LICENSE, THIRD-PARTY-LICENSES.md, SHA256SUMS and
# dist/rocksvision-<version>-macos-arm64.zip (+ .sha256). See docs/BUILD.md.
# Usage: scripts/build-macos-arm64.sh   (from any directory; paths with spaces are fine)
set -eu
RV_SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
. "$RV_SCRIPT_DIR/lib/common.sh"
. "$RV_SCRIPT_DIR/lib/macos-bundle.sh"
rv_init
rv_require_host Darwin arm64
# Native host build (no --target): the binary is target/release/rocksvision; rv_macos_build
# checks the rustc host and the Mach-O architecture (lipo), so a configured foreign target fails.
# No AppleDouble "._*" entries (extended attributes) in the archive.
export COPYFILE_DISABLE=1
rv_macos_build macos-arm64 "" aarch64-apple-darwin
# ditto -c -k (scripts/lib) still stores extended attributes (com.apple.provenance) as "._*"
# entries; rebuild the archive from the published folder without them (zip -X, no .DS_Store).
z="$ROOT/dist/$RV_EXE-$RV_VERSION-macos-arm64.zip"
(cd "$ROOT/dist" && rm -f "$z" && zip -X -r -y -q "$z" macos-arm64 -x '*.DS_Store')
rv_archive_sum "$z"
if unzip -Z1 "$z" | grep -q '\(^\|/\)\._'; then
  echo "error: $z contains AppleDouble ._ entries" >&2
  exit 1
fi
printf 'Archive without AppleDouble entries: %s\nSHA-256: %s\n' "$z" "$(cut -d' ' -f1 "$z.sha256")"
