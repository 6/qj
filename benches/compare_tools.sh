#!/usr/bin/env bash
# Compare jq and jaq on the benchmark files using hyperfine.
# jx doesn't have filter support yet — this establishes baseline numbers.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"

if ! command -v hyperfine &>/dev/null; then
    echo "Error: hyperfine not found. Install with: brew install hyperfine" >&2
    exit 1
fi

if ! command -v jq &>/dev/null; then
    echo "Error: jq not found." >&2
    exit 1
fi

# Check for jaq (optional)
JAQ_CMD=""
if command -v jaq &>/dev/null; then
    JAQ_CMD="jaq"
fi

echo "=== Single-file benchmarks ==="
for file in twitter.json citm_catalog.json canada.json; do
    if [ ! -f "$DIR/$file" ]; then
        echo "Skipping $file (not found). Run download_testdata.sh first."
        continue
    fi
    echo ""
    echo "--- $file ($(wc -c < "$DIR/$file" | tr -d ' ') bytes) ---"
    cmds=("jq '.' '$DIR/$file' > /dev/null")
    if [ -n "$JAQ_CMD" ]; then
        cmds+=("$JAQ_CMD '.' '$DIR/$file' > /dev/null")
    fi
    hyperfine --warmup 3 "${cmds[@]}"
done

echo ""
echo "=== NDJSON benchmarks ==="
for file in 100k.ndjson 1m.ndjson; do
    if [ ! -f "$DIR/$file" ]; then
        echo "Skipping $file (not found). Run generate_ndjson.sh first."
        continue
    fi
    echo ""
    echo "--- $file ($(wc -c < "$DIR/$file" | tr -d ' ') bytes) ---"
    cmds=("jq -c '.name' '$DIR/$file' > /dev/null")
    if [ -n "$JAQ_CMD" ]; then
        cmds+=("$JAQ_CMD -c '.name' '$DIR/$file' > /dev/null")
    fi
    hyperfine --warmup 3 "${cmds[@]}"
done
