#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GENERATED="$ROOT/crates/riscv-core/src/aot/generated.rs"
SDK_DIR="$ROOT/sdks/python/vpod"
OUT_DIR="$ROOT/target/wasm32-wasip2/release"

BASE_TARGET_DIR="$ROOT/target/base-wasm"
BACKUP="$ROOT/dist/.generated.rs.aot-backup"

RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
STABLE_TC=$(rustup toolchain list | grep '^stable-' | head -1 | awk '{print $1}')

if [[ -z "$STABLE_TC" ]]; then
    echo "error: no stable rustup toolchain found. Run: rustup toolchain install stable" >&2
    exit 1
fi

RUSTUP_CARGO="$RUSTUP_HOME/toolchains/$STABLE_TC/bin/cargo"
RUSTUP_RUSTC="$RUSTUP_HOME/toolchains/$STABLE_TC/bin/rustc"

echo "[build-wasm] toolchain: $STABLE_TC"

if [[ -f "$BACKUP" ]]; then
    echo "error: a previous run left $BACKUP behind, which means it was killed" >&2
    echo "       mid-build with generated.rs stubbed. That backup is the only copy" >&2
    echo "       of the translation — restore it before continuing:" >&2
    echo "         cp -p '$BACKUP' '$GENERATED' && rm '$BACKUP'" >&2
    exit 1
fi

if [[ ! -f "$GENERATED" ]]; then
    echo "[build-wasm] no generated.rs — writing stub"
    "$ROOT/scripts/aot-stub.sh"
fi

HAVE_AOT=0
if [[ $(wc -l < "$GENERATED" | tr -d ' ') -gt 100 ]]; then
    HAVE_AOT=1
fi

build_lib() {
    RUSTC="$RUSTUP_RUSTC" "$RUSTUP_CARGO" build -p wasi-component --lib --release --target wasm32-wasip2
}

if [[ "$HAVE_AOT" == "1" ]]; then
    echo "[build-wasm] building wasi-component (cli)..."
    RUSTC="$RUSTUP_RUSTC" "$RUSTUP_CARGO" build -p wasi-component --bin vpod-wasi-cli --release --target wasm32-wasip2

    echo "[build-wasm] building wasi-component (library, AOT)..."
    build_lib
    cp "$OUT_DIR/vpod_wasi_lib.wasm" "$SDK_DIR/vpod_wasi_lib_aot.wasm"

    echo "[build-wasm] building wasi-component (library, base)..."
    mkdir -p "$(dirname "$BACKUP")"

    cp -p "$GENERATED" "$BACKUP"
    trap 'cp -p "$BACKUP" "$GENERATED" && rm -f "$BACKUP"' EXIT INT TERM

    "$ROOT/scripts/aot-stub.sh" --force
    CARGO_TARGET_DIR="$BASE_TARGET_DIR" build_lib
    cp "$BASE_TARGET_DIR/wasm32-wasip2/release/vpod_wasi_lib.wasm" \
       "$SDK_DIR/vpod_wasi_lib.wasm"

    cp -p "$BACKUP" "$GENERATED"
    rm -f "$BACKUP"
    trap - EXIT INT TERM
else
    echo "[build-wasm] generated.rs is a stub — building base tier only"
    echo "[build-wasm] building wasi-component (cli)..."
    RUSTC="$RUSTUP_RUSTC" "$RUSTUP_CARGO" build -p wasi-component --bin vpod-wasi-cli --release --target wasm32-wasip2

    echo "[build-wasm] building wasi-component (library, base)..."
    build_lib
    cp "$OUT_DIR/vpod_wasi_lib.wasm" "$SDK_DIR/vpod_wasi_lib.wasm"
    rm -f "$SDK_DIR/vpod_wasi_lib_aot.wasm"
fi

echo "[build-wasm] building vpod host..."
cargo build -p vpod --release

echo "[build-wasm] done"
if [[ "$HAVE_AOT" == "1" ]]; then
    echo "  library (AOT):  $SDK_DIR/vpod_wasi_lib_aot.wasm"
fi
echo "  library (base): $SDK_DIR/vpod_wasi_lib.wasm"
echo "  cli:            $OUT_DIR/vpod-wasi-cli.wasm"
echo "  host:           $ROOT/target/release/vpod"
