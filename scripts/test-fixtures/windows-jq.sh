#!/usr/bin/env bash
set -euo pipefail

[ -x "${JQ_REAL:-}" ] || {
  echo 'jq fixture: JQ_REAL must name the host jq executable' >&2
  exit 1
}
"$JQ_REAL" "$@" | sed $'s/$/\r/'
