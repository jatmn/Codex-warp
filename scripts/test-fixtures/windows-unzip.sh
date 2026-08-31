#!/usr/bin/env bash
set -euo pipefail

[ -x "${UNZIP_REAL:-}" ] || {
  echo 'unzip fixture: UNZIP_REAL must name the host unzip executable' >&2
  exit 1
}
if [ "${1:-}" = -Z1 ]; then
  "$UNZIP_REAL" "$@" | sed $'s/$/\r/'
else
  "$UNZIP_REAL" "$@"
fi
