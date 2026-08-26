#!/usr/bin/env bash
# Reject Python and other new implementation languages before review.
# Allowlist matches the tracked tree: Rust, TOML, Markdown, shell, JavaScript,
# HTML, CSS, GitHub YAML, Cargo.lock, and a short list of extensionless repo files.
#
# Invariant: forbidden-ecosystem markers are denied by basename before any
# suffix/path allowlist is consulted. Suffix classes that the tree only uses in
# one place are not opened repo-wide (lockfiles are Cargo.lock; YAML is under
# .github/). Suffix allowlist never overrides a forbidden basename.
set -euo pipefail

is_forbidden() {
  local path="$1"
  local base="${path##*/}"

  # Python/conda/pixi project and lock files, including names that reuse
  # allowed suffixes (.toml, .lock, .yml).
  case "$base" in
    pyproject.toml | pixi.toml | \
    Pipfile | Pipfile.lock | poetry.lock | uv.lock | pdm.lock | \
    requirements.txt | requirements.yml | requirements.yaml | \
    environment.yml | environment.yaml | \
    conda-lock.yml | conda-lock.yaml | \
    setup.py | setup.cfg | tox.ini | conftest.py)
      return 0
      ;;
  esac

  case "$path" in
    *.rs | *.toml | *.md | *.sh | *.js | *.html | *.css) return 1 ;;
    LICENSE | NOTICE | .gitignore | .github/CODEOWNERS | .githooks/pre-commit | .githooks/pre-push | .githooks/pre-applypatch)
      return 1
      ;;
  esac

  case "$base" in
    Cargo.lock) return 1 ;;
  esac

  case "$path" in
    .github/*)
      case "$path" in
        *.yml | *.yaml) return 1 ;;
      esac
      ;;
  esac

  return 0
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
  expect_forbidden "tools/pyproject.toml"
  expect_forbidden "pixi.toml"
  expect_forbidden "tools/pixi.toml"
  expect_forbidden "poetry.lock"
  expect_forbidden "Pipfile"
  expect_forbidden "Pipfile.lock"
  expect_forbidden "tools/Pipfile.lock"
  expect_forbidden "uv.lock"
  expect_forbidden "tools/uv.lock"
  expect_forbidden "pdm.lock"
  expect_forbidden "src/app.ts"
  expect_forbidden "tools/helper.go"
  expect_forbidden "src/tool.c"
  expect_forbidden "src/tool.cpp"
  expect_forbidden "src/tool"
  expect_forbidden "scripts/helper"
  expect_forbidden "requirements.txt"
  expect_forbidden "requirements.yml"
  expect_forbidden "docs/requirements.yaml"
  expect_forbidden "environment.yml"
  expect_forbidden "environment.yaml"
  expect_forbidden "conda-lock.yml"
  expect_forbidden "conda-lock.yaml"
  expect_forbidden "Gemfile.lock"
  expect_forbidden "package-lock.json"
  expect_allowed "src/main.rs"
  expect_allowed "scripts/source-checks.sh"
  expect_allowed "src/webui_static/app-main.js"
  expect_allowed "codex-warp.toml"
  expect_allowed ".github/workflows/ci.yml"
  expect_allowed ".github/dependabot.yml"
  expect_allowed "docs/development.md"
  expect_allowed "LICENSE"
  expect_allowed "NOTICE"
  expect_allowed ".gitignore"
  expect_allowed ".github/CODEOWNERS"
  expect_allowed ".githooks/pre-commit"
  expect_allowed "Cargo.lock"
  expect_allowed "vendor/Cargo.lock"
  expect_allowed "_typos.toml"
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
