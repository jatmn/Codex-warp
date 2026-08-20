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
if [ "$mode" = index ]; then
  tree="$(git write-tree)"
  parent=()
  if git rev-parse --verify --quiet HEAD >/dev/null; then
    parent=(-p HEAD)
  fi
  treeish="$(printf 'preflight index snapshot\n' | git commit-tree "$tree" "${parent[@]}")"
fi

worktree="$(mktemp -d)"
rmdir "$worktree"
cleanup() {
  git -C "$root" worktree remove --force "$worktree" >/dev/null 2>&1 || true
}
trap cleanup EXIT
git -C "$root" worktree add --detach --quiet "$worktree" "$treeish"
(
  cd "$worktree"
  bash scripts/ci-preflight.sh --base "$base_ref"
)
