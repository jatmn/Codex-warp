#!/usr/bin/env bash
# Reject Python and other new implementation languages before review.
# Allowlist matches the tracked tree: Rust, Markdown, shell, JavaScript,
# HTML, CSS, the TOML files this repo already uses, GitHub YAML, Cargo.lock,
# and a short list of release-control-plane and extensionless repo files.
#
# Invariant: forbidden-ecosystem markers are denied by basename before any
# suffix/path allowlist is consulted, except for the exact, reviewed Node lock
# file used to validate release policy. Suffix classes that the tree only uses
# in specific names or directories are not opened repo-wide (lockfiles are
# Cargo.lock plus that one Node lock; YAML is under .github/; TOML is the
# existing application/config set plus two exact release inputs).
set -euo pipefail

is_release_automation_file() {
  local path="$1"
  case "$path" in
    .release-please-manifest.json | \
    dist-workspace.toml | \
    release-please-config.json | \
    rust-toolchain.toml | \
    tools/dist-manifest.schema.json | \
    tools/dist-tool-digests.sha256 | \
    tools/nightly-manifest.schema.json | \
    tools/nightly-packaging-contract.txt | \
    tools/recovery-recipes/schema.json | \
    tools/recovery-recipes/schemas/d22b592c38c543131d3327c50deba1cfdd3a22e05ac20c9bff4f3e949b9f7f5f.json | \
    tools/release-automation-policy.json | \
    tools/release-automation-policy.schema.json | \
    tools/release-contract.json | \
    tools/release-metadata.schema.json | \
    tools/release-please-policy/config.schema.json | \
    tools/release-please-policy/fixtures/dist-manifest.official.json | \
    tools/release-please-policy/fixtures/metadata-identity.official.json | \
    tools/release-please-policy/fixtures/recovery.invalid-identity.json | \
    tools/release-please-policy/fixtures/recovery.invalid-schema.json | \
    tools/release-please-policy/fixtures/recovery.valid.json | \
    tools/release-please-policy/fixtures/version-policy.json | \
    tools/release-please-policy/harness.mjs | \
    tools/release-please-policy/package-lock.json | \
    tools/release-please-policy/package.json | \
    tools/release-please-policy/patch-dist-workflow.mjs | \
    tools/release-please-policy/validate-json.mjs | \
    tools/release-please-policy/validate-policy-documents.mjs | \
    tools/release-please-policy/validate-workflows.mjs | \
    tools/release-tooling.json)
      return 0
      ;;
  esac
  if [[ "$path" =~ ^tools/recovery-recipes/official-v[0-9]+\.[0-9]+\.[0-9]+\.json$ ]] ||
     [[ "$path" =~ ^tools/recovery-recipes/nightly-nightly-[0-9]{8}-[0-9a-f]{12}\.json$ ]] ||
     [[ "$path" =~ ^tools/recovery-recipes/schemas/[0-9a-f]{64}\.json$ ]]; then
    return 0
  fi
  return 1
}

# Return 0 if this path is an approved TOML location.
# Approved paths: Cargo.toml, _typos.toml, deny.toml, codex-warp.toml,
# configs/<file>.toml, configs/model-families/<file>.toml, and
# configs/tool-policies/<file>.toml.
# Bash `case` globs treat `*` as matching `/`, so strip known prefixes and
# reject any remaining slash instead of using configs/*.toml.
is_tracked_toml() {
  local path="$1"
  local rest
  case "$path" in
    Cargo.toml | _typos.toml | deny.toml | codex-warp.toml) return 0 ;;
  esac
  rest="${path#configs/}"
  [ "$rest" != "$path" ] || return 1
  case "$rest" in
    model-families/*) rest="${rest#model-families/}" ;;
    tool-policies/*) rest="${rest#tool-policies/}" ;;
  esac
  case "$rest" in
    */*) return 1 ;;
    *.toml) return 0 ;;
  esac
  return 1
}

is_forbidden() {
  local path="$1"
  local base="${path##*/}"

  if is_release_automation_file "$path"; then
    return 1
  fi

  # Python/conda/pixi project and lock files, including names that reuse
  # allowed suffixes (.toml, .lock, .yml).
  case "$base" in
    pyproject.toml | pixi.toml | hatch.toml | pdm.toml | uv.toml | \
    ruff.toml | poetry.toml | Pylock.toml | pylock.toml | setuptools.toml | \
    black.toml | mypy.toml | isort.toml | pytest.toml | \
    Pipfile | Pipfile.lock | poetry.lock | uv.lock | pdm.lock | \
    requirements.txt | requirements.yml | requirements.yaml | \
    environment.yml | environment.yaml | \
    conda-lock.yml | conda-lock.yaml | \
    setup.py | setup.cfg | tox.ini | conftest.py)
      return 0
      ;;
  esac

  case "$path" in
    *.rs | *.md | *.sh | *.js | *.html | *.css) return 1 ;;
    LICENSE | NOTICE | .gitignore | .github/CODEOWNERS | .githooks/pre-commit | .githooks/pre-push | .githooks/pre-applypatch)
      return 1
      ;;
  esac

  if is_tracked_toml "$path"; then
    return 1
  fi

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
  expect_forbidden "hatch.toml"
  expect_forbidden "pdm.toml"
  expect_forbidden "uv.toml"
  expect_forbidden "ruff.toml"
  expect_forbidden "Pylock.toml"
  expect_forbidden "tools/stray.toml"
  expect_forbidden "black.toml"
  expect_forbidden "configs/black.toml"
  expect_forbidden "configs/a/new.toml"
  expect_forbidden "configs/a/b/c.toml"
  expect_forbidden "configs/other-dir/foo.toml"
  expect_forbidden "configs/model-families/nested/x.toml"
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
  expect_forbidden "tools/package-lock.json"
  expect_forbidden "tools/release-please-policy/nested/package-lock.json"
  expect_forbidden "tools/release-please-policy/extra.mjs"
  expect_forbidden "tools/release-please-policy/fixtures/extra.json"
  expect_forbidden "tools/recovery-recipes/schemas/extra.json"
  expect_forbidden "tools/recovery-recipes/schemas/abc.json"
  expect_forbidden "tools/recovery-recipes/official-latest.json"
  expect_forbidden "tools/recovery-recipes/nightly-nightly-20260830-ABCDEF123456.json"
  expect_forbidden "tools/extra.json"
  expect_allowed "src/main.rs"
  expect_allowed "scripts/source-checks.sh"
  expect_allowed "src/webui_static/app-main.js"
  expect_allowed "codex-warp.toml"
  expect_allowed "configs/openrouter.toml"
  expect_allowed "configs/model-families/qwen.toml"
  expect_allowed "configs/tool-policies/github.toml"
  expect_allowed "deny.toml"
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
  expect_allowed ".release-please-manifest.json"
  expect_allowed "dist-workspace.toml"
  expect_allowed "release-please-config.json"
  expect_allowed "rust-toolchain.toml"
  expect_allowed "tools/dist-tool-digests.sha256"
  expect_allowed "tools/nightly-packaging-contract.txt"
  expect_allowed "tools/recovery-recipes/schema.json"
  expect_allowed "tools/recovery-recipes/official-v1.2.3.json"
  expect_allowed "tools/recovery-recipes/nightly-nightly-20260830-abcdef123456.json"
  expect_allowed "tools/recovery-recipes/schemas/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"
  expect_allowed "tools/release-please-policy/package-lock.json"
  expect_allowed "tools/release-please-policy/validate-workflows.mjs"
  expect_allowed "tools/release-tooling.json"
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
