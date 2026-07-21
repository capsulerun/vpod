#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

TEMPLATE="${TEMPLATE:-docs/v0.4.1-registry/test/snapshots.json}"
OUT="${OUT:-dist/registry-bundle}"

if [ "${SKIP_BUILD:-0}" != "1" ]; then
    ./scripts/build-default-snapshot.sh
    ./scripts/build-data-snapshot.sh
fi

for snap in dist/alpine-3.23.0-256mb.snap dist/vsnap-data-512mb.snap; do
    [ -f "$snap" ] || { echo "error: $snap missing (build it or drop SKIP_BUILD)"; exit 1; }
done

mkdir -p "$OUT"
lz4 -9 -f dist/alpine-3.23.0-256mb.snap "$OUT/alpine-3.23.0-256mb.snap"
cp "$OUT/alpine-3.23.0-256mb.snap" "$OUT/vsnap-base-256mb.snap"
lz4 -9 -f dist/vsnap-data-512mb.snap "$OUT/vsnap-data-512mb.snap"

TEMPLATE="$TEMPLATE" OUT="$OUT" python3 - <<'PY'
import hashlib, json, os
from pathlib import Path

out = Path(os.environ["OUT"])
manifest = json.loads(Path(os.environ["TEMPLATE"]).read_text())

if os.environ.get("VERSION"):
    manifest["version"] = os.environ["VERSION"]

for entry in manifest["snapshots"]:
    data = (out / f"{entry['id']}.snap").read_bytes()
    entry["sha256"] = hashlib.sha256(data).hexdigest()
    entry["size"] = len(data)
    print(f"  {entry['id']}: sha256={entry['sha256'][:12]}… size={entry['size']:,}")

(out / "snapshots.json").write_text(json.dumps(manifest, indent=2) + "\n")
PY

echo
echo "bundle ready in $OUT:"
ls -lh "$OUT"
