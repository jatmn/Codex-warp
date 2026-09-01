#!/usr/bin/env bash
# Fail closed when a pull request resembles the configured Release Please PR.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

policy="${RELEASE_AUTOMATION_POLICY:-tools/release-automation-policy.json}"
event_path="${1:-${GITHUB_EVENT_PATH:-}}"
# An explicit event file is a PR fixture. Do not let a surrounding Actions
# `push` (or other) event name skip classification.
if [ -n "${1:-}" ]; then
  event_name=pull_request
else
  event_name="${GITHUB_EVENT_NAME:-pull_request}"
fi

if [ "$event_name" != 'pull_request' ]; then
  echo 'check-release-readiness: non-PR event; not applicable'
  exit 0
fi
if [ -z "$event_path" ] || [ ! -f "$event_path" ]; then
  echo 'check-release-readiness: a pull-request event JSON file is required' >&2
  exit 2
fi
if ! jq -e '
  .schemaVersion == 1 and
  (.enabled | type == "boolean") and
  (.repository | type == "string") and
  (.baseBranch | type == "string") and
  (.headRepository | type == "string") and
  (.headRef | type == "string") and
  (.appBot.type == "Bot") and
  (.requiredFiles | type == "array") and
  (.allowedFiles | type == "array")
' "$policy" >/dev/null; then
  echo "check-release-readiness: invalid policy: $policy" >&2
  exit 2
fi

actual_repository="$(jq -r '.repository.full_name // .pull_request.base.repo.full_name // empty' "$event_path")"
base_branch="$(jq -r '.pull_request.base.ref // empty' "$event_path")"
head_repository="$(jq -r '.pull_request.head.repo.full_name // empty' "$event_path")"
head_ref="$(jq -r '.pull_request.head.ref // empty' "$event_path")"
author_id="$(jq -r '.pull_request.user.id // empty' "$event_path")"
author_login="$(jq -r '.pull_request.user.login // empty' "$event_path")"
author_type="$(jq -r '.pull_request.user.type // empty' "$event_path")"

expected_repository="$(jq -r '.repository' "$policy")"
expected_base="$(jq -r '.baseBranch' "$policy")"
expected_head_repository="$(jq -r '.headRepository' "$policy")"
expected_head_ref="$(jq -r '.headRef' "$policy")"
expected_author_id="$(jq -r '.appBot.id // empty' "$policy")"
expected_author_login="$(jq -r '.appBot.login // empty' "$policy")"
expected_author_type="$(jq -r '.appBot.type' "$policy")"

branch_match=0
author_match=0
[ "$head_ref" = "$expected_head_ref" ] && branch_match=1
if [ -n "$expected_author_id" ] && [ -n "$expected_author_login" ] &&
   [ "$author_id" = "$expected_author_id" ] &&
   [ "$author_login" = "$expected_author_login" ] &&
   [ "$author_type" = "$expected_author_type" ]; then
  author_match=1
fi

if [ "$(jq -r '.enabled' "$policy")" != 'true' ]; then
  if [ "$branch_match" -eq 1 ] || [ "$author_match" -eq 1 ]; then
    echo 'check-release-readiness: automation-shaped PR found while release automation is disabled' >&2
    exit 1
  fi
  echo 'check-release-readiness: ordinary PR (release automation disabled)'
  exit 0
fi

identity_match=0
if [ "$actual_repository" = "$expected_repository" ] &&
   [ "$base_branch" = "$expected_base" ] &&
   [ "$head_repository" = "$expected_head_repository" ] &&
   [ "$branch_match" -eq 1 ] && [ "$author_match" -eq 1 ]; then
  identity_match=1
fi

if [ "$identity_match" -ne 1 ]; then
  if [ "$branch_match" -eq 1 ] || [ "$author_match" -eq 1 ]; then
    echo 'check-release-readiness: ambiguous automation-shaped PR; identity predicates do not all match' >&2
    exit 1
  fi
  echo 'check-release-readiness: ordinary PR'
  exit 0
fi

declare -a changed_files
if jq -e '.changed_files | type == "array"' "$event_path" >/dev/null 2>&1; then
  mapfile -t changed_files < <(jq -r '.changed_files[]' "$event_path")
else
  pr_number="$(jq -r '.pull_request.number // .number // empty' "$event_path")"
  [ -n "$pr_number" ] || { echo 'check-release-readiness: PR number is missing' >&2; exit 2; }
  command -v gh >/dev/null || { echo 'check-release-readiness: gh is required' >&2; exit 2; }
  mapfile -t changed_files < <(gh api --paginate "repos/$expected_repository/pulls/$pr_number/files" --jq '.[].filename')
fi

while IFS= read -r required; do
  if ! printf '%s\n' "${changed_files[@]}" | grep -Fqx -- "$required"; then
    echo "check-release-readiness: release PR is missing required file: $required" >&2
    exit 1
  fi
done < <(jq -r '.requiredFiles[]' "$policy")
for changed in "${changed_files[@]}"; do
  if ! jq -e --arg file "$changed" '.allowedFiles | index($file) != null' "$policy" >/dev/null; then
    echo "check-release-readiness: release PR changed an unexpected file: $changed" >&2
    exit 1
  fi
done

if jq -e '.release_state | type == "object"' "$event_path" >/dev/null 2>&1; then
  state_fixture="$(mktemp)"
  trap 'rm -f "$state_fixture"' EXIT
  jq '.release_state' "$event_path" >"$state_fixture"
  OFFICIAL_STATE_FIXTURE="$state_fixture" GITHUB_REPOSITORY="$expected_repository" \
    bash scripts/check-prior-official-releases.sh >/dev/null
else
  GITHUB_REPOSITORY="$expected_repository" bash scripts/check-prior-official-releases.sh >/dev/null
fi

echo 'check-release-readiness: release PR identity, file contract, and prior release state are ready'
