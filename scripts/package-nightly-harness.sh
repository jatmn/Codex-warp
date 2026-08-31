#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
sha="$(git rev-parse HEAD)"
date=20260830
base_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
version="$base_version-nightly.$date.${sha:0:12}"
temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
CODEX_WARP_BUILD_VERSION="$version" cargo build --release --locked >/dev/null
NIGHTLY_DATE="$date" \
NIGHTLY_SOURCE_SHA="$sha" \
NIGHTLY_VERSION="$version" \
NIGHTLY_TAG="nightly-$date-${sha:0:12}" \
TARGET=x86_64-unknown-linux-gnu \
BINARY_PATH=target/release/codex-warp \
OUTPUT_DIR="$temp" \
RUNNER_LABEL=local \
RUNNER_IMAGE=local-test \
WORKFLOW_URL=https://github.com/jatmn/Codex-warp/actions/runs/1 \
WORKFLOW_SHA="$sha" \
bash scripts/package-nightly.sh >/dev/null
archive="codex-warp-nightly-$date-${sha:0:12}-x86_64-unknown-linux-gnu.tar.xz"
(cd "$temp" && sha256sum -c "$archive.sha256" >/dev/null)
tar -xJf "$temp/$archive" -C "$temp"
"$temp/${archive%.tar.xz}/codex-warp" --version | grep -F "$version" >/dev/null
echo 'package-nightly-harness: ok'
