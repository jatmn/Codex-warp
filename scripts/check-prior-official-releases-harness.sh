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
check complete ok '{"tags":["v0.1.0"],"releases":[{"tag_name":"v0.1.0","draft":false,"published_at":"2026-08-30T00:00:00Z"}],"activeOfficialTags":[]}'
check draft fail '{"tags":["v0.1.0"],"releases":[{"tag_name":"v0.1.0","draft":true,"published_at":null}],"activeOfficialTags":[]}'
check missing fail '{"tags":["v0.1.0"],"releases":[],"activeOfficialTags":[]}'
check orphan-release fail '{"tags":[],"releases":[{"tag_name":"v0.1.0","draft":false,"published_at":"2026-08-30T00:00:00Z"}],"activeOfficialTags":[]}'
check active fail '{"tags":[],"releases":[],"activeOfficialTags":["v0.1.0"]}'
check ignore-nightly ok '{"tags":["nightly-20260830-111111111111"],"releases":[{"tag_name":"nightly-20260830-111111111111","draft":true,"published_at":null}],"activeOfficialTags":[]}'

echo 'check-prior-official-releases-harness: ok'
