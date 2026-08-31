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
  npm ci --omit=dev --ignore-scripts --no-audit --no-fund >/dev/null
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

assert_safe_overlay() {
  local workflow="$1"
  grep -F "'v[0-9]+.[0-9]+.[0-9]+'" "$workflow" >/dev/null
  grep -F 'queue: max' "$workflow" >/dev/null
  grep -F 'bash scripts/install-pinned-dist.sh' "$workflow" >/dev/null
  grep -F 'Upload only missing verified assets' "$workflow" >/dev/null
  grep -F 'Verify complete remote checksums' "$workflow" >/dev/null
  grep -F 'Publish exact verified draft' "$workflow" >/dev/null
  grep -F 'prepare-pr-upload-proof:' "$workflow" >/dev/null
  grep -F 'bash scripts/assemble-pr-upload-proof.sh target/distrib identity.json pr-upload-proof' "$workflow" >/dev/null
  grep -F 'attest-pr-upload-proof-metadata:' "$workflow" >/dev/null
  if grep -E 'cargo-dist-installer\.(sh|ps1)' "$workflow" >/dev/null ||
     awk '
       previous_permissions && /^[[:space:]]+contents:[[:space:]]+write([[:space:]]|$)/ { found = 1 }
       { previous_permissions = ($0 ~ /^[[:space:]]*permissions:[[:space:]]*$/) }
       END { exit found ? 0 : 1 }
     ' "$workflow"; then
    echo 'check-dist-workflow: unsafe installer or GITHUB_TOKEN write permission returned' >&2
    exit 1
  fi
}

assert_safe_overlay "$checked"

current_mode="$(sed -n 's/^pr-run-mode = "\(plan\|upload\)"$/\1/p' "$temp/dist-workspace.toml")"
case "$current_mode" in
  plan) alternate_mode='upload' ;;
  upload) alternate_mode='plan' ;;
  *) echo 'check-dist-workflow: expected exactly one supported pr-run-mode' >&2; exit 1 ;;
esac
awk -v mode="$alternate_mode" '
  /^pr-run-mode = "(plan|upload)"$/ { print "pr-run-mode = \"" mode "\""; next }
  { print }
' "$temp/dist-workspace.toml" >"$temp/dist-workspace.next.toml"
mv "$temp/dist-workspace.next.toml" "$temp/dist-workspace.toml"
(
  cd "$temp"
  bash scripts/generate-dist-workflow.sh >/dev/null
)
assert_safe_overlay "$temp/.github/workflows/release.yml"
if [ "$current_mode" = upload ]; then
  upload_workflow="$checked"
else
  upload_workflow="$temp/.github/workflows/release.yml"
fi
grep -F 'uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6' \
  "$upload_workflow" >/dev/null

echo "check-dist-workflow: generated $current_mode overlay is current; $alternate_mode overlay is safe"
