#!/usr/bin/env bash
#
# build.sh — Build teeproxyd for Android (aarch64 musl static)
#
# Output: target/aarch64-unknown-linux-musl/release/teeproxyd
#
# Prerequisites:
#   - aarch64-unknown-linux-musl target: rustup target add aarch64-unknown-linux-musl
#   - aarch64-linux-musl-gcc linker (or build on aarch64 Linux directly)
#
# On macOS without musl-cross, use the Linux build machine (orb) instead:
#   ssh orb "cd /path/to/teeproxyd && bash build.sh"

set -euo pipefail
cd "$(dirname "$0")"

echo "=== Building teeproxyd (aarch64-unknown-linux-musl, static) ==="

RUSTFLAGS="-C target-feature=+crt-static" \
    cargo build --release --target aarch64-unknown-linux-musl

OUT="target/aarch64-unknown-linux-musl/release/teeproxyd"
if [ -f "$OUT" ]; then
    echo ""
    echo "Build OK"
    file "$OUT"
    ls -lh "$OUT"
else
    echo "ERROR: binary not found at $OUT"
    exit 1
fi
