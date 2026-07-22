#!/bin/sh
#
# Writes a no-op crates/riscv-core/src/aot/generated.rs.
#
# riscv-core's `aot` feature is hardwired on by wasi-component, so anything
# building the component needs generated.rs to exist. A real one is produced by
# vpod-translate from a snapshot trace and is gitignored, so CI (and a fresh
# clone) has none. This stub satisfies the API: dispatch always declines and
# every block falls through to the interpreter.
#
# Refuses to clobber a real translation unless --force, because generated.rs
# cannot be restored from git and needs the full trace pipeline to rebuild.

set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/crates/riscv-core/src/aot/generated.rs"

if [ -f "$OUT" ] && [ "$1" != "--force" ]; then
    LINES=$(wc -l < "$OUT" | tr -d ' ')
    if [ "$LINES" -gt 100 ]; then
        echo "refusing to overwrite $OUT ($LINES lines — looks like a real translation)" >&2
        echo "it is gitignored and unrecoverable; re-run with --force if you mean it." >&2
        exit 1
    fi
fi

mkdir -p "$(dirname "$OUT")"
cat > "$OUT" <<'EOF'
// AOT stub (scripts/aot-stub.sh). No translated blocks: dispatch always
// declines, so execution falls through to the interpreter. Replaced by
// vpod-translate output in a real snapshot build.
use crate::execute::ExecContext;
use crate::system_bus::SystemBus;

pub fn dispatch<B: SystemBus>(
    _ctx: &mut ExecContext<B>,
    _pa_in: u64,
    _entry_pc: u64,
    _satp: u64,
    _fuel: u64,
    _rt_page: u64,
) -> Option<u64> {
    None
}

pub const AOT_PAGE_HASHES: &[(u64, u64)] = &[];
EOF

echo "wrote AOT stub: $OUT"
