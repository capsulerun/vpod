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
REGISTRY_BASE="$REGISTRY_BASE" SKIP_LIVE="${SKIP_LIVE:-0}" python3 - <<'PY'
import hashlib, json, os, urllib.request
from pathlib import Path

out = Path(os.environ["OUT"])
prefix = os.environ["PREFIX"].strip("/")
base = os.environ["REGISTRY_BASE"].rstrip("/")
skip_live = os.environ["SKIP_LIVE"] == "1"

snapshots = json.loads(Path(os.environ["CATALOG"]).read_text())["snapshots"]

live_by_id = {}
missing = [e["id"] for e in snapshots if not (out / f"{e['id']}.snap").exists()]

if missing and not skip_live:
    # Cloudflare rejects urllib's default UA with a 403.
    url = f"{base}/{prefix}/snapshots.json"
    request = urllib.request.Request(url, headers={"User-Agent": "vpod-ci"})
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            live = json.load(response)
        live_by_id = {entry["id"]: entry for entry in live["snapshots"]}
        print(f"borrowing entries for {', '.join(missing)} from {url}")
    except Exception as error:
        raise SystemExit(f"cannot read {url} to fill in {missing}: {error}")

for entry in snapshots:
    entry["url"] = f"{base}/{prefix}/{entry['id']}.snap"
    built = out / f"{entry['id']}.snap"

    if built.exists():
        data = built.read_bytes()
        entry["sha256"] = hashlib.sha256(data).hexdigest()
        entry["size"] = len(data)
        print(f"  {entry['id']}: sha256={entry['sha256'][:12]}… size={entry['size']:,}")
        continue

    previous = live_by_id.get(entry["id"])
    if previous is None:
        raise SystemExit(
            f"{entry['id']}: not built this run and not in the live registry "
            f"— no bytes to publish"
        )
    entry["sha256"] = previous["sha256"]
    entry["size"] = previous["size"]
    print(f"  {entry['id']}: not rebuilt, keeping live registry entry")

manifest = {"version": os.environ["VERSION"], "snapshots": snapshots}
(out / "snapshots.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY

echo
echo "bundle ready in $OUT (version $VERSION, targeting /$PREFIX/):"
ls -lh "$OUT"
