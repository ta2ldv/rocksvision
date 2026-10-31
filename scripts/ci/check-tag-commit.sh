#!/usr/bin/env bash
# CI gate (.github/workflows/release-build.yml, "Release packages"): the tag on GitHub must still
# point to the commit this run built (github.sha). Catches a tag that was moved/re-pushed after
# the run started, so a release never gets assets built from another commit. Read-only GETs.
# Resolution works for lightweight AND annotated tags (task 0017 D8; GET /commits/tags/<tag>
# answered 422 "No commit found for SHA: tags/v0.1.2" for the annotated tag v0.1.2):
#   GET /repos/<repo>/git/ref/tags/<tag>   404 -> tag missing
#     object.type "commit" -> object.sha                          (lightweight tag)
#     object.type "tag"    -> GET /repos/<repo>/git/tags/<object.sha>, its object must be a
#                             commit -> object.sha                (annotated tag, peeled once)
# Usage: GH_TOKEN=... scripts/ci/check-tag-commit.sh <owner/repo> <tag> <commit-sha>
# Exit: 0 tag -> commit-sha; 1 tag missing (404), points elsewhere or not to a commit; 2 usage;
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

# get <what> <api path> <body file>: prints the HTTP code; exits 3 on a transport failure.
get() {
  local rc=0 code
  code=$(curl -sS --retry 2 --max-time 30 -o "$3" -w '%{http_code}' \
    -H "Authorization: Bearer $GH_TOKEN" -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' "$api/repos/$repo/$2") || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "error: network/transport failure (curl exit $rc) resolving $1; failing closed" >&2
    exit 3
  fi
  printf '%s' "$code"
}
# object <body file>: prints "<type> <sha>" of .object; exits 3 when the answer is not that shape.
object() {
  jq -r 'if type == "object" and (.object | type) == "object" and (.object.type | type) == "string"
           and (.object.sha | type) == "string" and (.object.sha | test("^[0-9a-f]{40}$"))
         then "\(.object.type) \(.object.sha)" else error("no object") end' "$1" || {
    echo "error: unexpected answer resolving tag $tag; failing closed" >&2
    exit 3
  }
}

code=$(get "tag $tag" "git/ref/tags/$tag" "$tmp/ref")
case "$code" in
  200) ;;
  404)
    echo "error: tag $tag does not exist on GitHub (HTTP 404); push the tag first" >&2
    exit 1
    ;;
  *)
    echo "error: GitHub API answered HTTP $code resolving tag $tag; failing closed" >&2
    exit 3
    ;;
esac
jq -e --arg r "refs/tags/$tag" '.ref == $r' "$tmp/ref" >/dev/null 2>&1 || {
  echo "error: unexpected ref answer for tag $tag (not refs/tags/$tag); failing closed" >&2
  exit 3
}
# Assign first: under set -e a failing $(...) in an assignment exits (3); in a here-string it would not.
obj=$(object "$tmp/ref")
read -r kind sha <<<"$obj"
case "$kind" in
  commit) got=$sha ;;
  tag)
    code=$(get "annotated tag object $sha of $tag" "git/tags/$sha" "$tmp/tagobj")
    [ "$code" = 200 ] || {
      echo "error: GitHub API answered HTTP $code reading annotated tag object $sha of $tag; failing closed" >&2
      exit 3
    }
    obj=$(object "$tmp/tagobj")
    read -r kind got <<<"$obj"
    [ "$kind" = commit ] || {
      echo "error: annotated tag $tag points to a $kind ($got), not a commit; stopping" >&2
      exit 1
    }
    ;;
  *)
    echo "error: tag $tag points to a $kind ($sha), not a commit; stopping" >&2
    exit 1
    ;;
esac
[ "$got" = "$want" ] || {
  echo "error: tag $tag now points to $got, but this run built $want (tag moved?); stopping" >&2
  exit 1
}
echo "tag $tag -> $want (matches the built commit)"
