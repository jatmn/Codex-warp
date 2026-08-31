#!/usr/bin/env bash
# Assemble the exact non-publishable eleven-file proof from dist workflow outputs.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo 'usage: assemble-pr-upload-proof.sh <distrib-dir> <identity.json> <output-dir>' >&2
  exit 2
fi

root="$(git rev-parse --show-toplevel)"
cd "$root"
distrib="$1"
identity="$2"
output="$3"
manifest="$distrib/global-dist-manifest.json"
[ -d "$distrib" ] || { echo "assemble-pr-upload-proof: missing distrib directory: $distrib" >&2; exit 1; }
[ -f "$identity" ] || { echo "assemble-pr-upload-proof: missing identity: $identity" >&2; exit 1; }
[ -f "$manifest" ] || { echo "assemble-pr-upload-proof: missing final dist manifest: $manifest" >&2; exit 1; }
[ ! -e "$output" ] || { echo "assemble-pr-upload-proof: output already exists: $output" >&2; exit 1; }

node tools/release-please-policy/validate-json.mjs tools/dist-manifest.schema.json "$manifest"
[ "$(jq -r '.announcement_tag_is_implicit' "$manifest")" = true ] || {
  echo 'assemble-pr-upload-proof: PR dist manifest must use an implicit announcement tag' >&2
  exit 1
}

manifest_artifacts="$(jq -Sc '
  [.artifacts | to_entries[] | select(.value.kind == "executable-zip") |
    {target:.value.target_triples[0],archive:.value.name,archiveSha256:.value.checksums.sha256,checksum:.value.checksum}
  ] | sort_by(.target)
' "$manifest")"
[ "$(jq 'length' <<<"$manifest_artifacts")" -eq 4 ] || {
  echo 'assemble-pr-upload-proof: final dist manifest must describe four archives' >&2
  exit 1
}

while IFS= read -r target; do
  target_manifest="$distrib/$target-dist-manifest.json"
  runner="$distrib/$target-runner.json"
  [ -f "$target_manifest" ] || { echo "assemble-pr-upload-proof: missing target manifest: $target" >&2; exit 1; }
  [ -f "$runner" ] || { echo "assemble-pr-upload-proof: missing runner evidence: $target" >&2; exit 1; }
  node tools/release-please-policy/validate-json.mjs tools/dist-manifest.schema.json "$target_manifest"
  [ "$(jq -r '.dist_version' "$target_manifest")" = "$(jq -r '.dist_version' "$manifest")" ] || {
    echo "assemble-pr-upload-proof: dist version mismatch for $target" >&2
    exit 1
  }
  [ "$(jq -r --arg target "$target" '.target == $target' "$runner")" = true ] || {
    echo "assemble-pr-upload-proof: runner evidence target mismatch for $target" >&2
    exit 1
  }
  target_artifact="$(jq -Sc --arg target "$target" '
    [.artifacts | to_entries[] | select(.value.kind == "executable-zip" and .value.target_triples[0] == $target) |
      {target:.value.target_triples[0],archive:.value.name,archiveSha256:.value.checksums.sha256,checksum:.value.checksum}
    ]
  ' "$target_manifest")"
  expected_artifact="$(jq -Sc --arg target "$target" '[.[] | select(.target == $target)]' <<<"$manifest_artifacts")"
  [ "$target_artifact" = "$expected_artifact" ] || {
    echo "assemble-pr-upload-proof: target and final manifests disagree for $target" >&2
    exit 1
  }
done < <(jq -r '.targets[].triple' tools/release-contract.json)

identity_runners="$(jq -Sc '.runners | sort_by(.target)' "$identity")"
evidence_runners="$(jq -scS 'sort_by(.target)' "$distrib"/*-runner.json)"
[ "$identity_runners" = "$evidence_runners" ] || {
  echo 'assemble-pr-upload-proof: identity and runner evidence disagree' >&2
  exit 1
}

mkdir "$output"
while IFS=$'\t' read -r archive digest checksum; do
  [ -f "$distrib/$archive" ] || { echo "assemble-pr-upload-proof: missing archive: $archive" >&2; exit 1; }
  [ -f "$distrib/$checksum" ] || { echo "assemble-pr-upload-proof: missing checksum: $checksum" >&2; exit 1; }
  [ "$(sha256sum "$distrib/$archive" | awk '{print $1}')" = "$digest" ] || {
    echo "assemble-pr-upload-proof: archive digest mismatch: $archive" >&2
    exit 1
  }
  bash scripts/check-sha256-index.sh "$distrib/$checksum" "$digest" "$archive" >/dev/null
  cp "$distrib/$archive" "$distrib/$checksum" "$output/"
done < <(jq -r '.[] | [.archive,.archiveSha256,.checksum] | @tsv' <<<"$manifest_artifacts")

cp "$distrib/sha256.sum" "$output/sha256.sum"
cp "$manifest" "$output/dist-manifest.json"
bash scripts/generate-release-metadata.sh pr-upload-proof "$identity" "$manifest" "$output/codex-warp-release-metadata.json"
SOURCE_DIR="$root" bash scripts/check-release-contract.sh pr-upload-proof "$output" "$output/codex-warp-release-metadata.json" "$output/dist-manifest.json"
echo "assemble-pr-upload-proof: wrote $output"
