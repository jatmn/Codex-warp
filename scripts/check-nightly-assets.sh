#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 2 ] || { echo 'usage: check-nightly-assets.sh <asset-dir> <manifest.json>' >&2; exit 2; }
assets="$1"
manifest="$2"
root="$(git rev-parse --show-toplevel)"
cd "$root"
node tools/release-please-policy/validate-json.mjs tools/nightly-manifest.schema.json "$manifest"
[ "$(jq '.artifacts | map(.target) | unique | length' "$manifest")" -eq 4 ]
[ "$(jq '.artifacts | map(.archive) | unique | length' "$manifest")" -eq 4 ]
[ "$(jq -r '.tag' "$manifest")" = "nightly-$(jq -r '.date' "$manifest")-$(jq -r '.sourceSha' "$manifest" | cut -c1-12)" ]
[ "$(jq -r '.version' "$manifest")" = "$(jq -r '.baseVersion' "$manifest")-nightly.$(jq -r '.date' "$manifest").$(jq -r '.sourceSha' "$manifest" | cut -c1-12)" ]

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
  grep -Ex "${digest}  ${archive}" "$assets/$checksum" >/dev/null
  [ "$(wc -l <"$assets/$checksum")" -eq 1 ]
done < <(jq -r '.artifacts[] | [.archive,.archiveSha256,.checksumFile] | @tsv' "$manifest")
[ "$(wc -l <"$assets/sha256.sum")" -eq 4 ]
(cd "$assets" && sha256sum -c sha256.sum >/dev/null)
echo 'check-nightly-assets: ok'
