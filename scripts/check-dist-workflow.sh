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
  grep -F 'bash scripts/lookup-official-draft.sh' "$workflow" >/dev/null
  if grep -F '/releases/tags/$TAG' "$workflow" >/dev/null; then
    echo 'check-dist-workflow: official prepare must not look up drafts by tag endpoint' >&2
    exit 1
  fi
  grep -F 'Upload only missing verified assets' "$workflow" >/dev/null
  grep -F 'Verify complete remote checksums' "$workflow" >/dev/null
  if ! awk '
    $0 ~ /name: Verify complete remote checksums/ {in_step=1}
    in_step && /^      - / && $0 !~ /name: Verify complete remote checksums/ {in_step=0}
    in_step && /steps\.app-token\.outputs\.token/ {found=1}
    in_step && /github\.token/ {bad=1}
    END {exit (found && !bad) ? 0 : 1}
  ' "$workflow"; then
    echo 'check-dist-workflow: official remote checksum verify must use the App token' >&2
    exit 1
  fi
  grep -F 'Publish exact verified draft' "$workflow" >/dev/null
  grep -F 'prepare-pr-upload-proof:' "$workflow" >/dev/null
  grep -F 'bash scripts/assemble-pr-upload-proof.sh target/distrib identity.json pr-upload-proof' "$workflow" >/dev/null
  grep -F 'bash scripts/assemble-official-candidate.sh target/distrib identity.json dist-manifest.json release-assets' "$workflow" >/dev/null
  if awk '
    $0 ~ /name: Assemble and validate exact asset set/ {in_step=1}
    in_step && /^      - / && $0 !~ /name: Assemble and validate exact asset set/ {in_step=0}
    in_step && /upload_files/ {found=1}
    END {exit found ? 0 : 1}
  ' "$workflow"; then
    echo 'check-dist-workflow: official assemble must not copy dist host upload_files' >&2
    exit 1
  fi
  grep -F 'attest-pr-upload-proof-metadata:' "$workflow" >/dev/null
  grep -F "github.event.pull_request.head.repo.full_name == github.repository && fromJson(needs.plan.outputs.val).ci.github.pr_run_mode == 'upload'" "$workflow" >/dev/null
  grep -F 'name: Build native archives' "$workflow" >/dev/null
  if grep -F "name: build-local-artifacts (\${{ join(matrix.targets, ', ') }})" "$workflow" >/dev/null; then
    echo 'check-dist-workflow: skipped matrix jobs would publish an unevaluated check name' >&2
    exit 1
  fi
  if grep -E 'cargo-dist-installer\.(sh|ps1)' "$workflow" >/dev/null ||
     grep -E '^[[:space:]]+contents:[[:space:]]+write[[:space:]]*$' "$workflow" >/dev/null; then
    echo 'check-dist-workflow: unsafe installer or GITHUB_TOKEN write permission returned' >&2
    exit 1
  fi
}

assert_safe_overlay "$checked"
unsafe="$temp/unsafe-release.yml"
awk '
  !changed && /^      contents: read$/ { sub(/read$/, "write"); changed = 1 }
  { print }
  END { if (!changed) exit 1 }
' "$checked" >"$unsafe"
if node "$temp/tools/release-please-policy/patch-dist-workflow.mjs" "$unsafe" >/dev/null 2>&1; then
  echo 'check-dist-workflow: overlay validator accepted a job-level contents:write grant' >&2
  exit 1
fi

current_mode="$(sed -n 's/^pr-run-mode = "\(plan\|upload\)"$/\1/p' "$temp/dist-workspace.toml")"
[ "$current_mode" = plan ] || {
  echo 'check-dist-workflow: checked-in pr-run-mode must be steady-state plan' >&2
  exit 1
}
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
