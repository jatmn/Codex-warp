#!/usr/bin/env bash
set -euo pipefail

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

check() {
  local name="$1" expected="$2" body="$3"
  printf '%s\n' "$body" >"$tmp/$name.json"
  if OFFICIAL_STATE_FIXTURE="$tmp/$name.json" bash scripts/check-prior-official-releases.sh >/dev/null 2>&1; then
    actual=ok
  else
    actual=fail
  fi
  [ "$actual" = "$expected" ] || { echo "check-prior-official-releases-harness: $name was $actual, expected $expected" >&2; exit 1; }
}

check empty ok '{"tags":[],"releases":[],"activeOfficialTags":[]}'
check complete ok '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
check incomplete ok '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":false}],"activeOfficialTags":[]}'
check incomplete-prior ok '{"tags":["v0.1.0","v0.1.1"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":false},{"id":2,"tag_name":"v0.1.1","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
check incomplete-latest-with-complete-prior ok '{"tags":["v0.1.0","v0.1.1"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true},{"id":2,"tag_name":"v0.1.1","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":false}],"activeOfficialTags":[]}'
check prerelease fail '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":true,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
check draft fail '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":true,"prerelease":false,"published_at":null,"complete":false}],"activeOfficialTags":[]}'
check missing fail '{"tags":["v0.1.0"],"releases":[],"activeOfficialTags":[]}'
check orphan-release fail '{"tags":[],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
check_env() {
  local name="$1" expected="$2" body="$3"
  printf '%s\n' "$body" >"$tmp/$name.json"
  if OFFICIAL_ALLOW_MISSING_LATEST_TAG=1 OFFICIAL_STATE_FIXTURE="$tmp/$name.json" bash scripts/check-prior-official-releases.sh >/dev/null 2>&1; then
    actual=ok
  else
    actual=fail
  fi
  [ "$actual" = "$expected" ] || { echo "check-prior-official-releases-harness: $name was $actual, expected $expected" >&2; exit 1; }
}
check_env missing-latest-allowed ok '{"tags":["v0.1.0","v0.1.1"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
check_env missing-prior-with-allow fail '{"tags":["v0.1.0","v0.1.1"],"releases":[{"id":2,"tag_name":"v0.1.1","draft":false,"prerelease":false,"published_at":"2026-08-30T00:00:00Z","complete":true}],"activeOfficialTags":[]}'
check_env missing-latest-draft-still-fail fail '{"tags":["v0.1.0"],"releases":[{"id":1,"tag_name":"v0.1.0","draft":true,"prerelease":false,"published_at":null,"complete":false}],"activeOfficialTags":[]}'
check active fail '{"tags":[],"releases":[],"activeOfficialTags":["v0.1.0"]}'
check active-recovery fail '{"tags":[],"releases":[],"activeOfficialTags":["main"]}'
check pr-version-branch ok '{"tags":[],"releases":[],"activeRuns":[{"id":7,"name":"Release","event":"pull_request","head_branch":"v9.9.9"}]}'
check active-push fail '{"tags":[],"releases":[],"activeRuns":[{"id":8,"name":"Release","event":"push","head_branch":"v9.9.9"}]}'
check wrong-recovery-event ok '{"tags":[],"releases":[],"activeRuns":[{"id":9,"name":"Release Recovery","event":"pull_request","head_branch":"main"}]}'
check active-dispatch-recovery fail '{"tags":[],"releases":[],"activeRuns":[{"id":10,"name":"Release Recovery","event":"workflow_dispatch","head_branch":"main"}]}'
check ignore-nightly ok '{"tags":["nightly-20260830-111111111111"],"releases":[{"tag_name":"nightly-20260830-111111111111","draft":true,"published_at":null}],"activeOfficialTags":[]}'

grep -F 'scripts/verify-official-attestation.sh' scripts/check-prior-official-releases.sh >/dev/null
if grep -F 'gh attestation verify' scripts/check-prior-official-releases.sh >/dev/null; then
  echo 'check-prior-official-releases-harness: repository-only attestation verify remains' >&2
  exit 1
fi

echo 'check-prior-official-releases-harness: ok'
