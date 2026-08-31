#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
subject="$tmp/artifact.tar.xz"
metadata="$tmp/metadata.json"
log="$tmp/gh.log"
printf 'artifact\n' >"$subject"

gh() {
  printf '%s\n' "$*" >>"$VERIFY_ATTESTATION_LOG"
}
export -f gh
export VERIFY_ATTESTATION_LOG="$log"

source_sha='1111111111111111111111111111111111111111'
workflow_sha='2222222222222222222222222222222222222222'
jq -n --arg source "$source_sha" --arg workflow "$workflow_sha" \
  '{repository:"jatmn/Codex-warp",sourceSha:$source,tag:"v1.2.3",workflow:{name:"Release",workflowSha:$workflow}}' >"$metadata"
bash "$root/scripts/verify-official-attestation.sh" "$subject" "$metadata"
grep -F -- '--signer-workflow jatmn/Codex-warp/.github/workflows/release.yml' "$log" >/dev/null
grep -F -- '--source-ref refs/tags/v1.2.3' "$log" >/dev/null
grep -F -- "--source-digest $source_sha" "$log" >/dev/null
grep -F -- '--deny-self-hosted-runners' "$log" >/dev/null

: >"$log"
jq --arg workflow "$workflow_sha" '.workflow={name:"Release Recovery",workflowSha:$workflow}' "$metadata" >"$tmp/recovery.json"
bash "$root/scripts/verify-official-attestation.sh" "$subject" "$tmp/recovery.json"
grep -F -- '--signer-workflow jatmn/Codex-warp/.github/workflows/release-recovery.yml' "$log" >/dev/null
grep -F -- '--source-ref refs/heads/main' "$log" >/dev/null
grep -F -- "--source-digest $workflow_sha" "$log" >/dev/null

jq '.workflow.name="pull_request"' "$metadata" >"$tmp/untrusted.json"
if bash "$root/scripts/verify-official-attestation.sh" "$subject" "$tmp/untrusted.json" >/dev/null 2>&1; then
  echo 'verify-official-attestation-harness: untrusted workflow was accepted' >&2
  exit 1
fi

echo 'verify-official-attestation-harness: ok'
