#!/usr/bin/env bash
# Classify a failed Nightly origin for recover-orphan-tag.
# Prints `receipt` or `missing-receipt` to stdout.
set -euo pipefail

steps="${NIGHTLY_ORPHAN_STEPS_JSON:?}"
jq -e 'type == "array"' <<<"$steps" >/dev/null

step_count() {
  jq --arg name "$1" '[.[] | select(.name == $name)] | length' <<<"$steps"
}

step_conclusion() {
  jq -r --arg name "$1" '.[] | select(.name == $name) | .conclusion' <<<"$steps"
}

require_once() {
  local name="$1" count
  count="$(step_count "$name")"
  [ "$count" -eq 1 ] || {
    echo "classify-nightly-orphan-origin: expected one step named $name, found $count" >&2
    exit 1
  }
}

require_once 'Create immutable nightly tag and receipt'
require_once 'Create draft prerelease for existing tag'
require_once 'Upload complete candidate'
require_once 'Verify and publish exact prerelease'

draft_conclusion="$(step_conclusion 'Create draft prerelease for existing tag')"
upload_conclusion="$(step_conclusion 'Upload complete candidate')"
publish_conclusion="$(step_conclusion 'Verify and publish exact prerelease')"
[ "$draft_conclusion" != success ] || {
  echo 'classify-nightly-orphan-origin: draft creation succeeded; not an orphan-tag origin' >&2
  exit 1
}
[ "$upload_conclusion" != success ] || {
  echo 'classify-nightly-orphan-origin: candidate upload succeeded; not an orphan-tag origin' >&2
  exit 1
}
[ "$publish_conclusion" != success ] || {
  echo 'classify-nightly-orphan-origin: publish succeeded; not an orphan-tag origin' >&2
  exit 1
}

tag_conclusion="$(step_conclusion 'Create immutable nightly tag and receipt')"
case "$tag_conclusion" in
  success)
    printf 'receipt\n'
    ;;
  failure|cancelled)
    printf 'missing-receipt\n'
    ;;
  *)
    echo "classify-nightly-orphan-origin: tag step conclusion is not recoverable: $tag_conclusion" >&2
    exit 1
    ;;
esac
