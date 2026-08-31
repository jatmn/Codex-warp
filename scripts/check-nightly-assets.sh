#!/usr/bin/env bash
set -euo pipefail

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || { echo 'usage: check-nightly-assets.sh <asset-dir> <manifest.json> [source-dir]' >&2; exit 2; }
assets="$1"
manifest="$2"
source_dir="${3:-${SOURCE_DIR:-}}"
schema="${NIGHTLY_MANIFEST_SCHEMA_PATH:-tools/nightly-manifest.schema.json}"
root="$(git rev-parse --show-toplevel)"
cd "$root"
node tools/release-please-policy/validate-json.mjs "$schema" "$manifest"
[ "$(jq '.artifacts | map(.target) | unique | length' "$manifest")" -eq 4 ]
[ "$(jq '.artifacts | map(.archive) | unique | length' "$manifest")" -eq 4 ]
[ "$(jq -r '.tag' "$manifest")" = "nightly-$(jq -r '.date' "$manifest")-$(jq -r '.sourceSha' "$manifest" | cut -c1-12)" ]
[ "$(jq -r '.version' "$manifest")" = "$(jq -r '.baseVersion' "$manifest")-nightly.$(jq -r '.date' "$manifest")+$(jq -r '.sourceSha' "$manifest" | cut -c1-12)" ]

if [ -n "$source_dir" ]; then
  source_dir="$(cd "$source_dir" && pwd)"
  [ "$(git -C "$source_dir" rev-parse HEAD)" = "$(jq -r '.sourceSha' "$manifest")" ]
  base_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_dir/Cargo.toml" | head -1)"
  [ "$(jq -r '.baseVersion' "$manifest")" = "$base_version" ]
  [ "$(jq -r '.cargoLockSha256' "$manifest")" = "$(bash scripts/sha256-file.sh "$source_dir/Cargo.lock")" ]
  [ "$(jq -r '.rustToolchainSha256' "$manifest")" = "$(bash scripts/sha256-file.sh "$source_dir/rust-toolchain.toml")" ]
  [ "$(jq -r '.packagingContractSha256' "$manifest")" = "$(bash scripts/nightly-contract-digest.sh "$source_dir")" ]
  [ "$(jq -r '.packagingScriptSha256' "$manifest")" = "$(bash scripts/sha256-file.sh "$source_dir/scripts/package-nightly.sh")" ]
fi

expected="$(mktemp)"
actual="$(mktemp)"
trap 'rm -f "$expected" "$actual"' EXIT
{
  jq -r '.artifacts[] | .archive, .checksumFile' "$manifest"
  printf '%s\n' sha256.sum codex-warp-nightly-manifest.json
} | sort >"$expected"
find "$assets" -maxdepth 1 -type f -printf '%f\n' | sort >"$actual"
cmp "$expected" "$actual" >/dev/null || { echo 'check-nightly-assets: inventory mismatch' >&2; exit 1; }
[ "$(wc -l <"$expected")" -eq 10 ]
while IFS=$'\t' read -r archive digest checksum; do
  [ "$(sha256sum "$assets/$archive" | awk '{print $1}')" = "$digest" ]
  bash scripts/check-sha256-index.sh "$assets/$checksum" "$digest" "$archive" >/dev/null
done < <(jq -r '.artifacts[] | [.archive,.archiveSha256,.checksumFile] | @tsv' "$manifest")
checksum_args=("$assets/sha256.sum")
while IFS=$'\t' read -r archive digest; do
  checksum_args+=("$digest" "$archive")
done < <(jq -r '.artifacts[] | [.archive,.archiveSha256] | @tsv' "$manifest")
bash scripts/check-sha256-index.sh "${checksum_args[@]}" >/dev/null
(cd "$assets" && sha256sum -c sha256.sum >/dev/null)
echo 'check-nightly-assets: ok'
