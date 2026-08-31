#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
# Hook preflights export an isolated object database for the staged snapshot.
# The fixture repos below must not inherit it: doing so can make their refs and
# objects depend on the hook's temporary database instead of their own.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
repo="$tmp/repo"
origin="$tmp/origin.git"
mkdir -p "$repo/scripts/test-fixtures" "$repo/tools"
cp "$root/Cargo.toml" "$repo/"
cp "$root/scripts/prepare-nightly.sh" "$root/scripts/check-nightly-assets.sh" \
  "$root/scripts/check-release-contract.sh" "$root/scripts/check-sha256-index.sh" \
  "$root/scripts/nightly-contract-digest.sh" "$root/scripts/sha256-file.sh" "$repo/scripts/"
cp "$root/scripts/test-fixtures/nightly-gh.sh" "$repo/scripts/test-fixtures/"
cp "$root/tools/nightly-manifest.schema.json" "$root/tools/release-contract.json" \
  "$root/tools/nightly-packaging-contract.txt" "$repo/tools/"
ln -s "$root/tools/release-please-policy" "$repo/tools/release-please-policy"
git -C "$repo" init --quiet -b main
git -C "$repo" config user.email fixture@example.invalid
git -C "$repo" config user.name fixture
git -C "$repo" add .
git -C "$repo" commit --quiet -m fixture
git init --quiet --bare "$origin"
git -C "$repo" remote add origin "$origin"
git -C "$repo" push --quiet -u origin main
sha="$(git -C "$repo" rev-parse HEAD)"
fake_bin="$tmp/bin"
mkdir "$fake_bin"
ln -s "$repo/scripts/test-fixtures/nightly-gh.sh" "$fake_bin/gh"

run_prepare() {
  GITHUB_EVENT_NAME=schedule GITHUB_SHA="$sha" GITHUB_RUN_ID=1 \
    GITHUB_REPOSITORY=jatmn/Codex-warp GITHUB_OUTPUT="$tmp/output" \
    NIGHTLY_PUBLISH_ENABLED=false PATH="$fake_bin:$PATH" \
    NIGHTLY_GH_FAIL_RELEASES="${NIGHTLY_GH_FAIL_RELEASES:-0}" \
    NIGHTLY_GH_RELEASES_JSON="${NIGHTLY_GH_RELEASES_JSON:-[]}" \
    NIGHTLY_GH_RELEASE_JSON="${NIGHTLY_GH_RELEASE_JSON:-{}}" \
    NIGHTLY_GH_ASSET_DIR="${NIGHTLY_GH_ASSET_DIR:-}" \
    bash "$repo/scripts/prepare-nightly.sh"
}

cd "$repo"
run_prepare >/dev/null
grep -Fx 'action=build' "$tmp/output" >/dev/null
grep -Ex "version=[0-9]+\.[0-9]+\.[0-9]+-nightly\.20260830\+${sha:0:12}" "$tmp/output" >/dev/null

release_failure=0
NIGHTLY_GH_FAIL_RELEASES=1 run_prepare >/dev/null 2>&1 || release_failure=$?
if [ "$release_failure" -eq 0 ]; then
  echo 'prepare-nightly-harness: release enumeration failure was ignored' >&2
  exit 1
fi

old_sha="$sha"
old_tag="nightly-20260830-${old_sha:0:12}"
git -C "$repo" tag --no-sign "$old_tag" "$old_sha"
git -C "$repo" push --quiet origin "refs/tags/$old_tag"
git -C "$repo" commit --quiet --allow-empty -m newer
git -C "$repo" push --quiet origin main
sha="$(git -C "$repo" rev-parse HEAD)"

orphan_failure=0
run_prepare >/dev/null 2>&1 || orphan_failure=$?
if [ "$orphan_failure" -eq 0 ]; then
  echo 'prepare-nightly-harness: older orphan tag did not block a new candidate' >&2
  exit 1
fi

old_draft="[{\"id\":8,\"tag_name\":\"$old_tag\",\"draft\":true,\"prerelease\":true,\"published_at\":null}]"
draft_failure=0
NIGHTLY_GH_RELEASES_JSON="$old_draft" run_prepare >/dev/null 2>&1 || draft_failure=$?
if [ "$draft_failure" -eq 0 ]; then
  echo 'prepare-nightly-harness: older draft release did not block a new candidate' >&2
  exit 1
fi

old_complete="{\"id\":8,\"tag_name\":\"$old_tag\",\"draft\":false,\"prerelease\":true,\"published_at\":\"2026-08-30T09:05:00Z\"}"
NIGHTLY_GH_RELEASES_JSON="[$old_complete]" run_prepare >/dev/null
grep -Fx 'action=build' "$tmp/output" >/dev/null

tag="nightly-20260830-${sha:0:12}"
git -C "$repo" tag --no-sign "$tag" "$sha"
git -C "$repo" push --quiet origin "refs/tags/$tag"
assets="$tmp/assets"
mkdir "$assets"
printf '{}\n' >"$assets/codex-warp-nightly-manifest.json"
releases="[$old_complete,{\"id\":9,\"tag_name\":\"$tag\",\"draft\":false,\"prerelease\":true,\"published_at\":\"2026-08-30T10:05:00Z\"}]"
release="{\"id\":9,\"tag_name\":\"$tag\",\"draft\":false,\"prerelease\":true,\"published_at\":\"2026-08-30T10:05:00Z\"}"
corrupt_failure=0
NIGHTLY_GH_RELEASES_JSON="$releases" NIGHTLY_GH_RELEASE_JSON="$release" NIGHTLY_GH_ASSET_DIR="$assets" run_prepare >/dev/null 2>&1 || corrupt_failure=$?
if [ "$corrupt_failure" -eq 0 ]; then
  echo 'prepare-nightly-harness: corrupt published assets were accepted' >&2
  exit 1
fi

echo 'prepare-nightly-harness: ok'
