#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

bin="$tmp/bin"
mkdir "$bin"
releases_file="$tmp/releases.json"
printf '%s\n' '[]' >"$releases_file"

cat >"$bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" != api ]; then
  echo "lookup-official-draft-harness: unexpected gh command: $*" >&2
  exit 2
fi
shift
uses_paginate=0
uses_jq_items=0
endpoint=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    --paginate) uses_paginate=1 ;;
    --jq)
      shift
      [ "${1:-}" = '.[]' ] && uses_jq_items=1
      ;;
    repos/*/releases)
      endpoint=list
      ;;
    repos/*/releases/tags/*)
      echo 'lookup-official-draft-harness: published-tag lookup is not valid for drafts' >&2
      exit 1
      ;;
  esac
  shift || true
done
[ "$uses_paginate" -eq 1 ] && [ "$uses_jq_items" -eq 1 ] && [ "$endpoint" = list ] || {
  echo 'lookup-official-draft-harness: unexpected gh api invocation' >&2
  exit 2
}
jq -c '.[]' "${GH_RELEASES_FIXTURE:?}"
EOF
chmod +x "$bin/gh"

draft='{"id":380104054,"tag_name":"v0.1.0","draft":true,"published_at":null,"prerelease":false}'
published='{"id":1,"tag_name":"v0.1.0","draft":false,"published_at":"2026-08-31T00:00:00Z","prerelease":false}'
prerelease='{"id":2,"tag_name":"v0.1.0","draft":true,"published_at":null,"prerelease":true}'

run_lookup() {
  PATH="$bin:$PATH" GH_RELEASES_FIXTURE="$releases_file" \
    LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 LOOKUP_OFFICIAL_DRAFT_SLEEP=0 \
    bash scripts/lookup-official-draft.sh jatmn/Codex-warp "$1"
}

jq -n --argjson draft "$draft" '[$draft]' >"$releases_file"
got="$(run_lookup v0.1.0)"
jq -e '.id == 380104054 and .draft == true' <<<"$got" >/dev/null

jq -n --argjson published "$published" '[$published]' >"$releases_file"
if run_lookup v0.1.0 >/dev/null 2>&1; then
  echo 'lookup-official-draft-harness: accepted a published release as a draft' >&2
  exit 1
fi

jq -n --argjson prerelease "$prerelease" '[$prerelease]' >"$releases_file"
if run_lookup v0.1.0 >/dev/null 2>&1; then
  echo 'lookup-official-draft-harness: accepted a prerelease draft' >&2
  exit 1
fi

printf '%s\n' '[]' >"$releases_file"
if run_lookup v0.1.0 >/dev/null 2>&1; then
  echo 'lookup-official-draft-harness: accepted a missing draft' >&2
  exit 1
fi

jq -n --argjson a "$draft" --argjson b '{"id":9,"tag_name":"v0.1.0","draft":true,"published_at":null,"prerelease":false}' '[$a,$b]' >"$releases_file"
if run_lookup v0.1.0 >/dev/null 2>&1; then
  echo 'lookup-official-draft-harness: accepted duplicate tag releases' >&2
  exit 1
fi

if PATH="$bin:$PATH" GH_RELEASES_FIXTURE="$releases_file" \
     LOOKUP_OFFICIAL_DRAFT_ATTEMPTS=1 LOOKUP_OFFICIAL_DRAFT_SLEEP=0 \
     bash scripts/lookup-official-draft.sh jatmn/Codex-warp nightly-20260831 >/dev/null 2>&1; then
  echo 'lookup-official-draft-harness: accepted a non-official tag' >&2
  exit 1
fi

echo 'lookup-official-draft-harness: ok'
