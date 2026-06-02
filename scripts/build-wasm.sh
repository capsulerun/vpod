#!/usr/bin/env bash
set -euo pipefail

RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
STABLE_TC=$(rustup toolchain list | grep '^stable-' | head -1 | awk '{print $1}')

if [[ -z "$STABLE_TC" ]]; then
    echo "error: no stable rustup toolchain found. Run: rustup toolchain install stable" >&2
    exit 1
fi

RUSTUP_CARGO="$RUSTUP_HOME/toolchains/$STABLE_TC/bin/cargo"
RUSTUP_RUSTC="$RUSTUP_HOME/toolchains/$STABLE_TC/bin/rustc"

echo "[build-wasm] toolchain: $STABLE_TC"
echo "[build-wasm] building wasi-component (cli)..."
RUSTC="$RUSTUP_RUSTC" "$RUSTUP_CARGO" build -p wasi-component --bin capsulev-wasi-cli --release --target wasm32-wasip2

echo "[build-wasm] building wasi-component (library)..."
RUSTC="$RUSTUP_RUSTC" "$RUSTUP_CARGO" build -p wasi-component --lib --release --target wasm32-wasip2

echo "[build-wasm] building capsulev host..."
cargo build -p capsulev --release

echo "[build-wasm] done"
echo "  cli:       target/wasm32-wasip2/release/capsulev-wasi-cli.wasm"
echo "  library:   target/wasm32-wasip2/release/capsulev_wasi_lib.wasm"
echo "  host:      target/release/capsulev"
