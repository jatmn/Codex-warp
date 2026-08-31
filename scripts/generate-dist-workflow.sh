#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
temp="$(mktemp -d)"
config_backup="$temp/dist-workspace.toml"
cp dist-workspace.toml "$config_backup"
restore() {
  cp "$config_backup" dist-workspace.toml
  rm -rf "$temp"
}
trap restore EXIT

bash scripts/install-pinned-dist.sh --dest "$temp/dist"
# allow-dirty protects the reviewed overlay during ordinary dist commands. Drop
# only that exact line while emitting the pristine template, then restore it.
awk '$0 != "allow-dirty = [\"ci\"]"' "$config_backup" >"$temp/dist-workspace.next.toml"
cp "$temp/dist-workspace.next.toml" dist-workspace.toml
"$temp/dist" generate --mode=ci
cp "$config_backup" dist-workspace.toml
node tools/release-please-policy/patch-dist-workflow.mjs .github/workflows/release.yml
echo 'generate-dist-workflow: generated and applied the release overlay'
