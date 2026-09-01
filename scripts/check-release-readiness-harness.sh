#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

jq '.enabled = true | .appBot.id = 12345 | .appBot.login = "codex-warp-release[bot]"' \
  tools/release-automation-policy.json >"$tmp/policy.json"

fixture() {
  local name="$1" repository="$2" base="$3" head_repository="$4" head_ref="$5"
  local author_id="$6" author_login="$7" author_type="$8" actor="$9" files="${10}"
  local state="${11}"
  jq -n \
    --arg repository "$repository" --arg base "$base" \
    --arg head_repository "$head_repository" --arg head_ref "$head_ref" \
    --argjson author_id "$author_id" --arg author_login "$author_login" \
    --arg author_type "$author_type" --arg actor "$actor" \
    --argjson files "$files" --argjson state "$state" \
    '{repository:{full_name:$repository}, sender:{login:$actor}, pull_request:{number:7,base:{ref:$base,repo:{full_name:$repository}},head:{ref:$head_ref,repo:{full_name:$head_repository}},user:{id:$author_id,login:$author_login,type:$author_type}},changed_files:$files,release_state:$state}' \
    >"$tmp/$name.json"
}

allowed='[".release-please-manifest.json","CHANGELOG.md","Cargo.lock","Cargo.toml"]'
empty_state='{"tags":[],"releases":[],"activeOfficialTags":[]}'
genuine_args=('jatmn/Codex-warp' 'main' 'jatmn/Codex-warp' 'release-please--branches--main--components--codex-warp' '12345' 'codex-warp-release[bot]' 'Bot')

fixture genuine "${genuine_args[@]}" 'different-rerun-actor' "$allowed" "$empty_state"
fixture ordinary jatmn/Codex-warp main jatmn/Codex-warp feature/topic 99 contributor User contributor "$allowed" "$empty_state"
fixture branch-only jatmn/Codex-warp main jatmn/Codex-warp release-please--branches--main--components--codex-warp 99 contributor User contributor "$allowed" "$empty_state"
fixture author-only jatmn/Codex-warp main jatmn/Codex-warp feature/topic 12345 'codex-warp-release[bot]' Bot maintainer "$allowed" "$empty_state"
fixture fork-head jatmn/Codex-warp main attacker/Codex-warp release-please--branches--main--components--codex-warp 12345 'codex-warp-release[bot]' Bot maintainer "$allowed" "$empty_state"
fixture wrong-base jatmn/Codex-warp develop jatmn/Codex-warp release-please--branches--main--components--codex-warp 12345 'codex-warp-release[bot]' Bot maintainer "$allowed" "$empty_state"
fixture unexpected-file "${genuine_args[@]}" maintainer '[".release-please-manifest.json","CHANGELOG.md","Cargo.lock","Cargo.toml","src/version.rs"]' "$empty_state"
fixture draft "${genuine_args[@]}" maintainer "$allowed" '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":true,"prerelease":false,"published_at":null,"complete":false}],"activeOfficialTags":[]}'
fixture missing-release "${genuine_args[@]}" maintainer "$allowed" '{"tags":["v0.1.0"],"releases":[],"activeOfficialTags":[]}'
fixture active-finalizer "${genuine_args[@]}" maintainer "$allowed" '{"tags":[],"releases":[],"activeOfficialTags":["v0.1.0"]}'
fixture active-recovery "${genuine_args[@]}" maintainer "$allowed" '{"tags":[],"releases":[],"activeOfficialTags":["main"]}'
fixture incomplete "${genuine_args[@]}" maintainer "$allowed" '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":false}],"activeOfficialTags":[]}'

expect_ok() {
  RELEASE_AUTOMATION_POLICY="$tmp/policy.json" bash scripts/check-release-readiness.sh "$tmp/$1.json" >/dev/null
}
expect_fail() {
  if RELEASE_AUTOMATION_POLICY="$tmp/policy.json" bash scripts/check-release-readiness.sh "$tmp/$1.json" >/dev/null 2>&1; then
    echo "check-release-readiness-harness: unexpectedly accepted $1" >&2
    exit 1
  fi
}

expect_ok genuine
expect_ok ordinary
expect_fail branch-only
# Push CI sets GITHUB_EVENT_NAME=push; a fixture path must still classify.
if ! GITHUB_EVENT_NAME=push RELEASE_AUTOMATION_POLICY="$tmp/policy.json" \
    bash scripts/check-release-readiness.sh "$tmp/branch-only.json" >/dev/null 2>&1; then
  :
else
  echo 'check-release-readiness-harness: push event name skipped a fixture file' >&2
  exit 1
fi
expect_fail author-only
expect_fail fork-head
expect_fail wrong-base
expect_fail unexpected-file
expect_fail draft
expect_fail missing-release
expect_fail active-finalizer
expect_fail active-recovery
expect_ok incomplete
fixture incomplete-prior "${genuine_args[@]}" maintainer "$allowed" '{"tags":["v0.1.0","v0.1.1"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":false},{"id":2,"tag_name":"v0.1.1","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
expect_ok incomplete-prior

# A different event actor cannot change the creator-based classification.
if ! jq -e '.sender.login == "different-rerun-actor" and .pull_request.user.login == "codex-warp-release[bot]"' "$tmp/genuine.json" >/dev/null; then
  echo 'check-release-readiness-harness: creator/actor fixture is malformed' >&2
  exit 1
fi

# Live enabled policy must reject automation-shaped PRs that do not match the
# recorded App bot. The overlay fixture author is not the production bot.
if bash scripts/check-release-readiness.sh "$tmp/genuine.json" >/dev/null 2>&1; then
  echo 'check-release-readiness-harness: live policy accepted a non-matching automation-shaped PR' >&2
  exit 1
fi

[ "$(jq -r '.enabled' tools/release-automation-policy.json)" = true ]
live_id="$(jq '.appBot.id' tools/release-automation-policy.json)"
live_login="$(jq -r '.appBot.login' tools/release-automation-policy.json)"
jq --argjson id "$live_id" --arg login "$live_login" \
  '.pull_request.user.id=$id | .pull_request.user.login=$login' \
  "$tmp/genuine.json" >"$tmp/live-genuine.json"
if ! bash scripts/check-release-readiness.sh "$tmp/live-genuine.json" >/dev/null; then
  echo 'check-release-readiness-harness: live policy rejected the recorded App identity' >&2
  exit 1
fi

echo 'check-release-readiness-harness: ok'
