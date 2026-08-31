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
windows_bin="$tmp/windows-bin"
mkdir "$windows_bin"
ln -s "$root/scripts/test-fixtures/windows-cygpath.sh" "$windows_bin/cygpath"
ln -s "$root/scripts/test-fixtures/windows-jq.sh" "$windows_bin/jq"
ln -s "$root/scripts/test-fixtures/windows-powershell.sh" "$windows_bin/powershell.exe"
ln -s "$root/scripts/test-fixtures/windows-unzip.sh" "$windows_bin/unzip"
JQ_REAL="$(command -v jq)" UNZIP_REAL="$(command -v unzip)" \
  PATH="$windows_bin:$PATH" RUNNER_OS=Windows SKIP_VERSION_SMOKE=1 \
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
  printf '%s *%s\n\n' "$digest" "$name" >"$assets/$name.sha256"
done < <(jq -r '.artifacts | to_entries[] | select(.value.kind == "executable-zip") | [.key, .value.name] | @tsv' "$tmp/manifest.json")
jq -r '.artifacts | to_entries[] | select(.value.kind == "executable-zip") | [.value.checksums.sha256, .value.name] | @tsv' "$tmp/manifest.json" | sed $'s/\t/ */' >"$assets/sha256.sum"
printf '\n' >>"$assets/sha256.sum"
cp "$tmp/manifest.json" "$assets/dist-manifest.json"

jq \
  --arg contract "$(sha256sum tools/release-contract.json | awk '{print $1}')" \
  --arg lock "$(sha256sum Cargo.lock | awk '{print $1}')" \
  --arg toolchain "$(sha256sum rust-toolchain.toml | awk '{print $1}')" '
  .publishable = false |
  .releaseContractSha256 = $contract |
  .cargoLockSha256 = $lock |
  .rustToolchain.fileSha256 = $toolchain |
  .tag = null | .peeledTagSha = null | .releaseId = null |
  .pullRequest = {number:7,baseSha:"6666666666666666666666666666666666666666",headSha:"7777777777777777777777777777777777777777",buildSourceSha:.sourceSha,mergeSha:null}
' tools/release-please-policy/fixtures/metadata-identity.official.json >"$tmp/proof-identity.json"
distrib="$tmp/distrib"
mkdir "$distrib"
cp "$assets"/*.tar.xz "$assets"/*.zip "$assets"/*.sha256 "$assets/sha256.sum" "$distrib/"
cp "$tmp/manifest.json" "$distrib/global-dist-manifest.json"
while IFS= read -r target; do
  cp "$tmp/manifest.json" "$distrib/$target-dist-manifest.json"
  jq -c --arg target "$target" '.runners[] | select(.target == $target)' "$tmp/proof-identity.json" >"$distrib/$target-runner.json"
done < <(jq -r '.targets[].triple' tools/release-contract.json)

proof="$tmp/proof"
bash scripts/assemble-pr-upload-proof.sh "$distrib" "$tmp/proof-identity.json" "$proof" >/dev/null
bash scripts/check-release-contract.sh pr-upload-proof "$proof" "$proof/codex-warp-release-metadata.json" "$proof/dist-manifest.json" >/dev/null

jq '.publishable = true' "$proof/codex-warp-release-metadata.json" >"$tmp/invalid-metadata.json"
if bash scripts/check-release-contract.sh pr-upload-proof "$proof" "$tmp/invalid-metadata.json" "$proof/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted publishable proof metadata' >&2
  exit 1
fi
if bash scripts/check-release-contract.sh official-publication "$proof" "$proof/codex-warp-release-metadata.json" "$proof/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted proof assets for official publication' >&2
  exit 1
fi
cp -R "$proof" "$tmp/missing-sidecar"
rm "$tmp/missing-sidecar/codex-warp-release-metadata.json"
if bash scripts/check-release-contract.sh pr-upload-proof "$tmp/missing-sidecar" "$proof/codex-warp-release-metadata.json" "$proof/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted assets with a missing sidecar' >&2
  exit 1
fi
cp -R "$proof" "$tmp/modified-manifest"
printf '\n' >>"$tmp/modified-manifest/dist-manifest.json"
if bash scripts/check-release-contract.sh pr-upload-proof "$tmp/modified-manifest" "$proof/codex-warp-release-metadata.json" "$tmp/modified-manifest/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted a modified dist manifest' >&2
  exit 1
fi
cp -R "$proof" "$tmp/unexpected-file"
cp "$proof/codex-warp-release-metadata.json" "$tmp/unexpected-file/second-sidecar.json"
if bash scripts/check-release-contract.sh pr-upload-proof "$tmp/unexpected-file" "$proof/codex-warp-release-metadata.json" "$proof/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted an unexpected second sidecar' >&2
  exit 1
fi
cp -R "$proof" "$tmp/extra-checksum-entry"
checksum_file="$(jq -r '.dist.artifacts[0].checksumFile' "$proof/codex-warp-release-metadata.json")"
archive_file="$(jq -r '.dist.artifacts[0].archive' "$proof/codex-warp-release-metadata.json")"
archive_digest="$(jq -r '.dist.artifacts[0].archiveSha256' "$proof/codex-warp-release-metadata.json")"
printf '%s *%s\n' "$archive_digest" "$archive_file" >>"$tmp/extra-checksum-entry/$checksum_file"
if bash scripts/check-release-contract.sh pr-upload-proof "$tmp/extra-checksum-entry" "$proof/codex-warp-release-metadata.json" "$proof/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted an extra checksum entry' >&2
  exit 1
fi

cp -R "$proof" "$tmp/mismatched-announcement"
jq '.announcement_tag_is_implicit = false' "$tmp/mismatched-announcement/dist-manifest.json" >"$tmp/mismatched-manifest.json"
mv "$tmp/mismatched-manifest.json" "$tmp/mismatched-announcement/dist-manifest.json"
manifest_digest="$(bash scripts/sha256-file.sh "$tmp/mismatched-announcement/dist-manifest.json")"
jq --arg digest "$manifest_digest" '.dist.manifestSha256 = $digest' "$tmp/mismatched-announcement/codex-warp-release-metadata.json" >"$tmp/mismatched-metadata.json"
mv "$tmp/mismatched-metadata.json" "$tmp/mismatched-announcement/codex-warp-release-metadata.json"
if bash scripts/check-release-contract.sh pr-upload-proof "$tmp/mismatched-announcement" "$tmp/mismatched-announcement/codex-warp-release-metadata.json" "$tmp/mismatched-announcement/dist-manifest.json" >/dev/null 2>&1; then
  echo 'check-release-contract-harness: accepted mismatched announcement-tag mode' >&2
  exit 1
fi

echo 'check-release-contract-harness: ok'
