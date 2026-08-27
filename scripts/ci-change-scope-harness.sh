#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
fixture="$(mktemp -d)"
cleanup() {
  rm -rf "$fixture"
}
trap cleanup EXIT

git -C "$fixture" init --quiet
mkdir -p "$fixture/.git/ci-empty-hooks"
git -C "$fixture" config core.hooksPath .git/ci-empty-hooks
git -C "$fixture" config commit.gpgsign false
git -C "$fixture" config user.name ci-scope-harness
git -C "$fixture" config user.email ci-scope-harness@example.invalid
mkdir -p "$fixture/scripts" "$fixture/docs" "$fixture/src" "$fixture/.github/workflows"
cp "$root/scripts/ci-change-scope.sh" "$fixture/scripts/"
printf '# Guide\n' >"$fixture/README.md"
printf 'fn main() {}\n' >"$fixture/src/main.rs"
printf 'version = 3\n' >"$fixture/Cargo.lock"
printf 'name: Supply Chain\n' >"$fixture/.github/workflows/supply-chain.yml"
git -C "$fixture" add .
git -C "$fixture" commit --quiet -m base

assert_scope() {
  local expected_full="$1"
  local expected_supply="$2"
  local base="$3"
  local head="$4"
  local output
  output="$(cd "$fixture" && bash scripts/ci-change-scope.sh "$base...$head")"
  if [ "$output" != "$(printf 'full_ci=%s\nsupply_chain=%s' "$expected_full" "$expected_supply")" ]; then
    printf 'ci-change-scope-harness: unexpected output:\n%s\n' "$output" >&2
    exit 1
  fi
}

base="$(git -C "$fixture" rev-parse HEAD)"
printf '# Guide\n\nDocumentation update.\n' >"$fixture/README.md"
printf '# Details\n' >"$fixture/docs/details.md"
git -C "$fixture" add README.md docs/details.md
git -C "$fixture" commit --quiet -m docs
head="$(git -C "$fixture" rev-parse HEAD)"
assert_scope false false "$base" "$head"

base="$head"
printf 'fn main() { println!("changed"); }\n' >"$fixture/src/main.rs"
git -C "$fixture" add src/main.rs
git -C "$fixture" commit --quiet -m rust
head="$(git -C "$fixture" rev-parse HEAD)"
assert_scope true false "$base" "$head"

base="$head"
printf 'version = 4\n' >"$fixture/Cargo.lock"
git -C "$fixture" add Cargo.lock
git -C "$fixture" commit --quiet -m dependency
head="$(git -C "$fixture" rev-parse HEAD)"
assert_scope true true "$base" "$head"

base="$head"
git -C "$fixture" mv src/main.rs docs/removed-source.md
git -C "$fixture" commit --quiet -m rename
head="$(git -C "$fixture" rev-parse HEAD)"
assert_scope true false "$base" "$head"

base="$head"
printf 'name: Supply Chain Updated\n' >"$fixture/.github/workflows/supply-chain.yml"
git -C "$fixture" add .github/workflows/supply-chain.yml
git -C "$fixture" commit --quiet -m workflow
head="$(git -C "$fixture" rev-parse HEAD)"
assert_scope true true "$base" "$head"

if (cd "$fixture" && bash scripts/ci-change-scope.sh missing...HEAD >/dev/null 2>&1); then
  echo 'ci-change-scope-harness: invalid diff range unexpectedly succeeded' >&2
  exit 1
fi

echo 'ci-change-scope-harness: ok'
