#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

CATALOG="${CATALOG:-registry/catalog.json}"
OUT="${OUT:-dist/registry-bundle}"
BASELINE="perf.json"
VERSION="${VERSION:-0.0.0-local}"
REGISTRY_BASE="${REGISTRY_BASE:-https://registry.vpod.sh}"

if [ -z "${PREFIX:-}" ]; then
    echo "error: set PREFIX to the channel this bundle is for." >&2
    echo "       PREFIX=tests/v1   a CI fixture channel" >&2
    echo "       PREFIX=v1         production, what every installed SDK pulls" >&2
    exit 1
fi
PREFIX="${PREFIX#/}"
PREFIX="${PREFIX%/}"

if [ "$PREFIX" = "v1" ] && [ "${CONFIRM_PRODUCTION:-0}" != "1" ]; then
    echo "error: PREFIX=v1 publishes to production, which every installed SDK pulls." >&2
    echo "       Re-run with CONFIRM_PRODUCTION=1 if that is what you mean." >&2
    exit 1
fi

# source snapshot in dist/ : id published to the registry
SNAPSHOTS=(
    "dist/alpine-3.23.0-256mb.snap:alpine-3.23.0-256mb"
    "dist/alpine-3.23.0-512mb.snap:vsnap-base-512mb"
    "dist/alpine-3.23.0-1024mb.snap:vsnap-base-1024mb"
    "dist/vsnap-data-512mb.snap:vsnap-data-512mb"
)

# published id : additional id serving the identical bytes
ALIASES=(
    "alpine-3.23.0-256mb:vsnap-base-256mb"
)

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    ./scripts/build-default-snapshot.sh
    ./scripts/build-default-snapshot.sh --ram 512
    ./scripts/build-default-snapshot.sh --ram 1024
    ./scripts/build-data-snapshot.sh
fi

for pair in "${SNAPSHOTS[@]}"; do
    source_snapshot="${pair%%:*}"
    [ -f "$source_snapshot" ] || {
        echo "error: $source_snapshot missing (build it, or drop SKIP_BUILD)" >&2
        exit 1
    }
done

mkdir -p "$OUT"

for pair in "${SNAPSHOTS[@]}"; do
    source_snapshot="${pair%%:*}"
    published_id="${pair##*:}"
    echo "compressing $source_snapshot -> $published_id.snap"
    lz4 -d -c "$source_snapshot" | lz4 -9 > "$OUT/$published_id.snap"
done

for pair in "${ALIASES[@]}"; do
    from_id="${pair%%:*}"
    to_id="${pair##*:}"
    cp "$OUT/$from_id.snap" "$OUT/$to_id.snap"
done


for snapshot in "$OUT"/*.snap; do
    outer=$(head -c 4 "$snapshot" | xxd -p | tr -d '\n')
    if [ "$outer" != "04224d18" ]; then
        echo "error: $snapshot is not lz4-framed (starts with $outer)" >&2
        exit 1
    fi
    inner=$({ lz4 -dc "$snapshot" 2>/dev/null || true; } | head -c 4 | xxd -p | tr -d '\n')
    if [ "$inner" != "56504f44" ]; then
        echo "error: $snapshot does not hold VPOD under one lz4 layer" >&2
        echo "       found $inner, so it is framed more than once" >&2
        exit 1
    fi
done
echo "framing verified: one lz4 layer over VPOD"


BASELINE_PORT=""
if [ "${SKIP_BASELINE:-0}" != "1" ]; then
    BASELINE_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
fi

CATALOG="$CATALOG" OUT="$OUT" PREFIX="$PREFIX" VERSION="$VERSION" \
BASELINE_PORT="$BASELINE_PORT" \
REGISTRY_BASE="$REGISTRY_BASE" python3 - <<'PY'
import hashlib, json, os
from pathlib import Path

catalog = Path(os.environ["CATALOG"])
out = Path(os.environ["OUT"])
prefix = os.environ["PREFIX"].strip("/")
base = os.environ["REGISTRY_BASE"].rstrip("/")

snapshots = json.loads(catalog.read_text())["snapshots"]

def digest_and_size(path):
    sha256 = hashlib.sha256()
    size = 0
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            sha256.update(chunk)
            size += len(chunk)
    return sha256.hexdigest(), size

for entry in snapshots:
    built = out / f"{entry['id']}.snap"
    if not built.exists():
        raise SystemExit(
            f"{entry['id']}: listed in {catalog} but not produced by this build. "
            f"Add it to SNAPSHOTS or ALIASES."
        )

    entry["url"] = f"{base}/{prefix}/{entry['id']}.snap"
    entry["sha256"], entry["size"] = digest_and_size(built)
    print(f"  {entry['id']}: sha256={entry['sha256'][:12]}… size={entry['size']:,}")

manifest = {"version": os.environ["VERSION"], "snapshots": snapshots}
(out / "snapshots.json").write_text(json.dumps(manifest, indent=2) + "\n")

port = os.environ.get("BASELINE_PORT")
if port:
    local = json.loads(json.dumps(manifest))
    for entry in local["snapshots"]:
        entry["url"] = f"http://127.0.0.1:{port}/{entry['id']}.snap"
    (out / "snapshots.local.json").write_text(json.dumps(local, indent=2) + "\n")
PY

if [ -n "$BASELINE_PORT" ]; then
    echo
    echo "── Recording the perf baseline against these bytes..."

    python3 -m http.server "$BASELINE_PORT" --bind 127.0.0.1 --directory "$OUT" \
        >/dev/null 2>&1 &
    server=$!
    disown "$server" 2>/dev/null || true
    trap 'kill "$server" 2>/dev/null || true; rm -f "$OUT/snapshots.local.json"' EXIT

    for _ in $(seq 1 50); do
        curl -sf "http://127.0.0.1:$BASELINE_PORT/snapshots.local.json" >/dev/null && break
        sleep 0.1
    done

    if (cd sdks/python && VPOD_PERF_RECORD=1 \
            VPOD_REGISTRY="http://127.0.0.1:$BASELINE_PORT/snapshots.local.json" \
            VPOD_PERF_RECORD_TO="../../$OUT/$BASELINE" \
            python3 -m pytest tests/test_performance.py -m performance -q -s); then
        echo "   baseline written to $OUT/$BASELINE"
    else
        echo "   WARNING: the baseline was not recorded, so this bundle is incomplete." >&2
        echo "   Strict perf runs will fail until $BASELINE describes these bytes." >&2
        echo "   Needs the python SDK installed: cd sdks/python && pip install -e .[dev]" >&2
    fi

    kill "$server" 2>/dev/null || true
    rm -f "$OUT/snapshots.local.json"
    trap - EXIT
fi

echo
echo "bundle ready in $OUT (version $VERSION, targeting /$PREFIX/):"
ls -lh "$OUT"
