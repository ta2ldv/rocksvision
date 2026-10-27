#!/usr/bin/env bash
# CI gate for the release step of .github/workflows/release-build.yml ("Release packages": a
# pushed tag or a manual run on a tag ref). Read-only (GET requests only); fail-closed.
#
# Usage: GH_TOKEN=... scripts/ci/check-release-ready.sh <owner/repo> <tag> <asset-name>...
#   Decides the release path for <tag>:
#   exit 0  PRESENT: a published (non-draft) release exists, its REST answer says
#           "immutable": false and none of the asset names is attached -> UPLOAD-ONLY path
#           (the existing release's title/notes are never edited)
#   exit 10 ABSENT: no release (published, or draft visible to the token) for <tag> -> CREATE path
#   exit 1  NOT USABLE: a draft release for <tag>, immutable true, immutability not verifiable
#           (no boolean `immutable` field), or an asset name already exists (never overwritten)
#   exit 2  usage/setup
#   exit 3  API/network/unparsable answer (401/403, 5xx, rate limit, bad JSON, inconsistency)
#
# Usage: GH_TOKEN=... scripts/ci/check-release-ready.sh --verify-uploaded <owner/repo> <tag> <asset-name>...
#   After the upload: exit 0 when the release is published (not draft) and EVERY asset name is
#   attached with state "uploaded"; 1 when not; 2 usage; 3 API/network error.
#
# Drafts: GET releases/tags/<tag> never returns drafts, so a 404 is followed by a paginated list
# of all releases. Drafts are listed only for tokens with push access; a read-only token (the
# validate job) can therefore report ABSENT for a draft, the write token of the release job sees
# it and exits 1.
set -euo pipefail
mode=ready
if [ "${1:-}" = --verify-uploaded ]; then
  mode=verify
  shift
fi
repo=${1:-}
tag=${2:-}
[ -n "$repo" ] && [ -n "$tag" ] && [ "$#" -ge 3 ] || {
  echo "usage: check-release-ready.sh [--verify-uploaded] <owner/repo> <tag> <asset-name>..." >&2
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

# paginate <url-with-?> <out-file>: appends every element of every page (JSON lines) to out-file.
paginate() {
  local page=1 code n
  : >"$2"
  while :; do
    code=$(get "$1&per_page=100&page=$page")
    if [ "$code" != 200 ]; then
      echo "error: GitHub API answered HTTP $code for $1 (page $page); failing closed" >&2
      exit 3
    fi
    n=$(jq 'if type == "array" then length else error("not an array") end' "$tmp/body") || {
      echo "error: unexpected list response for $1 (page $page); failing closed" >&2
      exit 3
    }
    jq -c '.[]' "$tmp/body" >>"$2"
    [ "$n" -eq 100 ] || break
    page=$((page + 1))
    [ "$page" -le 20 ] || {
      echo "error: more than 2000 entries listed for $1; failing closed" >&2
      exit 3
    }
  done
}

# 1) The published release for the tag (this endpoint never returns drafts).
code=$(get "$api/repos/$repo/releases/tags/$tag")
case "$code" in
  200) ;;
  404)
    if [ "$mode" = verify ]; then
      echo "error: no published release for $tag after the upload step" >&2
      exit 1
    fi
    paginate "$api/repos/$repo/releases?" "$tmp/releases"
    match=$(jq -rs --arg t "$tag" 'map(select(.tag_name == $t))
      | if length == 0 then "none" elif any(.[]; .draft != false) then "draft" else "published" end' \
      "$tmp/releases") || {
      echo "error: unexpected release list entries; failing closed" >&2
      exit 3
    }
    case "$match" in
      none)
        echo "release $tag: ABSENT (no published or visible draft release) -> create path"
        exit 10
        ;;
      draft)
        echo "error: a DRAFT release for $tag exists; this workflow neither publishes nor edits drafts (publish it in the UI, or delete it), stopping" >&2
        exit 1
        ;;
      *)
        echo "error: releases/tags/$tag answered 404 but the release list has a non-draft entry for it; failing closed" >&2
        exit 3
        ;;
    esac
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
id=$(jq -r '.id' "$tmp/body")
if [ "$mode" = ready ]; then
  # Immutable releases (GitHub setting): assets cannot be added after publication. Fail closed:
  # only an explicit `"immutable": false` passes; true -> refuse, missing/other -> cannot verify.
  # Field name confirmed 2026-10-27 (GET releases/tags/v0.1.1 answered "immutable": false).
  case "$(jq -r 'if has("immutable") then .immutable | tostring else "missing" end' "$tmp/body")" in
    false) ;;
    true)
      echo "error: release $tag is immutable; assets cannot be uploaded after publication (turn immutable releases off, or let the workflow create the release)" >&2
      exit 1
      ;;
    *)
      echo "error: cannot verify whether release $tag is immutable (no boolean 'immutable' field in the API answer); stopping" >&2
      exit 1
      ;;
  esac
fi
echo "published release for $tag: found (id $id)"

# 2) Existing assets of that release (paginated).
paginate "$api/repos/$repo/releases/$id/assets?" "$tmp/assets"
jq -r '.name' "$tmp/assets" >"$tmp/names" || {
  echo "error: unexpected asset list entries; failing closed" >&2
  exit 3
}
if [ "$mode" = verify ]; then
  jq -r 'select(.state == "uploaded") | .name' "$tmp/assets" >"$tmp/uploaded"
  missing=0
  for a in "$@"; do
    grep -Fxq -- "$a" "$tmp/uploaded" || {
      echo "error: asset $a is not attached (state uploaded) to release $tag" >&2
      missing=1
    }
  done
  [ "$missing" -eq 0 ] || exit 1
  echo "release $tag: all $# asset names attached"
  exit 0
fi
clash=0
for a in "$@"; do
  if grep -Fxq -- "$a" "$tmp/names"; then
    echo "error: asset $a already exists on release $tag; never overwritten" >&2
    clash=1
  fi
done
[ "$clash" -eq 0 ] || exit 1
echo "release $tag: PRESENT, none of the $# asset names exist yet ($(wc -l <"$tmp/names" | tr -d ' ') other assets present) -> upload-only path"
