#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

# Exercise the workflow's actual Bash block so a change to the step is tested.
awk '
  /^      - / { step = 0; run = 0 }
  /^        [^ ]/ { run = 0 }
  /^        id: rust-diff$/ { step = 1 }
  step && /^        run: \|$/ { run = 1; next }
  run && /^          / { print substr($0, 11); found = 1 }
  END { if (!found) exit 1 }
' "$root/.github/workflows/mutants.yml" >"$fixture/step.sh" || {
  echo 'mutants-diff-harness: could not find the rust-diff run block' >&2
  exit 1
}

# The preflight hooks can supply an object store for the checkout's snapshot.
unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
git init --quiet --initial-branch=base "$fixture/repo"
cd "$fixture/repo"
git config core.hooksPath .git/empty-hooks
git config commit.gpgsign false
git config user.name mutants-diff-harness
git config user.email mutants-diff-harness@example.invalid
git remote add origin .
git commit --quiet --allow-empty -m 'Event base'
event_base="$(git rev-parse HEAD)"

git switch --quiet -c pr
printf '# Guide\n' >README.md
git add README.md
git commit --quiet -m 'PR documentation'
docs_head="$(git rev-parse HEAD)"
mkdir -p src
printf 'pub fn pr_change() {}\n' >src/pr.rs
git add src/pr.rs
git diff --cached -- src/pr.rs >"$fixture/expected.diff"
git commit --quiet -m 'PR Rust change'
rust_head="$(git rev-parse HEAD)"

git switch --quiet base
mkdir -p src
printf 'pub fn base_change() {}\n' >src/base.rs
git add src/base.rs
git commit --quiet -m 'Base advances independently'
advanced_base="$(git rev-parse HEAD)"

run_diff() {
  rm -f git.diff "$fixture/output"
  BASE_SHA="$event_base" PR_HEAD_SHA="$1" GITHUB_OUTPUT="$fixture/output" \
    bash --noprofile --norc -eo pipefail "$fixture/step.sh"
}

git merge --quiet --no-ff "$docs_head" -m 'Test the documentation merge'
run_diff "$docs_head" >"$fixture/log"
if [ "$(cat "$fixture/output")" != needed=false ] || [ ! -f git.diff ] || [ -s git.diff ]; then
  echo 'mutants-diff-harness: documentation merge should emit an empty Rust diff' >&2
  exit 1
fi

git switch --quiet --detach "$advanced_base"
git merge --quiet --no-ff "$rust_head" -m 'Test the Rust merge'
run_diff "$rust_head" >"$fixture/log"
if [ "$(cat "$fixture/output")" != needed=true ] || ! diff -u "$fixture/expected.diff" git.diff; then
  echo 'mutants-diff-harness: Rust merge should emit only the PR Rust patch' >&2
  exit 1
fi

if run_diff "$docs_head" >"$fixture/log" 2>&1 || [ -e "$fixture/output" ] || [ -e git.diff ]; then
  echo 'mutants-diff-harness: mismatched PR head should fail before emitting a diff or output' >&2
  exit 1
fi

echo 'mutants-diff-harness: ok'
