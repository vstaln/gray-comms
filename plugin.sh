#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/gray-comms.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/gray-comms" ;;
    *) rel="x86_64-apple-darwin/gray-comms" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/gray-comms" ;;
    *) rel="x86_64-unknown-linux-musl/gray-comms" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/gray-comms.exe" "$here/target/release/gray-comms" "$here/target/debug/gray-comms.exe" "$here/target/debug/gray-comms"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "gray-comms binary not found in $here" >&2
exit 1
