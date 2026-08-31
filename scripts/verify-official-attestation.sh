#!/usr/bin/env bash
# Verify official artifacts against a reviewed release workflow identity.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo 'usage: verify-official-attestation.sh <subject> <release-metadata.json>' >&2
  exit 2
fi

subject="$1"
metadata="$2"
[ -f "$subject" ] && [ -f "$metadata" ] || {
  echo 'verify-official-attestation: subject or metadata is missing' >&2
  exit 2
}

repository="$(jq -er '.repository' "$metadata")"
source_sha="$(jq -er '.sourceSha' "$metadata")"
tag="$(jq -er '.tag' "$metadata")"
workflow_name="$(jq -er '.workflow.name' "$metadata")"
workflow_sha="$(jq -er '.workflow.workflowSha' "$metadata")"
[[ "$repository" =~ ^[^/]+/[^/]+$ ]]
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]]
[[ "$workflow_sha" =~ ^[0-9a-f]{40}$ ]]
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]

case "$workflow_name" in
  Release)
    signer_workflow="$repository/.github/workflows/release.yml"
    source_ref="refs/tags/$tag"
    attested_source="$source_sha"
    ;;
  'Release Recovery')
    signer_workflow="$repository/.github/workflows/release-recovery.yml"
    source_ref='refs/heads/main'
    attested_source="$workflow_sha"
    ;;
  *)
    echo "verify-official-attestation: untrusted workflow identity: $workflow_name" >&2
    exit 1
    ;;
esac

gh attestation verify "$subject" \
  --repo "$repository" \
  --signer-workflow "$signer_workflow" \
  --source-ref "$source_ref" \
  --source-digest "$attested_source" \
  --deny-self-hosted-runners
