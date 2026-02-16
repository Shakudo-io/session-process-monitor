#!/bin/bash
set -e
rustup target add x86_64-unknown-linux-musl 2>/dev/null || true
cargo build --release --target x86_64-unknown-linux-musl
BINARY="target/x86_64-unknown-linux-musl/release/session-process-monitor"
SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY")
echo "Binary size: $(echo "scale=2; $SIZE / 1048576" | bc) MB"
if [ "$SIZE" -gt 5242880 ]; then
  echo "WARNING: Binary exceeds 5MB limit!"
  exit 1
fi
echo "Build successful: $BINARY"
