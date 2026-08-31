#!/usr/bin/env bash
set -euo pipefail

required=(NIGHTLY_DATE NIGHTLY_SOURCE_SHA NIGHTLY_VERSION NIGHTLY_TAG TARGET BINARY_PATH OUTPUT_DIR RUNNER_LABEL RUNNER_IMAGE WORKFLOW_URL WORKFLOW_SHA)
for variable in "${required[@]}"; do
  [ -n "${!variable:-}" ] || { echo "package-nightly: $variable is required" >&2; exit 2; }
done
[[ "$NIGHTLY_DATE" =~ ^[0-9]{8}$ ]]
[[ "$NIGHTLY_SOURCE_SHA" =~ ^[0-9a-f]{40}$ ]]
[[ "$NIGHTLY_TAG" == "nightly-$NIGHTLY_DATE-${NIGHTLY_SOURCE_SHA:0:12}" ]]
[[ "$NIGHTLY_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+-nightly\.$NIGHTLY_DATE\.${NIGHTLY_SOURCE_SHA:0:12}$ ]]

root="$(git rev-parse --show-toplevel)"
cd "$root"
base_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
[ "$NIGHTLY_VERSION" = "$base_version-nightly.$NIGHTLY_DATE.${NIGHTLY_SOURCE_SHA:0:12}" ]
[ "$(git rev-parse HEAD)" = "$NIGHTLY_SOURCE_SHA" ]
[ -f "$BINARY_PATH" ]
mkdir -p "$OUTPUT_DIR"

case "$TARGET" in
  x86_64-unknown-linux-gnu|aarch64-apple-darwin|x86_64-apple-darwin)
    extension='tar.xz'; binary_name='codex-warp' ;;
  x86_64-pc-windows-msvc)
    extension='zip'; binary_name='codex-warp.exe' ;;
  *) echo "package-nightly: unsupported target $TARGET" >&2; exit 2 ;;
esac
archive_base="codex-warp-$NIGHTLY_TAG-$TARGET"
archive="$archive_base.$extension"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
stage="$temp/$archive_base"
mkdir -p "$stage/configs"
cp "$BINARY_PATH" "$stage/$binary_name"
chmod +x "$stage/$binary_name" 2>/dev/null || true
cp codex-warp.toml README.md LICENSE NOTICE CHANGELOG.md "$stage/"
cp -R configs/. "$stage/configs/"

packaging_tools='{}'
record_tool() {
  local name="$1" command_path
  command_path="$(command -v "$2")"
  packaging_tools="$(jq --arg name "$name" --arg digest "$(bash scripts/sha256-file.sh "$command_path")" '. + {($name):$digest}' <<<"$packaging_tools")"
}
if [ "$extension" = 'tar.xz' ]; then
  record_tool tar tar
  record_tool xz xz
  tar -cJf "$OUTPUT_DIR/$archive" -C "$temp" "$archive_base"
else
  if command -v 7z >/dev/null 2>&1; then
    record_tool 7z 7z
    (cd "$stage" && 7z a -bd -tzip "$OUTPUT_DIR/$archive" . >/dev/null)
  else
    record_tool zip zip
    (cd "$stage" && zip -qr "$OUTPUT_DIR/$archive" .)
  fi
fi
archive_sha="$(bash scripts/sha256-file.sh "$OUTPUT_DIR/$archive")"
printf '%s  %s\n' "$archive_sha" "$archive" >"$OUTPUT_DIR/$archive.sha256"

native_tools="$({ cmake --version 2>/dev/null | head -1; xcodebuild -version 2>/dev/null | paste -sd ' ' -; nasm -v 2>/dev/null; cc --version 2>/dev/null | head -1; } || true)"
[ -n "$native_tools" ] || native_tools='not reported by runner'
jq -n \
  --arg repository 'jatmn/Codex-warp' --arg tag "$NIGHTLY_TAG" --arg date "$NIGHTLY_DATE" \
  --arg source "$NIGHTLY_SOURCE_SHA" --arg base "$base_version" --arg version "$NIGHTLY_VERSION" \
  --arg workflow "$WORKFLOW_URL" --arg workflow_sha "$WORKFLOW_SHA" \
  --arg lock "$(bash scripts/sha256-file.sh Cargo.lock)" \
  --arg toolchain "$(bash scripts/sha256-file.sh rust-toolchain.toml)" \
  --arg contract "$(bash scripts/nightly-contract-digest.sh)" \
  --arg script "$(bash scripts/sha256-file.sh scripts/package-nightly.sh)" \
  --arg target "$TARGET" --arg archive "$archive" --arg archive_sha "$archive_sha" \
  --arg checksum "$archive.sha256" --arg runner "$RUNNER_LABEL" --arg image "$RUNNER_IMAGE" \
  --arg rustc "$(rustc -Vv)" --arg cargo "$(cargo -Vv)" --arg native "$native_tools" \
  --argjson packaging_tools "$packaging_tools" \
  '{repository:$repository,tag:$tag,date:$date,sourceSha:$source,baseVersion:$base,version:$version,workflow:$workflow,workflowSha:$workflow_sha,cargoLockSha256:$lock,rustToolchainSha256:$toolchain,packagingContractSha256:$contract,packagingScriptSha256:$script,artifact:{target:$target,archive:$archive,archiveSha256:$archive_sha,checksumFile:$checksum,runnerLabel:$runner,runnerImage:$image,rustcVv:$rustc,cargoVv:$cargo,nativeTools:$native,packagingTools:$packaging_tools}}' \
  >"$OUTPUT_DIR/$TARGET-nightly-evidence.json"

bash scripts/check-release-contract.sh archive "$OUTPUT_DIR/$archive" "$TARGET" "$root" "$NIGHTLY_VERSION"
echo "package-nightly: wrote $archive"
