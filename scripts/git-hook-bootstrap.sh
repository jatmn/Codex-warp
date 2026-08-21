#!/usr/bin/env bash
# Durable Git hook entry point that dispatches to the checked-out implementation.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
hook_name="$(basename "$0")"
versioned_hook="$root/.githooks/$hook_name"
if [ ! -f "$versioned_hook" ]; then
  echo "codex-warp preflight: this branch does not provide .githooks/$hook_name; switch to a supported branch or reinstall the hooks." >&2
  exit 1
fi
exec bash "$versioned_hook" "$@"
