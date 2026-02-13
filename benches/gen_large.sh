#!/usr/bin/env bash
# Generate large benchmark files from twitter.json test data.
# Requires: jq
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
TWITTER="$DIR/twitter.json"

if [ ! -f "$TWITTER" ]; then
    echo "Error: $TWITTER not found. Run benches/download_testdata.sh first."
    exit 1
fi

# --- large_twitter.json (~50MB) ---
# Wrap twitter.json's statuses array repeated ~80x into a single large array.
LARGE_JSON="$DIR/large_twitter.json"
if [ -f "$LARGE_JSON" ]; then
    echo "large_twitter.json already exists ($(wc -c < "$LARGE_JSON" | tr -d ' ') bytes)"
else
    echo "Generating large_twitter.json (~50MB)..."
    # Extract the statuses array once, then repeat it ~80 times into one big array.
    STATUSES=$(jq -c '.statuses' "$TWITTER")
    {
        echo -n '{"statuses":['
        for i in $(seq 1 110); do
            if [ "$i" -gt 1 ]; then
                echo -n ','
            fi
            # Strip the outer [] and emit the elements
            echo -n "$STATUSES" | sed 's/^\[//;s/\]$//'
        done
        echo ']}'
    } > "$LARGE_JSON"
    SIZE=$(wc -c < "$LARGE_JSON" | tr -d ' ')
    echo "  $SIZE bytes ($(( SIZE / 1024 / 1024 ))MB)"
fi

# --- large.jsonl (~50MB) ---
# NDJSON: twitter.json statuses as individual lines, repeated.
LARGE_JSONL="$DIR/large.jsonl"
if [ -f "$LARGE_JSONL" ]; then
    echo "large.jsonl already exists ($(wc -c < "$LARGE_JSONL" | tr -d ' ') bytes)"
else
    echo "Generating large.jsonl (~50MB)..."
    # Extract each status as a compact JSON line
    LINES=$(jq -c '.statuses[]' "$TWITTER")
    {
        for i in $(seq 1 110); do
            echo "$LINES"
        done
    } > "$LARGE_JSONL"
    SIZE=$(wc -c < "$LARGE_JSONL" | tr -d ' ')
    echo "  $SIZE bytes ($(( SIZE / 1024 / 1024 ))MB)"
fi

echo "Done. Large benchmark data in $DIR/"
