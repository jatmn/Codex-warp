#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
# Hook preflights export an isolated object database for the staged snapshot.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
bin="$tmp/bin"
mkdir "$bin"
releases_file="$tmp/releases.json"
post_log="$tmp/post.log"
printf '%s\n' '[]' >"$releases_file"
: >"$post_log"

cat >"$bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" != api ]; then
  echo "create-missing-official-draft-harness: unexpected gh command: $*" >&2
  exit 2
fi
shift
method=GET
uses_paginate=0
uses_jq_items=0
uses_input=0
endpoint=''
input_data=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --paginate) uses_paginate=1 ;;
    --method)
      shift
      method="${1:-}"
      ;;
    --jq)
      shift
      [ "${1:-}" = '.[]' ] && uses_jq_items=1
      ;;
    --input)
      shift
      uses_input=1
      if [ "${1:-}" = - ]; then
        input_data="$(cat)"
      else
        input_data="$(cat "${1:?}")"
      fi
      ;;
    repos/*/releases/tags/*)
      echo 'create-missing-official-draft-harness: published-tag lookup is not valid for drafts' >&2
      exit 1
      ;;
    repos/*/releases)
      endpoint=releases
      ;;
  esac
  shift || true
done

if [ "$method" = GET ]; then
  [ "$uses_paginate" -eq 1 ] && [ "$uses_jq_items" -eq 1 ] && [ "$endpoint" = releases ] || {
    echo 'create-missing-official-draft-harness: unexpected gh api list invocation' >&2
    exit 2
  }
  jq -c '.[]' "${GH_RELEASES_FIXTURE:?}"
  exit 0
fi

[ "$method" = POST ] && [ "$endpoint" = releases ] && [ "$uses_input" -eq 1 ] || {
  echo 'create-missing-official-draft-harness: unexpected gh api mutation' >&2
  exit 2
}
printf '%s\n' "$input_data" >>"${GH_POST_LOG:?}"
jq -e '
  .draft == true and .prerelease == false and .generate_release_notes == false and
  .make_latest == "false" and .tag_name == .name and
  (.target_commitish | test("^[0-9a-f]{40}$")) and
  (.body | length) > 0
' <<<"$input_data" >/dev/null || {
  echo 'create-missing-official-draft-harness: POST body was not an unpublished official draft' >&2
  exit 1
}
tag="$(jq -r '.tag_name' <<<"$input_data")"
existing="$(jq --arg tag "$tag" '[.[] | select(.tag_name == $tag)] | length' "${GH_RELEASES_FIXTURE:?}")"
[ "$existing" -eq 0 ] || {
  echo 'create-missing-official-draft-harness: POST would duplicate a tag release' >&2
  exit 1
}
created="$(jq -n --argjson body "$input_data" --argjson id "${GH_NEXT_RELEASE_ID:-99}" '
  $body + {id:$id, published_at:null}
')"
jq --argjson created "$created" '. + [$created]' "${GH_RELEASES_FIXTURE:?}" >"${GH_RELEASES_FIXTURE}.next"
mv "${GH_RELEASES_FIXTURE}.next" "${GH_RELEASES_FIXTURE:?}"
printf '%s\n' "$created"
EOF
chmod +x "$bin/gh"

init_repo() {
  local dest="$1" version="$2"
  mkdir -p "$dest"
  git -C "$dest" init --quiet -b main
  printf '[package]\nname = "codex-warp"\nversion = "%s"\n' "$version" >"$dest/Cargo.toml"
  cat >"$dest/CHANGELOG.md" <<CHANGELOG
# Changelog

## [${version}](https://example.test/compare/v0.0.0...v${version}) (2026-09-01)

### Bug Fixes

* **release:** fixture notes for ${version}
CHANGELOG
  git -C "$dest" add Cargo.toml CHANGELOG.md
  git -C "$dest" -c user.name=fixture -c user.email=fixture@example.invalid \
    commit --quiet -m "release ${version}"
}

run_create() {
  local repo="$1" out="$2"
  : >"$post_log"
  PATH="$bin:$PATH" \
    GH_RELEASES_FIXTURE="$releases_file" \
    GH_POST_LOG="$post_log" \
    GH_NEXT_RELEASE_ID=99 \
    GITHUB_REPOSITORY=jatmn/Codex-warp \
    GITHUB_OUTPUT="$out" \
    LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 \
    LOOKUP_OFFICIAL_DRAFT_SLEEP=0 \
    bash "$root/scripts/create-missing-official-draft.sh"
}

expect_fail() {
  local name="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    echo "create-missing-official-draft-harness: $name unexpectedly succeeded" >&2
    exit 1
  fi
}

post_count() {
  if [ -s "$post_log" ]; then
    wc -l <"$post_log"
  else
    printf '0\n'
  fi
}

# Latest official tag has no GitHub Release: create one unpublished draft.
repo="$tmp/create"
origin="$tmp/create.origin.git"
init_repo "$repo" 0.1.3
git -C "$repo" tag --no-sign v0.1.3
git -C "$repo" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit --quiet --allow-empty -m overlay
git init --quiet --bare "$origin"
git -C "$repo" remote add origin "$origin"
git -C "$repo" push --quiet -u origin main
git -C "$repo" push --quiet origin refs/tags/v0.1.3
printf '%s\n' '[]' >"$releases_file"
out="$tmp/create.out"
(
  cd "$repo"
  run_create "$repo" "$out" >/dev/null
)
grep -Fx 'created=true' "$out" >/dev/null
grep -Fx 'tag=v0.1.3' "$out" >/dev/null
grep -Fx 'release_id=99' "$out" >/dev/null
sha="$(git -C "$repo" rev-parse 'v0.1.3^{}')"
grep -Fx "sha=$sha" "$out" >/dev/null
jq -e --arg sha "$sha" 'length == 1 and .[0].id == 99 and .[0].draft == true and .[0].target_commitish == $sha' \
  "$releases_file" >/dev/null
[ "$(post_count)" -eq 1 ]

# Latest tag already published: do not POST.
printf '%s\n' '[]' >"$releases_file"
jq -n --arg sha "$sha" '[{id:1,tag_name:"v0.1.3",draft:false,prerelease:false,published_at:"2026-09-01T00:00:00Z",target_commitish:$sha}]' \
  >"$releases_file"
out="$tmp/published.out"
(
  cd "$repo"
  run_create "$repo" "$out" >/dev/null
)
grep -Fx 'created=false' "$out" >/dev/null
[ "$(post_count)" -eq 0 ]

# Existing unpublished draft must fail closed.
jq -n --arg sha "$sha" '[{id:2,tag_name:"v0.1.3",draft:true,prerelease:false,published_at:null,target_commitish:$sha}]' \
  >"$releases_file"
: >"$post_log"
expect_fail existing-draft bash -c "cd \"$repo\" && PATH=\"$bin:\$PATH\" GH_RELEASES_FIXTURE=\"$releases_file\" GH_POST_LOG=\"$post_log\" GITHUB_REPOSITORY=jatmn/Codex-warp GITHUB_OUTPUT=\"$tmp/draft.out\" LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 LOOKUP_OFFICIAL_DRAFT_SLEEP=0 bash \"$root/scripts/create-missing-official-draft.sh\""
[ "$(post_count)" -eq 0 ]

# Prior official tag cannot be missing even when the latest tag is missing.
repo2="$tmp/prior"
origin2="$tmp/prior.origin.git"
init_repo "$repo2" 0.1.2
git -C "$repo2" tag --no-sign v0.1.2
printf '[package]\nname = "codex-warp"\nversion = "0.1.3"\n' >"$repo2/Cargo.toml"
cat >"$repo2/CHANGELOG.md" <<'CHANGELOG'
# Changelog

## [0.1.3](https://example.test/compare/v0.1.2...v0.1.3) (2026-09-01)

### Bug Fixes

* **release:** fixture notes for 0.1.3

## [0.1.2](https://example.test/compare/v0.0.0...v0.1.2) (2026-09-01)

### Bug Fixes

* **release:** fixture notes for 0.1.2
CHANGELOG
git -C "$repo2" add Cargo.toml CHANGELOG.md
git -C "$repo2" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit --quiet -m 'release 0.1.3'
git -C "$repo2" tag --no-sign v0.1.3
git init --quiet --bare "$origin2"
git -C "$repo2" remote add origin "$origin2"
git -C "$repo2" push --quiet -u origin main
git -C "$repo2" push --quiet origin refs/tags/v0.1.2 refs/tags/v0.1.3
printf '%s\n' '[]' >"$releases_file"
: >"$post_log"
expect_fail missing-prior bash -c "cd \"$repo2\" && PATH=\"$bin:\$PATH\" GH_RELEASES_FIXTURE=\"$releases_file\" GH_POST_LOG=\"$post_log\" GITHUB_REPOSITORY=jatmn/Codex-warp GITHUB_OUTPUT=\"$tmp/prior.out\" LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 LOOKUP_OFFICIAL_DRAFT_SLEEP=0 bash \"$root/scripts/create-missing-official-draft.sh\""
[ "$(post_count)" -eq 0 ]

# Latest missing with a published prior tag: create only for the latest.
jq -n '[{id:3,tag_name:"v0.1.2",draft:false,prerelease:false,published_at:"2026-09-01T00:00:00Z"}]' \
  >"$releases_file"
out="$tmp/latest-missing.out"
(
  cd "$repo2"
  run_create "$repo2" "$out" >/dev/null
)
grep -Fx 'created=true' "$out" >/dev/null
grep -Fx 'tag=v0.1.3' "$out" >/dev/null
[ "$(post_count)" -eq 1 ]

# Cargo version at the tagged SHA must match the tag.
repo3="$tmp/mismatch"
origin3="$tmp/mismatch.origin.git"
init_repo "$repo3" 0.0.9
git -C "$repo3" tag --no-sign v0.1.3
git init --quiet --bare "$origin3"
git -C "$repo3" remote add origin "$origin3"
git -C "$repo3" push --quiet -u origin main
git -C "$repo3" push --quiet origin refs/tags/v0.1.3
printf '%s\n' '[]' >"$releases_file"
: >"$post_log"
expect_fail cargo-mismatch bash -c "cd \"$repo3\" && PATH=\"$bin:\$PATH\" GH_RELEASES_FIXTURE=\"$releases_file\" GH_POST_LOG=\"$post_log\" GITHUB_REPOSITORY=jatmn/Codex-warp GITHUB_OUTPUT=\"$tmp/mismatch.out\" LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 LOOKUP_OFFICIAL_DRAFT_SLEEP=0 bash \"$root/scripts/create-missing-official-draft.sh\""
[ "$(post_count)" -eq 0 ]

# Tagged commit must be an ancestor of origin/main.
repo4="$tmp/orphan"
origin4="$tmp/orphan.origin.git"
init_repo "$repo4" 0.1.3
git -C "$repo4" checkout --quiet -b side
git -C "$repo4" -c user.name=fixture -c user.email=fixture@example.invalid \
  commit --quiet --allow-empty -m side
git -C "$repo4" tag --no-sign v0.1.3
git -C "$repo4" checkout --quiet main
git init --quiet --bare "$origin4"
git -C "$repo4" remote add origin "$origin4"
git -C "$repo4" push --quiet -u origin main
git -C "$repo4" push --quiet origin refs/tags/v0.1.3
printf '%s\n' '[]' >"$releases_file"
: >"$post_log"
expect_fail orphan-tag bash -c "cd \"$repo4\" && PATH=\"$bin:\$PATH\" GH_RELEASES_FIXTURE=\"$releases_file\" GH_POST_LOG=\"$post_log\" GITHUB_REPOSITORY=jatmn/Codex-warp GITHUB_OUTPUT=\"$tmp/orphan.out\" LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 LOOKUP_OFFICIAL_DRAFT_SLEEP=0 bash \"$root/scripts/create-missing-official-draft.sh\""
[ "$(post_count)" -eq 0 ]

# No official tags: skip without POST.
repo5="$tmp/none"
origin5="$tmp/none.origin.git"
init_repo "$repo5" 0.0.1
git init --quiet --bare "$origin5"
git -C "$repo5" remote add origin "$origin5"
git -C "$repo5" push --quiet -u origin main
printf '%s\n' '[]' >"$releases_file"
out="$tmp/none.out"
(
  cd "$repo5"
  run_create "$repo5" "$out" >/dev/null
)
grep -Fx 'created=false' "$out" >/dev/null
[ "$(post_count)" -eq 0 ]

if grep -F '/releases/tags/' "$root/scripts/create-missing-official-draft.sh" >/dev/null; then
  echo 'create-missing-official-draft-harness: script looks up drafts by published tag' >&2
  exit 1
fi
if grep -E 'gh release( |$)' "$root/scripts/create-missing-official-draft.sh" >/dev/null; then
  echo 'create-missing-official-draft-harness: script uses gh release instead of POST /releases' >&2
  exit 1
fi
if grep -F 'draft=false' "$root/scripts/create-missing-official-draft.sh" >/dev/null; then
  echo 'create-missing-official-draft-harness: script can POST a published release' >&2
  exit 1
fi

echo 'create-missing-official-draft-harness: ok'
