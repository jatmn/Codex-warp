#!/usr/bin/env bash
# Enable the versioned local preflight hooks for this checkout.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
git -C "$root" config core.hooksPath .githooks
echo 'Installed .githooks as this checkout’s Git hooks path.'
