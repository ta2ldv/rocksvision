#!/usr/bin/env bash
# CI gate for release-triggered builds (.github/workflows/release-build.yml): the published
# release for <tag> MUST exist (it is the trigger) and NONE of the given asset names may already
# be attached to it (assets are never overwritten). (File name kept from W1: "absent" = assets.)
# Usage: GH_TOKEN=... scripts/ci/check-release-absent.sh <owner/repo> <tag> <asset-name>...
# Fail-closed: any answer other than "published release found, assets absent" (404, draft,
# network error, 401/403, 5xx, rate limit, unparsable JSON) stops the workflow; the log says which.
set -euo pipefail
repo=${1:-}
tag=${2:-}
[ -n "$repo" ] && [ -n "$tag" ] && [ "$#" -ge 3 ] || {
  echo "usage: check-release-absent.sh <owner/repo> <tag> <asset-name>..." >&2
  exit 2
}
shift 2
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

# get <url> -> prints HTTP status; body in $tmp/body. Network failure -> exit 3.
get() {
  local code rc=0
  code=$(curl -sS --retry 2 --max-time 30 -o "$tmp/body" -w '%{http_code}' \
    -H "Authorization: Bearer $GH_TOKEN" -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' "$1") || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "error: network/transport failure (curl exit $rc) for $1; failing closed" >&2
    exit 3
  fi
  printf '%s' "$code"
}

# 1) The published release for the tag (this endpoint never returns drafts).
code=$(get "$api/repos/$repo/releases/tags/$tag")
case "$code" in
  200) ;;
  404)
    echo "error: no published release for $tag (HTTP 404); this workflow only adds assets to an existing published release" >&2
    exit 1
    ;;
  *)
    echo "error: GitHub API answered HTTP $code for releases/tags/$tag (permission, rate limit or server error); failing closed" >&2
    exit 3
    ;;
esac
jq -e --arg t "$tag" 'type == "object" and .tag_name == $t and (.id | type) == "number"' "$tmp/body" >/dev/null || {
  echo "error: unexpected release response for $tag; failing closed" >&2
  exit 3
}
if [ "$(jq -r '.draft' "$tmp/body")" != false ]; then
  echo "error: release for $tag is a draft, not published; stopping" >&2
  exit 1
fi
# Immutable releases (GitHub setting): assets cannot be added after publication. The `immutable`
# field name is taken from the GitHub REST release schema (to verify); absent -> "unknown".
if [ "$(jq -r '.immutable // "unknown"' "$tmp/body")" = true ]; then
  echo "error: release $tag is immutable; assets cannot be uploaded after publication (turn immutable releases off for this flow)" >&2
  exit 1
fi
id=$(jq -r '.id' "$tmp/body")
echo "published release for $tag: found (id $id)"

# 2) Existing assets of that release (paginated); fail if any wanted name is already present.
: >"$tmp/names"
page=1
while :; do
  code=$(get "$api/repos/$repo/releases/$id/assets?per_page=100&page=$page")
  if [ "$code" != 200 ]; then
    echo "error: GitHub API answered HTTP $code listing assets (page $page); failing closed" >&2
    exit 3
  fi
  n=$(jq 'if type == "array" then length else error("not an array") end' "$tmp/body") || {
    echo "error: unexpected asset list response (page $page); failing closed" >&2
    exit 3
  }
  jq -r '.[].name' "$tmp/body" >>"$tmp/names"
  [ "$n" -eq 100 ] || break
  page=$((page + 1))
  [ "$page" -le 20 ] || {
    echo "error: more than 2000 assets listed; failing closed" >&2
    exit 3
  }
done
clash=0
for a in "$@"; do
  if grep -Fxq -- "$a" "$tmp/names"; then
    echo "error: asset $a already exists on release $tag; never overwritten" >&2
    clash=1
  fi
done
[ "$clash" -eq 0 ] || exit 1
echo "release $tag: none of the $# asset names exist yet ($(wc -l <"$tmp/names" | tr -d ' ') other assets present)"
