#!/usr/bin/env bash
# CI gate for tag-triggered release builds (.github/workflows/release-build.yml).
# Usage: scripts/ci/validate-tag.sh <tag>
# Passes only for a strict stable tag vMAJOR.MINOR.PATCH (no leading zeros, no suffix) whose
# version is >= 0.1.1 and equals the package version in Cargo.toml. v0.1.0 (the existing
# manually published release) and older tags always fail.
set -euo pipefail
tag=${1:-}
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd -P)
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ $tag =~ ^v(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$ ]] ||
  die "tag '$tag' is not a strict stable vMAJOR.MINOR.PATCH tag"
maj=${BASH_REMATCH[1]} min=${BASH_REMATCH[2]} pat=${BASH_REMATCH[3]}
# Minimum 0.1.1: numeric comparison (each part <= 9 digits, so no overflow).
if ((maj == 0 && (min == 0 || (min == 1 && pat < 1)))); then
  die "tag $tag is below the minimum 0.1.1 for this workflow (v0.1.0 and older are never processed)"
fi
cargo_ver=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
[ -n "$cargo_ver" ] || die "package version not found in Cargo.toml"
[ "v$cargo_ver" = "$tag" ] || die "tag $tag does not match Cargo.toml version $cargo_ver"
printf 'tag %s: format ok, >= 0.1.1, matches Cargo.toml version %s\n' "$tag" "$cargo_ver"
