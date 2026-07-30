#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

CATALOG="${CATALOG:-registry/catalog.json}"
OUT="${OUT:-dist/registry-bundle}"
PREFIX="${PREFIX:-v1}"
VERSION="${VERSION:-0.0.0-local}"
REGISTRY_BASE="${REGISTRY_BASE:-https://registry.vpod.sh}"

# source snapshot in dist/ : id published to the registry
SNAPSHOTS=(
    "dist/alpine-3.23.0-256mb.snap:alpine-3.23.0-256mb"
    "dist/alpine-3.23.0-512mb.snap:vsnap-base-512mb"
    "dist/vsnap-data-512mb.snap:vsnap-data-512mb"
)

# published id : additional id serving the identical bytes
ALIASES=(
    "alpine-3.23.0-256mb:vsnap-base-256mb"
)

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    ./scripts/build-default-snapshot.sh
    ./scripts/build-default-snapshot.sh --ram 512
    ./scripts/build-data-snapshot.sh
fi

for pair in "${SNAPSHOTS[@]}"; do
    source_snapshot="${pair%%:*}"
    [ -f "$source_snapshot" ] || {
        echo "error: $source_snapshot missing (build it or drop SKIP_BUILD)" >&2
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

CATALOG="$CATALOG" OUT="$OUT" PREFIX="$PREFIX" VERSION="$VERSION" \
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
PY

echo
echo "bundle ready in $OUT (version $VERSION, targeting /$PREFIX/):"
ls -lh "$OUT"
