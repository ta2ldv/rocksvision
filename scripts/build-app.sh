#!/bin/sh
# Compatibility wrapper: the macOS (Apple Silicon) build now lives in build-macos-arm64.sh and
# writes dist/macos-arm64/RocksVision.app (formerly dist/RocksVision.app).
set -eu
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
"$dir/build-macos-arm64.sh" "$@"
echo "Note: scripts/build-app.sh is a wrapper for scripts/build-macos-arm64.sh; the bundle is now $(dirname -- "$dir")/dist/macos-arm64/RocksVision.app (no longer dist/RocksVision.app)."
