#!/usr/bin/env bash
set -euo pipefail

root="${1:-$(git rev-parse --show-toplevel)}"
list="$root/tools/nightly-packaging-contract.txt"
[ -f "$list" ] || { echo 'nightly-contract-digest: contract list is missing' >&2; exit 1; }
temp="$(mktemp)"
trap 'rm -f "$temp"' EXIT

while IFS= read -r input; do
  case "$input" in ''|'#'*) continue ;; esac
  if [[ "$input" == */ ]]; then
    [ -d "$root/${input%/}" ] || { echo "nightly-contract-digest: missing $input" >&2; exit 1; }
    find "$root/${input%/}" -type f -print
  else
    [ -f "$root/$input" ] || { echo "nightly-contract-digest: missing $input" >&2; exit 1; }
    printf '%s\n' "$root/$input"
  fi
done <"$list" | sort | while IFS= read -r file; do
  relative="${file#"$root/"}"
  printf '%s\0%s\n' "$relative" "$(bash "$root/scripts/sha256-file.sh" "$file")"
done >"$temp"
bash "$root/scripts/sha256-file.sh" "$temp"
