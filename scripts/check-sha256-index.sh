#!/usr/bin/env bash
# Validate one checksum index against an exact digest/filename inventory.
set -euo pipefail

if [ "$#" -lt 3 ] || [ $((($# - 1) % 2)) -ne 0 ]; then
  echo 'usage: check-sha256-index.sh <checksum-file> <sha256> <filename> [<sha256> <filename> ...]' >&2
  exit 2
fi

checksum_file="$1"
shift
[ -f "$checksum_file" ] || { echo "check-sha256-index: not a file: $checksum_file" >&2; exit 2; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
expected="$tmp/expected"
actual="$tmp/actual"
: >"$expected"
: >"$actual"

while [ "$#" -gt 0 ]; do
  digest="$1"
  filename="$2"
  shift 2
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo "check-sha256-index: invalid expected digest: $digest" >&2; exit 1; }
  [ -n "$filename" ] || { echo 'check-sha256-index: expected filename is empty' >&2; exit 1; }
  printf '%s\t%s\n' "$digest" "$filename" >>"$expected"
done

while IFS= read -r line || [ -n "$line" ]; do
  line="${line%$'\r'}"
  [ -n "$line" ] || continue
  digest="${line:0:64}"
  marker="${line:64:2}"
  filename="${line:66}"
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo "check-sha256-index: invalid digest record in $checksum_file" >&2; exit 1; }
  [ "$marker" = '  ' ] || [ "$marker" = ' *' ] || {
    echo "check-sha256-index: invalid checksum marker in $checksum_file" >&2
    exit 1
  }
  [ -n "$filename" ] || { echo "check-sha256-index: empty filename in $checksum_file" >&2; exit 1; }
  printf '%s\t%s\n' "$digest" "$filename" >>"$actual"
done <"$checksum_file"

LC_ALL=C sort "$expected" -o "$expected"
LC_ALL=C sort "$actual" -o "$actual"
cmp "$expected" "$actual" >/dev/null || {
  echo "check-sha256-index: checksum inventory differs from the contract: $checksum_file" >&2
  exit 1
}

echo "check-sha256-index: ok: $checksum_file"
