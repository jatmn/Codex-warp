#!/usr/bin/env bash
# Download cargo-dist without executing a remote installer and verify its bytes.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
version="$(jq -r '.dist.version' "$root/tools/release-tooling.json")"
destination=''
if [ "${1:-}" = '--dest' ] && [ -n "${2:-}" ]; then
  destination="$2"
elif [ "$#" -ne 0 ]; then
  echo 'usage: install-pinned-dist.sh [--dest <path>]' >&2
  exit 2
fi

system="$(uname -s)"
machine="$(uname -m)"
case "$system:$machine" in
  Linux:x86_64) archive='cargo-dist-x86_64-unknown-linux-gnu.tar.xz' ;;
  Darwin:x86_64) archive='cargo-dist-x86_64-apple-darwin.tar.xz' ;;
  Darwin:arm64) archive='cargo-dist-aarch64-apple-darwin.tar.xz' ;;
  MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64) archive='cargo-dist-x86_64-pc-windows-msvc.zip' ;;
  *) echo "install-pinned-dist: unsupported host $system/$machine" >&2; exit 1 ;;
esac
expected="$(awk -v archive="$archive" '$2 == archive {print $1}' "$root/tools/dist-tool-digests.sha256")"
[ -n "$expected" ] || { echo "install-pinned-dist: no digest for $archive" >&2; exit 1; }

temp="$(mktemp -d)"
trap 'rm -rf "$temp"' EXIT
url="https://github.com/axodotdev/cargo-dist/releases/download/v$version/$archive"
curl --proto '=https' --tlsv1.2 --fail --location --retry 3 --output "$temp/$archive" "$url"
actual="$(bash "$root/scripts/sha256-file.sh" "$temp/$archive")"
[ "$actual" = "$expected" ] || { echo "install-pinned-dist: digest mismatch for $archive" >&2; exit 1; }

case "$archive" in
  *.tar.xz) tar -xJf "$temp/$archive" -C "$temp" ;;
  *.zip) unzip -q "$temp/$archive" -d "$temp" ;;
esac
binary="$(find "$temp" -type f \( -name dist -o -name dist.exe \) -print -quit)"
[ -n "$binary" ] || { echo 'install-pinned-dist: archive contains no dist binary' >&2; exit 1; }

if [ -z "$destination" ]; then
  install_root="${CARGO_HOME:-$HOME/.cargo}/bin"
  mkdir -p "$install_root"
  destination="$install_root/dist"
  case "$archive" in *.zip) destination="$destination.exe" ;; esac
fi
mkdir -p "$(dirname "$destination")"
cp "$binary" "$destination"
chmod +x "$destination"
[ "$($destination --version)" = "cargo-dist $version" ] || { echo 'install-pinned-dist: version check failed' >&2; exit 1; }
if [ -n "${GITHUB_PATH:-}" ]; then
  printf '%s\n' "$(dirname "$destination")" >>"$GITHUB_PATH"
fi
echo "install-pinned-dist: verified cargo-dist $version ($actual)"
