#!/usr/bin/env bash
# CI gate for tag runs (.github/workflows/release-build.yml, "Release packages": a pushed tag or
# a manual run on a tag ref). Checks the tag against Cargo.toml of the CHECKED-OUT commit (the
# workflow checks out github.sha, i.e. the tagged commit).
# Usage: scripts/ci/validate-tag.sh <tag>
# Exit: 0  strict stable vMAJOR.MINOR.PATCH (no leading zeros, no suffix), >= 0.1.1 and equal to
#          the package version in Cargo.toml -> release path
#       10 strict tag but older than 0.1.1 (v0.1.0 and older are never processed) -> the
#          workflow treats this as a NEUTRAL SKIP (validate passes, nothing else runs)
#       1  not a strict vMAJOR.MINOR.PATCH tag (typo such as v0.1.02, suffix), tag != Cargo.toml
#          version, or no version in Cargo.toml -> FAILURE (nothing else runs)
set -euo pipefail
tag=${1:-}
root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd -P)
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

[[ $tag =~ ^v(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$ ]] ||
  die "tag '$tag' is not a strict stable vMAJOR.MINOR.PATCH tag (no leading zeros, no suffix); nothing is built or released"
maj=${BASH_REMATCH[1]} min=${BASH_REMATCH[2]} pat=${BASH_REMATCH[3]}
# Minimum 0.1.1: numeric comparison (each part <= 9 digits, so no overflow).
if ((maj == 0 && (min == 0 || (min == 1 && pat < 1)))); then
  printf 'skip: tag %s is below the minimum 0.1.1 (v0.1.0 and older are never processed); nothing is built or released\n' "$tag"
  exit 10
fi
cargo_ver=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
[ -n "$cargo_ver" ] || die "package version not found in Cargo.toml"
[ "v$cargo_ver" = "$tag" ] || die "tag $tag does not match Cargo.toml version $cargo_ver at this commit; bump Cargo.toml, commit, and tag that commit"
printf 'tag %s: format ok, >= 0.1.1, matches Cargo.toml version %s\n' "$tag" "$cargo_ver"
