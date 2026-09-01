#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
printf 'hello\n' >"$temp/lf"
printf 'hello\r\n' >"$temp/crlf"
lf="$(bash scripts/sha256-file.sh "$temp/lf")"
crlf="$(bash scripts/sha256-file.sh "$temp/crlf")"
norm_lf="$(bash scripts/sha256-lf-file.sh "$temp/lf")"
norm_crlf="$(bash scripts/sha256-lf-file.sh "$temp/crlf")"
[ "$lf" != "$crlf" ] || {
  echo 'sha256-lf-file-harness: CRLF and LF files hashed the same before normalization' >&2
  exit 1
}
[ "$norm_lf" = "$lf" ] || {
  echo 'sha256-lf-file-harness: LF file changed after CR stripping' >&2
  exit 1
}
[ "$norm_crlf" = "$lf" ] || {
  echo 'sha256-lf-file-harness: CRLF file did not hash as LF' >&2
  exit 1
}
echo 'sha256-lf-file-harness: ok'
