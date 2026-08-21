#!/usr/bin/env bash
# Install durable hooks that dispatch to the versioned local preflight scripts.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
base_ref="origin/main"
if [ "${1:-}" = "--base" ]; then
  [ "$#" -eq 2 ] || {
    echo 'Usage: bash scripts/install-git-hooks.sh [--base origin/<base-branch>]' >&2
    exit 2
  }
  base_ref="$2"
elif [ "$#" -ne 0 ]; then
  echo 'Usage: bash scripts/install-git-hooks.sh [--base origin/<base-branch>]' >&2
  exit 2
fi

git -C "$root" config extensions.worktreeConfig true
git_dir="$(git -C "$root" rev-parse --absolute-git-dir)"
hooks_dir="$git_dir/codex-warp-hooks"
mkdir -p "$hooks_dir"
cp "$root/.githooks/pre-commit" "$hooks_dir/pre-commit"
cp "$root/.githooks/pre-push" "$hooks_dir/pre-push"
chmod 755 "$hooks_dir/pre-commit" "$hooks_dir/pre-push"
git -C "$root" config --worktree core.hooksPath "$hooks_dir"
git -C "$root" config --worktree codex-warp.preflight-base "$base_ref"
echo "Installed durable preflight hooks with base $base_ref for this checkout."
