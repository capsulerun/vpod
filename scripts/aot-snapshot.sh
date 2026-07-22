#!/bin/sh
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SNAP=""
WORKLOAD="default"
MAX_BLOCKS=""
COVERAGE=""
FORCE=0

while [ $# -gt 0 ]; do
    case "$1" in
        --workload)   WORKLOAD="$2";   shift 2 ;;
        --max-blocks) MAX_BLOCKS="$2"; shift 2 ;;
        --coverage)   COVERAGE="$2";   shift 2 ;;
        --force)      FORCE=1;         shift ;;
        -*) echo "unknown arg: $1" >&2; exit 1 ;;
        *)  SNAP="$1"; shift ;;
    esac
done

if [ -z "$SNAP" ]; then
    echo "usage: $0 <snapshot> [--workload default|data] [--max-blocks N] [--coverage PCT] [--force]" >&2
    exit 1
fi
if [ ! -f "$SNAP" ]; then
    echo "error: snapshot not found: $SNAP" >&2
    exit 1
fi
case "$WORKLOAD" in
    default|data) ;;
    *) echo "error: unknown workload '$WORKLOAD' (expected: default, data)" >&2; exit 1 ;;
esac

GENERATED="$ROOT/crates/riscv-core/src/aot/generated.rs"
VPOD="$ROOT/target/release/vpod-native"
AOT_TRACE="$ROOT/dist/.aot-trace.txt"

if [ -f "$GENERATED" ] && [ "$FORCE" = "0" ]; then
    LINES=$(wc -l < "$GENERATED" | tr -d ' ')
    if [ "$LINES" -gt 100 ]; then
        echo "note: overwriting existing translation ($LINES lines) in 3s — Ctrl-C to abort," >&2
        echo "      or back it up first; it is gitignored and unrecoverable." >&2
        sleep 3
    fi
fi

echo "=== AOT translation pass ==="
echo "Snapshot : $SNAP"
echo "Workload : $WORKLOAD"
echo ""

echo "── AOT: tracing representative workload on the snapshot..."
(cd "$ROOT" && cargo build --release -p native-cli --features aot-trace)


set -- \
    --setup "python3 -c 'print(sum(i*i for i in range(200000)))'" \
    --setup "python3 -c 'exec(\"s=0\nfor i in range(200000): s=(s+i*i)^(i&0xff)\nprint(s)\")'"

case "$WORKLOAD" in
    default)
        set -- "$@" \
            --setup "python3 -c 'import json,os; print(json.dumps({\"cwd\": os.getcwd()}))'"
        ;;
    data)
        set -- "$@" \
            --setup "python3 -c 'import numpy as np; a = np.arange(100000); print(int((a * a).sum()))'" \
            --setup "python3 -c 'import pandas as pd; df = pd.DataFrame({\"x\": range(20000)}); print(int(df.x.sum()))'"
        ;;
esac

set -- "$@" \
    --setup "i=0; while [ \$i -lt 100 ]; do echo x > /tmp/aot-\$i; i=\$((\$i+1)); done; cat /tmp/aot-* | wc -l; rm -f /tmp/aot-*" \
    --setup "uv venv /tmp/aot-venv && rm -rf /tmp/aot-venv"


set -- "$@" \
    --setup "apk update && apk add jq && echo '{\"a\":[1,2,3]}' | jq -c '.a | add' && echo VPOD_AOT_APK_OK"

TRACE_LOG="$ROOT/dist/.aot-trace-run.log"
VPOD_AOT_TRACE="$AOT_TRACE" "$VPOD" --snapshot-load "$SNAP" --net "$@" 2>&1 | tee "$TRACE_LOG"

if [ ! -s "$AOT_TRACE" ]; then
    echo "error: aot trace is empty — the workload did not run" >&2
    exit 1
fi

if ! grep -q VPOD_AOT_APK_OK "$TRACE_LOG"; then
    echo "" >&2
    echo "error: the apk trace step did not complete — apk's code would be left" >&2
    echo "       untranslated (it is ~1.6B guest insns, 98% emulated CPU)." >&2
    echo "       Check network/DNS from the guest; see $TRACE_LOG" >&2
    exit 1
fi
rm -f "$TRACE_LOG"

echo "── AOT: translating hot blocks..."
(cd "$ROOT" && cargo build --release -p vpod-translate)

TRANSLATE_ARGS=""
[ -n "$MAX_BLOCKS" ] && TRANSLATE_ARGS="$TRANSLATE_ARGS --max-blocks $MAX_BLOCKS"
[ -n "$COVERAGE" ]   && TRANSLATE_ARGS="$TRANSLATE_ARGS --coverage $COVERAGE"

# generated.rs is gitignored and may be the only file in its directory,
# so on a fresh checkout the directory itself does not exist yet.
mkdir -p "$(dirname "$GENERATED")"

# shellcheck disable=SC2086
"$ROOT/target/release/vpod-translate" $TRANSLATE_ARGS "$SNAP" "$AOT_TRACE" "$GENERATED"

echo "── AOT: rebuilding vpod with translated blocks..."
(cd "$ROOT" && cargo build --release -p native-cli --features aot)
rm -f "$AOT_TRACE"

echo ""
echo "=== Done ==="
echo ""
echo "Translation: $GENERATED"
echo "Native vpod rebuilt with --features aot."
echo "For the wasm component, run: ./scripts/build-wasm.sh"
