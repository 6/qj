#!/usr/bin/env bash
# Download standard JSON benchmark files.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DIR"

SIMDJSON="https://raw.githubusercontent.com/simdjson/simdjson/master/jsonexamples"
# canada.json was removed from simdjson repo; use nativejson-benchmark instead.
NATIVEJSON="https://raw.githubusercontent.com/miloyip/nativejson-benchmark/master/data"

download() {
    local url="$1"
    local dest="$2"
    local name
    name="$(basename "$dest")"
    if [ -f "$dest" ] && [ "$(wc -c < "$dest" | tr -d ' ')" -gt 100 ]; then
        echo "$name already exists ($(wc -c < "$dest" | tr -d ' ') bytes)"
    else
        echo "Downloading $name..."
        curl -sL "$url" -o "$dest"
        local size
        size="$(wc -c < "$dest" | tr -d ' ')"
        if [ "$size" -lt 100 ]; then
            echo "  WARNING: download may have failed ($size bytes)"
        else
            echo "  $size bytes"
        fi
    fi
}

download "$SIMDJSON/twitter.json" "$DIR/twitter.json"
download "$SIMDJSON/citm_catalog.json" "$DIR/citm_catalog.json"
download "$NATIVEJSON/canada.json" "$DIR/canada.json"

echo "Done. Test data in $DIR/"
