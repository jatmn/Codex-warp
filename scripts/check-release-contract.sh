#!/usr/bin/env bash
# Validate archive layout or the complete official/proof asset transaction.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
contract="${RELEASE_CONTRACT_PATH:-tools/release-contract.json}"
dist_schema="${DIST_MANIFEST_SCHEMA_PATH:-tools/dist-manifest.schema.json}"
metadata_schema="${RELEASE_METADATA_SCHEMA_PATH:-tools/release-metadata.schema.json}"

die() {
  echo "check-release-contract: $*" >&2
  exit 1
}

validate_archive() {
  [ "$#" -eq 4 ] || die 'archive requires <archive> <target> <source-dir> <expected-version>'
  local archive="$1" target="$2" source="$3" version="$4"
  [ -f "$archive" ] || die "archive is missing: $archive"
  [ -d "$source" ] || die "source directory is missing: $source"

  local expected binary filename basename payload temp list logical windows_archive windows_destination reported_version
  expected="$(jq -r --arg target "$target" '.targets[] | select(.triple == $target) | .archive' "$contract")"
  binary="$(jq -r --arg target "$target" '.targets[] | select(.triple == $target) | .binary' "$contract")"
  expected="${expected%$'\r'}"
  binary="${binary%$'\r'}"
  [ -n "$expected" ] && [ "$expected" != 'null' ] || die "unsupported target: $target"
  filename="$(basename "$archive")"
  if [ "$filename" != "$expected" ] &&
     ! [[ "$filename" =~ ^codex-warp-nightly-[0-9]{8}-[0-9a-f]{12}-${target//./\.}\.(tar\.xz|zip)$ ]]; then
    die "archive name does not match the official or nightly contract: $filename"
  fi
  case "$filename" in
    *.tar.xz) basename="${filename%.tar.xz}" ;;
    *.zip) basename="${filename%.zip}" ;;
    *) die "unsupported archive extension: $filename" ;;
  esac

  temp="$(mktemp -d)"
  trap 'rm -rf "$temp"' RETURN
  list="$temp/inventory.txt"
  if [[ "$filename" == *.tar.xz ]]; then
    tar -tJf "$archive" >"$list"
    mkdir "$temp/extract"
    tar -xJf "$archive" -C "$temp/extract"
    payload="$temp/extract/$basename"
    [ -d "$payload" ] || die "tar must have exactly the top-level directory $basename/"
    if find "$temp/extract" -mindepth 1 -maxdepth 1 ! -name "$basename" -print -quit | grep -q .; then
      die 'tar contains more than one top-level entry'
    fi
    sed -e "s#^$basename/##" -e '/^$/d' "$list" >"$temp/logical.txt"
  else
    unzip -Z1 "$archive" >"$list"
    mkdir "$temp/zip"
    if [ "${RUNNER_OS:-}" = Windows ]; then
      if command -v powershell.exe >/dev/null && command -v cygpath >/dev/null; then
        windows_archive="$(cygpath -w "$archive")"
        windows_destination="$(cygpath -w "$temp/zip")"
        CODEX_WARP_ARCHIVE_PATH="$windows_archive" \
          CODEX_WARP_ARCHIVE_DESTINATION="$windows_destination" \
          MSYS2_ARG_CONV_EXCL='*' \
          powershell.exe -NoLogo -NoProfile -NonInteractive -Command \
          '$ErrorActionPreference = "Stop"; Expand-Archive -LiteralPath $env:CODEX_WARP_ARCHIVE_PATH -DestinationPath $env:CODEX_WARP_ARCHIVE_DESTINATION -Force' \
          >/dev/null
      else
        command -v 7z >/dev/null || die 'PowerShell or 7z is required to validate Windows archives'
        7z x -bd -y "-o$temp/zip" "$archive" >/dev/null
      fi
    else
      unzip -qq "$archive" -d "$temp/zip"
    fi
    payload="$temp/zip"
    cp "$list" "$temp/logical.txt"
  fi
  logical="$temp/logical.txt"

  if grep -En '(^/|(^|/)\.\.(/|$))' "$list" >/dev/null; then
    die 'archive contains an unsafe path'
  fi
  if find "$payload" -type l -print -quit | grep -q .; then
    die 'archive contains a symbolic link'
  fi
  if [ ! -f "$payload/$binary" ]; then
    find "$payload" -maxdepth 3 -type f -print >&2 || true
    die "archive is missing $binary"
  fi
  while IFS= read -r required; do
    required="${required%$'\r'}"
    if [ ! -f "$payload/$required" ]; then
      find "$payload" -maxdepth 3 -type f -print >&2 || true
      die "archive is missing $required"
    fi
    [ "$(bash scripts/sha256-lf-file.sh "$payload/$required")" = "$(bash scripts/sha256-lf-file.sh "$source/$required")" ] ||
      die "$required does not match the selected source"
  done < <(jq -r '.requiredArchiveEntries[]' "$contract")
  (cd "$source/configs" && find . -type f -print | sed 's#^\./##' | LC_ALL=C sort) >"$temp/source-configs.txt"
  [ -s "$temp/source-configs.txt" ] || die 'selected source has no configuration files'
  while IFS= read -r config_file; do
    [ -f "$payload/configs/$config_file" ] || die "archive is missing configs/$config_file"
    [ "$(bash scripts/sha256-lf-file.sh "$payload/configs/$config_file")" = "$(bash scripts/sha256-lf-file.sh "$source/configs/$config_file")" ] ||
      die "configs/$config_file does not match the selected source"
  done <"$temp/source-configs.txt"
  (cd "$payload/configs" && find . -type f -print | sed 's#^\./##' | LC_ALL=C sort) >"$temp/archive-configs.txt"
  cmp "$temp/source-configs.txt" "$temp/archive-configs.txt" >/dev/null || die 'archive configuration inventory differs from the selected source'

  while IFS= read -r entry; do
    entry="${entry%$'\r'}"
    entry="${entry%/}"
    [ -z "$entry" ] && continue
    case "$entry" in
      "$binary"|codex-warp.toml|README.md|LICENSE|NOTICE|CHANGELOG.md|configs|configs/*) ;;
      *) die "unexpected archive entry: $entry" ;;
    esac
  done <"$logical"
  while IFS= read -r forbidden; do
    forbidden="${forbidden%$'\r'}"
    if grep -En "$forbidden" "$logical" >/dev/null; then
      die "archive contains a forbidden path matching: $forbidden"
    fi
  done < <(jq -r '.forbiddenPathPatterns[]' "$contract")

  if [ "${SKIP_VERSION_SMOKE:-0}" != '1' ]; then
    [ -x "$payload/$binary" ] || die "$binary is not executable"
    reported_version="$("$payload/$binary" --version)"
    [ "$reported_version" = "codex-warp $version" ] ||
      die "$binary --version reported '$reported_version', expected 'codex-warp $version'"
    "$payload/$binary" --help >/dev/null || die "$binary --help failed"
  fi
  trap - RETURN
  rm -rf "$temp"
  echo "check-release-contract: archive ok: $filename"
}

dist_artifacts() {
  jq -c '
    . as $manifest |
    [.artifacts | to_entries[] | select(.value.kind == "executable-zip") |
      {target:.value.target_triples[0], archive:.value.name, archiveSha256:.value.checksums.sha256, checksumFile:$manifest.artifacts[.value.checksum].name}
    ] | sort_by(.target)
  ' "$1"
}

validate_assets() {
  [ "$#" -eq 4 ] || die 'asset validation requires <profile> <asset-dir> <metadata.json> <dist-manifest.json>'
  local profile="$1" assets="$2" metadata="$3" manifest="$4"
  [ "$profile" = 'official-publication' ] || [ "$profile" = 'pr-upload-proof' ] || die "unknown profile: $profile"
  [ -d "$assets" ] && [ -f "$metadata" ] && [ -f "$manifest" ] || die 'asset validation inputs are missing'
  node tools/release-please-policy/validate-json.mjs "$dist_schema" "$manifest"
  node tools/release-please-policy/validate-json.mjs "$metadata_schema" "$metadata"

  local expected_mode manifest_sha contract_sha schema_sha mode
  expected_mode="$([ "$profile" = 'official-publication' ] && echo official || echo pr-upload-proof)"
  mode="$(jq -r '.mode' "$metadata")"
  [ "$mode" = "$expected_mode" ] || die "$profile rejects metadata mode $mode"
  manifest_sha="$(sha256sum "$manifest" | awk '{print $1}')"
  contract_sha="$(sha256sum "$contract" | awk '{print $1}')"
  schema_sha="$(sha256sum "$dist_schema" | awk '{print $1}')"
  [ "$(jq -r '.dist.manifestSha256' "$metadata")" = "$manifest_sha" ] || die 'dist manifest digest mismatch'
  [ "$(jq -r '.releaseContractSha256' "$metadata")" = "$contract_sha" ] || die 'release contract digest mismatch'
  [ "$(jq -r '.dist.manifestSchemaSha256' "$metadata")" = "$schema_sha" ] || die 'dist schema digest mismatch'
  [ "$(jq -r '.dist.version' "$metadata")" = "$(jq -r '.dist_version' "$manifest")" ] || die 'dist version mismatch'
  [ "$(jq -r '.dist.announcementTagIsImplicit' "$metadata")" = "$(jq -r '.announcement_tag_is_implicit' "$manifest")" ] || die 'dist announcement-tag mode mismatch'
  [ "$(jq -c '.dist.artifacts | sort_by(.target)' "$metadata")" = "$(dist_artifacts "$manifest")" ] || die 'metadata and dist artifact mappings disagree'

  if [ -n "${SOURCE_DIR:-}" ]; then
    [ "$(jq -r '.cargoLockSha256' "$metadata")" = "$(sha256sum "$SOURCE_DIR/Cargo.lock" | awk '{print $1}')" ] || die 'Cargo.lock digest mismatch'
    [ "$(jq -r '.rustToolchain.fileSha256' "$metadata")" = "$(sha256sum "$SOURCE_DIR/rust-toolchain.toml" | awk '{print $1}')" ] || die 'rust-toolchain.toml digest mismatch'
  fi

  if [ "$profile" = 'official-publication' ]; then
    local tag source_sha peeled
    [ "$(jq -r '.publishable' "$metadata")" = true ] || die 'official metadata must be publishable'
    tag="$(jq -r '.tag' "$metadata")"
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die 'invalid official tag'
    [ "$tag" = "v$(jq -r '.cargoVersion' "$metadata")" ] || die 'tag and Cargo version disagree'
    peeled="$(git rev-parse "refs/tags/$tag^{}" 2>/dev/null)" || die "official tag does not exist: $tag"
    source_sha="$(jq -r '.sourceSha' "$metadata")"
    [ "$peeled" = "$source_sha" ] && [ "$(jq -r '.peeledTagSha' "$metadata")" = "$source_sha" ] || die 'official tag does not peel to the recorded source'
  else
    [ "$(jq -r '.publishable' "$metadata")" = false ] || die 'proof metadata must not be publishable'
    [ "$(jq -r '.tag' "$metadata")" = null ] || die 'proof metadata must not claim a tag'
  fi

  local expected_list actual_list unified
  expected_list="$(mktemp)"
  actual_list="$(mktemp)"
  trap 'rm -f "$expected_list" "$actual_list"' RETURN
  {
    jq -r '.dist.artifacts[] | .archive, .checksumFile' "$metadata"
    jq -r '.unifiedChecksumFilename, .distManifestFilename, .metadataFilename' "$contract"
  } | sort >"$expected_list"
  find "$assets" -maxdepth 1 -type f -printf '%f\n' | sort >"$actual_list"
  if ! cmp "$expected_list" "$actual_list" >/dev/null; then
    echo 'check-release-contract: expected assets:' >&2
    cat "$expected_list" >&2
    echo 'check-release-contract: actual assets:' >&2
    cat "$actual_list" >&2
    die 'release asset inventory differs from the contract'
  fi
  [ "$(wc -l <"$expected_list")" -eq 11 ] || die 'derived official asset contract must contain eleven files'
  [ "$(sha256sum "$assets/$(jq -r '.distManifestFilename' "$contract")" | awk '{print $1}')" = "$manifest_sha" ] || die 'asset dist manifest differs from the validated manifest'
  [ "$(sha256sum "$assets/$(jq -r '.metadataFilename' "$contract")" | awk '{print $1}')" = "$(sha256sum "$metadata" | awk '{print $1}')" ] || die 'asset metadata differs from the validated sidecar'

  while IFS=$'\t' read -r archive digest checksum; do
    [ "$(sha256sum "$assets/$archive" | awk '{print $1}')" = "$digest" ] || die "archive digest mismatch: $archive"
    bash scripts/check-sha256-index.sh "$assets/$checksum" "$digest" "$archive" >/dev/null || die "invalid checksum file: $checksum"
  done < <(jq -r '.dist.artifacts[] | [.archive, .archiveSha256, .checksumFile] | @tsv' "$metadata")
  unified="$assets/$(jq -r '.unifiedChecksumFilename' "$contract")"
  local checksum_args
  checksum_args=("$unified")
  while IFS=$'\t' read -r archive digest; do
    checksum_args+=("$digest" "$archive")
  done < <(jq -r '.dist.artifacts[] | [.archive, .archiveSha256] | @tsv' "$metadata")
  bash scripts/check-sha256-index.sh "${checksum_args[@]}" >/dev/null || die 'unified checksum index differs from the contract'
  (cd "$assets" && sha256sum -c "$(basename "$unified")" >/dev/null) || die 'unified checksum verification failed'

  trap - RETURN
  rm -f "$expected_list" "$actual_list"
  echo "check-release-contract: $profile assets ok"
}

case "${1:-}" in
  archive) shift; validate_archive "$@" ;;
  official-publication|pr-upload-proof) profile="$1"; shift; validate_assets "$profile" "$@" ;;
  *) die 'usage: check-release-contract.sh archive <archive> <target> <source-dir> <version> | <official-publication|pr-upload-proof> <asset-dir> <metadata.json> <dist-manifest.json>' ;;
esac
