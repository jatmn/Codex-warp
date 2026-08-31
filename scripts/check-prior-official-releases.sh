#!/usr/bin/env bash
# Refuse a new official release while an earlier stable tag/draft/finalizer is incomplete.
set -euo pipefail

repository="${GITHUB_REPOSITORY:-jatmn/Codex-warp}"

if [ -n "${OFFICIAL_STATE_FIXTURE:-}" ]; then
  [ -f "$OFFICIAL_STATE_FIXTURE" ] || { echo 'check-prior-official-releases: fixture is missing' >&2; exit 2; }
  releases="$(jq -c '.releases // []' "$OFFICIAL_STATE_FIXTURE")"
  tags="$(jq -c '.tags // []' "$OFFICIAL_STATE_FIXTURE")"
  active="$(jq -c '.activeOfficialTags // []' "$OFFICIAL_STATE_FIXTURE")"
else
  command -v gh >/dev/null || { echo 'check-prior-official-releases: gh is required' >&2; exit 2; }
  releases="$(gh api --paginate "repos/$repository/releases" --jq '[.[] | {tag_name,draft,published_at}]' | jq -sc 'add // []')"
  tags="$(gh api --paginate "repos/$repository/tags" --jq '[.[].name]' | jq -sc 'add // []')"
  active="$({
    gh api "repos/$repository/actions/runs?status=in_progress" | jq --argjson current "${GITHUB_RUN_ID:-0}" '[.workflow_runs[] | select(.id != $current and ((.name == "Release") or (.name == "Release Recovery"))) | .head_branch]'
    gh api "repos/$repository/actions/runs?status=queued" | jq --argjson current "${GITHUB_RUN_ID:-0}" '[.workflow_runs[] | select(.id != $current and ((.name == "Release") or (.name == "Release Recovery"))) | .head_branch]'
  } | jq -sc 'add // []')"
fi

if jq -e '[.[] | select(.draft == true and (.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")))] | length > 0' <<<"$releases" >/dev/null; then
  echo 'check-prior-official-releases: an official draft is still outstanding' >&2
  exit 1
fi
while IFS= read -r tag; do
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
  count="$(jq --arg tag "$tag" '[.[] | select(.tag_name == $tag and .draft == false and .published_at != null)] | length' <<<"$releases")"
  [ "$count" -eq 1 ] || { echo "check-prior-official-releases: $tag does not have exactly one published release" >&2; exit 1; }
done < <(jq -r '.[]' <<<"$tags")
while IFS= read -r tag; do
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
  jq -e --arg tag "$tag" 'index($tag) != null' <<<"$tags" >/dev/null || {
    echo "check-prior-official-releases: published release has no matching tag: $tag" >&2
    exit 1
  }
done < <(jq -r '.[] | select(.draft == false and .published_at != null) | .tag_name' <<<"$releases")
if jq -e '[.[] | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))] | length > 0' <<<"$active" >/dev/null; then
  echo 'check-prior-official-releases: an official finalizer or recovery run is active' >&2
  exit 1
fi

echo 'check-prior-official-releases: ready'
