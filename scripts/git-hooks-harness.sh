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
mkdir -p "$repo/scripts" "$repo/empty-hooks" "$repo/custom-hooks"
git -C "$repo" config core.hooksPath "$repo/empty-hooks"
cp "$root/scripts/run-preflight-hook.sh" "$repo/scripts/"
cp "$root/scripts/git-hook-bootstrap.sh" "$repo/scripts/"
cp "$root/scripts/install-git-hooks.sh" "$repo/scripts/"

cat >"$repo/scripts/ci-preflight.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
test "${1:-}" = --base
test "${2:-}" = HEAD
if [ -n "${HOOK_MARKER:-}" ]; then
  printf '%s\n' "$(git rev-parse HEAD)" >>"$HOOK_MARKER"
fi
case "$(cat validation-target.txt)" in
  unborn|index|staged) ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$repo/scripts/ci-preflight.sh"

printf 'unborn\n' >"$repo/validation-target.txt"
git -C "$repo" add scripts validation-target.txt
if git -C "$repo" fsck --no-reflogs --unreachable 2>/dev/null | grep -Eq 'unreachable (commit|tree)'; then
  echo 'git-hooks-harness: fixture unexpectedly contains an unreachable snapshot object' >&2
  exit 1
fi
(cd "$repo" && bash scripts/run-preflight-hook.sh --index --base HEAD)
if git -C "$repo" fsck --no-reflogs --unreachable 2>/dev/null | grep -Eq 'unreachable (commit|tree)'; then
  echo 'git-hooks-harness: index snapshot left an unreachable object' >&2
  exit 1
fi
printf 'base\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
git -C "$repo" commit --quiet -m initial
legacy_branch="$(git -C "$repo" branch --show-current)"
git -C "$repo" switch --quiet -c protected
mkdir -p "$repo/.githooks"
cp "$root/.githooks/pre-commit" "$repo/.githooks/"
cp "$root/.githooks/pre-merge-commit" "$repo/.githooks/"
cp "$root/.githooks/pre-applypatch" "$repo/.githooks/"
cp "$root/.githooks/pre-push" "$repo/.githooks/"
git -C "$repo" add .githooks
git -C "$repo" commit --quiet -m hooks

# Migrate the PR's original installer, which used .githooks directly. The
# durable dispatcher must not invoke that same versioned hook twice.
git -C "$repo" config --worktree core.hooksPath .githooks
git -C "$repo" config --worktree codex-warp.preflight-base HEAD
(
  cd "$repo"
  bash scripts/install-git-hooks.sh --base HEAD
)
hooks_dir="$(git -C "$repo" config --worktree --get core.hooksPath)"
test "$(git -C "$repo" config --worktree --get codex-warp.previous-hooks-path)" = "$repo/.githooks"
legacy_calls="$repo/legacy-hook-calls"
export HOOK_MARKER="$legacy_calls"
printf 'index\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
git -C "$repo" commit --quiet -m legacy-hook-migration
test "$(wc -l <"$legacy_calls")" -eq 1
: >"$legacy_calls"
legacy_sha="$(git -C "$repo" rev-parse HEAD)"
printf 'refs/heads/protected %s refs/heads/protected 0000000000000000000000000000000000000000\n' "$legacy_sha" |
  (cd "$repo" && bash "$hooks_dir/pre-push")
test "$(wc -l <"$legacy_calls")" -eq 1
unset HOOK_MARKER

# Switch to an unrelated existing hook directory to verify that a fresh
# installation chains users' hooks, not just the old Codex Warp path.
git -C "$repo" config --worktree core.hooksPath "$repo/custom-hooks"
git -C "$repo" config --worktree --unset codex-warp.previous-hooks-path
cat >"$repo/custom-hooks/pre-commit" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'pre-commit\n' >>"$CUSTOM_HOOK_CALLS"
EOF
cat >"$repo/custom-hooks/commit-msg" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'commit-msg\n' >>"$CUSTOM_HOOK_CALLS"
EOF
cat >"$repo/custom-hooks/pre-push" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
cat >>"$CUSTOM_PUSH_CALLS"
EOF
chmod +x "$repo/custom-hooks/pre-commit" "$repo/custom-hooks/commit-msg" "$repo/custom-hooks/pre-push"
git -C "$repo" config core.hooksPath "$repo/custom-hooks"
custom_calls="$repo/custom-hook-calls"
export CUSTOM_HOOK_CALLS="$custom_calls"
(
  cd "$repo"
  bash scripts/install-git-hooks.sh --base HEAD
)
hooks_dir="$(git -C "$repo" config --worktree --get core.hooksPath)"
test "$(git -C "$repo" config --worktree --get codex-warp.previous-hooks-path)" = "$repo/custom-hooks"
for hook_name in pre-commit pre-merge-commit pre-applypatch pre-push commit-msg; do
  test -f "$hooks_dir/$hook_name"
done
# Reinstallation must retain the originally active hook path rather than
# treating the durable dispatcher directory as the hook chain to preserve.
(
  cd "$repo"
  bash scripts/install-git-hooks.sh --base HEAD
)
test "$(git -C "$repo" config --worktree --get codex-warp.previous-hooks-path)" = "$repo/custom-hooks"
printf 'staged\n' >"$repo/validation-target.txt"
git -C "$repo" add validation-target.txt
printf 'worktree\n' >"$repo/validation-target.txt"
git -C "$repo" commit --quiet -m index-branch
test "$(git -C "$repo" show HEAD:validation-target.txt)" = staged
test "$(sed -n '1p' "$custom_calls")" = pre-commit
test "$(sed -n '2p' "$custom_calls")" = commit-msg

git -C "$repo" switch --quiet -c merge-source
printf 'merge source\n' >"$repo/merge-source.txt"
git -C "$repo" add merge-source.txt
git -C "$repo" commit --quiet -m merge-source
git -C "$repo" switch --quiet protected
printf 'protected\n' >"$repo/protected.txt"
git -C "$repo" add protected.txt
git -C "$repo" commit --quiet -m protected
merge_calls="$repo/merge-hook-calls"
export HOOK_MARKER="$merge_calls"
git -C "$repo" merge --no-ff --no-edit merge-source
test -s "$merge_calls"
unset HOOK_MARKER

git -C "$repo" switch --quiet -c mail-source
printf 'mail source\n' >"$repo/mail-source.txt"
git -C "$repo" add mail-source.txt
git -C "$repo" commit --quiet -m mail-source
git -C "$repo" format-patch -1 --stdout >"$repo/mail.patch"
git -C "$repo" switch --quiet protected
applypatch_calls="$repo/applypatch-hook-calls"
export HOOK_MARKER="$applypatch_calls"
git -C "$repo" am "$repo/mail.patch"
test -s "$applypatch_calls"
unset HOOK_MARKER

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
custom_push_calls="$repo/custom-push-calls"
export CUSTOM_PUSH_CALLS="$custom_push_calls"
printf 'refs/heads/first %s refs/heads/first 0000000000000000000000000000000000000000\nrefs/heads/second %s refs/heads/second 0000000000000000000000000000000000000000\n' "$first_sha" "$second_sha" |
  (cd "$repo" && bash "$hooks_dir/pre-push")
test "$(sed -n '1p' "$calls")" = "$first_sha"
test "$(sed -n '2p' "$calls")" = "$second_sha"
test "$(wc -l <"$calls")" -eq 2
test "$(wc -l <"$custom_push_calls")" -eq 2
unset CUSTOM_PUSH_CALLS

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

# Linked worktrees share the common .git/hooks directory, rather than using a
# hooks directory below their per-worktree Git metadata.
git -C "$repo" config --worktree --unset-all core.hooksPath || true
git -C "$repo" config --local --unset-all core.hooksPath || true
mkdir -p "$repo/.git/hooks"
cat >"$repo/.git/hooks/pre-commit" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'pre-commit\n' >>"$DEFAULT_HOOK_CALLS"
EOF
cat >"$repo/.git/hooks/commit-msg" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf 'commit-msg\n' >>"$DEFAULT_HOOK_CALLS"
EOF
chmod +x "$repo/.git/hooks/pre-commit" "$repo/.git/hooks/commit-msg"
linked_worktree="$repo/linked-worktree"
git -C "$repo" worktree add --detach --quiet "$linked_worktree" second
(
  cd "$linked_worktree"
  bash scripts/install-git-hooks.sh --base HEAD
)
test "$(git -C "$linked_worktree" config --worktree --get codex-warp.previous-hooks-path)" = "$repo/.git/hooks"
default_calls="$repo/default-hook-calls"
export DEFAULT_HOOK_CALLS="$default_calls"
printf 'staged\n' >"$linked_worktree/validation-target.txt"
git -C "$linked_worktree" add validation-target.txt
git -C "$linked_worktree" commit --quiet -m linked-default-hooks
test "$(sed -n '1p' "$default_calls")" = pre-commit
test "$(sed -n '2p' "$default_calls")" = commit-msg
unset DEFAULT_HOOK_CALLS

echo 'git-hooks-harness: ok'
