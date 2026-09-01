#!/usr/bin/env bash
# Print the unpublished official GitHub Release JSON for a SemVer tag.
# GET /releases/tags/{tag} omits drafts, and contents:read tokens cannot see
# unpublished releases. Callers must use a token that can view drafts.
set -euo pipefail

repo="${1:?repository}"
tag="${2:?tag}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "lookup-official-draft: invalid official tag: $tag" >&2
  exit 2
}

attempts="${LOOKUP_OFFICIAL_DRAFT_ATTEMPTS:-12}"
sleep_secs="${LOOKUP_OFFICIAL_DRAFT_SLEEP:-5}"
[[ "$attempts" =~ ^[1-9][0-9]*$ ]] || {
  echo 'lookup-official-draft: LOOKUP_OFFICIAL_DRAFT_ATTEMPTS must be a positive integer' >&2
  exit 2
}

release=''
for attempt in $(seq 1 "$attempts"); do
  matches="$(
    gh api --paginate "repos/${repo}/releases" --jq '.[]' |
      jq -s --arg tag "$tag" '[.[] | select(.tag_name == $tag)]'
  )"
  count="$(jq 'length' <<<"$matches")"
  if [ "$count" -gt 1 ]; then
    echo "lookup-official-draft: multiple GitHub Releases share $tag" >&2
    exit 1
  fi
  if [ "$count" -eq 1 ]; then
    release="$(jq -c '.[0]' <<<"$matches")"
    break
  fi
  if [ "$attempt" -lt "$attempts" ]; then
    sleep "$sleep_secs"
  fi
done

[ -n "$release" ] || {
  echo "Release Please did not create the expected draft for $tag" >&2
  exit 1
}
jq -e --arg tag "$tag" \
  '.tag_name == $tag and .draft == true and .published_at == null and .prerelease == false' \
  <<<"$release" >/dev/null || {
  echo "lookup-official-draft: $tag is not an unpublished non-prerelease draft" >&2
  exit 1
}
printf '%s\n' "$release"
