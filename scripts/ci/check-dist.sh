#!/usr/bin/env bash
# CI check of one platform's build output (after scripts/build-<platform>.*).
# Usage: scripts/ci/check-dist.sh <platform> <target triple>
# Verifies dist/<platform>/BUILD-INFO.txt (name, version from Cargo.toml, target), LICENSE and
# THIRD-PARTY-LICENSES.md, dist/<platform>/SHA256SUMS, and the archive + its .sha256 file.
# Prints the archive path on success (last line).
set -euo pipefail
platform=${1:-}
triple=${2:-}
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd -P)
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}
[ -n "$platform" ] && [ -n "$triple" ] || die "usage: check-dist.sh <platform> <target triple>"
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

name=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
ver=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
dist="$root/dist/$platform"
info="$dist/BUILD-INFO.txt"
[ -f "$info" ] || die "$info missing"
# Tolerate CRLF just in case; the scripts write LF.
field() { sed -n "s/^$1: //p" "$info" | tr -d '\r' | head -n1; }
[ "$(field name)" = "$name" ] || die "BUILD-INFO name '$(field name)' != $name"
[ "$(field version)" = "$ver" ] || die "BUILD-INFO version '$(field version)' != Cargo.toml $ver"
[ "$(field platform)" = "$platform" ] || die "BUILD-INFO platform '$(field platform)' != $platform"
[ "$(field target)" = "$triple" ] || die "BUILD-INFO target '$(field target)' != $triple"
for f in LICENSE THIRD-PARTY-LICENSES.md SHA256SUMS; do
  [ -f "$dist/$f" ] || die "$dist/$f missing"
done
cmp -s "$root/LICENSE" "$dist/LICENSE" || die "$dist/LICENSE differs from the repository LICENSE"
(cd "$dist" && tr -d '\r' <SHA256SUMS | while read -r h f; do
  [ "$(sha "$f")" = "$h" ] || die "SHA256SUMS mismatch for $f"
done)

archive=""
for ext in zip tar.gz; do
  [ -f "$root/dist/$name-$ver-$platform.$ext" ] && archive="$root/dist/$name-$ver-$platform.$ext"
done
[ -n "$archive" ] || die "archive dist/$name-$ver-$platform.{zip,tar.gz} missing"
[ -f "$archive.sha256" ] || die "$archive.sha256 missing"
read -r want fname <"$archive.sha256"
fname=${fname%$'\r'}
[ "$fname" = "$(basename "$archive")" ] || die "$archive.sha256 names '$fname'"
[ "$(sha "$archive")" = "$want" ] || die "$archive does not match its .sha256"
echo "dist ok: $platform $ver $triple"
grep -E '^(rustc|cargo|vcs|build|dll imports|runtime):' "$info" | tr -d '\r' || true
echo "$archive"
