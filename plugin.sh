#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/gray-search.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/gray-search" ;;
    *) rel="x86_64-apple-darwin/gray-search" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/gray-search" ;;
    *) rel="x86_64-unknown-linux-musl/gray-search" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/gray-search.exe" "$here/target/release/gray-search" "$here/target/debug/gray-search.exe" "$here/target/debug/gray-search"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "gray-search binary not found in $here" >&2
exit 1
