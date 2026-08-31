#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
sha="$(git rev-parse HEAD)"
date=20260830
base_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
version="$base_version-nightly.$date+${sha:0:12}"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
CODEX_WARP_BUILD_VERSION="$version" cargo build --release --locked >/dev/null
for target in x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin x86_64-pc-windows-msvc; do
  output="$temp/$target"
  mkdir -p "$output"
  NIGHTLY_DATE="$date" \
  NIGHTLY_SOURCE_SHA="$sha" \
  NIGHTLY_VERSION="$version" \
  NIGHTLY_TAG="nightly-$date-${sha:0:12}" \
  TARGET="$target" \
  BINARY_PATH=target/release/codex-warp \
  OUTPUT_DIR="$output" \
  RUNNER_LABEL=local \
  RUNNER_IMAGE=local-test \
  WORKFLOW_URL=https://github.com/jatmn/Codex-warp/actions/runs/1 \
  WORKFLOW_SHA="$sha" \
  bash scripts/package-nightly.sh >/dev/null
done

linux_archive="codex-warp-nightly-$date-${sha:0:12}-x86_64-unknown-linux-gnu.tar.xz"
(cd "$temp/x86_64-unknown-linux-gnu" && sha256sum -c "$linux_archive.sha256" >/dev/null)
tar -xJf "$temp/x86_64-unknown-linux-gnu/$linux_archive" -C "$temp"
[ "$("$temp/${linux_archive%.tar.xz}/codex-warp" --version)" = "codex-warp $version" ]

windows_archive="codex-warp-nightly-$date-${sha:0:12}-x86_64-pc-windows-msvc.zip"
(cd "$temp/x86_64-pc-windows-msvc" && sha256sum -c "$windows_archive.sha256" >/dev/null)
unzip -Z1 "$temp/x86_64-pc-windows-msvc/$windows_archive" | grep -Fx codex-warp.exe >/dev/null

assets="$temp/assets"
mkdir "$assets"
find "$temp" -mindepth 2 -maxdepth 2 -type f \( -name '*.tar.xz' -o -name '*.zip' -o -name '*.sha256' \) -exec cp {} "$assets/" \;
mapfile -t evidence < <(find "$temp" -mindepth 2 -maxdepth 2 -name '*-nightly-evidence.json' -print | sort)
jq -s '.[0] as $f | {"$schema":"./nightly-manifest.schema.json",schemaVersion:1,fileName:"codex-warp-nightly-manifest.json",repository:$f.repository,tag:$f.tag,date:$f.date,sourceSha:$f.sourceSha,baseVersion:$f.baseVersion,version:$f.version,workflow:$f.workflow,workflowSha:$f.workflowSha,cargoLockSha256:$f.cargoLockSha256,rustToolchainSha256:$f.rustToolchainSha256,packagingContractSha256:$f.packagingContractSha256,packagingScriptSha256:$f.packagingScriptSha256,artifacts:(map(.artifact)|sort_by(.target))}' "${evidence[@]}" >"$assets/codex-warp-nightly-manifest.json"
jq -r '.artifacts[] | [.archiveSha256,.archive] | @tsv' "$assets/codex-warp-nightly-manifest.json" | sed $'s/\t/  /' >"$assets/sha256.sum"
bash scripts/check-nightly-assets.sh "$assets" "$assets/codex-warp-nightly-manifest.json" "$root" >/dev/null

mismatch="$temp/mismatched-tag-assets"
cp -R "$assets" "$mismatch"
wrong_tag="nightly-20260829-${sha:0:12}"
old_archive="$linux_archive"
wrong_archive="codex-warp-$wrong_tag-x86_64-unknown-linux-gnu.tar.xz"
mv "$mismatch/$old_archive" "$mismatch/$wrong_archive"
mv "$mismatch/$old_archive.sha256" "$mismatch/$wrong_archive.sha256"
sed -i "s/$old_archive/$wrong_archive/g" "$mismatch/$wrong_archive.sha256" "$mismatch/sha256.sum"
jq --arg old "$old_archive" --arg wrong "$wrong_archive" '(.artifacts[] | select(.archive==$old)) |= (.archive=$wrong | .checksumFile=($wrong+".sha256"))' "$mismatch/codex-warp-nightly-manifest.json" >"$temp/mismatched-manifest.json"
mv "$temp/mismatched-manifest.json" "$mismatch/codex-warp-nightly-manifest.json"
if bash scripts/check-nightly-assets.sh "$mismatch" "$mismatch/codex-warp-nightly-manifest.json" "$root" >/dev/null 2>&1; then
  echo 'package-nightly-harness: accepted an archive name from a different nightly tag' >&2
  exit 1
fi
echo 'package-nightly-harness: ok'
