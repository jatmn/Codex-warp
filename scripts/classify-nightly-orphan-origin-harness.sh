#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

steps() {
  local tag_conclusion="$1" draft_conclusion="$2"
  jq -n --arg tag "$tag_conclusion" --arg draft "$draft_conclusion" '
    [
      {name:"Create immutable nightly tag and receipt",conclusion:$tag},
      {name:"Create draft prerelease for existing tag",conclusion:$draft},
      {name:"Upload complete candidate",conclusion:"skipped"},
      {name:"Verify and publish exact prerelease",conclusion:"skipped"}
    ]'
}

classify() {
  NIGHTLY_ORPHAN_STEPS_JSON="$1" bash "$root/scripts/classify-nightly-orphan-origin.sh"
}

got="$(classify "$(steps success skipped)")"
[ "$got" = receipt ] || {
  echo "classify-nightly-orphan-origin-harness: expected receipt, got $got" >&2
  exit 1
}

got="$(classify "$(steps failure skipped)")"
[ "$got" = missing-receipt ] || {
  echo "classify-nightly-orphan-origin-harness: expected missing-receipt for failure, got $got" >&2
  exit 1
}

got="$(classify "$(steps cancelled skipped)")"
[ "$got" = missing-receipt ] || {
  echo "classify-nightly-orphan-origin-harness: expected missing-receipt for cancelled, got $got" >&2
  exit 1
}

got="$(classify "$(steps timed_out skipped)")"
[ "$got" = missing-receipt ] || {
  echo "classify-nightly-orphan-origin-harness: expected missing-receipt for timed_out, got $got" >&2
  exit 1
}

if classify "$(steps skipped skipped)" >/dev/null 2>"$tmp/skipped.err"; then
  echo 'classify-nightly-orphan-origin-harness: skipped tag step was accepted' >&2
  exit 1
fi
grep -F 'tag step conclusion is not recoverable: skipped' "$tmp/skipped.err" >/dev/null

if classify "$(steps success success)" >/dev/null 2>"$tmp/draft.err"; then
  echo 'classify-nightly-orphan-origin-harness: successful draft was accepted as orphan origin' >&2
  exit 1
fi
grep -F 'draft creation succeeded' "$tmp/draft.err" >/dev/null

if NIGHTLY_ORPHAN_STEPS_JSON='[]' bash "$root/scripts/classify-nightly-orphan-origin.sh" >/dev/null 2>"$tmp/empty.err"; then
  echo 'classify-nightly-orphan-origin-harness: empty steps were accepted' >&2
  exit 1
fi
grep -F 'expected one step named Create immutable nightly tag and receipt' "$tmp/empty.err" >/dev/null

echo 'classify-nightly-orphan-origin-harness: ok'
