#!/usr/bin/env bash
# Release step of .github/workflows/release-build.yml ("Release packages"), run only after all
# four platform builds passed. The ONLY GitHub write paths:
#   release ABSENT  -> gh release create <tag> --verify-tag (never creates a tag) with a minimal
#                      generated body (version, commit, run link) and the eight assets
#   release PRESENT -> gh release upload <tag> <eight assets> (no --clobber); the existing
#                      release's title, notes, draft/latest flags are never edited
#   anything else   -> stop (draft, immutable, asset name exists, API error; see
#                      check-release-ready.sh), nothing written
# Before any write: the tag on GitHub must still point to the built commit (check-tag-commit.sh).
# After the write: every asset must be attached to a published release (--verify-uploaded).
# Usage: GH_TOKEN=... scripts/ci/publish-release.sh <owner/repo> <tag> <commit-sha> <asset-dir>
# Exit: 0 created or uploaded and verified; 2 usage/missing asset file; otherwise the failing
# helper's/gh's exit code (1 not usable, 3 API error, gh's own code on a failed write).
set -euo pipefail
repo=${1:-}
tag=${2:-}
sha=${3:-}
dir=${4:-}
[ "$#" -eq 4 ] && [ -n "$repo" ] && [ -n "$tag" ] && [ -n "$sha" ] && [ -d "$dir" ] || {
  echo "usage: publish-release.sh <owner/repo> <tag> <commit-sha> <asset-dir>" >&2
  exit 2
}
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
v=${tag#v}
names=()
files=()
for f in "rocksvision-$v-macos-arm64.zip" "rocksvision-$v-macos-x64.zip" \
  "rocksvision-$v-ubuntu-x64.tar.gz" "rocksvision-$v-windows-x64.zip"; do
  for n in "$f" "$f.sha256"; do
    [ -f "$dir/$n" ] || {
      echo "error: $dir/$n missing" >&2
      exit 2
    }
    names+=("$n")
    files+=("$dir/$n")
  done
done
summary() { if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then printf '%s\n' "$@" >>"$GITHUB_STEP_SUMMARY"; fi; }

bash "$here/check-tag-commit.sh" "$repo" "$tag" "$sha"
rc=0
bash "$here/check-release-ready.sh" "$repo" "$tag" "${names[@]}" || rc=$?
case "$rc" in
  0)
    action="uploaded the eight assets to the EXISTING release $tag (title and notes untouched)"
    gh release upload "$tag" --repo "$repo" "${files[@]}"
    ;;
  10)
    action="CREATED release $tag (minimal notes) with the eight assets"
    notes=$(mktemp)
    trap 'rm -f "$notes"' EXIT
    run_url=-
    if [ -n "${GITHUB_RUN_ID:-}" ]; then
      run_url="${GITHUB_SERVER_URL:-https://github.com}/$repo/actions/runs/$GITHUB_RUN_ID"
    fi
    {
      printf 'RocksVision %s (tag %s, commit %s).\n\n' "$v" "$tag" "$sha"
      printf 'Created by the "Build and release packages" workflow after the macOS arm64, macOS x64, Ubuntu x64 and Windows x64 builds and tests passed (run: %s).\n' "$run_url"
      printf 'Each archive has a .sha256 file. These notes may be edited on GitHub; later workflow runs never change an existing release.\n'
    } >"$notes"
    gh release create "$tag" --repo "$repo" --verify-tag --title "RocksVision $tag" \
      --notes-file "$notes" "${files[@]}"
    ;;
  *)
    summary "## Release packages: NOT published ($tag)" "" "Release check exited $rc; nothing was written. See the log."
    exit "$rc"
    ;;
esac
bash "$here/check-release-ready.sh" --verify-uploaded "$repo" "$tag" "${names[@]}"
echo "$action"
summary "## Release packages: $action" "" "Commit: $sha" ""
for n in "${names[@]}"; do summary "- $n"; done
