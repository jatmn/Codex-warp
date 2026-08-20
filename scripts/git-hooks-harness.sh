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
mkdir -p "$repo/scripts" "$repo/.githooks"
cp "$root/scripts/run-preflight-hook.sh" "$repo/scripts/"

cat >"$repo/scripts/ci-preflight.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
test "$(cat validation-target.txt)" = index
EOF
chmod +x "$repo/scripts/ci-preflight.sh"

printf 'base\n' >"$repo/validation-target.txt"
git -C "$repo" add scripts validation-target.txt
git -C "$repo" commit --quiet -m initial
printf 'index\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
printf 'worktree\n' >"$repo/validation-target.txt"
(cd "$repo" && bash scripts/run-preflight-hook.sh --index --base HEAD)

cp "$root/.githooks/pre-push" "$repo/.githooks/"
cat >"$repo/scripts/run-preflight-hook.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$2" >>"$HOOK_CALLS"
EOF
chmod +x "$repo/scripts/run-preflight-hook.sh"
git -C "$repo" add validation-target.txt
git -C "$repo" commit --quiet -m first-branch
first_sha="$(git -C "$repo" rev-parse HEAD)"
git -C "$repo" switch --quiet -c second
printf 'second\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
git -C "$repo" commit --quiet -m second-branch
second_sha="$(git -C "$repo" rev-parse HEAD)"
calls="$repo/hook-calls"
export HOOK_CALLS="$calls"
printf 'refs/heads/first %s refs/heads/first 0000000000000000000000000000000000000000\nrefs/heads/second %s refs/heads/second 0000000000000000000000000000000000000000\n' "$first_sha" "$second_sha" |
  (cd "$repo" && bash .githooks/pre-push)
test "$(sed -n '1p' "$calls")" = "$first_sha"
test "$(sed -n '2p' "$calls")" = "$second_sha"
test "$(wc -l <"$calls")" -eq 2

echo 'git-hooks-harness: ok'
