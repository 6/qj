#!/usr/bin/env bash
# Generate derived benchmark data files.
# Requires: twitter.json (run: bash benches/download_data.sh --json)
#
# Generates large_twitter.json (~50MB) from twitter.json.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DIR"

TWITTER="$DIR/twitter.json"
if [ ! -f "$TWITTER" ]; then
    echo "Error: $TWITTER not found. Run: bash benches/download_data.sh --json"
    exit 1
fi

# large_twitter.json (~50MB)
# Wrap twitter.json's statuses array repeated ~110x into a single large array.
LARGE_JSON="$DIR/large_twitter.json"
if [ -f "$LARGE_JSON" ]; then
    echo "large_twitter.json already exists ($(wc -c < "$LARGE_JSON" | tr -d ' ') bytes)"
else
    echo "Generating large_twitter.json (~50MB)..."
    STATUSES=$(jq -c '.statuses' "$TWITTER")
    {
        echo -n '{"statuses":['
        for i in $(seq 1 110); do
            if [ "$i" -gt 1 ]; then
                echo -n ','
            fi
            echo -n "$STATUSES" | sed 's/^\[//;s/\]$//'
        done
        echo ']}'
    } > "$LARGE_JSON"
    SIZE=$(wc -c < "$LARGE_JSON" | tr -d ' ')
    echo "  $SIZE bytes ($(( SIZE / 1024 / 1024 ))MB)"
fi

echo "Done. Benchmark data in $DIR/"
