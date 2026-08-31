#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
subject="$tmp/artifact.tar.xz"
manifest="$tmp/manifest.json"
log="$tmp/gh.log"
printf 'artifact\n' >"$subject"

source_sha='1111111111111111111111111111111111111111'
workflow_sha='2222222222222222222222222222222222222222'
run_json() {
  jq -n --arg name "$1" --arg path "$2" --arg sha "$3" \
    '{name:$name,path:$path,head_sha:$sha,head_branch:"main"}'
}

gh() {
  if [ "${1:-}" = attestation ]; then
    printf '%s\n' "$*" >>"$VERIFY_ATTESTATION_LOG"
    return 0
  fi
  if [ "${1:-}" = api ]; then
    printf '%s\n' "$NIGHTLY_RUN_JSON"
    return 0
  fi
  echo "verify-nightly-attestation-harness: unsupported gh command: $*" >&2
  return 2
}
export -f gh
export VERIFY_ATTESTATION_LOG="$log"

jq -n --arg source "$source_sha" --arg workflow "$workflow_sha" \
  '{repository:"jatmn/Codex-warp",sourceSha:$source,tag:"nightly-20260831-111111111111",workflow:"https://github.com/jatmn/Codex-warp/actions/runs/9",workflowSha:$workflow}' \
  >"$manifest"

NIGHTLY_RUN_JSON="$(run_json Nightly '.github/workflows/nightly.yml' "$source_sha")"
export NIGHTLY_RUN_JSON
bash "$root/scripts/verify-nightly-attestation.sh" "$subject" "$manifest"
grep -F -- '--signer-workflow jatmn/Codex-warp/.github/workflows/nightly.yml' "$log" >/dev/null
grep -F -- '--source-ref refs/heads/main' "$log" >/dev/null
grep -F -- "--source-digest $source_sha" "$log" >/dev/null
grep -F -- '--deny-self-hosted-runners' "$log" >/dev/null

: >"$log"
NIGHTLY_RUN_JSON="$(run_json 'Nightly Recovery' '.github/workflows/nightly-recovery.yml' "$workflow_sha")"
export NIGHTLY_RUN_JSON
bash "$root/scripts/verify-nightly-attestation.sh" "$subject" "$manifest"
grep -F -- '--signer-workflow jatmn/Codex-warp/.github/workflows/nightly-recovery.yml' "$log" >/dev/null
grep -F -- "--source-digest $workflow_sha" "$log" >/dev/null

NIGHTLY_RUN_JSON="$(run_json CI '.github/workflows/ci.yml' "$source_sha")"
export NIGHTLY_RUN_JSON
if bash "$root/scripts/verify-nightly-attestation.sh" "$subject" "$manifest" >/dev/null 2>&1; then
  echo 'verify-nightly-attestation-harness: untrusted workflow was accepted' >&2
  exit 1
fi

NIGHTLY_RUN_JSON="$(run_json Nightly '.github/workflows/nightly.yml' "$workflow_sha")"
export NIGHTLY_RUN_JSON
if bash "$root/scripts/verify-nightly-attestation.sh" "$subject" "$manifest" >/dev/null 2>&1; then
  echo 'verify-nightly-attestation-harness: mismatched Nightly source digest was accepted' >&2
  exit 1
fi

echo 'verify-nightly-attestation-harness: ok'
