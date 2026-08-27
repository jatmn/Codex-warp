#!/usr/bin/env bash
# Classify a Git diff for GitHub Actions without trusting filename conventions
# outside the repository's explicit documentation surface.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo 'Usage: scripts/ci-change-scope.sh <git-diff-range>' >&2
  exit 2
fi

root="$(git rev-parse --show-toplevel)"
cd "$root"

changed_paths="$(mktemp)"
cleanup() {
  rm -f "$changed_paths"
}
trap cleanup EXIT

# Disable rename detection so moving a source file into a documentation path
# still reports the removed source path and receives the full CI suite.
git diff --name-only --no-renames -z "$1" -- >"$changed_paths"

full_ci=false
supply_chain=false
while IFS= read -r -d '' path; do
  case "$path" in
    README.md | AGENTS.md | CONTRIBUTING.md | SECURITY.md | LICENSE | NOTICE | \
      docs/*.md | .github/pull_request_template.md)
      ;;
    *)
      full_ci=true
      ;;
  esac

  case "$path" in
    Cargo.toml | Cargo.lock | deny.toml | .github/workflows/supply-chain.yml)
      supply_chain=true
      ;;
  esac
done <"$changed_paths"

printf 'full_ci=%s\n' "$full_ci"
printf 'supply_chain=%s\n' "$supply_chain"
