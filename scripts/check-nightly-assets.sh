#!/usr/bin/env bash
set -euo pipefail

[ "$#" -ge 2 ] && [ "$#" -le 3 ] || { echo 'usage: check-nightly-assets.sh <asset-dir> <manifest.json> [source-dir]' >&2; exit 2; }
assets="$1"
manifest="$2"
source_dir="${3:-${SOURCE_DIR:-}}"
schema="${NIGHTLY_MANIFEST_SCHEMA_PATH:-tools/nightly-manifest.schema.json}"
scripts_dir="$(cd "$(dirname "$0")" && pwd)"
root="$(git rev-parse --show-toplevel)"
cd "$root"
node tools/release-please-policy/validate-json.mjs "$schema" "$manifest"
[ "$(jq '.artifacts | map(.target) | unique | length' "$manifest")" -eq 4 ]
[ "$(jq '.artifacts | map(.archive) | unique | length' "$manifest")" -eq 4 ]
[ "$(jq -r '.tag' "$manifest")" = "nightly-$(jq -r '.date' "$manifest")-$(jq -r '.sourceSha' "$manifest" | cut -c1-12)" ]
[ "$(jq -r '.version' "$manifest")" = "$(jq -r '.baseVersion' "$manifest")-nightly.$(jq -r '.date' "$manifest")+$(jq -r '.sourceSha' "$manifest" | cut -c1-12)" ]
tag="$(jq -r '.tag' "$manifest")"
version="$(jq -r '.version' "$manifest")"
contract="${RELEASE_CONTRACT_PATH:-${source_dir:+$source_dir/}tools/release-contract.json}"
[ -f "$contract" ] || { echo "check-nightly-assets: release contract is missing: $contract" >&2; exit 1; }
expected_targets="$(jq -c '[.targets[].triple] | sort' "$contract")"
actual_targets="$(jq -c '[.artifacts[].target] | sort' "$manifest")"
[ "$actual_targets" = "$expected_targets" ] || { echo 'check-nightly-assets: target inventory mismatch' >&2; exit 1; }

while IFS=$'\t' read -r target archive checksum; do
  official_archive="$(jq -r --arg target "$target" '.targets[] | select(.triple == $target) | .archive' "$contract")"
  case "$official_archive" in
    *.tar.xz) extension=tar.xz ;;
    *.zip) extension=zip ;;
    *) echo "check-nightly-assets: unsupported archive contract for $target" >&2; exit 1 ;;
  esac
  expected_archive="codex-warp-${tag}-${target}.${extension}"
  [ "$archive" = "$expected_archive" ] || { echo "check-nightly-assets: archive name mismatch for $target" >&2; exit 1; }
  [ "$checksum" = "$archive.sha256" ] || { echo "check-nightly-assets: checksum name mismatch for $target" >&2; exit 1; }
done < <(jq -r '.artifacts[] | [.target,.archive,.checksumFile] | @tsv' "$manifest")

if [ -n "$source_dir" ]; then
  source_dir="$(cd "$source_dir" && pwd)"
  [ "$(git -C "$source_dir" rev-parse HEAD)" = "$(jq -r '.sourceSha' "$manifest")" ]
  base_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_dir/Cargo.toml" | head -1)"
  [ "$(jq -r '.baseVersion' "$manifest")" = "$base_version" ]
  [ "$(jq -r '.cargoLockSha256' "$manifest")" = "$(bash "$scripts_dir/sha256-lf-file.sh" "$source_dir/Cargo.lock")" ]
  [ "$(jq -r '.rustToolchainSha256' "$manifest")" = "$(bash "$scripts_dir/sha256-lf-file.sh" "$source_dir/rust-toolchain.toml")" ]
  [ "$(jq -r '.packagingContractSha256' "$manifest")" = "$(bash "$scripts_dir/nightly-contract-digest.sh" "$source_dir")" ]
  [ "$(jq -r '.packagingScriptSha256' "$manifest")" = "$(bash "$scripts_dir/sha256-lf-file.sh" "$source_dir/scripts/package-nightly.sh")" ]
  while IFS=$'\t' read -r target archive; do
    SKIP_VERSION_SMOKE=1 RELEASE_CONTRACT_PATH="$source_dir/tools/release-contract.json" \
      bash "$scripts_dir/check-release-contract.sh" archive "$assets/$archive" "$target" "$source_dir" "$version" >/dev/null
  done < <(jq -r '.artifacts[] | [.target,.archive] | @tsv' "$manifest")
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
