#!/bin/bash
# Prints the src-only and src+tests *.rs fingerprints (read-only).
set -eu
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
echo "src-only  $(find src -type f -name '*.rs' | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256)"
echo "src+tests $(find src tests -type f -name '*.rs' | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256)"
echo "binary    $(shasum -a 256 dist/macos-arm64/RocksVision.app/Contents/MacOS/rocksvision)"
