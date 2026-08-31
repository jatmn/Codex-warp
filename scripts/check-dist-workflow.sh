#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
checked="$root/.github/workflows/release.yml"
[ -f "$checked" ] || { echo 'check-dist-workflow: release.yml is missing' >&2; exit 1; }
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
git ls-files --cached --others --exclude-standard -z |
  tar --null --files-from=- --create --file=- |
  tar --extract --file=- --directory="$temp"
git -C "$temp" init --quiet
mkdir -p "$temp/.git/empty-hooks"
git -C "$temp" config core.hooksPath .git/empty-hooks
git -C "$temp" config user.name dist-workflow-check
git -C "$temp" config user.email dist-workflow-check@example.invalid
git -C "$temp" add .
git -C "$temp" commit --quiet -m fixture
(
  cd "$temp/tools/release-please-policy"
  npm ci --ignore-scripts --no-audit --no-fund >/dev/null
)
(
  cd "$temp"
  bash scripts/generate-dist-workflow.sh >/dev/null
)
cmp "$checked" "$temp/.github/workflows/release.yml" >/dev/null || {
  echo 'check-dist-workflow: generated release workflow drifted; run bash scripts/generate-dist-workflow.sh' >&2
  diff -u "$checked" "$temp/.github/workflows/release.yml" || true
  exit 1
}

rg -F "'v[0-9]+.[0-9]+.[0-9]+'" "$checked" >/dev/null
rg -F 'queue: max' "$checked" >/dev/null
rg -F 'bash scripts/install-pinned-dist.sh' "$checked" >/dev/null
rg -F 'Upload only missing verified assets' "$checked" >/dev/null
rg -F 'Verify complete remote checksums' "$checked" >/dev/null
rg -F 'Publish exact verified draft' "$checked" >/dev/null
if rg 'cargo-dist-installer\.(sh|ps1)' "$checked" >/dev/null ||
   rg -U 'permissions:\n\s+contents: write' "$checked" >/dev/null; then
  echo 'check-dist-workflow: unsafe installer or GITHUB_TOKEN write permission returned' >&2
  exit 1
fi
echo 'check-dist-workflow: generated overlay is current and safe'
