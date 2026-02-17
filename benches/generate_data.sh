#!/usr/bin/env bash
# Generate derived benchmark data files.
# Requires: twitter.json (run: bash benches/download_data.sh --json)
#
# Flags:
#   --json      Generate large_twitter.json (~50MB) and large.jsonl (~50MB)
#   --ndjson    Generate synthetic 100k.ndjson and 1m.ndjson
#   --all       Generate everything
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DIR"

DO_JSON=false
DO_NDJSON=false

# Parse flags
if [ $# -eq 0 ]; then
    echo "Usage: bash benches/generate_data.sh [--json] [--ndjson] [--all]"
    exit 1
fi

for arg in "$@"; do
    case "$arg" in
        --json) DO_JSON=true ;;
        --ndjson) DO_NDJSON=true ;;
        --all) DO_JSON=true; DO_NDJSON=true ;;
        *) echo "Unknown flag: $arg"; exit 1 ;;
    esac
done

# --- Large JSON files (from twitter.json) ---
if $DO_JSON; then
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

    # large.jsonl (~50MB)
    # NDJSON: twitter.json statuses as individual lines, repeated.
    LARGE_JSONL="$DIR/large.jsonl"
    if [ -f "$LARGE_JSONL" ]; then
        echo "large.jsonl already exists ($(wc -c < "$LARGE_JSONL" | tr -d ' ') bytes)"
    else
        echo "Generating large.jsonl (~50MB)..."
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
    echo
fi

# --- Synthetic NDJSON files ---
if $DO_NDJSON; then
    cargo build --release --features bench --bin gen_ndjson 2>/dev/null
    GEN="$(cd "$(dirname "$0")/.." && pwd)/target/release/gen_ndjson"

    if [ ! -f "$DIR/100k.ndjson" ]; then
        echo "Generating 100k.ndjson (100K lines)..."
        "$GEN" 100000 > "$DIR/100k.ndjson"
        echo "  $(wc -c < "$DIR/100k.ndjson" | tr -d ' ') bytes"
    else
        echo "100k.ndjson already exists"
    fi

    if [ ! -f "$DIR/1m.ndjson" ]; then
        echo "Generating 1m.ndjson (1M lines)..."
        "$GEN" 1000000 > "$DIR/1m.ndjson"
        echo "  $(wc -c < "$DIR/1m.ndjson" | tr -d ' ') bytes"
    else
        echo "1m.ndjson already exists"
    fi

    echo "Done. NDJSON data in $DIR/"
fi
