#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cargo build --locked >/dev/null
archive_name='codex-warp-x86_64-unknown-linux-gnu'
mkdir -p "$tmp/$archive_name/configs"
cp target/debug/codex-warp "$tmp/$archive_name/codex-warp"
cp codex-warp.toml README.md LICENSE NOTICE CHANGELOG.md "$tmp/$archive_name/"
cp -R configs/. "$tmp/$archive_name/configs/"
tar -cJf "$tmp/$archive_name.tar.xz" -C "$tmp" "$archive_name"
bash scripts/check-release-contract.sh archive "$tmp/$archive_name.tar.xz" x86_64-unknown-linux-gnu "$root" 0.0.1 >/dev/null

windows_name='codex-warp-x86_64-pc-windows-msvc'
mkdir -p "$tmp/$windows_name/configs"
cp target/debug/codex-warp "$tmp/$windows_name/codex-warp.exe"
cp codex-warp.toml README.md LICENSE NOTICE CHANGELOG.md "$tmp/$windows_name/"
cp -R configs/. "$tmp/$windows_name/configs/"
(cd "$tmp/$windows_name" && 7z a -bd -tzip "$tmp/$windows_name.zip" . >/dev/null)
RUNNER_OS=Windows SKIP_VERSION_SMOKE=1 \
  bash scripts/check-release-contract.sh archive "$tmp/$windows_name.zip" x86_64-pc-windows-msvc "$root" 0.0.1 >/dev/null

cp README.md "$tmp/$archive_name/unexpected.txt"
mkdir "$tmp/invalid"
tar -cJf "$tmp/invalid/$archive_name.tar.xz" -C "$tmp" "$archive_name"
if bash scripts/check-release-contract.sh archive "$tmp/invalid/$archive_name.tar.xz" x86_64-unknown-linux-gnu "$root" 0.0.1 >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted an unexpected archive file' >&2
  exit 1
fi

assets="$tmp/assets"
mkdir -p "$assets"
cp tools/release-please-policy/fixtures/dist-manifest.official.json "$tmp/manifest.json"
jq '.announcement_tag_is_implicit = true' "$tmp/manifest.json" >"$tmp/manifest.next.json"
mv "$tmp/manifest.next.json" "$tmp/manifest.json"
while IFS=$'\t' read -r id name; do
  printf 'fixture bytes for %s\n' "$name" >"$assets/$name"
  digest="$(sha256sum "$assets/$name" | awk '{print $1}')"
  jq --arg id "$id" --arg digest "$digest" '.artifacts[$id].checksums.sha256 = $digest' "$tmp/manifest.json" >"$tmp/manifest.next.json"
  mv "$tmp/manifest.next.json" "$tmp/manifest.json"
  printf '%s  %s\n' "$digest" "$name" >"$assets/$name.sha256"
done < <(jq -r '.artifacts | to_entries[] | select(.value.kind == "executable-zip") | [.key, .value.name] | @tsv' "$tmp/manifest.json")
jq -r '.artifacts | to_entries[] | select(.value.kind == "executable-zip") | [.value.checksums.sha256, .value.name] | @tsv' "$tmp/manifest.json" | sed $'s/\t/  /' >"$assets/sha256.sum"
cp "$tmp/manifest.json" "$assets/dist-manifest.json"

jq --arg contract "$(sha256sum tools/release-contract.json | awk '{print $1}')" '
  .publishable = false |
  .releaseContractSha256 = $contract |
  .tag = null | .peeledTagSha = null | .releaseId = null |
  .pullRequest = {number:7,baseSha:"6666666666666666666666666666666666666666",headSha:"7777777777777777777777777777777777777777",buildSourceSha:.sourceSha,mergeSha:null}
' tools/release-please-policy/fixtures/metadata-identity.official.json >"$tmp/proof-identity.json"
bash scripts/generate-release-metadata.sh pr-upload-proof "$tmp/proof-identity.json" "$tmp/manifest.json" "$assets/codex-warp-release-metadata.json" >/dev/null
bash scripts/check-release-contract.sh pr-upload-proof "$assets" "$assets/codex-warp-release-metadata.json" "$assets/dist-manifest.json" >/dev/null

jq '.publishable = true' "$assets/codex-warp-release-metadata.json" >"$tmp/invalid-metadata.json"
if bash scripts/check-release-contract.sh pr-upload-proof "$assets" "$tmp/invalid-metadata.json" "$assets/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted publishable proof metadata' >&2
  exit 1
fi

echo 'check-release-contract-harness: ok'
