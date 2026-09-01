#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

scripts_dir="$(cd "$(dirname "$0")" && pwd)"
root="${1:-$(git rev-parse --show-toplevel)}"
cd "$root"
list="tools/nightly-packaging-contract.txt"
[ -f "$list" ] || { echo 'nightly-contract-digest: contract list is missing' >&2; exit 1; }
temp="$(mktemp)"
trap 'rm -f "$temp"' EXIT

# Prefer git ls-files only when this directory is the work tree root. A copied
# contract tree under target/tmp is still "inside" the parent clone; find must
# hash that copy, not the parent's index. Runners package from the clone root,
# where ls-files is identical on Linux, macOS, and Windows.
list_input_files() {
  local input="$1"
  if [[ "$input" == */ ]]; then
    local dir="${input%/}"
    [ -d "$dir" ] || { echo "nightly-contract-digest: missing $input" >&2; exit 1; }
    local toplevel
    toplevel="$(git rev-parse --show-toplevel 2>/dev/null || true)"
    if [ -n "$toplevel" ] && [ "$toplevel" -ef "$PWD" ]; then
      git ls-files -- "$dir"
    else
      find "$dir" -type f -print
    fi
  else
    [ -f "$input" ] || { echo "nightly-contract-digest: missing $input" >&2; exit 1; }
    printf '%s\n' "$input"
  fi
}

while IFS= read -r input || [ -n "$input" ]; do
  input="${input%$'\r'}"
  case "$input" in ''|'#'*) continue ;; esac
  list_input_files "$input"
done <"$list" | sed 's#\\#/#g; s#^\./##' | LC_ALL=C sort -u | while IFS= read -r file; do
  file="${file%$'\r'}"
  printf '%s\0%s\n' "$file" "$(bash "$scripts_dir/sha256-lf-file.sh" "$file")"
done >"$temp"
digest="$(bash "$scripts_dir/sha256-file.sh" "$temp")"
[[ "$digest" =~ ^[0-9a-f]{64}$ ]] || { echo 'nightly-contract-digest: invalid digest' >&2; exit 1; }
printf '%s\n' "$digest"
