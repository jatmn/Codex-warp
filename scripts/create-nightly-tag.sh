#!/usr/bin/env bash
# Create an immutable nightly tag with App-token HTTP classification and a
# retried post-create peel before writing the tag-creation receipt.
set -euo pipefail

: "${GITHUB_REPOSITORY:?}"
: "${SOURCE_SHA:?}"
: "${TAG:?}"
: "${WORKFLOW_SHA:?}"
: "${GITHUB_RUN_ID:?}"
: "${GITHUB_RUN_ATTEMPT:?}"
[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]
[[ "$TAG" =~ ^nightly-[0-9]{8}-[0-9a-f]{12}$ ]]
[[ "$WORKFLOW_SHA" =~ ^[0-9a-f]{40}$ ]]
[[ "$GITHUB_RUN_ID" =~ ^[1-9][0-9]*$ ]]
[[ "$GITHUB_RUN_ATTEMPT" =~ ^[1-9][0-9]*$ ]]

root="$(git rev-parse --show-toplevel)"
intent="${NIGHTLY_INTENT_FILE:-nightly-intent.json}"
receipt="${NIGHTLY_RECEIPT_FILE:-nightly-tag-receipt.json}"
peel_attempts="${NIGHTLY_TAG_PEEL_ATTEMPTS:-8}"
peel_sleep="${NIGHTLY_TAG_PEEL_SLEEP:-1}"
[[ "$peel_attempts" =~ ^[1-9][0-9]*$ ]]
[[ "$peel_sleep" =~ ^[0-9]+([.][0-9]+)?$ ]]
[ -f "$intent" ] || { echo "create-nightly-tag: missing intent file: $intent" >&2; exit 1; }

api_body() {
  sed -n '/^{/,$p'
}

http_code() {
  local raw="$1"
  if [[ "$raw" =~ HTTP/[0-9.]+[[:space:]]+([0-9]{3}) ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  if [[ "$raw" =~ \(HTTP[[:space:]]+([0-9]{3})\) ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
    return 0
  fi
  return 1
}

lookup_tag() {
  local token="${1-}"
  if [ -n "$token" ]; then
    GH_TOKEN="$token" gh api --include "repos/$GITHUB_REPOSITORY/git/ref/tags/$TAG" 2>&1
  else
    gh api --include "repos/$GITHUB_REPOSITORY/git/ref/tags/$TAG" 2>&1
  fi
}

# Returns 0 and prints the peeled SHA, 1 for a classified 404, 2 for hard failure.
try_peel() {
  local token="${1-}"
  local attempt=0
  local peeled peeled_sha
  while [ "$attempt" -lt "$peel_attempts" ]; do
    attempt=$((attempt + 1))
    if peeled="$(lookup_tag "$token")"; then
      [ "$(http_code "$peeled" || true)" = 200 ] || {
        echo 'create-nightly-tag: peel returned an unexpected success status' >&2
        printf '%s\n' "$peeled" >&2
        return 2
      }
      peeled_sha="$(api_body <<<"$peeled" | jq -er '.object.sha')"
      [ "$peeled_sha" = "$SOURCE_SHA" ] || return 2
      printf '%s\n' "$peeled_sha"
      return 0
    fi
    [ "$(http_code "$peeled" || true)" = 404 ] || {
      echo 'create-nightly-tag: unable to peel nightly tag after creation' >&2
      printf '%s\n' "$peeled" >&2
      return 2
    }
    [ "$attempt" -lt "$peel_attempts" ] || return 1
    sleep "$peel_sleep"
  done
  return 1
}

lookup=''
if lookup="$(lookup_tag "${GH_TOKEN-}")"; then
  [ "$(http_code "$lookup" || true)" = 200 ] || {
    echo 'create-nightly-tag: tag lookup returned an unexpected success status' >&2
    printf '%s\n' "$lookup" >&2
    exit 1
  }
  echo "nightly tag appeared before creation: $TAG" >&2
  exit 1
fi
[ "$(http_code "$lookup" || true)" = 404 ] || {
  echo 'unable to prove nightly tag absence with the mutation token' >&2
  printf '%s\n' "$lookup" >&2
  exit 1
}

created=''
if created="$(gh api --include --method POST "repos/$GITHUB_REPOSITORY/git/refs" -f ref="refs/tags/$TAG" -f sha="$SOURCE_SHA" 2>&1)"; then
  [ "$(http_code "$created" || true)" = 201 ] || {
    echo 'create-nightly-tag: create returned an unexpected success status' >&2
    printf '%s\n' "$created" >&2
    exit 1
  }
  [ "$(api_body <<<"$created" | jq -er '.object.sha')" = "$SOURCE_SHA" ]
else
  [ "$(http_code "$created" || true)" = 422 ] || {
    echo 'create-nightly-tag: create failed without a classifiable race' >&2
    printf '%s\n' "$created" >&2
    exit 1
  }
  echo "create-nightly-tag: tag create raced with 422: $TAG" >&2
  printf '%s\n' "$created" >&2
  exit 1
fi

peeled_sha=''
peel_status=0
peeled_sha="$(try_peel "${GH_TOKEN-}")" || peel_status=$?
if [ "$peel_status" -eq 2 ]; then
  exit 2
fi
if [ "$peel_status" -ne 0 ]; then
  if [ -n "${NIGHTLY_TAG_READ_TOKEN:-}" ] && [ "$NIGHTLY_TAG_READ_TOKEN" != "${GH_TOKEN-}" ]; then
    peel_status=0
    peeled_sha="$(try_peel "$NIGHTLY_TAG_READ_TOKEN")" || peel_status=$?
    if [ "$peel_status" -eq 2 ]; then
      exit 2
    fi
  fi
fi
[ -n "$peeled_sha" ] || {
  echo 'create-nightly-tag: peel kept returning 404 after tag create' >&2
  exit 1
}

jq -n \
  --arg tag "$TAG" \
  --arg sha "$peeled_sha" \
  --arg intent "$(bash "$root/scripts/sha256-file.sh" "$intent")" \
  --arg workflow "$WORKFLOW_SHA" \
  --argjson run "$GITHUB_RUN_ID" \
  --argjson attempt "$GITHUB_RUN_ATTEMPT" \
  '{schemaVersion:1,tag:$tag,peeledSha:$sha,intentSha256:$intent,workflowSha:$workflow,runId:$run,runAttempt:$attempt,apiStatus:201}' \
  >"$receipt"
