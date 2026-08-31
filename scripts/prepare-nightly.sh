#!/usr/bin/env bash
# Classify a nightly candidate using immutable refs and verified published state.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
: "${GITHUB_EVENT_NAME:?}"
: "${GITHUB_SHA:?}"
: "${GITHUB_RUN_ID:?}"
: "${GITHUB_REPOSITORY:?}"

if [ "$GITHUB_EVENT_NAME" = workflow_dispatch ] && [ "${GITHUB_REF:-}" != refs/heads/main ]; then
  echo 'prepare-nightly: manual dispatch must use refs/heads/main' >&2
  exit 1
fi
git fetch --no-tags origin main
remote_tags="$(git ls-remote --tags --refs origin 'refs/tags/nightly-*')" || {
  echo 'prepare-nightly: unable to enumerate remote nightly tags' >&2
  exit 1
}
while IFS=$'\t' read -r _object ref; do
  [ -z "$ref" ] && continue
  [[ "$ref" =~ ^refs/tags/nightly-[0-9]{8}-[0-9a-f]{12}$ ]] || {
    echo "prepare-nightly: malformed remote nightly ref: $ref" >&2
    exit 1
  }
  git fetch --force origin "$ref:$ref"
done <<<"$remote_tags"
remote_branch="$(git ls-remote --heads origin refs/heads/nightly)" || {
  echo 'prepare-nightly: unable to inspect the remote nightly branch' >&2
  exit 1
}
if [ -n "$remote_branch" ]; then
  git fetch --force origin '+refs/heads/nightly:refs/remotes/origin/nightly'
fi

live_main="$(git rev-parse origin/main)"
explicit=false
source_sha="$GITHUB_SHA"
if [ -n "${NIGHTLY_SOURCE_SHA_INPUT:-}" ]; then
  [[ "$NIGHTLY_SOURCE_SHA_INPUT" =~ ^[0-9a-f]{40}$ ]] || { echo 'prepare-nightly: source_sha must be a full lowercase SHA' >&2; exit 1; }
  source_sha="$NIGHTLY_SOURCE_SHA_INPUT"
  explicit=true
fi
git cat-file -e "$source_sha^{commit}"
git merge-base --is-ancestor "$source_sha" "$live_main" || { echo 'prepare-nightly: selected SHA is not reachable from main' >&2; exit 1; }

created_at="${NIGHTLY_RUN_CREATED_AT:-}"
if [ -z "$created_at" ]; then
  created_at="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID" --jq '.created_at')"
fi
[[ "$created_at" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T ]] || { echo 'prepare-nightly: run created_at is invalid' >&2; exit 1; }
date="$(TZ=America/Los_Angeles date --date="$created_at" +%Y%m%d)"
short="${source_sha:0:12}"
base_version="$(git show "$source_sha:Cargo.toml" | sed -n 's/^version = "\([^"]*\)"/\1/p' | head -1)"
[[ "$base_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo 'prepare-nightly: Cargo version is not stable SemVer' >&2; exit 1; }
tag="nightly-$date-$short"
version="$base_version-nightly.$date+$short"
nightly_releases="$(gh api --paginate "repos/$GITHUB_REPOSITORY/releases?per_page=100" --jq '[.[] | select(.tag_name | test("^nightly-[0-9]{8}-[0-9a-f]{12}$")) | {id,tag_name,draft,prerelease,published_at}]' | jq -sc 'add // []')"
while IFS=$'\t' read -r release_id release_tag; do
  if ! git show-ref --verify --quiet "refs/tags/$release_tag"; then
    echo "prepare-nightly: release $release_id has no immutable tag ref: $release_tag" >&2
    exit 1
  fi
done < <(jq -r '.[] | [.id,.tag_name] | @tsv' <<<"$nightly_releases")
while IFS= read -r immutable_tag; do
  [ -n "$immutable_tag" ] || continue
  release_count="$(jq --arg tag "$immutable_tag" '[.[] | select(.tag_name == $tag)] | length' <<<"$nightly_releases")"
  [ "$release_count" -eq 1 ] || {
    echo "prepare-nightly: outstanding nightly transaction $immutable_tag does not have exactly one release object; use recovery" >&2
    exit 1
  }
  jq -e --arg tag "$immutable_tag" \
    '[.[] | select(.tag_name == $tag)] | length == 1 and .[0].draft == false and .[0].prerelease == true and .[0].published_at != null' \
    <<<"$nightly_releases" >/dev/null || {
      echo "prepare-nightly: outstanding nightly transaction $immutable_tag is not completely published; use recovery" >&2
      exit 1
    }
done < <(git tag --list 'nightly-*' | LC_ALL=C sort)

publish=false
if [ "$GITHUB_EVENT_NAME" = schedule ] && [ "${NIGHTLY_PUBLISH_ENABLED:-false}" = true ]; then
  publish=true
elif [ "$GITHUB_EVENT_NAME" = workflow_dispatch ] && [ "${NIGHTLY_DRY_RUN:-true}" = false ]; then
  publish=true
fi

branch_sha=''
branch_state=absent
if git show-ref --verify --quiet refs/remotes/origin/nightly; then
  branch_sha="$(git rev-parse refs/remotes/origin/nightly)"
  if [ "$branch_sha" = "$source_sha" ]; then
    branch_state=equal
  elif git merge-base --is-ancestor "$branch_sha" "$source_sha"; then
    branch_state=ancestor
  elif git merge-base --is-ancestor "$source_sha" "$branch_sha"; then
    branch_state=descendant
  else
    branch_state=divergent
  fi
fi
[ "$branch_state" != divergent ] || { echo 'prepare-nightly: nightly branch diverges from selected source' >&2; exit 1; }

verify_published() {
  local verify_sha="$1" verify_tag="$2" verify_temp release manifest source_tree workflow_sha
  verify_temp="$(mktemp -d)"
  if ! release="$(gh api "repos/$GITHUB_REPOSITORY/releases/tags/$verify_tag")"; then rm -rf "$verify_temp"; return 1; fi
  if ! jq -e --arg tag "$verify_tag" '.tag_name == $tag and .draft == false and .prerelease == true and .published_at != null' <<<"$release" >/dev/null; then rm -rf "$verify_temp"; return 1; fi
  if [ "$(git rev-parse "refs/tags/$verify_tag^{}")" != "$verify_sha" ]; then rm -rf "$verify_temp"; return 1; fi
  if ! gh release download "$verify_tag" --repo "$GITHUB_REPOSITORY" --dir "$verify_temp/assets" >/dev/null; then rm -rf "$verify_temp"; return 1; fi
  manifest="$verify_temp/assets/codex-warp-nightly-manifest.json"
  if [ ! -f "$manifest" ]; then rm -rf "$verify_temp"; return 1; fi
  source_tree="$verify_temp/source"
  if ! git worktree add --quiet --detach "$source_tree" "$verify_sha"; then rm -rf "$verify_temp"; return 1; fi
  if ! NIGHTLY_MANIFEST_SCHEMA_PATH="$source_tree/tools/nightly-manifest.schema.json" SOURCE_DIR="$source_tree" bash scripts/check-nightly-assets.sh "$verify_temp/assets" "$manifest"; then git worktree remove --force "$source_tree" >/dev/null 2>&1 || true; rm -rf "$verify_temp"; return 1; fi
  if ! jq -e --arg tag "$verify_tag" --arg sha "$verify_sha" '.tag == $tag and .sourceSha == $sha' "$manifest" >/dev/null; then git worktree remove --force "$source_tree"; rm -rf "$verify_temp"; return 1; fi
  workflow_sha="$(jq -r '.workflowSha' "$manifest")"
  if ! git cat-file -e "$workflow_sha^{commit}" || ! git merge-base --is-ancestor "$workflow_sha" "$live_main"; then git worktree remove --force "$source_tree"; rm -rf "$verify_temp"; return 1; fi
  while IFS=$'\t' read -r target archive; do
    if ! SKIP_VERSION_SMOKE=1 RELEASE_CONTRACT_PATH="$source_tree/tools/release-contract.json" bash scripts/check-release-contract.sh archive "$verify_temp/assets/$archive" "$target" "$source_tree" "$(jq -r '.version' "$manifest")"; then git worktree remove --force "$source_tree"; rm -rf "$verify_temp"; return 1; fi
    if ! bash scripts/verify-nightly-attestation.sh "$verify_temp/assets/$archive" "$manifest"; then git worktree remove --force "$source_tree"; rm -rf "$verify_temp"; return 1; fi
  done < <(jq -r '.artifacts[] | [.target,.archive] | @tsv' "$manifest")
  git worktree remove --force "$source_tree"
  VERIFIED_TAG="$verify_tag"
  VERIFIED_RELEASE_ID="$(jq -r '.id' <<<"$release")"
  VERIFIED_DATE="$(jq -r '.date' "$manifest")"
  VERIFIED_VERSION="$(jq -r '.version' "$manifest")"
  rm -rf "$verify_temp"
}

if [ "$branch_state" = ancestor ]; then
  mapfile -t branch_tags < <(git tag --list 'nightly-*' --points-at "$branch_sha")
  [ "${#branch_tags[@]}" -eq 1 ] && verify_published "$branch_sha" "${branch_tags[0]}" || {
    echo 'prepare-nightly: ancestor nightly branch lacks one verified published release' >&2
    exit 1
  }
fi

mapfile -t source_tags < <(git tag --list 'nightly-*' --points-at "$source_sha")
[ "${#source_tags[@]}" -le 1 ] || { echo 'prepare-nightly: multiple nightly tags target the selected SHA' >&2; exit 1; }
if [ "${#source_tags[@]}" -eq 1 ]; then
  release_count="$(jq --arg tag "${source_tags[0]}" '[.[] | select(.tag_name == $tag)] | length' <<<"$nightly_releases")"
  [ "$release_count" -eq 1 ] || { echo "prepare-nightly: ${source_tags[0]} does not have exactly one release object" >&2; exit 1; }
fi
release_id=''
action=''
if [ "${#source_tags[@]}" -eq 1 ]; then
  verify_published "$source_sha" "${source_tags[0]}" || { echo 'prepare-nightly: selected SHA has incomplete or invalid release state' >&2; exit 1; }
  tag="$VERIFIED_TAG"
  release_id="$VERIFIED_RELEASE_ID"
  date="$VERIFIED_DATE"
  version="$VERIFIED_VERSION"
  case "$branch_state" in
    equal) action=noop ;;
    absent|ancestor) action=branch-repair ;;
    descendant) action=obsolete ;;
    *) echo 'prepare-nightly: corrupt branch/release relationship' >&2; exit 1 ;;
  esac
elif [ "$branch_state" = equal ]; then
  echo 'prepare-nightly: nightly branch points to a SHA without a complete release' >&2
  exit 1
elif [ "$branch_state" = descendant ]; then
  mapfile -t descendant_tags < <(git tag --list 'nightly-*' --points-at "$branch_sha")
  [ "${#descendant_tags[@]}" -eq 1 ] && verify_published "$branch_sha" "${descendant_tags[0]}" || {
    echo 'prepare-nightly: descendant nightly branch lacks one verified published release' >&2
    exit 1
  }
  action=obsolete
elif [ "$explicit" = true ]; then
  echo 'prepare-nightly: explicit historical selection cannot create a fresh nightly' >&2
  exit 1
elif [ "$source_sha" != "$live_main" ] && [ "$publish" = true ]; then
  action=obsolete
elif [ "$branch_state" = absent ] || [ "$branch_state" = ancestor ]; then
  action=build
else
  echo 'prepare-nightly: unhandled state' >&2
  exit 1
fi

if git show-ref --verify --quiet "refs/tags/$tag" && [ "$(git rev-parse "refs/tags/$tag^{}")" != "$source_sha" ]; then
  echo 'prepare-nightly: deterministic tag targets another SHA' >&2
  exit 1
fi

output="${GITHUB_OUTPUT:-/dev/stdout}"
for pair in \
  "publish=$publish" "action=$action" "explicit=$explicit" "source_sha=$source_sha" \
  "live_main_sha=$live_main" "date=$date" "short_sha=$short" "base_version=$base_version" \
  "version=$version" "tag=$tag" "branch_sha=$branch_sha" "branch_state=$branch_state" \
  "release_id=$release_id"; do
  printf '%s\n' "$pair" >>"$output"
done
echo "prepare-nightly: action=$action publish=$publish tag=$tag source=$source_sha"
