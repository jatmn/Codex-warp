#!/usr/bin/env bash
# Create one unpublished official GitHub draft for the newest official SemVer
# tag when that tag exists and has no GitHub Release. The pinned Release Please
# Action treats the Git tag as the release and will not POST a draft. This
# script does not move the tag, change Cargo.toml, or open a newer version.
# Callers must use a token that can view and create drafts.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="${GITHUB_REPOSITORY:?}"
git_root="$(git rev-parse --show-toplevel)"
cd "$git_root"
command -v gh >/dev/null || {
  echo 'create-missing-official-draft: gh is required' >&2
  exit 2
}

write_output() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s\n' "$@" >>"$GITHUB_OUTPUT"
  fi
}

git fetch --no-tags origin main >/dev/null 2>&1
git fetch origin 'refs/tags/v*:refs/tags/v*' >/dev/null 2>&1 || true

mapfile -t official_tags < <(
  { git tag --list 'v*.*.*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' || true; } | sort -V
)
if [ "${#official_tags[@]}" -eq 0 ]; then
  echo 'create-missing-official-draft: no official tags' >&2
  write_output 'created=false'
  exit 0
fi

latest="${official_tags[-1]}"
sha="$(git rev-parse "${latest}^{}")"
[[ "$sha" =~ ^[0-9a-f]{40}$ ]] || {
  echo "create-missing-official-draft: $latest did not peel to a commit" >&2
  exit 1
}
git merge-base --is-ancestor "$sha" origin/main || {
  echo "create-missing-official-draft: $latest is not an ancestor of origin/main" >&2
  exit 1
}

version="$(git show "$sha:Cargo.toml" | sed -n 's/^version = "\([^"]*\)"/\1/p' | head -1)"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "create-missing-official-draft: Cargo.toml at $sha is not stable SemVer" >&2
  exit 1
}
[ "$latest" = "v$version" ] || {
  echo "create-missing-official-draft: $latest does not match Cargo version $version" >&2
  exit 1
}

releases="$(
  gh api --paginate "repos/${repo}/releases" --jq '.[]' | jq -s '.'
)"

while IFS= read -r tag; do
  [ "$tag" = "$latest" ] && continue
  published="$(jq --arg tag "$tag" '[.[] | select(.tag_name == $tag and .draft == false and .prerelease == false and .published_at != null)] | length' <<<"$releases")"
  [ "$published" -eq 1 ] || {
    echo "create-missing-official-draft: prior tag $tag is not one published release" >&2
    exit 1
  }
done < <(printf '%s\n' "${official_tags[@]}")

count="$(jq --arg tag "$latest" '[.[] | select(.tag_name == $tag)] | length' <<<"$releases")"
if [ "$count" -gt 1 ]; then
  echo "create-missing-official-draft: multiple GitHub Releases share $latest" >&2
  exit 1
fi
if [ "$count" -eq 1 ]; then
  jq -e --arg tag "$latest" \
    '.[] | select(.tag_name == $tag) | .draft == false and .prerelease == false and .published_at != null' \
    <<<"$releases" >/dev/null || {
    echo "create-missing-official-draft: $latest already has a GitHub Release that is not a published stable release" >&2
    exit 1
  }
  echo "create-missing-official-draft: $latest already has a published release" >&2
  write_output 'created=false'
  exit 0
fi

body="$(
  git show "$sha:CHANGELOG.md" | awk -v ver="$version" '
    index($0, "## [" ver "]") == 1 { grab = 1; print; next }
    grab && /^## / { exit }
    grab { print }
  '
)"
[ -n "$(printf '%s' "$body" | tr -d '[:space:]')" ] || {
  echo "create-missing-official-draft: CHANGELOG.md at $sha has no $version section" >&2
  exit 1
}

payload="$(jq -cn \
  --arg tag "$latest" \
  --arg sha "$sha" \
  --arg body "$body" \
  '{
    tag_name: $tag,
    name: $tag,
    target_commitish: $sha,
    draft: true,
    prerelease: false,
    make_latest: "false",
    generate_release_notes: false,
    body: $body
  }')"
release="$(printf '%s\n' "$payload" | gh api --method POST "repos/${repo}/releases" --input -)"
jq -e --arg tag "$latest" \
  '.tag_name == $tag and .name == $tag and .draft == true and .published_at == null and .prerelease == false' \
  <<<"$release" >/dev/null || {
  echo 'create-missing-official-draft: POST did not return an unpublished official draft' >&2
  exit 1
}
release_id="$(jq -r '.id' <<<"$release")"
[[ "$release_id" =~ ^[1-9][0-9]*$ ]] || {
  echo 'create-missing-official-draft: POST returned an invalid release id' >&2
  exit 1
}

git fetch --force --no-tags origin "refs/tags/${latest}:refs/tags/${latest}" >/dev/null
[ "$(git rev-parse "${latest}^{}")" = "$sha" ] || {
  echo "create-missing-official-draft: $latest moved during draft creation" >&2
  exit 1
}

bash "$script_dir/lookup-official-draft.sh" "$repo" "$latest" >/dev/null

echo "create-missing-official-draft: created $latest id=$release_id at $sha" >&2
write_output \
  'created=true' \
  "tag=$latest" \
  "sha=$sha" \
  "release_id=$release_id"
