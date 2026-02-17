#!/usr/bin/env bash
# Download GH Archive hourly dumps and produce NDJSON files.
# Source: https://www.gharchive.org/
#
# Default (no flags):
#   2024-01-15, 2 hours -> gharchive.ndjson (~1.1GB), gharchive.json, gharchive.ndjson.gz
#
# --xsmall:
#   2024-01-15, 1 hour  -> gharchive_xsmall.ndjson (~500MB)
#
# --medium:
#   2024-01-15, 6 hours -> gharchive_medium.ndjson (~3.4GB, ~1.2M records)
#
# --large:
#   2026-02-01, 24 hours -> gharchive_large.ndjson (~6.2GB)
#
# Set QJ_GHARCHIVE_HOURS to override the hour count.
#   QJ_GHARCHIVE_HOURS=2 bash benches/download_gharchive.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DIR"

if [ "${1:-}" = "--large" ]; then
    HOURS="${QJ_GHARCHIVE_HOURS:-24}"
    DATE="2026-02-01"
    SUFFIX="_large"
elif [ "${1:-}" = "--xsmall" ]; then
    HOURS="${QJ_GHARCHIVE_HOURS:-1}"
    DATE="2024-01-15"
    SUFFIX="_xsmall"
elif [ "${1:-}" = "--medium" ]; then
    HOURS="${QJ_GHARCHIVE_HOURS:-6}"
    DATE="2024-01-15"
    SUFFIX="_medium"
else
    HOURS="${QJ_GHARCHIVE_HOURS:-2}"
    DATE="2024-01-15"
    SUFFIX=""
fi

NDJSON="$DIR/gharchive${SUFFIX}.ndjson"
TMPDIR="$DIR/.gharchive${SUFFIX}_tmp"

# --- Skip if NDJSON already exists with >1MB ---
if [ -f "$NDJSON" ] && [ "$(wc -c < "$NDJSON" | tr -d ' ')" -gt 1000000 ]; then
    echo "gharchive${SUFFIX}.ndjson already exists ($(wc -c < "$NDJSON" | tr -d ' ') bytes)"
    echo "Done. Delete to re-download."
    exit 0
fi

# --- Download hourly dumps ---
mkdir -p "$TMPDIR"
echo "Downloading $HOURS hours of GH Archive data ($DATE)..."
for h in $(seq 0 $((HOURS - 1))); do
    GZ="$TMPDIR/${DATE}-${h}.json.gz"
    if [ -f "$GZ" ] && [ "$(wc -c < "$GZ" | tr -d ' ')" -gt 1000 ]; then
        echo "  hour $h: cached"
        continue
    fi
    URL="https://data.gharchive.org/${DATE}-${h}.json.gz"
    echo "  hour $h: downloading..."
    if ! curl -sL --fail "$URL" -o "$GZ"; then
        echo "  WARNING: failed to download hour $h, skipping"
        rm -f "$GZ"
    fi
done

# --- Concatenate into NDJSON ---
echo "Building gharchive${SUFFIX}.ndjson..."
: > "$NDJSON"
for h in $(seq 0 $((HOURS - 1))); do
    GZ="$TMPDIR/${DATE}-${h}.json.gz"
    if [ -f "$GZ" ]; then
        gunzip -c "$GZ" >> "$NDJSON"
    fi
done
NDJSON_SIZE=$(wc -c < "$NDJSON" | tr -d ' ')
echo "  $NDJSON_SIZE bytes ($(( NDJSON_SIZE / 1024 / 1024 ))MB)"

# --- Default variant also gets JSON array + gzip for benchmarks ---
if [ -z "${SUFFIX:-}" ]; then
    JSON="$DIR/gharchive.json"
    echo "Building gharchive.json (NDJSON -> JSON array)..."
    awk 'BEGIN { printf "[" }
         NR > 1 { printf "," }
         { printf "%s", $0 }
         END { printf "]\n" }' "$NDJSON" > "$JSON"
    JSON_SIZE=$(wc -c < "$JSON" | tr -d ' ')
    echo "  $JSON_SIZE bytes ($(( JSON_SIZE / 1024 / 1024 ))MB)"

    NDJSON_GZ="$DIR/gharchive.ndjson.gz"
    echo "Building gharchive.ndjson.gz..."
    gzip -c "$NDJSON" > "$NDJSON_GZ"
    GZ_SIZE=$(wc -c < "$NDJSON_GZ" | tr -d ' ')
    echo "  $GZ_SIZE bytes ($(( GZ_SIZE / 1024 / 1024 ))MB)"
fi

# --- Cleanup temp files ---
rm -rf "$TMPDIR"
echo "Done. GH Archive data in $DIR/"
