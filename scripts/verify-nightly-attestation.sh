#!/usr/bin/env bash
# Verify nightly archives against a reviewed Nightly or Nightly Recovery identity.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo 'usage: verify-nightly-attestation.sh <subject> <nightly-manifest.json>' >&2
  exit 2
fi

subject="$1"
manifest="$2"
[ -f "$subject" ] && [ -f "$manifest" ] || {
  echo 'verify-nightly-attestation: subject or manifest is missing' >&2
  exit 2
}

repository="$(jq -er '.repository' "$manifest")"
source_sha="$(jq -er '.sourceSha' "$manifest")"
tag="$(jq -er '.tag' "$manifest")"
workflow_url="$(jq -er '.workflow' "$manifest")"
workflow_sha="$(jq -er '.workflowSha' "$manifest")"
[[ "$repository" =~ ^[^/]+/[^/]+$ ]]
[[ "$source_sha" =~ ^[0-9a-f]{40}$ ]]
[[ "$workflow_sha" =~ ^[0-9a-f]{40}$ ]]
[[ "$tag" =~ ^nightly-[0-9]{8}-[0-9a-f]{12}$ ]]
[[ "$workflow_url" =~ ^https://github\.com/[^/]+/[^/]+/actions/runs/[0-9]+$ ]]
run_id="${workflow_url##*/}"

run="$(gh api "repos/$repository/actions/runs/$run_id")"
workflow_name="$(jq -er '.name' <<<"$run")"
workflow_path="$(jq -er '.path' <<<"$run")"
head_sha="$(jq -er '.head_sha' <<<"$run")"
head_branch="$(jq -er '.head_branch' <<<"$run")"

case "$workflow_name" in
  Nightly)
    [ "$workflow_path" = '.github/workflows/nightly.yml' ]
    [ "$head_branch" = main ]
    [ "$head_sha" = "$source_sha" ]
    signer_workflow="$repository/.github/workflows/nightly.yml"
    source_ref='refs/heads/main'
    attested_source="$source_sha"
    ;;
  'Nightly Recovery')
    [ "$workflow_path" = '.github/workflows/nightly-recovery.yml' ]
    [ "$head_branch" = main ]
    [ "$head_sha" = "$workflow_sha" ]
    signer_workflow="$repository/.github/workflows/nightly-recovery.yml"
    source_ref='refs/heads/main'
    attested_source="$workflow_sha"
    ;;
  *)
    echo "verify-nightly-attestation: untrusted workflow identity: $workflow_name" >&2
    exit 1
    ;;
esac

gh attestation verify "$subject" \
  --repo "$repository" \
  --signer-workflow "$signer_workflow" \
  --source-ref "$source_ref" \
  --source-digest "$attested_source" \
  --deny-self-hosted-runners
