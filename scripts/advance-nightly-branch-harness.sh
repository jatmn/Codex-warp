#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
state="$tmp/state"
log="$tmp/gh.log"

gh() {
  local endpoint='' method=GET
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --method) method="$2"; shift 2 ;;
      repos/*) endpoint="$1"; shift ;;
      *) shift ;;
    esac
  done
  printf '%s %s\n' "$method" "$endpoint" >>"$NIGHTLY_GH_LOG"
  case "$endpoint" in
    */releases/tags/*)
      printf '{"tag_name":"%s","draft":false,"published_at":"2026-08-31T10:00:00Z","prerelease":true}\n' "$TAG"
      ;;
    */git/ref/tags/*)
      printf '%s\n' "$SOURCE_SHA"
      ;;
    */git/ref/heads/nightly)
      [ "$method" = GET ]
      if [ -s "$NIGHTLY_BRANCH_STATE" ]; then
        printf 'HTTP/2.0 200 OK\n\n{"object":{"sha":"%s"}}\n' "$(cat "$NIGHTLY_BRANCH_STATE")"
      else
        printf 'HTTP/2.0 404 Not Found\n\n{"message":"Not Found"}\n' >&2
        return 1
      fi
      ;;
    */git/refs/heads/nightly)
      [ "$method" = PATCH ]
      printf '%s\n' "$SOURCE_SHA" >"$NIGHTLY_BRANCH_STATE"
      printf 'HTTP/2.0 200 OK\n\n{"object":{"sha":"%s"}}\n' "$SOURCE_SHA"
      ;;
    */git/refs)
      [ "$method" = POST ]
      printf '%s\n' "${BRANCH_RACE_SHA:-$SOURCE_SHA}" >"$NIGHTLY_BRANCH_STATE"
      if [ "${BRANCH_CREATE_STATUS:-201}" = 422 ]; then
        printf 'HTTP/2.0 422 Unprocessable Entity\n\n{"message":"Reference already exists"}\n' >&2
        return 1
      fi
      printf 'HTTP/2.0 201 Created\n\n{"object":{"sha":"%s"}}\n' "$SOURCE_SHA"
      ;;
    *)
      echo "advance-nightly-branch-harness: unsupported gh endpoint: $endpoint" >&2
      return 2
      ;;
  esac
}
export -f gh
export NIGHTLY_BRANCH_STATE="$state"
export NIGHTLY_GH_LOG="$log"
export GITHUB_REPOSITORY='jatmn/Codex-warp'
export SOURCE_SHA='1111111111111111111111111111111111111111'
export TAG='nightly-20260831-111111111111'

: >"$state"
: >"$log"
BRANCH_CREATE_STATUS=201 bash "$root/scripts/advance-nightly-branch.sh" >"$tmp/created"
grep -Fx created "$tmp/created" >/dev/null
if grep -E 'PATCH .*/git/ref/heads/nightly$' "$NIGHTLY_GH_LOG" >/dev/null; then
  echo 'advance-nightly-branch-harness: create path used the singular PATCH endpoint' >&2
  exit 1
fi

: >"$state"
: >"$log"
BRANCH_CREATE_STATUS=422 BRANCH_RACE_SHA="$SOURCE_SHA" bash "$root/scripts/advance-nightly-branch.sh" >"$tmp/equal"
grep -Fx already-equal "$tmp/equal" >/dev/null

: >"$state"
if BRANCH_CREATE_STATUS=422 BRANCH_RACE_SHA='3333333333333333333333333333333333333333' \
  bash "$root/scripts/advance-nightly-branch.sh" >/dev/null 2>&1; then
  echo 'advance-nightly-branch-harness: divergent 422 winner was accepted' >&2
  exit 1
fi

ancestor="$(git -C "$root" rev-parse --verify HEAD^)"
source_sha="$(git -C "$root" rev-parse --verify HEAD)"
export SOURCE_SHA="$source_sha"
export TAG="nightly-20260831-${source_sha:0:12}"
printf '%s\n' "$ancestor" >"$state"
: >"$log"
bash "$root/scripts/advance-nightly-branch.sh" >"$tmp/fast-forwarded"
grep -Fx fast-forwarded "$tmp/fast-forwarded" >/dev/null
grep -E 'PATCH .*/git/refs/heads/nightly$' "$NIGHTLY_GH_LOG" >/dev/null
if grep -E 'PATCH .*/git/ref/heads/nightly$' "$NIGHTLY_GH_LOG" >/dev/null; then
  echo 'advance-nightly-branch-harness: fast-forward used the singular PATCH endpoint' >&2
  exit 1
fi

echo 'advance-nightly-branch-harness: ok'
