#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
grep -Fx 'scripts/nightly-contract-digest.sh' tools/nightly-packaging-contract.txt >/dev/null

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

copy_contract_tree() {
  local dest="$1"
  mkdir -p "$dest"
  while IFS= read -r input; do
    case "$input" in ''|'#'*) continue ;; esac
    if [[ "$input" == */ ]]; then
      mkdir -p "$dest/$(dirname "${input%/}")"
      cp -a "$root/${input%/}" "$dest/${input%/}"
    else
      mkdir -p "$dest/$(dirname "$input")"
      cp -a "$root/$input" "$dest/$input"
    fi
  done <"$root/tools/nightly-packaging-contract.txt"
}

copy_contract_tree "$tmp/bound"
bound_digest="$(bash "$tmp/bound/scripts/nightly-contract-digest.sh" "$tmp/bound")"
[[ "$bound_digest" =~ ^[0-9a-f]{64}$ ]]

printf '\n# contract-binding marker\n' >>"$tmp/bound/scripts/nightly-contract-digest.sh"
mutated_helper="$(bash "$tmp/bound/scripts/nightly-contract-digest.sh" "$tmp/bound")"
if [ "$mutated_helper" = "$bound_digest" ]; then
  echo 'nightly-contract-digest-harness: mutating the digest helper did not change packagingContractSha256' >&2
  exit 1
fi

copy_contract_tree "$tmp/skip"
sed -i 's/find "$root\/${input%\/}" -type f -print/continue/' "$tmp/skip/scripts/nightly-contract-digest.sh"
skipped="$(bash "$tmp/skip/scripts/nightly-contract-digest.sh" "$tmp/skip")"
if [ "$skipped" = "$bound_digest" ]; then
  echo 'nightly-contract-digest-harness: skipping selected packaging inputs did not change packagingContractSha256' >&2
  exit 1
fi

copy_contract_tree "$tmp/unbound"
grep -Fxv 'scripts/nightly-contract-digest.sh' "$root/tools/nightly-packaging-contract.txt" \
  >"$tmp/unbound/tools/nightly-packaging-contract.txt"
unbound_digest="$(bash "$tmp/unbound/scripts/nightly-contract-digest.sh" "$tmp/unbound")"
printf '\n# contract-binding marker\n' >>"$tmp/unbound/scripts/nightly-contract-digest.sh"
unbound_mutated="$(bash "$tmp/unbound/scripts/nightly-contract-digest.sh" "$tmp/unbound")"
if [ "$unbound_mutated" != "$unbound_digest" ]; then
  echo 'nightly-contract-digest-harness: an unbound helper mutation unexpectedly changed the digest' >&2
  exit 1
fi

echo 'nightly-contract-digest-harness: ok'
