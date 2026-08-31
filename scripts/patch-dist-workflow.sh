#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
workflow="${1:-.github/workflows/release.yml}"
node tools/release-please-policy/patch-dist-workflow.mjs "$workflow"
