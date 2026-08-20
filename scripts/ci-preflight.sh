#!/usr/bin/env bash
# Run every required Linux CI gate locally. The Windows-only job is intentionally
# excluded because it requires a Windows host and NASM.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

usage() {
  cat <<'EOF'
Usage: bash scripts/ci-preflight.sh [--base <git-revision>]

Run the required local preflight for a commit, PR submission, or PR update.
By default, the PR base is origin/main. Pass --base origin/<branch> for a PR
whose base is not main.
EOF
}

base_ref="origin/main"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --base)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      base_ref="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
done

if ! git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null; then
  echo "ci-preflight: base revision not found: $base_ref" >&2
  echo "ci-preflight: fetch it or pass --base <git-revision>" >&2
  exit 2
fi

require_command() {
  local command="$1"
  local install="$2"
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "ci-preflight: missing $command; install it with: $install" >&2
    exit 2
  fi
}

require_command typos 'cargo install typos-cli --locked'
require_command cargo-mutants 'cargo install cargo-mutants --locked'
require_command cargo-deny 'cargo install cargo-deny --locked'
require_command cargo-audit 'cargo install cargo-audit --locked'

echo 'ci-preflight: cargo update --workspace --locked'
cargo update --workspace --locked

echo 'ci-preflight: typos'
typos

echo 'ci-preflight: SOURCE_CHECKS_SKIP_TYPOS=1 bash scripts/source-checks.sh'
SOURCE_CHECKS_SKIP_TYPOS=1 bash scripts/source-checks.sh

echo 'ci-preflight: cargo test --locked'
cargo test --locked

echo 'ci-preflight: cargo build --locked'
cargo build --locked

echo "ci-preflight: RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps"
RUSTDOCFLAGS='-D warnings' cargo doc --locked --no-deps

echo 'ci-preflight: target/debug/codex-warp --version'
target/debug/codex-warp --version

echo 'ci-preflight: target/debug/codex-warp --help'
target/debug/codex-warp --help >/dev/null

echo "ci-preflight: git diff --check $base_ref"
git diff --check "$base_ref"

if git ls-files --others --exclude-standard -- '*.rs' | grep -q .; then
  echo 'ci-preflight: stage or remove untracked Rust files before running mutation checks' >&2
  exit 2
fi

mutants_diff="$(mktemp)"
mutants_output_dir="$(mktemp -d)"
trap 'rm -f "$mutants_diff"; rm -rf "$mutants_output_dir"' EXIT
git diff "$base_ref" -- '*.rs' >"$mutants_diff"
if [ -s "$mutants_diff" ]; then
  echo "ci-preflight: cargo mutants -o $mutants_output_dir --no-shuffle -vV --in-diff $mutants_diff -- --locked"
  cargo mutants -o "$mutants_output_dir" --no-shuffle -vV --in-diff "$mutants_diff" -- --locked
else
  echo 'ci-preflight: no Rust diff; skipping cargo mutants (matches CI)'
fi

echo 'ci-preflight: cargo deny check bans licenses sources'
cargo deny check bans licenses sources

echo 'ci-preflight: cargo audit (non-blocking, matches CI)'
if ! cargo audit; then
  echo 'ci-preflight: cargo audit reported advisories; CI records these as non-blocking' >&2
fi

echo 'ci-preflight: all required local Linux checks passed'
