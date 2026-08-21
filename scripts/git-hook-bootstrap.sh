#!/usr/bin/env bash
# Durable Git hook entry point that dispatches to the checked-out implementation.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
hook_name="$(basename "$0")"
previous_hooks_dir="$(git -C "$root" config --worktree --get codex-warp.previous-hooks-path || true)"
previous_hook="${previous_hooks_dir:+$previous_hooks_dir/$hook_name}"
versioned_hook=""
case "$hook_name" in
  pre-commit|pre-applypatch|pre-push)
    versioned_hook="$root/.githooks/$hook_name"
    ;;
esac

# The first version of this installer pointed core.hooksPath directly at
# .githooks. On migration that directory is both the old path and this
# checkout's versioned implementation, so dispatching it twice would run the
# preflight twice. Other hook names in that old directory remain chainable.
if [ -n "$versioned_hook" ] && [ "$previous_hook" = "$versioned_hook" ]; then
  previous_hook=""
fi

# pre-push receives its ref updates on stdin. Replay that input to both hook
# chains so a pre-existing hook cannot consume the data needed by preflight.
if [ "$hook_name" = pre-push ]; then
  input_file="$(mktemp)"
  trap 'rm -f "$input_file"' EXIT
  cat >"$input_file"
  if [ -n "$previous_hook" ] && [ -x "$previous_hook" ]; then
    "$previous_hook" "$@" <"$input_file"
  fi
  if [ -f "$versioned_hook" ]; then
    bash "$versioned_hook" "$@" <"$input_file"
    exit $?
  fi
else
  if [ -n "$previous_hook" ] && [ -x "$previous_hook" ]; then
    "$previous_hook" "$@"
  fi
  if [ -f "$versioned_hook" ]; then
    exec bash "$versioned_hook" "$@"
  fi
fi

if [ -n "$versioned_hook" ]; then
  echo "codex-warp preflight: this branch does not provide .githooks/$hook_name; switch to a supported branch or reinstall the hooks." >&2
  exit 1
fi
