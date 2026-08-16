#!/usr/bin/env bash
# Mechanical nits that should fail before a human or AI review round.
# Clippy is crate-wide: `cargo clippy --all-targets -- -D warnings`.
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

if [ "$skip_typos" != "1" ]; then
  if ! command -v typos >/dev/null 2>&1; then
    echo "source-checks: install typos with: cargo install typos-cli --locked" >&2
    fail=1
  elif ! typos; then
    fail=1
  fi
fi

docs_files=(README.md AGENTS.md CONTRIBUTING.md SECURITY.md docs)
if grep -RInE '[[:blank:]]$' "${docs_files[@]}"; then
  echo "source-checks: trailing whitespace in docs" >&2
  fail=1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "source-checks: node is required for docs prose, JS syntax, and the chart harness" >&2
  fail=1
elif ! node scripts/docs_prose_check.js "${docs_files[@]}"; then
  fail=1
fi

if command -v node >/dev/null 2>&1; then
  js_files=(
    src/webui_static/theme-bootstrap.js
    src/webui_static/chart-math.js
    src/webui_static/footer-status.js
    src/webui_static/app-main.js
  )
  for js in "${js_files[@]}"; do
    if ! node --check "$js"; then
      fail=1
    fi
  done

  if ! node scripts/webui_chart_harness.js; then
    fail=1
  fi
fi

if [ "$run_clippy" = "1" ]; then
  echo "source-checks: clippy --all-targets -D warnings"
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
