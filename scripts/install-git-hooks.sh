#!/usr/bin/env bash
# Enable the versioned local preflight hooks for this checkout.
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

git -C "$root" config core.hooksPath .githooks
git -C "$root" config codex-warp.preflight-base "$base_ref"
echo "Installed .githooks with preflight base $base_ref for this checkout."
