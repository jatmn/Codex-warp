#!/usr/bin/env bash
# Exercise hook tree selection and Git's multi-ref pre-push protocol without
# running the full project preflight.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
repo="$(mktemp -d)"
cleanup() {
  rm -rf "$repo"
}
trap cleanup EXIT

git -C "$repo" init --quiet
git -C "$repo" config user.name hook-harness
git -C "$repo" config user.email hook-harness@example.invalid
mkdir -p "$repo/scripts" "$repo/empty-hooks"
git -C "$repo" config core.hooksPath "$repo/empty-hooks"
cp "$root/scripts/run-preflight-hook.sh" "$repo/scripts/"
cp "$root/scripts/install-git-hooks.sh" "$repo/scripts/"

cat >"$repo/scripts/ci-preflight.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
test "${1:-}" = --base
test "${2:-}" = HEAD
case "$(cat validation-target.txt)" in
  unborn|index) ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$repo/scripts/ci-preflight.sh"

printf 'unborn\n' >"$repo/validation-target.txt"
git -C "$repo" add scripts validation-target.txt
(cd "$repo" && bash scripts/run-preflight-hook.sh --index --base HEAD)
printf 'base\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
git -C "$repo" commit --quiet -m initial
legacy_branch="$(git -C "$repo" branch --show-current)"
git -C "$repo" switch --quiet -c protected
mkdir -p "$repo/.githooks"
cp "$root/.githooks/pre-commit" "$repo/.githooks/"
cp "$root/.githooks/pre-push" "$repo/.githooks/"
git -C "$repo" add .githooks
git -C "$repo" commit --quiet -m hooks
(
  cd "$repo"
  bash scripts/install-git-hooks.sh --base HEAD
)
hooks_dir="$(git -C "$repo" config --worktree --get core.hooksPath)"
test -f "$hooks_dir/pre-commit"
test -f "$hooks_dir/pre-push"
printf 'index\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
printf 'worktree\n' >"$repo/validation-target.txt"
git -C "$repo" commit --quiet -m index-branch
test "$(git -C "$repo" show HEAD:validation-target.txt)" = index

cat >"$repo/scripts/run-preflight-hook.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$2" >>"$HOOK_CALLS"
EOF
chmod +x "$repo/scripts/run-preflight-hook.sh"
git -C "$repo" config --worktree core.hooksPath "$repo/empty-hooks"
first_sha="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" switch --quiet -c second
printf 'second\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
git -C "$repo" commit --quiet -m second-branch
second_sha="$(git -C "$repo" rev-parse HEAD)"
calls="$repo/hook-calls"
export HOOK_CALLS="$calls"
printf 'refs/heads/first %s refs/heads/first 0000000000000000000000000000000000000000\nrefs/heads/second %s refs/heads/second 0000000000000000000000000000000000000000\n' "$first_sha" "$second_sha" |
  (cd "$repo" && bash "$hooks_dir/pre-push")
test "$(sed -n '1p' "$calls")" = "$first_sha"
test "$(sed -n '2p' "$calls")" = "$second_sha"
test "$(wc -l <"$calls")" -eq 2

git -C "$repo" checkout -- scripts/run-preflight-hook.sh
git -C "$repo" config --worktree core.hooksPath "$hooks_dir"
git -C "$repo" switch --quiet "$legacy_branch"
legacy_head="$(git -C "$repo" rev-parse HEAD)"
printf 'legacy\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
if git -C "$repo" commit --quiet -m legacy; then
  echo 'git-hooks-harness: legacy branch commit bypassed the installed preflight hook' >&2
  exit 1
fi
test "$(git -C "$repo" rev-parse HEAD)" = "$legacy_head"

echo 'git-hooks-harness: ok'
