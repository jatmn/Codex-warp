#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

pass() {
  PR_TITLE="$1" bash scripts/check-pr-title.sh >/dev/null
}

fail() {
  if PR_TITLE="$1" bash scripts/check-pr-title.sh >/dev/null 2>&1; then
    echo "check-pr-title-harness: unexpectedly accepted: $1" >&2
    exit 1
  fi
}

pass 'feat(config): add reusable provider fragments'
pass 'fix(webui): preserve inherited stream usage'
pass 'perf(codec): reduce markup scanning allocations'
pass 'feat!: change the provider selection contract'
pass 'build(deps): bump serde from 1.0.1 to 1.0.2'
pass 'chore(main): release 0.1.0'
pass 'revert: restore the previous retry policy'

fail ''
fail 'feature: use an unsupported type'
fail 'fix missing separator'
fail 'fix(): empty scope'
fail 'fix: '
fail $'fix: first line\nci: injected command'
fail 'Fix: types are case-sensitive'

echo 'check-pr-title-harness: ok'
