#!/usr/bin/env bash
# Assemble the exact publishable eleven-file official candidate from dist outputs.
# Copy contract-named files from the distrib directory. Do not trust dist host
# upload_files: that list is the files the current dist command built, so a host
# pass may contain only sha256.sum (or extra CI scratch files) rather than the
# eleven-file release set.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo 'usage: assemble-official-candidate.sh <distrib-dir> <identity.json> <dist-manifest.json> <output-dir>' >&2
  exit 2
fi

root="$(git rev-parse --show-toplevel)"
cd "$root"
distrib="$1"
identity="$2"
manifest="$3"
output="$4"
[ -d "$distrib" ] || { echo "assemble-official-candidate: missing distrib directory: $distrib" >&2; exit 1; }
[ -f "$identity" ] || { echo "assemble-official-candidate: missing identity: $identity" >&2; exit 1; }
[ -f "$manifest" ] || { echo "assemble-official-candidate: missing dist manifest: $manifest" >&2; exit 1; }
[ ! -e "$output" ] || { echo "assemble-official-candidate: output already exists: $output" >&2; exit 1; }

node tools/release-please-policy/validate-json.mjs tools/dist-manifest.schema.json "$manifest"
[ "$(jq -r '.announcement_tag_is_implicit' "$manifest")" = false ] || {
  echo 'assemble-official-candidate: official dist manifest must use an explicit announcement tag' >&2
  exit 1
}
[ "$(jq -r '.artifacts["sha256.sum"].kind' "$manifest")" = 'unified-checksum' ] || {
  echo 'assemble-official-candidate: official dist manifest must include sha256.sum' >&2
  exit 1
}

manifest_artifacts="$(jq -Sc '
  [.artifacts | to_entries[] | select(.value.kind == "executable-zip") |
    {target:.value.target_triples[0],archive:.value.name,archiveSha256:.value.checksums.sha256,checksum:.value.checksum}
  ] | sort_by(.target)
' "$manifest")"
[ "$(jq 'length' <<<"$manifest_artifacts")" -eq 4 ] || {
  echo 'assemble-official-candidate: final dist manifest must describe four archives' >&2
  exit 1
}

while IFS= read -r target; do
  target_manifest="$distrib/$target-dist-manifest.json"
  runner="$distrib/$target-runner.json"
  [ -f "$target_manifest" ] || { echo "assemble-official-candidate: missing target manifest: $target" >&2; exit 1; }
  [ -f "$runner" ] || { echo "assemble-official-candidate: missing runner evidence: $target" >&2; exit 1; }
  node tools/release-please-policy/validate-json.mjs tools/dist-manifest.schema.json "$target_manifest"
  [ "$(jq -r '.dist_version' "$target_manifest")" = "$(jq -r '.dist_version' "$manifest")" ] || {
    echo "assemble-official-candidate: dist version mismatch for $target" >&2
    exit 1
  }
  [ "$(jq -r --arg target "$target" '.target == $target' "$runner")" = true ] || {
    echo "assemble-official-candidate: runner evidence target mismatch for $target" >&2
    exit 1
  }
  target_artifact="$(jq -Sc --arg target "$target" '
    [.artifacts | to_entries[] | select(.value.kind == "executable-zip" and .value.target_triples[0] == $target) |
      {target:.value.target_triples[0],archive:.value.name,archiveSha256:.value.checksums.sha256,checksum:.value.checksum}
    ]
  ' "$target_manifest")"
  expected_artifact="$(jq -Sc --arg target "$target" '[.[] | select(.target == $target)]' <<<"$manifest_artifacts")"
  [ "$target_artifact" = "$expected_artifact" ] || {
    echo "assemble-official-candidate: target and final manifests disagree for $target" >&2
    exit 1
  }
done < <(jq -r '.targets[].triple' tools/release-contract.json)

identity_runners="$(jq -Sc '.runners | sort_by(.target)' "$identity")"
evidence_runners="$(jq -scS 'sort_by(.target)' "$distrib"/*-runner.json)"
[ "$identity_runners" = "$evidence_runners" ] || {
  echo 'assemble-official-candidate: identity and runner evidence disagree' >&2
  exit 1
}

mkdir "$output"
while IFS=$'\t' read -r archive digest checksum; do
  [ -f "$distrib/$archive" ] || { echo "assemble-official-candidate: missing archive: $archive" >&2; exit 1; }
  [ -f "$distrib/$checksum" ] || { echo "assemble-official-candidate: missing checksum: $checksum" >&2; exit 1; }
  [ "$(sha256sum "$distrib/$archive" | awk '{print $1}')" = "$digest" ] || {
    echo "assemble-official-candidate: archive digest mismatch: $archive" >&2
    exit 1
  }
  bash scripts/check-sha256-index.sh "$distrib/$checksum" "$digest" "$archive" >/dev/null
  cp "$distrib/$archive" "$distrib/$checksum" "$output/"
done < <(jq -r '.[] | [.archive,.archiveSha256,.checksum] | @tsv' <<<"$manifest_artifacts")

[ -f "$distrib/sha256.sum" ] || { echo 'assemble-official-candidate: missing sha256.sum' >&2; exit 1; }
cp "$distrib/sha256.sum" "$output/sha256.sum"
cp "$manifest" "$output/dist-manifest.json"
bash scripts/generate-release-metadata.sh official "$identity" "$manifest" "$output/codex-warp-release-metadata.json"
echo "assemble-official-candidate: wrote $output"
