#!/usr/bin/env bash
# Mechanical nits that should fail before a human or AI review round.
# Clippy fails only on added/edited Rust lines so baseline warnings elsewhere
# are not hidden and are not required drive-by cleanups.
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

if ! node scripts/docs_prose_check.js "${docs_files[@]}"; then
  fail=1
fi

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

if [ "$run_clippy" = "1" ]; then
  clippy_diff="$(mktemp)"
  {
    if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ] && [ -n "${GITHUB_BASE_REF:-}" ]; then
      git diff -U0 "origin/${GITHUB_BASE_REF}...HEAD" -- "*.rs" || true
    elif git rev-parse --verify origin/main >/dev/null 2>&1; then
      git diff -U0 origin/main...HEAD -- "*.rs" || true
    fi
    git diff -U0 -- "*.rs" || true
    git diff -U0 --cached -- "*.rs" || true
  } >"$clippy_diff"
  if grep -q '^+++ ' "$clippy_diff"; then
    echo "source-checks: clippy on added or edited Rust lines"
    clippy_json="$(mktemp)"
    if cargo clippy --locked --all-targets --message-format=json >"$clippy_json"; then
      if ! node scripts/filter_clippy_changed.js "$clippy_diff" <"$clippy_json"; then
        echo "source-checks: clippy warnings on changed lines are findings" >&2
        fail=1
      fi
    else
      echo "source-checks: clippy failed to run" >&2
      fail=1
    fi
    rm -f "$clippy_json"
  fi
  rm -f "$clippy_diff"
fi

if [ "$fail" -ne 0 ]; then
  echo "source-checks: failed" >&2
  exit 1
fi

echo "source-checks: ok"
