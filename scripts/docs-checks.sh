#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [ -z "$root" ]; then
  root="$(cd "$(dirname "$0")/.." && pwd)"
fi
cd "$root"

skip_typos="${DOCS_CHECKS_SKIP_TYPOS:-0}"
fail=0

if [ "$skip_typos" != "1" ]; then
  if ! command -v typos >/dev/null 2>&1; then
    echo "docs-checks: install typos with: cargo install typos-cli --locked" >&2
    fail=1
  elif ! typos . .github/pull_request_template.md; then
    fail=1
  fi
fi

docs_files=(README.md AGENTS.md CONTRIBUTING.md SECURITY.md docs .github/pull_request_template.md)
trailing_whitespace=0
while IFS= read -r -d '' doc; do
  if grep -nIHE '[[:blank:]]$' "$doc"; then
    trailing_whitespace=1
  fi
done < <(find -P "${docs_files[@]}" -type f -print0)
if [ "$trailing_whitespace" -ne 0 ]; then
  echo "docs-checks: trailing whitespace in docs" >&2
  fail=1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "docs-checks: node is required for docs prose checks" >&2
  fail=1
elif ! node scripts/docs_prose_check.js "${docs_files[@]}"; then
  fail=1
fi

prose_fixture="$(mktemp -d)"
trap 'rm -rf "$prose_fixture" "${prose_fixture}-evil"' EXIT
mkdir -p "$prose_fixture/scripts" "$prose_fixture/docs" "$prose_fixture/targets"
cp scripts/docs_prose_check.js "$prose_fixture/scripts/docs_prose_check.js"

touch "$prose_fixture/docs/unsafe name.md"
unsafe_status=0
if (cd "$prose_fixture" && node scripts/docs_prose_check.js docs) >/dev/null 2>&1; then
  unsafe_status=0
else
  unsafe_status=$?
fi
if [ "$unsafe_status" -ne 2 ]; then
  echo "docs-checks: prose checker accepted an unsafe child filename" >&2
  fail=1
fi
rm "$prose_fixture/docs/unsafe name.md"

touch "$prose_fixture/targets/unsafe name.md"
ln -s '../targets/unsafe name.md' "$prose_fixture/docs/safe.md"
unsafe_status=0
if (cd "$prose_fixture" && node scripts/docs_prose_check.js docs) >/dev/null 2>&1; then
  unsafe_status=0
else
  unsafe_status=$?
fi
if [ "$unsafe_status" -ne 2 ]; then
  echo "docs-checks: prose checker accepted an unsafe symlink target" >&2
  fail=1
fi
rm "$prose_fixture/docs/safe.md" "$prose_fixture/targets/unsafe name.md"

printf "i'm lowercase\n" > "$prose_fixture/targets/content"
ln -s ../targets/content "$prose_fixture/docs/guide.md"
logical_status=0
logical_output="$(cd "$prose_fixture" && node scripts/docs_prose_check.js docs 2>&1)" || logical_status=$?
if [ "$logical_status" -ne 1 ] || ! grep -Fq 'docs/guide.md:1:' <<< "$logical_output"; then
  echo "docs-checks: prose checker lost a safe logical Markdown path" >&2
  fail=1
fi

mkdir -p "$prose_fixture/targets/manual"
printf "i'm lowercase\n" > "$prose_fixture/targets/manual/page.md"
ln -s ../targets/manual "$prose_fixture/docs/reference"
logical_status=0
logical_output="$(cd "$prose_fixture" && node scripts/docs_prose_check.js docs/reference 2>&1)" || logical_status=$?
if [ "$logical_status" -ne 1 ] || ! grep -Fq 'docs/reference/page.md:1:' <<< "$logical_output"; then
  echo "docs-checks: prose checker lost a safe logical directory path" >&2
  fail=1
fi

for probe in ".." "../docs" "/etc/passwd" "docs/../docs"; do
  probe_status=0
  if (cd "$prose_fixture" && node scripts/docs_prose_check.js "$probe") >/dev/null 2>&1; then
    probe_status=0
  else
    probe_status=$?
  fi
  if [ "$probe_status" -ne 2 ]; then
    echo "docs-checks: prose checker accepted a traversal probe: $probe" >&2
    fail=1
  fi
done

mkdir -p "${prose_fixture}-evil"
printf "i'm lowercase\n" > "${prose_fixture}-evil/secret.md"
ln -s "${prose_fixture}-evil/secret.md" "$prose_fixture/docs/alias.md"
prefix_status=0
if (cd "$prose_fixture" && node scripts/docs_prose_check.js docs) >/dev/null 2>&1; then
  prefix_status=0
else
  prefix_status=$?
fi
if [ "$prefix_status" -ne 2 ]; then
  echo "docs-checks: prose checker followed a prefix-sibling symlink" >&2
  fail=1
fi
rm -f "$prose_fixture/docs/alias.md"
rm -rf "${prose_fixture}-evil"

mkdir -p "$prose_fixture/..cache"
printf "i'm lowercase\n" > "$prose_fixture/..cache/page.md"
dotdot_status=0
dotdot_output="$(cd "$prose_fixture" && node scripts/docs_prose_check.js ..cache 2>&1)" || dotdot_status=$?
if [ "$dotdot_status" -ne 1 ] || ! grep -Fq '..cache/page.md:1:' <<< "$dotdot_output"; then
  echo "docs-checks: prose checker rejected a safe ..cache documentation path" >&2
  fail=1
fi
rm -rf "$prose_fixture/..cache"

if [ "$fail" -ne 0 ]; then
  echo "docs-checks: failed" >&2
  exit 1
fi

echo "docs-checks: ok"
