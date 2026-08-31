#!/usr/bin/env bash
set -euo pipefail

[ "$#" -eq 2 ] && [ "$1" = -w ] || {
  echo 'cygpath fixture: expected -w <path>' >&2
  exit 1
}
printf '%s\n' "$2"
