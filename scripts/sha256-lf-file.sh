#!/usr/bin/env bash
# Print the lowercase SHA-256 digest of one text file after stripping CR.
# Windows checkouts may materialize CRLF bytes; identity hashes must match the
# LF git blobs used on Linux collectors and other runners.
set -euo pipefail

[ "$#" -eq 1 ] || { echo 'usage: sha256-lf-file.sh <file>' >&2; exit 2; }
[ -f "$1" ] || { echo "sha256-lf-file: not a file: $1" >&2; exit 2; }

dir="$(cd "$(dirname "$0")" && pwd)"
temp="$(mktemp)"
trap 'rm -f "$temp"' EXIT
tr -d '\r' <"$1" >"$temp"
bash "$dir/sha256-file.sh" "$temp"
