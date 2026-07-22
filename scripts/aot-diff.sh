#!/usr/bin/env bash
# Differential harness driver (phase 5 M2): random programs, interpreter vs
# AOT lockstep. Usage: scripts/aot-diff.sh [seed] [num-programs]
# WARNING: overwrites crates/riscv-core/dist/aot-compiled.rs (gitignored).
set -euo pipefail
cd "$(dirname "$0")/.."

SEED="${1:-1}"
NUM="${2:-32}"
DIR="${TMPDIR:-/tmp}/vpod-diff-$SEED"

cargo build --release -p vpod-translate
./target/release/vpod-translate gen "$DIR" "$NUM" "$SEED" \
    crates/riscv-core/src/aot/generated.rs

VPOD_DIFF_DIR="$DIR" cargo test --release -p riscv-core --features aot \
    --test differential -- --nocapture
