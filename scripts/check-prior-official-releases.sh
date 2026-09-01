#!/usr/bin/env bash
# Refuse a new official release while an earlier stable transaction is incomplete.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
repository="${GITHUB_REPOSITORY:-jatmn/Codex-warp}"
fixture=false

verify_complete_release() {
  local tag="$1" release_id="$2" temp source source_tree assets metadata manifest expected_targets
  temp="$(mktemp -d)"
  git fetch --force --no-tags origin "refs/tags/$tag:refs/tags/$tag" >/dev/null
  source="$(git rev-parse "refs/tags/$tag^{}")"
  source_tree="$temp/source"
  git worktree add --quiet --detach "$source_tree" "$source"
  assets="$temp/assets"
  mkdir "$assets"

  gh api --paginate "repos/$repository/releases/$release_id/assets?per_page=100" \
    --jq '.[] | [.id,.name,.state] | @tsv' >"$temp/assets.tsv"
  while IFS=$'\t' read -r asset_id name state; do
    [ "$name" = "$(basename "$name")" ]
    [ "$state" = uploaded ]
    gh api -H 'Accept: application/octet-stream' \
      "repos/$repository/releases/assets/$asset_id" >"$assets/$name"
  done <"$temp/assets.tsv"

  metadata="$assets/codex-warp-release-metadata.json"
  manifest="$assets/dist-manifest.json"
  jq -e --arg tag "$tag" --arg source "$source" --argjson id "$release_id" '
    .mode == "official" and .publishable == true and .tag == $tag and
    .sourceSha == $source and .peeledTagSha == $source and .releaseId == $id and
    .cargoVersion == ($tag | ltrimstr("v")) and
    .dist.announcementTagIsImplicit == false and (.dist.artifacts | length) == 4
  ' "$metadata" >/dev/null
  jq -e '.announcement_tag_is_implicit == false' "$manifest" >/dev/null
  [ "$(bash scripts/sha256-file.sh "$manifest")" = "$(jq -r '.dist.manifestSha256' "$metadata")" ]
  [ "$(bash scripts/sha256-file.sh "$source_tree/tools/release-contract.json")" = "$(jq -r '.releaseContractSha256' "$metadata")" ]
  [ "$(bash scripts/sha256-file.sh "$source_tree/tools/dist-manifest.schema.json")" = "$(jq -r '.dist.manifestSchemaSha256' "$metadata")" ]
  [ "$(bash scripts/sha256-file.sh "$source_tree/Cargo.lock")" = "$(jq -r '.cargoLockSha256' "$metadata")" ]
  [ "$(bash scripts/sha256-file.sh "$source_tree/rust-toolchain.toml")" = "$(jq -r '.rustToolchain.fileSha256' "$metadata")" ]
  [ "$(jq -r '.dist.version' "$metadata")" = "$(jq -r '.dist_version' "$manifest")" ]
  [ "$(jq -c '.dist.artifacts | sort_by(.target)' "$metadata")" = "$(jq -c '. as $m | [.artifacts | to_entries[] | select(.value.kind == "executable-zip") | {target:.value.target_triples[0],archive:.value.name,archiveSha256:.value.checksums.sha256,checksumFile:$m.artifacts[.value.checksum].name}] | sort_by(.target)' "$manifest")" ]
  expected_targets="$(jq -c '[.targets[] | {target:.triple,archive}] | sort_by(.target)' "$source_tree/tools/release-contract.json")"
  [ "$expected_targets" = "$(jq -c '[.dist.artifacts[] | {target,archive}] | sort_by(.target)' "$metadata")" ]
  [ "$(jq -c '[.targets[].triple] | sort' "$source_tree/tools/release-contract.json")" = "$(jq -c '[.runners[].target] | sort' "$metadata")" ]

  expected="$temp/expected.txt"
  {
    jq -r '.dist.artifacts[] | .archive, .checksumFile' "$metadata"
    printf '%s\n' sha256.sum dist-manifest.json codex-warp-release-metadata.json
  } | LC_ALL=C sort >"$expected"
  find "$assets" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort >"$temp/actual.txt"
  cmp "$expected" "$temp/actual.txt" >/dev/null
  [ "$(wc -l <"$expected")" -eq 11 ]

  checksum_args=("$assets/sha256.sum")
  while IFS=$'\t' read -r archive digest checksum; do
    [ "$(bash scripts/sha256-file.sh "$assets/$archive")" = "$digest" ]
    bash scripts/check-sha256-index.sh "$assets/$checksum" "$digest" "$archive" >/dev/null
    checksum_args+=("$digest" "$archive")
    bash scripts/verify-official-attestation.sh "$assets/$archive" "$metadata" >/dev/null
  done < <(jq -r '.dist.artifacts[] | [.archive,.archiveSha256,.checksumFile] | @tsv' "$metadata")
  bash scripts/check-sha256-index.sh "${checksum_args[@]}" >/dev/null
  (cd "$assets" && sha256sum -c sha256.sum >/dev/null)
  bash scripts/verify-official-attestation.sh "$metadata" "$metadata" >/dev/null
  git worktree remove --force "$source_tree"
  rm -rf "$temp"
}

if [ -n "${OFFICIAL_STATE_FIXTURE:-}" ]; then
  fixture=true
  [ -f "$OFFICIAL_STATE_FIXTURE" ] || { echo 'check-prior-official-releases: fixture is missing' >&2; exit 2; }
  releases="$(jq -c '.releases // []' "$OFFICIAL_STATE_FIXTURE")"
  tags="$(jq -c '.tags // []' "$OFFICIAL_STATE_FIXTURE")"
  if jq -e 'has("activeRuns")' "$OFFICIAL_STATE_FIXTURE" >/dev/null; then
    active="$(jq -c --argjson current "${GITHUB_RUN_ID:-0}" '[.activeRuns[] | select(.id != $current and ((.name == "Release Recovery" and .event == "workflow_dispatch" and .head_branch == "main") or (.name == "Release" and .event == "push" and ((.head_branch // "") | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))))) | {id,name,event,head_branch}]' "$OFFICIAL_STATE_FIXTURE")"
  else
    active="$(jq -c '.activeOfficialTags // []' "$OFFICIAL_STATE_FIXTURE")"
  fi
else
  command -v gh >/dev/null || { echo 'check-prior-official-releases: gh is required' >&2; exit 2; }
  releases="$(gh api --paginate "repos/$repository/releases?per_page=100" --jq '[.[] | {id,tag_name,draft,prerelease,published_at}]' | jq -sc 'add // []')"
  tags="$(gh api --paginate "repos/$repository/tags?per_page=100" --jq '[.[].name]' | jq -sc 'add // []')"
  in_progress="$(gh api --paginate "repos/$repository/actions/runs?status=in_progress&per_page=100" | jq -sc --argjson current "${GITHUB_RUN_ID:-0}" '[.[] | .workflow_runs[] | select(.id != $current and ((.name == "Release Recovery" and .event == "workflow_dispatch" and .head_branch == "main") or (.name == "Release" and .event == "push" and ((.head_branch // "") | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))))) | {id,name,event,head_branch}]')"
  queued="$(gh api --paginate "repos/$repository/actions/runs?status=queued&per_page=100" | jq -sc --argjson current "${GITHUB_RUN_ID:-0}" '[.[] | .workflow_runs[] | select(.id != $current and ((.name == "Release Recovery" and .event == "workflow_dispatch" and .head_branch == "main") or (.name == "Release" and .event == "push" and ((.head_branch // "") | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$"))))) | {id,name,event,head_branch}]')"
  active="$(jq -n --argjson in_progress "$in_progress" --argjson queued "$queued" '$in_progress + $queued')"
fi

if jq -e '[.[] | select(.draft == true and (.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")))] | length > 0' <<<"$releases" >/dev/null; then
  echo 'check-prior-official-releases: an official draft is still outstanding' >&2
  exit 1
fi
latest_official="$({ jq -r '.[]' <<<"$tags" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' || true; } | sort -V | tail -1)"
while IFS= read -r tag; do
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
  count="$(jq --arg tag "$tag" '[.[] | select(.tag_name == $tag and .draft == false and .prerelease == false and .published_at != null)] | length' <<<"$releases")"
  if [ "$count" -ne 1 ]; then
    if [ "${OFFICIAL_ALLOW_MISSING_LATEST_TAG:-}" = 1 ] &&
      [ "$tag" = "$latest_official" ] && [ "$count" -eq 0 ]; then
      echo "check-prior-official-releases: allowing missing-draft continuation for $tag"
      continue
    fi
    echo "check-prior-official-releases: $tag does not have exactly one stable published release" >&2
    exit 1
  fi
  release_id="$(jq -r --arg tag "$tag" '.[] | select(.tag_name == $tag and .draft == false and .prerelease == false and .published_at != null) | .id // empty' <<<"$releases")"
  if [ "$fixture" = true ]; then
    if jq -e --arg tag "$tag" '.[] | select(.tag_name == $tag and .complete == true)' <<<"$releases" >/dev/null; then
      :
    else
      echo "check-prior-official-releases: allowing broken published official $tag; published bytes cannot be repaired"
    fi
  else
    [[ "$release_id" =~ ^[1-9][0-9]*$ ]]
    if verify_complete_release "$tag" "$release_id"; then
      :
    else
      echo "check-prior-official-releases: allowing broken published official $tag; published bytes cannot be repaired"
    fi
  fi
done < <(jq -r '.[]' <<<"$tags")

while IFS= read -r tag; do
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || continue
  jq -e --arg tag "$tag" 'index($tag) != null' <<<"$tags" >/dev/null || {
    echo "check-prior-official-releases: published release has no matching tag: $tag" >&2
    exit 1
  }
done < <(jq -r '.[] | select(.draft == false and .published_at != null) | .tag_name' <<<"$releases")

if [ "$(jq 'length' <<<"$active")" -ne 0 ]; then
  echo 'check-prior-official-releases: an official finalizer or recovery run is active' >&2
  exit 1
fi

echo 'check-prior-official-releases: ready'
