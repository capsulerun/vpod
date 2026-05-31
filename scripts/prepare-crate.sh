#!/bin/bash
set -e

echo "Building WASM component..."
cargo build --release --target wasm32-wasip2 -p wasm-component

echo "Copying WASM to capsulev crate..."
cp target/wasm32-wasip2/release/wasm-component.wasm crates/capsulev/

echo "Verifying capsulev builds with bundled WASM..."
cargo build --release -p capsulev

echo ""
echo "✓ Ready for publishing!"
echo "  Run: cargo publish -p capsulev --dry-run"
echo "  Then: cargo publish -p capsulev"
