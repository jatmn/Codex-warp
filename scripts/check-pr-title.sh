#!/usr/bin/env bash
# Validate the squash-merge title that becomes the commit on main.
set -euo pipefail

title="${PR_TITLE:-${1:-}}"
pattern='^(feat|fix|perf|refactor|docs|test|build|ci|chore|revert)(\([^()[:cntrl:]]+\))?!?: [^[:space:]].*$'

if [ -z "$title" ] || [[ "$title" == *$'\n'* ]] || [[ "$title" == *$'\r'* ]] ||
   ! [[ "$title" =~ $pattern ]]; then
  cat >&2 <<'EOF'
Invalid pull request title.

Use: type(optional-scope)!: concise description
Types: feat, fix, perf, refactor, docs, test, build, ci, chore, revert
Example: fix(webui): preserve inherited stream usage
EOF
  exit 1
fi

echo "check-pr-title: ok"
