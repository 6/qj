#!/usr/bin/env bash
# Master script: generate/download all benchmark data.
# Each sub-script is idempotent — safe to re-run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Setting up all benchmark data ==="
echo

bash "$SCRIPT_DIR/download_data.sh" --all        # json test files + gharchive.ndjson
echo
bash "$SCRIPT_DIR/download_data.sh" --large       # gharchive_large.ndjson
echo
bash "$SCRIPT_DIR/generate_data.sh" --all         # large_twitter.json, large.jsonl, 100k/1m.ndjson
echo

echo "=== All benchmark data ready ==="
