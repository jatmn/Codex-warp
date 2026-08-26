#!/usr/bin/env bash
# Reject Python and other new implementation languages before review.
# Allowed tree today: Rust, TOML, Markdown, shell, JavaScript, HTML, CSS, YAML.
set -euo pipefail

is_forbidden() {
  local path="$1"
  local base="${path##*/}"
  case "$path" in
    *.py | *.pyi | *.pyw | *.pyc) return 0 ;;
    *.go | *.rb | *.php | *.java | *.kt | *.kts | *.scala | *.cs | *.swift | *.ts | *.tsx | *.jsx | *.vue)
      return 0
      ;;
  esac
  case "$base" in
    requirements.txt | Pipfile | poetry.lock | pyproject.toml | setup.py | setup.cfg | tox.ini | conftest.py)
      return 0
      ;;
  esac
  return 1
}

self_test() {
  local fail=0
  expect_forbidden() {
    if ! is_forbidden "$1"; then
      echo "language-policy-check: expected forbidden: $1" >&2
      fail=1
    fi
  }
  expect_allowed() {
    if is_forbidden "$1"; then
      echo "language-policy-check: expected allowed: $1" >&2
      fail=1
    fi
  }
  expect_forbidden "scripts/foo.py"
  expect_forbidden "src/conftest.py"
  expect_forbidden "pyproject.toml"
  expect_forbidden "src/app.ts"
  expect_forbidden "tools/helper.go"
  expect_allowed "src/main.rs"
  expect_allowed "scripts/source-checks.sh"
  expect_allowed "src/webui_static/app-main.js"
  expect_allowed "codex-warp.toml"
  expect_allowed ".github/workflows/ci.yml"
  expect_allowed "docs/development.md"
  if [ "$fail" -ne 0 ]; then
    echo "language-policy-check: self-test failed" >&2
    exit 1
  fi
}

self_test

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$root" ]; then
  echo "language-policy-check: must run inside the git work tree" >&2
  exit 1
fi
cd "$root"

fail=0
while IFS= read -r path; do
  [ -n "$path" ] || continue
  if is_forbidden "$path"; then
    echo "language-policy-check: forbidden implementation language: $path" >&2
    fail=1
  fi
done < <(git ls-files)

if [ "$fail" -ne 0 ]; then
  echo "language-policy-check: Python and other new implementation languages are not allowed" >&2
  exit 1
fi

echo "language-policy-check: ok"
