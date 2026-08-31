#!/usr/bin/env bash
set -euo pipefail

[ -n "${CODEX_WARP_ARCHIVE_PATH:-}" ] || {
  echo 'powershell fixture: CODEX_WARP_ARCHIVE_PATH is required' >&2
  exit 1
}
[ -n "${CODEX_WARP_ARCHIVE_DESTINATION:-}" ] || {
  echo 'powershell fixture: CODEX_WARP_ARCHIVE_DESTINATION is required' >&2
  exit 1
}
command_line="$*"
case "$command_line" in
  *'$env:CODEX_WARP_ARCHIVE_PATH'*'$env:CODEX_WARP_ARCHIVE_DESTINATION'*) ;;
  *)
    echo 'powershell fixture: command must consume both named environment variables' >&2
    exit 1
    ;;
esac
7z x -bd -y "-o$CODEX_WARP_ARCHIVE_DESTINATION" "$CODEX_WARP_ARCHIVE_PATH" >/dev/null
