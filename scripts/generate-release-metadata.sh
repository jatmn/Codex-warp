#!/usr/bin/env bash
# Build the project-owned sidecar without modifying dist-manifest.json.
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo 'usage: generate-release-metadata.sh <official|pr-upload-proof> <identity.json> <dist-manifest.json> <output.json>' >&2
  exit 2
fi

profile="$1"
identity="$2"
manifest="$3"
output="$4"
case "$profile" in official|pr-upload-proof) ;; *) echo "unknown profile: $profile" >&2; exit 2 ;; esac
[ -f "$identity" ] && [ -f "$manifest" ] || { echo 'metadata inputs are missing' >&2; exit 2; }

manifest_sha="$(sha256sum "$manifest" | awk '{print $1}')"
artifacts="$(jq -c '
  . as $manifest |
  [.artifacts | to_entries[] |
    select(.value.kind == "executable-zip") |
    {
      target: .value.target_triples[0],
      archive: .value.name,
      archiveSha256: .value.checksums.sha256,
      checksumFile: $manifest.artifacts[.value.checksum].name
    }
  ] | sort_by(.target)
' "$manifest")"

if [ "$(jq 'length' <<<"$artifacts")" -ne 4 ] ||
   ! jq -e 'all(.[]; (.archiveSha256 | test("^[0-9a-f]{64}$")) and (.checksumFile | type == "string"))' <<<"$artifacts" >/dev/null; then
  echo 'generate-release-metadata: dist manifest lacks the four complete archive/checksum mappings' >&2
  exit 1
fi

jq \
  --arg profile "$profile" \
  --arg manifest_sha "$manifest_sha" \
  --argjson implicit "$(jq '.announcement_tag_is_implicit' "$manifest")" \
  --argjson artifacts "$artifacts" '
    . + {
      "$schema": "./release-metadata.schema.json",
      schemaVersion: 1,
      fileName: "codex-warp-release-metadata.json",
      mode: $profile,
      dist: (.dist + {
        manifestSha256: $manifest_sha,
        announcementTagIsImplicit: $implicit,
        artifacts: $artifacts
      })
    }
  ' "$identity" >"$output"

echo "generate-release-metadata: wrote $output"
