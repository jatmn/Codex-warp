#!/usr/bin/env bash
# Create or fast-forward refs/heads/nightly with explicit API race handling.
set -euo pipefail

: "${GITHUB_REPOSITORY:?}"
: "${SOURCE_SHA:?}"
: "${TAG:?}"
[[ "$SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]
[[ "$TAG" =~ ^nightly-[0-9]{8}-[0-9a-f]{12}$ ]]

endpoint="repos/$GITHUB_REPOSITORY/git/ref/heads/nightly"
update_endpoint="repos/$GITHUB_REPOSITORY/git/refs/heads/nightly"
api_body() {
  sed -n '/^{/,$p'
}

verify_release_identity() {
  local release tag_sha
  release="$(gh api "repos/$GITHUB_REPOSITORY/releases/tags/$TAG")"
  jq -e --arg tag "$TAG" '.tag_name==$tag and .draft==false and .published_at!=null and .prerelease==true' <<<"$release" >/dev/null
  tag_sha="$(gh api "repos/$GITHUB_REPOSITORY/git/ref/tags/$TAG" --jq '.object.sha')"
  [ "$tag_sha" = "$SOURCE_SHA" ]
}

read_branch() {
  local response
  if response="$(gh api --include "$endpoint" 2>&1)"; then
    grep -Eq '^HTTP/[0-9.]+ 200([[:space:]]|$)' <<<"$response" || {
      echo 'advance-nightly-branch: branch lookup returned an unexpected success status' >&2
      return 2
    }
    api_body <<<"$response" | jq -er '.object.sha'
    return 0
  fi
  if grep -Eq '^HTTP/[0-9.]+ 404([[:space:]]|$)' <<<"$response"; then
    return 1
  fi
  echo 'advance-nightly-branch: unable to classify the nightly branch' >&2
  printf '%s\n' "$response" >&2
  return 2
}

verify_release_identity
branch_action=''
branch_sha=''
if branch_sha="$(read_branch)"; then
  if [ "$branch_sha" = "$SOURCE_SHA" ]; then
    branch_action=already-equal
  else
    git cat-file -e "$branch_sha^{commit}" 2>/dev/null || git fetch --no-tags origin "$branch_sha"
    git merge-base --is-ancestor "$branch_sha" "$SOURCE_SHA" || {
      echo "advance-nightly-branch: existing branch is not an ancestor: $branch_sha" >&2
      exit 1
    }
    update="$(gh api --include --method PATCH "$update_endpoint" -f sha="$SOURCE_SHA" -F force=false 2>&1)" || {
      echo 'advance-nightly-branch: non-force fast-forward failed' >&2
      printf '%s\n' "$update" >&2
      exit 1
    }
    grep -Eq '^HTTP/[0-9.]+ 200([[:space:]]|$)' <<<"$update"
    branch_action=fast-forwarded
  fi
else
  status=$?
  [ "$status" -eq 1 ] || exit "$status"
  # The immediately preceding App-token read proved an exact 404. A 422 can
  # only be accepted when the racing creator chose the exact selected SHA.
  if created="$(gh api --include --method POST "repos/$GITHUB_REPOSITORY/git/refs" -f ref=refs/heads/nightly -f sha="$SOURCE_SHA" 2>&1)"; then
    grep -Eq '^HTTP/[0-9.]+ 201([[:space:]]|$)' <<<"$created" || {
      echo 'advance-nightly-branch: create returned an unexpected success status' >&2
      exit 1
    }
    [ "$(api_body <<<"$created" | jq -er '.object.sha')" = "$SOURCE_SHA" ]
    branch_action=created
  else
    grep -Eq '^HTTP/[0-9.]+ 422([[:space:]]|$)' <<<"$created" || {
      echo 'advance-nightly-branch: create failed without a classifiable race' >&2
      printf '%s\n' "$created" >&2
      exit 1
    }
    branch_sha="$(read_branch)" || {
      echo 'advance-nightly-branch: 422 winner could not be reread' >&2
      exit 1
    }
    [ "$branch_sha" = "$SOURCE_SHA" ] || {
      echo "advance-nightly-branch: 422 winner selected another SHA: $branch_sha" >&2
      exit 1
    }
    branch_action=already-equal
  fi
fi

final_sha="$(read_branch)"
[ "$final_sha" = "$SOURCE_SHA" ] || {
  echo "advance-nightly-branch: final branch mismatch: $final_sha" >&2
  exit 1
}
verify_release_identity
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  printf 'branch_action=%s\n' "$branch_action" >>"$GITHUB_OUTPUT"
fi
printf '%s\n' "$branch_action"
