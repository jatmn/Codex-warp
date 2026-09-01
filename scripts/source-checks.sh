#!/usr/bin/env bash
# Mechanical nits that should fail before a human or AI review round.
# Clippy is crate-wide: `cargo clippy --locked --all-targets --all-features -- -D warnings`.
set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$root" ]; then
  root="$(cd "$(dirname "$0")/.." && pwd)"
fi
cd "$root"

skip_typos="${SOURCE_CHECKS_SKIP_TYPOS:-0}"
run_clippy="${SOURCE_CHECKS_CLIPPY:-1}"
fail=0

if ! cargo fmt --check; then
  fail=1
fi

if ! DOCS_CHECKS_SKIP_TYPOS="$skip_typos" bash scripts/docs-checks.sh; then
  fail=1
fi

if ! bash scripts/ci-change-scope-harness.sh; then
  fail=1
fi

if ! bash scripts/git-hooks-harness.sh; then
  fail=1
fi

if ! bash scripts/check-pr-title-harness.sh; then
  fail=1
fi

if ! bash scripts/check-release-readiness-harness.sh; then
  fail=1
fi

if ! bash scripts/check-prior-official-releases-harness.sh; then
  fail=1
fi

if ! bash scripts/language-policy-check.sh; then
  fail=1
fi

if command -v node >/dev/null 2>&1; then
  while IFS= read -r js; do
    [ -n "$js" ] || continue
    if ! node --check "$js"; then
      fail=1
    fi
  done < <(git ls-files '*.js' '*.mjs')

  if ! bash scripts/release-please-policy-harness.sh; then
    fail=1
  fi

  if ! node scripts/webui_chart_harness.js; then
    fail=1
  fi

  if ! node scripts/webui_analytics_filters_harness.js; then
    fail=1
  fi

  if ! node scripts/webui_analytics_chart_visibility_harness.js; then
    fail=1
  fi

  if ! bash scripts/check-release-contract-harness.sh; then
    fail=1
  fi

  if ! bash scripts/lookup-official-draft-harness.sh; then
    fail=1
  fi

  if ! bash scripts/create-missing-official-draft-harness.sh; then
    fail=1
  fi

  if ! bash scripts/verify-official-attestation-harness.sh; then
    fail=1
  fi

  if ! bash scripts/verify-nightly-attestation-harness.sh; then
    fail=1
  fi

  if ! bash scripts/nightly-contract-digest-harness.sh; then
    fail=1
  fi

  if ! bash scripts/sha256-lf-file-harness.sh; then
    fail=1
  fi

  if ! bash scripts/check-sha256-index-harness.sh; then
    fail=1
  fi

  if ! (
    rg() {
      echo 'source-checks: workflow checks must not require undeclared ripgrep' >&2
      return 127
    }
    export -f rg
    bash scripts/check-dist-workflow.sh
  ); then
    fail=1
  fi

  if ! bash scripts/package-nightly-harness.sh; then
    fail=1
  fi

  if ! bash scripts/prepare-nightly-harness.sh; then
    fail=1
  fi

  if ! bash scripts/advance-nightly-branch-harness.sh; then
    fail=1
  fi
else
  echo 'source-checks: Node is required for release policy validation' >&2
  fail=1
fi

if [ "$run_clippy" = "1" ]; then
  echo "source-checks: cargo clippy --locked --all-targets --all-features -- -D warnings"
  if ! cargo clippy --locked --all-targets --all-features -- -D warnings; then
    echo "source-checks: clippy warnings are findings" >&2
    fail=1
  fi
fi

if [ "$fail" -ne 0 ]; then
  echo "source-checks: failed" >&2
  exit 1
fi

echo "source-checks: ok"
