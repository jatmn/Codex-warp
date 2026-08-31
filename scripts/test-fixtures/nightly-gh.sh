#!/usr/bin/env bash
set -euo pipefail

case "${1:-} ${2:-}" in
  'api '*)
    endpoint=''
    for argument in "$@"; do
      case "$argument" in repos/*) endpoint="$argument" ;; esac
    done
    case "$endpoint" in
      */actions/runs/*) printf '%s\n' '2026-08-30T10:00:00Z' ;;
      */releases\?*)
        [ "${NIGHTLY_GH_FAIL_RELEASES:-0}" != 1 ] || exit 1
        printf '%s\n' "${NIGHTLY_GH_RELEASES_JSON:-[]}"
        ;;
      */releases/tags/*) printf '%s\n' "${NIGHTLY_GH_RELEASE_JSON:-{}}" ;;
      *) echo "nightly-gh fixture: unsupported api endpoint: $endpoint" >&2; exit 2 ;;
    esac
    ;;
  'release download')
    destination=''
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --dir ]; then destination="$2"; break; fi
      shift
    done
    [ -n "$destination" ] && [ -n "${NIGHTLY_GH_ASSET_DIR:-}" ]
    mkdir -p "$destination"
    cp -R "$NIGHTLY_GH_ASSET_DIR/." "$destination/"
    ;;
  'attestation verify') exit 0 ;;
  *) echo "nightly-gh fixture: unsupported command: $*" >&2; exit 2 ;;
esac
