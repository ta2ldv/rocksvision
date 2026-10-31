#!/usr/bin/env bash
# Local self-test of the release gates WITHOUT any GitHub access or write: puts a fake `curl`
# and a fake `gh` (both under /private/tmp/rocksvision-ci-fake/bin) first on PATH and runs
# validate-tag.sh, check-tag-commit.sh, check-release-ready.sh and publish-release.sh against
# scripted API answers. The fake gh records every subcommand it is asked to run (gh.log), so the
# upload-only path can be shown to never call `gh release edit`/`create`.
# Usage: bash scripts/ci/selftest-release-ready.sh      Exit: 0 all cases as expected, 1 otherwise.
set -uo pipefail
here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
root=$(CDPATH='' cd -- "$here/../.." && pwd -P)
base=/private/tmp/rocksvision-ci-fake
rm -rf "$base"
mkdir -p "$base/bin" "$base/assets"
command -v jq >/dev/null 2>&1 || {
  echo "error: jq is required" >&2
  exit 1
}

cat >"$base/bin/curl" <<'EOF'
#!/usr/bin/env bash
# Fake curl: answers from files in $FAKE_STATE (see selftest-release-ready.sh).
out=/dev/null url=
while [ $# -gt 0 ]; do
  case $1 in
    -o) out=$2; shift 2 ;;
    -w | -H | --retry | --max-time) shift 2 ;;
    -*) shift ;;
    *) url=$1; shift ;;
  esac
done
s=$FAKE_STATE
echo "GET $url" >>"$s/curl.log"
[ -f "$s/curl_fail" ] && { echo "curl: (7) Failed to connect" >&2; exit 7; }
reply() { printf '%s' "$2" >"$out"; printf '%s' "$1"; }
case $url in
  */releases/tags/*)
    if [ -f "$s/http_tags" ]; then reply "$(cat "$s/http_tags")" '{"message":"error"}'
    elif [ -f "$s/badjson" ]; then reply 200 'not json'
    elif [ -f "$s/release.json" ]; then reply 200 "$(cat "$s/release.json")"
    else reply 404 '{"message":"Not Found"}'; fi ;;
  */releases/*/assets\?*)
    case $url in *page=1) reply 200 "$(cat "$s/assets.json" 2>/dev/null || echo '[]')" ;; *) reply 200 '[]' ;; esac ;;
  */releases\?*)
    case $url in *page=1) reply 200 "$(cat "$s/releases.json" 2>/dev/null || echo '[]')" ;; *) reply 200 '[]' ;; esac ;;
  # Tag resolution (check-tag-commit.sh): commit_sha = commit the tag resolves to; tag_annotated
  # (content = tag object sha) makes the ref point to a tag object; tagobj_type overrides the
  # type that tag object points to; http_commit / http_tagobj force an HTTP code; ref_badjson
  # returns a ref answer without an object.
  */git/ref/tags/*)
    t=${url##*/git/ref/tags/}
    if [ -f "$s/http_commit" ]; then reply "$(cat "$s/http_commit")" '{"message":"error"}'
    elif [ -f "$s/ref_badjson" ]; then reply 200 "{\"ref\":\"refs/tags/$t\"}"
    elif [ -f "$s/commit_sha" ] && [ -f "$s/tag_annotated" ]; then
      reply 200 "{\"ref\":\"refs/tags/$t\",\"object\":{\"type\":\"tag\",\"sha\":\"$(cat "$s/tag_annotated")\"}}"
    elif [ -f "$s/commit_sha" ]; then
      reply 200 "{\"ref\":\"refs/tags/$t\",\"object\":{\"type\":\"commit\",\"sha\":\"$(cat "$s/commit_sha")\"}}"
    else reply 404 '{"message":"Not Found"}'; fi ;;
  */git/tags/*)
    if [ -f "$s/http_tagobj" ]; then reply "$(cat "$s/http_tagobj")" '{"message":"error"}'
    elif [ -f "$s/tag_annotated" ] && [ "${url##*/}" = "$(cat "$s/tag_annotated")" ]; then
      reply 200 "{\"sha\":\"${url##*/}\",\"object\":{\"type\":\"$(cat "$s/tagobj_type" 2>/dev/null || echo commit)\",\"sha\":\"$(cat "$s/commit_sha")\"}}"
    else reply 404 '{"message":"Not Found"}'; fi ;;
  *) reply 500 '{"message":"unexpected url in fake"}' ;;
esac
EOF

cat >"$base/bin/gh" <<'EOF'
#!/usr/bin/env bash
# Fake gh: records the call, emulates only `release create` and `release upload`.
s=$FAKE_STATE
echo "gh $*" >>"$s/gh.log"
[ "${1:-}" = release ] || exit 99
sub=${2:-}
tag=${3:-}
shift 3
files=()
while [ $# -gt 0 ]; do
  case $1 in
    --repo | --title | --notes-file | --notes) shift 2 ;;
    --*) shift ;;
    *) files+=("$1"); shift ;;
  esac
done
add_assets() {
  [ -f "$s/assets.json" ] || echo '[]' >"$s/assets.json"
  for f in "$@"; do
    [ -f "$s/gh_drop_one" ] && [ "$f" = "${files[0]}" ] && continue
    jq --arg n "${f##*/}" '. + [{name: $n, state: "uploaded"}]' "$s/assets.json" >"$s/a.tmp" && mv "$s/a.tmp" "$s/assets.json"
  done
}
case $sub in
  create)
    [ -f "$s/release.json" ] && { echo "a release with the same tag name already exists" >&2; exit 1; }
    jq -n --arg t "$tag" '{id: 7, tag_name: $t, draft: false, immutable: false, body: "generated"}' >"$s/release.json"
    add_assets "${files[@]}" ;;
  upload) add_assets "${files[@]}" ;;
  *) echo "fake gh: release $sub is not allowed in this flow" >&2; exit 99 ;;
esac
EOF
chmod +x "$base/bin/curl" "$base/bin/gh"
PATH="$base/bin:$PATH"
export PATH GH_TOKEN=fake-token GITHUB_API_URL=https://api.example.invalid
unset GITHUB_STEP_SUMMARY GITHUB_RUN_ID

cargo_ver=$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
tag=v$cargo_ver
sha=0123456789abcdef0123456789abcdef01234567
other=fedcba9876543210fedcba9876543210fedcba98
names=()
for f in "rocksvision-$cargo_ver-macos-arm64.zip" "rocksvision-$cargo_ver-macos-x64.zip" \
  "rocksvision-$cargo_ver-ubuntu-x64.tar.gz" "rocksvision-$cargo_ver-windows-x64.zip"; do
  for n in "$f" "$f.sha256"; do
    names+=("$n")
    printf 'fake %s\n' "$n" >"$base/assets/$n"
  done
done
published() { # published <immutable-json-fragment>
  printf '{"id":7,"tag_name":"%s","draft":false%s,"body":"Hand-written notes by the owner."}' "$tag" "$1"
}

fails=0
n=0
# case <id> <expected-exit> <description> <setup-fn> <command...>
case_() {
  local id=$1 want=$2 desc=$3 setup=$4 got
  shift 4
  n=$((n + 1))
  export FAKE_STATE="$base/state-$id"
  rm -rf "$FAKE_STATE"
  mkdir -p "$FAKE_STATE"
  : >"$FAKE_STATE/gh.log"
  $setup
  "$@" >"$FAKE_STATE/out.log" 2>&1
  got=$?
  last_gh=$(cut -d' ' -f1-3 <"$FAKE_STATE/gh.log" | tr '\n' ';')
  if [ "$got" = "$want" ]; then res=ok; else res=FAIL; fails=$((fails + 1)); fi
  printf '%-4s %-4s exit %-3s (want %-3s) %s | gh: %s\n' "$res" "$id" "$got" "$want" "$desc" "${last_gh:--}"
  [ "$res" = ok ] || sed 's/^/       /' "$FAKE_STATE/out.log"
}
s_none() { :; }
s_present() { published ',"immutable":false' >"$FAKE_STATE/release.json"; echo '[{"name":"notes.txt","state":"uploaded"}]' >"$FAKE_STATE/assets.json"; echo "$sha" >"$FAKE_STATE/commit_sha"; }
s_clash() { s_present; jq --arg n "${names[2]}" '. + [{name: $n, state: "uploaded"}]' "$FAKE_STATE/assets.json" >"$FAKE_STATE/x" && mv "$FAKE_STATE/x" "$FAKE_STATE/assets.json"; }
s_absent() { echo "$sha" >"$FAKE_STATE/commit_sha"; echo '[{"id":3,"tag_name":"v0.1.1","draft":false}]' >"$FAKE_STATE/releases.json"; }
s_draft_list() { s_absent; printf '[{"id":9,"tag_name":"%s","draft":true}]' "$tag" >"$FAKE_STATE/releases.json"; }
s_draft_tags() { echo "$sha" >"$FAKE_STATE/commit_sha"; printf '{"id":9,"tag_name":"%s","draft":true,"immutable":false}' "$tag" >"$FAKE_STATE/release.json"; }
s_immut() { s_present; published ',"immutable":true' >"$FAKE_STATE/release.json"; }
s_immut_missing() { s_present; published '' >"$FAKE_STATE/release.json"; }
s_500() { s_present; echo 500 >"$FAKE_STATE/http_tags"; }
s_403() { s_present; echo 403 >"$FAKE_STATE/http_tags"; }
s_net() { s_present; : >"$FAKE_STATE/curl_fail"; }
s_bad() { s_present; : >"$FAKE_STATE/badjson"; }
s_moved() { s_present; echo "$other" >"$FAKE_STATE/commit_sha"; }
s_notag() { s_present; rm -f "$FAKE_STATE/commit_sha"; }
s_commit500() { s_present; echo 502 >"$FAKE_STATE/http_commit"; }
tagobj=7c857d4800000000000000000000000000000001
s_annot() { s_present; echo "$tagobj" >"$FAKE_STATE/tag_annotated"; }
s_annot_moved() { s_annot; echo "$other" >"$FAKE_STATE/commit_sha"; }
s_annot_tree() { s_annot; echo tree >"$FAKE_STATE/tagobj_type"; }
s_annot_500() { s_annot; echo 500 >"$FAKE_STATE/http_tagobj"; }
s_ref_bad() { s_present; : >"$FAKE_STATE/ref_badjson"; }
s_absent_annot() { s_absent; echo "$tagobj" >"$FAKE_STATE/tag_annotated"; }
s_all() { s_present; for a in "${names[@]}"; do jq --arg n "$a" '. + [{name: $n, state: "uploaded"}]' "$FAKE_STATE/assets.json" >"$FAKE_STATE/x" && mv "$FAKE_STATE/x" "$FAKE_STATE/assets.json"; done; }
s_absent_drop() { s_absent; : >"$FAKE_STATE/gh_drop_one"; }

V="bash $here/validate-tag.sh"
R="bash $here/check-release-ready.sh"
T="bash $here/check-tag-commit.sh"
P="bash $here/publish-release.sh"
echo "== validate-tag.sh (Cargo.toml version $cargo_ver)"
case_ V1 0 "$tag equals Cargo.toml" s_none $V "$tag"
case_ V2 10 "v0.1.0 older than 0.1.1 -> neutral skip" s_none $V v0.1.0
case_ V3 10 "v0.0.9 older -> neutral skip" s_none $V v0.0.9
case_ V4 1 "v99.0.0 != Cargo.toml -> fail" s_none $V v99.0.0
case_ V5 1 "v0.1.02 not strict -> fail" s_none $V v0.1.02
case_ V6 1 "$tag-rc1 not strict -> fail" s_none $V "$tag-rc1"
case_ V7 1 "v1.0 not strict -> fail" s_none $V v1.0
echo "== check-release-ready.sh"
case_ R1 10 "release absent -> create path" s_absent $R repo/x "$tag" "${names[@]}"
case_ R2 0 "present, notes, no same-name asset -> upload-only" s_present $R repo/x "$tag" "${names[@]}"
case_ R3 1 "present, one same-name asset -> fail" s_clash $R repo/x "$tag" "${names[@]}"
case_ R4 1 "draft (404 on tags, draft in list) -> fail" s_draft_list $R repo/x "$tag" "${names[@]}"
case_ R5 1 "draft returned by tags endpoint -> fail" s_draft_tags $R repo/x "$tag" "${names[@]}"
case_ R6 1 "immutable true -> fail" s_immut $R repo/x "$tag" "${names[@]}"
case_ R7 1 "immutable field missing -> fail" s_immut_missing $R repo/x "$tag" "${names[@]}"
case_ R8 3 "API HTTP 500 -> fail closed" s_500 $R repo/x "$tag" "${names[@]}"
case_ R9 3 "API HTTP 403 -> fail closed" s_403 $R repo/x "$tag" "${names[@]}"
case_ R10 3 "network failure -> fail closed" s_net $R repo/x "$tag" "${names[@]}"
case_ R11 3 "unparsable answer -> fail closed" s_bad $R repo/x "$tag" "${names[@]}"
case_ R12 0 "--verify-uploaded, all eight attached" s_all $R --verify-uploaded repo/x "$tag" "${names[@]}"
case_ R13 1 "--verify-uploaded, assets missing" s_present $R --verify-uploaded repo/x "$tag" "${names[@]}"
echo "== check-tag-commit.sh"
case_ T1 0 "lightweight tag at built commit" s_present $T repo/x "$tag" "$sha"
case_ T2 1 "lightweight tag moved to another commit" s_moved $T repo/x "$tag" "$sha"
case_ T3 1 "tag missing on GitHub (404)" s_notag $T repo/x "$tag" "$sha"
case_ T4 3 "API HTTP 502" s_commit500 $T repo/x "$tag" "$sha"
case_ T5 0 "annotated tag -> tag object -> built commit" s_annot $T repo/x "$tag" "$sha"
case_ T6 1 "annotated tag -> tag object -> other commit" s_annot_moved $T repo/x "$tag" "$sha"
case_ T7 1 "annotated tag object points to a tree" s_annot_tree $T repo/x "$tag" "$sha"
case_ T8 3 "tag object read HTTP 500 -> fail closed" s_annot_500 $T repo/x "$tag" "$sha"
case_ T9 3 "ref answer without object -> fail closed" s_ref_bad $T repo/x "$tag" "$sha"
grep -q "/git/tags/$tagobj\$" "$base/state-T5/curl.log" && ! grep -q '/commits/' "$base/state-T5/curl.log" || {
  echo "FAIL T5: expected GET git/ref/tags then git/tags/<tag object>, no commits/ endpoint"
  fails=$((fails + 1))
}
echo "== publish-release.sh (end to end, fake gh records every call)"
case_ P1 0 "absent -> gh release create --verify-tag + 8 assets, verified" s_absent $P repo/x "$tag" "$sha" "$base/assets"
grep -q -- '--verify-tag' "$FAKE_STATE/gh.log" && [ "$(wc -l <"$FAKE_STATE/gh.log")" -eq 1 ] || {
  echo "FAIL P1: expected exactly one 'gh release create ... --verify-tag' call"
  fails=$((fails + 1))
}
case_ P2 0 "present with hand-written notes -> upload only" s_present $P repo/x "$tag" "$sha" "$base/assets"
before=$(published ',"immutable":false')
if [ "$(cat "$FAKE_STATE/release.json")" != "$before" ] || [ "$(grep -c '' "$FAKE_STATE/gh.log")" -ne 1 ] ||
  ! grep -q '^gh release upload ' "$FAKE_STATE/gh.log" || grep -Eq 'release (edit|create|delete)' "$FAKE_STATE/gh.log"; then
  echo "FAIL P2: upload-only path touched more than the assets"
  fails=$((fails + 1))
else
  echo "          P2 proof: gh.log has exactly one 'gh release upload' call (no edit/create/delete); release title/body unchanged"
fi
case_ P3 1 "present, one same-name asset -> no gh call" s_clash $P repo/x "$tag" "$sha" "$base/assets"
case_ P4 1 "draft -> no gh call" s_draft_list $P repo/x "$tag" "$sha" "$base/assets"
case_ P5 1 "immutable -> no gh call" s_immut $P repo/x "$tag" "$sha" "$base/assets"
case_ P6 1 "tag moved -> no gh call" s_moved $P repo/x "$tag" "$sha" "$base/assets"
case_ P7 3 "API error -> no gh call" s_500 $P repo/x "$tag" "$sha" "$base/assets"
case_ P8 1 "create drops one asset -> post-write verify fails" s_absent_drop $P repo/x "$tag" "$sha" "$base/assets"
case_ P9 0 "annotated tag at built commit, absent -> create + 8 assets" s_absent_annot $P repo/x "$tag" "$sha" "$base/assets"
for id in P3 P4 P5 P6 P7; do
  [ -s "$base/state-$id/gh.log" ] && {
    echo "FAIL $id: gh was called"
    fails=$((fails + 1))
  }
done
echo "== $n cases, $fails failure(s)"
[ "$fails" -eq 0 ]
