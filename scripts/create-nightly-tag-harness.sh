#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
log="$tmp/gh.log"
peel_remaining_file="$tmp/peel-remaining"
lookup_mode_file="$tmp/lookup-mode"
create_status_file="$tmp/create-status"
printf '1\n' >"$peel_remaining_file"
printf 'missing-headers\n' >"$lookup_mode_file"
printf '201\n' >"$create_status_file"

gh() {
  local endpoint='' method=GET
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --include) shift ;;
      --method) method="$2"; shift 2 ;;
      repos/*) endpoint="$1"; shift ;;
      -f|-F) shift 2 ;;
      *) shift ;;
    esac
  done
  printf '%s %s\n' "$method" "$endpoint" >>"$NIGHTLY_GH_LOG"
  case "$endpoint" in
    */git/ref/tags/*)
      if [ "$method" != GET ]; then
        echo "create-nightly-tag-harness: unexpected tag method: $method" >&2
        return 2
      fi
      if [ ! -s "$NIGHTLY_TAG_STATE" ]; then
        if [ "$(cat "$NIGHTLY_LOOKUP_MODE")" = missing-headers ]; then
          printf 'gh: Not Found (HTTP 404)\n' >&2
        else
          printf 'HTTP/2.0 404 Not Found\n\n{"message":"Not Found","status":"404"}\n' >&2
        fi
        return 1
      fi
      if [ -n "${NIGHTLY_TAG_READ_TOKEN:-}" ] && [ "${GH_TOKEN:-}" = "$NIGHTLY_TAG_READ_TOKEN" ]; then
        printf 'HTTP/2.0 200 OK\n\n{"object":{"sha":"%s"}}\n' "$(cat "$NIGHTLY_TAG_STATE")"
        return 0
      fi
      remaining="$(cat "$NIGHTLY_PEEL_REMAINING")"
      if [ "$remaining" -gt 0 ]; then
        printf '%s\n' "$((remaining - 1))" >"$NIGHTLY_PEEL_REMAINING"
        printf 'gh: Not Found (HTTP 404)\n' >&2
        return 1
      fi
      object_sha="$(cat "$NIGHTLY_TAG_STATE")"
      if [ -n "${NIGHTLY_PEEL_OBJECT_SHA:-}" ]; then
        object_sha="$NIGHTLY_PEEL_OBJECT_SHA"
      fi
      printf 'HTTP/2.0 200 OK\n\n{"object":{"sha":"%s"}}\n' "$object_sha"
      ;;
    */git/refs)
      [ "$method" = POST ]
      status="$(cat "$NIGHTLY_CREATE_STATUS")"
      if [ "$status" = 422 ]; then
        printf 'HTTP/2.0 422 Unprocessable Entity\n\n{"message":"Reference already exists"}\n' >&2
        return 1
      fi
      if [ "$status" != 201 ]; then
        printf 'gh: Not Found (HTTP 404)\n' >&2
        return 1
      fi
      printf '%s\n' "$SOURCE_SHA" >"$NIGHTLY_TAG_STATE"
      printf 'HTTP/2.0 201 Created\n\n{"object":{"sha":"%s"}}\n' "$SOURCE_SHA"
      ;;
    *)
      echo "create-nightly-tag-harness: unsupported gh endpoint: $endpoint" >&2
      return 2
      ;;
  esac
}
export -f gh
export NIGHTLY_GH_LOG="$log"
export NIGHTLY_TAG_STATE="$tmp/tag-state"
export NIGHTLY_PEEL_REMAINING="$peel_remaining_file"
export NIGHTLY_LOOKUP_MODE="$lookup_mode_file"
export NIGHTLY_CREATE_STATUS="$create_status_file"
export GITHUB_REPOSITORY='jatmn/Codex-warp'
export SOURCE_SHA='1111111111111111111111111111111111111111'
export TAG='nightly-20260913-111111111111'
export WORKFLOW_SHA='2222222222222222222222222222222222222222'
export GITHUB_RUN_ID=34762668596
export GITHUB_RUN_ATTEMPT=1
export NIGHTLY_INTENT_FILE="$tmp/nightly-intent.json"
export NIGHTLY_RECEIPT_FILE="$tmp/nightly-tag-receipt.json"
export NIGHTLY_TAG_PEEL_ATTEMPTS=3
export NIGHTLY_TAG_PEEL_SLEEP=0
printf '{}\n' >"$NIGHTLY_INTENT_FILE"
: >"$NIGHTLY_TAG_STATE"
: >"$log"

run_create() {
  bash "$root/scripts/create-nightly-tag.sh"
}

# gh 2.100-style 404 with no HTTP status line, then a delayed peel.
run_create
jq -e --arg tag "$TAG" --arg sha "$SOURCE_SHA" --arg workflow "$WORKFLOW_SHA" \
  --argjson run "$GITHUB_RUN_ID" --argjson attempt "$GITHUB_RUN_ATTEMPT" \
  '.tag==$tag and .peeledSha==$sha and .workflowSha==$workflow and .runId==$run and .runAttempt==$attempt and .apiStatus==201 and (.intentSha256|test("^[0-9a-f]{64}$"))' \
  "$NIGHTLY_RECEIPT_FILE" >/dev/null
grep -c '^GET .*/git/ref/tags/' "$log" | grep -Fx 3 >/dev/null
grep -E '^POST .*/git/refs$' "$log" >/dev/null

# Header-bearing 404 still proves absence.
: >"$NIGHTLY_TAG_STATE"
: >"$log"
printf 'headers\n' >"$lookup_mode_file"
printf '0\n' >"$peel_remaining_file"
rm -f "$NIGHTLY_RECEIPT_FILE"
run_create
jq -e '.apiStatus==201' "$NIGHTLY_RECEIPT_FILE" >/dev/null

# Existing tag fails closed before POST.
: >"$log"
printf '%s\n' "$SOURCE_SHA" >"$NIGHTLY_TAG_STATE"
printf '0\n' >"$peel_remaining_file"
appeared=0
run_create >/dev/null 2>"$tmp/appeared.err" || appeared=$?
[ "$appeared" -ne 0 ]
grep -F "nightly tag appeared before creation: $TAG" "$tmp/appeared.err" >/dev/null
if grep -E '^POST ' "$log" >/dev/null; then
  echo 'create-nightly-tag-harness: existing tag reached POST' >&2
  exit 1
fi

# 422 after absence proof fails closed without a receipt.
: >"$NIGHTLY_TAG_STATE"
: >"$log"
printf 'missing-headers\n' >"$lookup_mode_file"
printf '201\n' >"$create_status_file"
printf '422\n' >"$create_status_file"
rm -f "$NIGHTLY_RECEIPT_FILE"
raced=0
run_create >/dev/null 2>"$tmp/race.err" || raced=$?
[ "$raced" -ne 0 ]
[ ! -f "$NIGHTLY_RECEIPT_FILE" ]
grep -F 'tag create raced with 422' "$tmp/race.err" >/dev/null

# Unclassifiable lookup is not treated as absence.
: >"$NIGHTLY_TAG_STATE"
: >"$log"
gh() {
  printf 'GET %s\n' "$1" >>"$NIGHTLY_GH_LOG"
  printf 'gh: Gateway Timeout\n' >&2
  return 1
}
export -f gh
unclassified=0
run_create >/dev/null 2>"$tmp/unclassified.err" || unclassified=$?
[ "$unclassified" -ne 0 ]
grep -F 'unable to prove nightly tag absence with the mutation token' "$tmp/unclassified.err" >/dev/null

# Restore the classified fixture gh() after the unclassified override.
gh() {
  local endpoint='' method=GET
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --include) shift ;;
      --method) method="$2"; shift 2 ;;
      repos/*) endpoint="$1"; shift ;;
      -f|-F) shift 2 ;;
      *) shift ;;
    esac
  done
  printf '%s %s\n' "$method" "$endpoint" >>"$NIGHTLY_GH_LOG"
  case "$endpoint" in
    */git/ref/tags/*)
      if [ "$method" != GET ]; then
        echo "create-nightly-tag-harness: unexpected tag method: $method" >&2
        return 2
      fi
      if [ ! -s "$NIGHTLY_TAG_STATE" ]; then
        printf 'gh: Not Found (HTTP 404)\n' >&2
        return 1
      fi
      if [ -n "${NIGHTLY_TAG_READ_TOKEN:-}" ] && [ "${GH_TOKEN:-}" = "$NIGHTLY_TAG_READ_TOKEN" ]; then
        printf 'HTTP/2.0 200 OK\n\n{"object":{"sha":"%s"}}\n' "$(cat "$NIGHTLY_TAG_STATE")"
        return 0
      fi
      remaining="$(cat "$NIGHTLY_PEEL_REMAINING")"
      if [ "$remaining" -gt 0 ]; then
        printf '%s\n' "$((remaining - 1))" >"$NIGHTLY_PEEL_REMAINING"
        printf 'gh: Not Found (HTTP 404)\n' >&2
        return 1
      fi
      object_sha="$(cat "$NIGHTLY_TAG_STATE")"
      if [ -n "${NIGHTLY_PEEL_OBJECT_SHA:-}" ]; then
        object_sha="$NIGHTLY_PEEL_OBJECT_SHA"
      fi
      printf 'HTTP/2.0 200 OK\n\n{"object":{"sha":"%s"}}\n' "$object_sha"
      ;;
    */git/refs)
      [ "$method" = POST ]
      printf '%s\n' "$SOURCE_SHA" >"$NIGHTLY_TAG_STATE"
      printf 'HTTP/2.0 201 Created\n\n{"object":{"sha":"%s"}}\n' "$SOURCE_SHA"
      ;;
    *)
      echo "create-nightly-tag-harness: unsupported gh endpoint: $endpoint" >&2
      return 2
      ;;
  esac
}
export -f gh

# Mutation-token peel 404s are retried with the job token before failing.
: >"$NIGHTLY_TAG_STATE"
: >"$log"
printf '99\n' >"$peel_remaining_file"
printf 'missing-headers\n' >"$lookup_mode_file"
rm -f "$NIGHTLY_RECEIPT_FILE"
export GH_TOKEN='mutation-token'
export NIGHTLY_TAG_READ_TOKEN='read-token'
run_create
jq -e --arg tag "$TAG" --arg sha "$SOURCE_SHA" '.tag==$tag and .peeledSha==$sha and .apiStatus==201' \
  "$NIGHTLY_RECEIPT_FILE" >/dev/null
grep -E '^GET .*/git/ref/tags/' "$log" >/dev/null
grep -E '^POST .*/git/refs$' "$log" >/dev/null

# Without a distinct job token, persistent mutation 404s still fail closed.
: >"$NIGHTLY_TAG_STATE"
: >"$log"
printf '99\n' >"$peel_remaining_file"
rm -f "$NIGHTLY_RECEIPT_FILE"
unset NIGHTLY_TAG_READ_TOKEN
export GH_TOKEN='mutation-token'
stuck=0
run_create >/dev/null 2>"$tmp/stuck.err" || stuck=$?
[ "$stuck" -ne 0 ]
[ ! -f "$NIGHTLY_RECEIPT_FILE" ]
grep -F 'peel kept returning 404 after tag create' "$tmp/stuck.err" >/dev/null

# A 200 peel with the wrong SHA must fail closed without a receipt.
: >"$NIGHTLY_TAG_STATE"
: >"$log"
printf '0\n' >"$peel_remaining_file"
rm -f "$NIGHTLY_RECEIPT_FILE"
unset NIGHTLY_TAG_READ_TOKEN
export GH_TOKEN='mutation-token'
export NIGHTLY_PEEL_OBJECT_SHA='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
wrong_sha=0
run_create >/dev/null 2>"$tmp/wrong-sha.err" || wrong_sha=$?
[ "$wrong_sha" -ne 0 ]
[ ! -f "$NIGHTLY_RECEIPT_FILE" ]
unset NIGHTLY_PEEL_OBJECT_SHA

echo 'create-nightly-tag-harness: ok'
