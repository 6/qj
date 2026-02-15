#!/usr/bin/env bash
# Master script: generate/download all benchmark data.
# Each sub-script is idempotent — safe to re-run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "=== Setting up all benchmark data ==="
echo

bash "$SCRIPT_DIR/download_testdata.sh"    # twitter.json, citm_catalog.json, canada.json
echo
bash "$SCRIPT_DIR/gen_large.sh"            # large_twitter.json, large.jsonl
echo
bash "$SCRIPT_DIR/generate_ndjson.sh"      # 100k.ndjson, 1m.ndjson
echo
bash "$SCRIPT_DIR/download_gharchive.sh"   # gharchive.ndjson, gharchive.json
echo

echo "=== All benchmark data ready ==="
