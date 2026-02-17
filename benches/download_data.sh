#!/usr/bin/env bash
# Download benchmark data files.
#
# Flags:
#   --json        twitter.json (base file for generating large_twitter.json)
#   --gharchive   GH Archive NDJSON (default: 2 hours, ~1.1GB)
#     Size variants (combine with --gharchive):
#     --xsmall    1 hour  -> gharchive_xsmall.ndjson (~500MB)
#     --medium    6 hours -> gharchive_medium.ndjson (~3.4GB, ~1.2M records)
#     --large     24 hours -> gharchive_large.ndjson (~4.7GB)
#   --all         Download everything (json + gharchive default)
#
# Set QJ_GHARCHIVE_HOURS to override the hour count.
#   QJ_GHARCHIVE_HOURS=2 bash benches/download_data.sh --gharchive
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DIR"

DO_JSON=false
DO_GHARCHIVE=false
GHARCHIVE_SIZE=""

# Parse flags
if [ $# -eq 0 ]; then
    echo "Usage: bash benches/download_data.sh [--json] [--gharchive] [--xsmall|--medium|--large] [--all]"
    exit 1
fi

for arg in "$@"; do
    case "$arg" in
        --json) DO_JSON=true ;;
        --gharchive) DO_GHARCHIVE=true ;;
        --xsmall|--medium|--large) DO_GHARCHIVE=true; GHARCHIVE_SIZE="$arg" ;;
        --all) DO_JSON=true; DO_GHARCHIVE=true ;;
        *) echo "Unknown flag: $arg"; exit 1 ;;
    esac
done

# --- JSON benchmark files ---
if $DO_JSON; then
    SIMDJSON="https://raw.githubusercontent.com/simdjson/simdjson/master/jsonexamples"

    TWITTER="$DIR/twitter.json"
    if [ -f "$TWITTER" ] && [ "$(wc -c < "$TWITTER" | tr -d ' ')" -gt 100 ]; then
        echo "twitter.json already exists ($(wc -c < "$TWITTER" | tr -d ' ') bytes)"
    else
        echo "Downloading twitter.json..."
        curl -sL "$SIMDJSON/twitter.json" -o "$TWITTER"
        SIZE="$(wc -c < "$TWITTER" | tr -d ' ')"
        if [ "$SIZE" -lt 100 ]; then
            echo "  WARNING: download may have failed ($SIZE bytes)"
        else
            echo "  $SIZE bytes"
        fi
    fi
    echo "Done. JSON test data in $DIR/"
    echo
fi

# --- GH Archive NDJSON ---
if $DO_GHARCHIVE; then
    if [ "$GHARCHIVE_SIZE" = "--large" ]; then
        HOURS="${QJ_GHARCHIVE_HOURS:-24}"
        DATE="2026-02-01"
        SUFFIX="_large"
    elif [ "$GHARCHIVE_SIZE" = "--xsmall" ]; then
        HOURS="${QJ_GHARCHIVE_HOURS:-1}"
        DATE="2024-01-15"
        SUFFIX="_xsmall"
    elif [ "$GHARCHIVE_SIZE" = "--medium" ]; then
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

    # Skip if NDJSON already exists with >1MB
    if [ -f "$NDJSON" ] && [ "$(wc -c < "$NDJSON" | tr -d ' ')" -gt 1000000 ]; then
        echo "gharchive${SUFFIX}.ndjson already exists ($(wc -c < "$NDJSON" | tr -d ' ') bytes)"
        echo "Done. Delete to re-download."
    else
        # Download hourly dumps
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

        # Concatenate into NDJSON
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

        # Default variant also gets gzip for compressed-file benchmarks
        if [ -z "${SUFFIX:-}" ]; then
            NDJSON_GZ="$DIR/gharchive.ndjson.gz"
            echo "Building gharchive.ndjson.gz..."
            gzip -c "$NDJSON" > "$NDJSON_GZ"
            GZ_SIZE=$(wc -c < "$NDJSON_GZ" | tr -d ' ')
            echo "  $GZ_SIZE bytes ($(( GZ_SIZE / 1024 / 1024 ))MB)"
        fi

        # Cleanup temp files
        rm -rf "$TMPDIR"
        echo "Done. GH Archive data in $DIR/"
    fi
fi
