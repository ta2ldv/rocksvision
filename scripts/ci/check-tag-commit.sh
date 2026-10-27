#!/usr/bin/env bash
# CI gate (.github/workflows/release-build.yml, "Release packages"): the tag on GitHub must still
# point to the commit this run built (github.sha). Catches a tag that was moved/re-pushed after
# the run started, so a release never gets assets built from another commit. Read-only GET;
# GitHub peels annotated tags (GET /repos/<repo>/commits/tags/<tag> -> commit sha).
# Usage: GH_TOKEN=... scripts/ci/check-tag-commit.sh <owner/repo> <tag> <commit-sha>
# Exit: 0 tag -> commit-sha; 1 tag missing (404/422) or points elsewhere; 2 usage;
#       3 API/network/unparsable answer (fail-closed).
set -euo pipefail
repo=${1:-}
tag=${2:-}
want=${3:-}
[ "$#" -eq 3 ] && [ -n "$repo" ] && [ -n "$tag" ] && [[ $want =~ ^[0-9a-f]{40}$ ]] || {
  echo "usage: check-tag-commit.sh <owner/repo> <tag> <40-hex commit sha>" >&2
  exit 2
}
[ -n "${GH_TOKEN:-}" ] || {
  echo "error: GH_TOKEN is not set" >&2
  exit 2
}
command -v jq >/dev/null 2>&1 || {
  echo "error: jq is required" >&2
  exit 2
}
api=${GITHUB_API_URL:-https://api.github.com}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
rc=0
code=$(curl -sS --retry 2 --max-time 30 -o "$tmp/body" -w '%{http_code}' \
  -H "Authorization: Bearer $GH_TOKEN" -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' "$api/repos/$repo/commits/tags/$tag") || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "error: network/transport failure (curl exit $rc) resolving tag $tag; failing closed" >&2
  exit 3
fi
case "$code" in
  200) ;;
  404 | 422)
    echo "error: tag $tag does not exist on GitHub (HTTP $code); push the tag first" >&2
    exit 1
    ;;
  *)
    echo "error: GitHub API answered HTTP $code resolving tag $tag; failing closed" >&2
    exit 3
    ;;
esac
got=$(jq -r 'if type == "object" and (.sha | type) == "string" then .sha else error("no sha") end' "$tmp/body") || {
  echo "error: unexpected commit response for tag $tag; failing closed" >&2
  exit 3
}
[ "$got" = "$want" ] || {
  echo "error: tag $tag now points to $got, but this run built $want (tag moved?); stopping" >&2
  exit 1
}
echo "tag $tag -> $want (matches the built commit)"
