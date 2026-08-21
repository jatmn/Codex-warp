#!/usr/bin/env bash
# Run the preflight from the exact Git tree a hook is about to record or publish.
set -euo pipefail

usage() {
  echo 'Usage: scripts/run-preflight-hook.sh (--index|--head <commit>) --base <git-revision>' >&2
  exit 2
}

mode=""
treeish=""
base_ref=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --index) mode=index ;;
    --head) shift; [ "$#" -gt 0 ] || usage; mode=head; treeish="$1" ;;
    --base) shift; [ "$#" -gt 0 ] || usage; base_ref="$1" ;;
    *) usage ;;
  esac
  shift
done
[ -n "$mode" ] && [ -n "$base_ref" ] || usage

root="$(git rev-parse --show-toplevel)"
object_dir="$(mktemp -d)"
object_alternates="$(git -C "$root" rev-parse --path-format=absolute --git-path objects)"
if [ "$mode" = index ]; then
  tree="$(GIT_OBJECT_DIRECTORY="$object_dir" GIT_ALTERNATE_OBJECT_DIRECTORIES="$object_alternates" git write-tree)"
  parent=()
  if git rev-parse --verify --quiet HEAD >/dev/null; then
    parent=(-p HEAD)
  fi
  treeish="$(printf 'preflight index snapshot\n' | GIT_OBJECT_DIRECTORY="$object_dir" GIT_ALTERNATE_OBJECT_DIRECTORIES="$object_alternates" git commit-tree "$tree" "${parent[@]}")"
fi

# Git hook invocations export the caller's index and worktree environment.
# The snapshot above intentionally consumes that index; the detached worktree
# below must not inherit it.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

worktree="$(mktemp -d)"
rmdir "$worktree"
cleanup() {
  GIT_OBJECT_DIRECTORY="$object_dir" GIT_ALTERNATE_OBJECT_DIRECTORIES="$object_alternates" \
    git -C "$root" worktree remove --force "$worktree" >/dev/null 2>&1 || true
  rm -rf "$object_dir"
}
trap cleanup EXIT
GIT_OBJECT_DIRECTORY="$object_dir" GIT_ALTERNATE_OBJECT_DIRECTORIES="$object_alternates" \
  git -C "$root" worktree add --detach --quiet "$worktree" "$treeish"
(
  cd "$worktree"
  export GIT_OBJECT_DIRECTORY="$object_dir"
  export GIT_ALTERNATE_OBJECT_DIRECTORIES="$object_alternates"
  bash scripts/ci-preflight.sh --base "$base_ref"
)
