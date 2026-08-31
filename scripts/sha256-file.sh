#!/usr/bin/env bash
# Print the lowercase SHA-256 digest of exactly one file on Linux, macOS, or Windows Git Bash.
set -euo pipefail

[ "$#" -eq 1 ] || { echo 'usage: sha256-file.sh <file>' >&2; exit 2; }
[ -f "$1" ] || { echo "sha256-file: not a file: $1" >&2; exit 2; }

if command -v sha256sum >/dev/null 2>&1; then
  digest="$(sha256sum "$1" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  digest="$(shasum -a 256 "$1" | awk '{print $1}')"
elif command -v openssl >/dev/null 2>&1; then
  digest="$(openssl dgst -sha256 -r "$1" | awk '{print $1}')"
else
  echo 'sha256-file: no SHA-256 implementation is available' >&2
  exit 1
fi

[[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'sha256-file: invalid digest output' >&2; exit 1; }
printf '%s\n' "$digest"
