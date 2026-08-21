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
common_git_dir="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)"
default_hooks_dir="$common_git_dir/hooks"
hooks_dir="$git_dir/codex-warp-hooks"
installed_hooks_path="$(git -C "$root" config --worktree --get core.hooksPath || true)"
previous_hooks_dir="$(git -C "$root" config --worktree --get codex-warp.previous-hooks-path || true)"

# Git permits only one hooks directory. Preserve the path that was active
# before this installer took ownership, so the durable dispatchers can chain
# a user's existing hooks instead of replacing them. The first durable
# bootstrap version did not save a previous path; migrate it to the default
# hook directory instead of treating its own dispatcher directory as prior.
if [ "$installed_hooks_path" = "$hooks_dir" ] && [ -z "$previous_hooks_dir" ]; then
  previous_hooks_dir="$default_hooks_dir"
  git -C "$root" config --worktree codex-warp.previous-hooks-path "$previous_hooks_dir"
elif [ "$installed_hooks_path" != "$hooks_dir" ]; then
  previous_hooks_path="$(git -C "$root" config --path --get core.hooksPath || true)"
  if [ -z "$previous_hooks_path" ]; then
    previous_hooks_dir="$default_hooks_dir"
  elif [[ "$previous_hooks_path" = /* ]]; then
    previous_hooks_dir="$previous_hooks_path"
  else
    previous_hooks_dir="$root/$previous_hooks_path"
  fi
  git -C "$root" config --worktree codex-warp.previous-hooks-path "$previous_hooks_dir"
fi

mkdir -p "$hooks_dir"
# Keep dispatchers for every current client-side hook name, not just the
# preflight hooks, because core.hooksPath replaces the entire hook directory.
hook_names=(
  applypatch-msg pre-applypatch post-applypatch pre-commit pre-merge-commit
  prepare-commit-msg commit-msg post-commit pre-rebase post-checkout post-merge
  pre-push post-rewrite pre-auto-gc sendemail-validate fsmonitor-watchman
  reference-transaction post-index-change p4-changelist p4-prepare-changelist
  p4-post-changelist p4-pre-submit
)
if [ -d "$previous_hooks_dir" ]; then
  for existing_hook in "$previous_hooks_dir"/*; do
    [ -f "$existing_hook" ] || continue
    hook_names+=("$(basename "$existing_hook")")
  done
fi
for hook_name in "${hook_names[@]}"; do
  cp "$root/scripts/git-hook-bootstrap.sh" "$hooks_dir/$hook_name"
  chmod 755 "$hooks_dir/$hook_name"
done
# This dispatcher keeps any pre-existing merge policy active. Its bootstrap
# intentionally has no versioned Codex Warp implementation because Git does
# not expose the second parent of an automatic merge while the hook runs.
git -C "$root" config --worktree core.hooksPath "$hooks_dir"
git -C "$root" config --worktree codex-warp.preflight-base "$base_ref"
echo "Installed durable preflight hooks with base $base_ref for this checkout; existing hooks remain chained from $previous_hooks_dir."
