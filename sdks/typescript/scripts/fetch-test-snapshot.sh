#!/usr/bin/env bash

set -euo pipefail

url="${1:?usage: fetch-test-snapshot.sh <url> <output-path>}"
output="${2:?usage: fetch-test-snapshot.sh <url> <output-path>}"

LZ4_MAGIC=04224d18

magic_of_stdin() {
    head -c 4 | xxd -p | tr -d '\n'
}

magic_of() {
    magic_of_stdin < "$1"
}

magic_after_one_decode() {
    { lz4 -dc "$1" 2>/dev/null || true; } | magic_of_stdin
}

curl -sSf -o "$output" "$url"
echo "fetched $(wc -c < "$output" | tr -d ' ') bytes from $url"

layers_removed=0
while [ "$(magic_of "$output")" = "$LZ4_MAGIC" ] &&
      [ "$(magic_after_one_decode "$output")" = "$LZ4_MAGIC" ]; do
    lz4 -dc "$output" > "$output.peeled"
    mv "$output.peeled" "$output"
    layers_removed=$((layers_removed + 1))
    echo "peeled one lz4 layer, now $(wc -c < "$output" | tr -d ' ') bytes"
done

outer=$(magic_of "$output")

if [ "$outer" = "$LZ4_MAGIC" ]; then
    inner=$(magic_after_one_decode "$output")
    if [ "$inner" != "56504f44" ]; then
        echo "after one lz4 layer the payload is '$inner', not VPOD" >&2
        exit 1
    fi
    echo "ready: one lz4 layer wrapping VPOD ($layers_removed peeled)"
    exit 0
fi

if [ "$outer" = "56504f44" ]; then
    echo "ready: raw VPOD, no lz4 framing ($layers_removed peeled)"
    exit 0
fi

echo "not a snapshot: starts with '$outer', expected lz4 or VPOD" >&2
exit 1
