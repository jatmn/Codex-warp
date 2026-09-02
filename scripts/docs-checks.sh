#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$root" ]; then
  root="$(cd "$(dirname "$0")/.." && pwd)"
fi
cd "$root"

skip_typos="${DOCS_CHECKS_SKIP_TYPOS:-0}"
fail=0

if [ "$skip_typos" != "1" ]; then
  if ! command -v typos >/dev/null 2>&1; then
    echo "docs-checks: install typos with: cargo install typos-cli --locked" >&2
    fail=1
  elif ! typos . .github/pull_request_template.md; then
    fail=1
  fi
fi

docs_files=(README.md AGENTS.md CONTRIBUTING.md SECURITY.md docs .github/pull_request_template.md)
trailing_whitespace=0
while IFS= read -r -d '' doc; do
  if grep -nIHE '[[:blank:]]$' "$doc"; then
    trailing_whitespace=1
  fi
done < <(find -P "${docs_files[@]}" -type f -print0)
if [ "$trailing_whitespace" -ne 0 ]; then
  echo "docs-checks: trailing whitespace in docs" >&2
  fail=1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "docs-checks: node is required for docs prose checks" >&2
  fail=1
elif ! node scripts/docs_prose_check.js "${docs_files[@]}"; then
  fail=1
fi

prose_fixture="$(mktemp -d "$root/.docs-prose-check.XXXXXX")"
trap 'rm -rf "$prose_fixture"' EXIT
touch "$prose_fixture/unsafe name.md"
if node scripts/docs_prose_check.js "${prose_fixture#"$root/"}" >/dev/null 2>&1; then
  echo "docs-checks: prose checker accepted an unsafe child filename" >&2
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  echo "docs-checks: failed" >&2
  exit 1
fi

echo "docs-checks: ok"
