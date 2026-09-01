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

copy_contract_tree "$tmp/crlf"
python3 - "$tmp/crlf/tools/nightly-packaging-contract.txt" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_bytes().replace(b'\r\n', b'\n').replace(b'\n', b'\r\n')
path.write_bytes(text)
PY
if ! crlf_digest="$(bash "$tmp/crlf/scripts/nightly-contract-digest.sh" "$tmp/crlf")"; then
  echo 'nightly-contract-digest-harness: a CRLF contract list failed to digest' >&2
  exit 1
fi
[[ "$crlf_digest" =~ ^[0-9a-f]{64}$ ]]

printf '\n# contract-binding marker\n' >>"$tmp/bound/scripts/nightly-contract-digest.sh"
mutated_helper="$(bash "$tmp/bound/scripts/nightly-contract-digest.sh" "$tmp/bound")"
if [ "$mutated_helper" = "$bound_digest" ]; then
  echo 'nightly-contract-digest-harness: mutating the digest helper did not change packagingContractSha256' >&2
  exit 1
fi

copy_contract_tree "$tmp/skip"
sed -i 's/find "$dir" -type f -print/true/' "$tmp/skip/scripts/nightly-contract-digest.sh"
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

copy_contract_tree "$tmp/tracked"
(
  cd "$tmp/tracked"
  git init --quiet
  mkdir -p .git/empty-hooks
  git config core.hooksPath .git/empty-hooks
  git config user.name digest-harness
  git config user.email digest-harness@example.invalid
  git add .
  git commit --quiet -m fixture
)
tracked_digest="$(bash "$tmp/tracked/scripts/nightly-contract-digest.sh" "$tmp/tracked")"
[[ "$tracked_digest" =~ ^[0-9a-f]{64}$ ]]
touch "$tmp/tracked/configs/.DS_Store"
printf 'not packaged\n' >"$tmp/tracked/configs/.untracked.toml"
junk_digest="$(bash "$tmp/tracked/scripts/nightly-contract-digest.sh" "$tmp/tracked")"
if [ "$junk_digest" != "$tracked_digest" ]; then
  echo 'nightly-contract-digest-harness: untracked packaging junk changed packagingContractSha256' >&2
  exit 1
fi

python3 - "$tmp/tracked/LICENSE" <<'PY'
from pathlib import Path
import sys
path = Path(sys.argv[1])
text = path.read_bytes().replace(b'\r\n', b'\n').replace(b'\n', b'\r\n')
path.write_bytes(text)
PY
crlf_tracked="$(bash "$tmp/tracked/scripts/nightly-contract-digest.sh" "$tmp/tracked")"
if [ "$crlf_tracked" != "$tracked_digest" ]; then
  echo 'nightly-contract-digest-harness: CRLF worktree bytes changed packagingContractSha256' >&2
  exit 1
fi

copy_contract_tree "$tmp/historical"
grep -Fxv 'scripts/sha256-lf-file.sh' "$root/tools/nightly-packaging-contract.txt" \
  >"$tmp/historical/tools/nightly-packaging-contract.txt"
rm -f "$tmp/historical/scripts/sha256-lf-file.sh"
historical="$(bash "$root/scripts/nightly-contract-digest.sh" "$tmp/historical")"
[[ "$historical" =~ ^[0-9a-f]{64}$ ]] || {
  echo 'nightly-contract-digest-harness: a historical tree without sha256-lf-file.sh failed to digest' >&2
  exit 1
}

echo 'nightly-contract-digest-harness: ok'
