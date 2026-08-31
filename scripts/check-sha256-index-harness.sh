#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
first="$(printf first | sha256sum | awk '{print $1}')"
second="$(printf second | sha256sum | awk '{print $1}')"

printf '%s  %s\n%s  %s\n' "$first" first.bin "$second" second.bin >"$temp/valid.sum"
bash scripts/check-sha256-index.sh "$temp/valid.sum" "$first" first.bin "$second" second.bin >/dev/null

printf '%s  %s\n%s  %s\n%s  %s\n' "$first" first.bin "$first" first.bin "$second" second.bin >"$temp/duplicate.sum"
if bash scripts/check-sha256-index.sh "$temp/duplicate.sum" "$first" first.bin "$second" second.bin >/dev/null 2>&1; then
  echo 'check-sha256-index-harness: duplicate checksum records were accepted' >&2
  exit 1
fi

printf '%s  %s\n' "$first" first.bin >"$temp/omitted.sum"
if bash scripts/check-sha256-index.sh "$temp/omitted.sum" "$first" first.bin "$second" second.bin >/dev/null 2>&1; then
  echo 'check-sha256-index-harness: an omitted checksum record was accepted' >&2
  exit 1
fi

echo 'check-sha256-index-harness: ok'
