#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
workspace="$root/tools/release-please-policy"

actual_node="$(node --version)"
if [ "$actual_node" != 'v24.20.0' ]; then
  echo "release-please-policy-harness: expected Node v24.20.0, found $actual_node" >&2
  exit 1
fi

(
  cd "$workspace"
  npm ci --ignore-scripts --no-audit --no-fund
  npm test
)
