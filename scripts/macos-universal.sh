#!/bin/sh
# A universal (arm64 + x86_64) macOS rufous for a host app to ship as a helper
# (docs/sandboxed-helper.md, boubou-f8d). Default features only, macOS 11.0 and later.
# Builds under target/<triple>/release, so the target/release/rufous the bench drivers
# launch is never touched. Prints the binary's path, its minimum macOS and its SHA-256.
set -eu
cd "$(dirname "$0")/.."
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --locked -p rufous-cli --target "$t"
done
out=target/universal-apple-darwin/release
mkdir -p "$out"
lipo -create -output "$out/rufous" \
    target/aarch64-apple-darwin/release/rufous \
    target/x86_64-apple-darwin/release/rufous
# Only the system's own libraries: a sandboxed helper cannot load anything else.
foreign=$(otool -L "$out/rufous" | grep -v ':$' | awk '{print $1}' | grep -v -e '^/System/Library/' -e '^/usr/lib/' || true)
if [ -n "$foreign" ]; then
    echo "links outside the system: $foreign" >&2
    exit 1
fi
lipo -archs "$out/rufous"
vtool -show-build "$out/rufous" | awk '/minos/ {print "minos " $2}' | sort -u
shasum -a 256 "$out/rufous"
