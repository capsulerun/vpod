#!/bin/bash
set -e

echo "Building WASM component..."
cargo build --release --target wasm32-wasip2 -p wasi-component

echo "Copying WASM to vpod crate..."
cp target/wasm32-wasip2/release/wasi-component.wasm crates/vpod/

echo "Verifying vpod builds with bundled WASM..."
cargo build --release -p vpod

echo ""
echo "✓ Ready for publishing!"
echo "  Run: cargo publish -p vpod --dry-run"
echo "  Then: cargo publish -p vpod"
