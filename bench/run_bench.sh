#!/usr/bin/env bash
# Benchmark jx vs jq vs jaq vs gojq on small and large files.
# Requires: hyperfine, jq, jaq, gojq, and a release build of jx.
set -euo pipefail

JX="./target/release/jx"
DATA="bench/data"

if [ ! -f "$JX" ]; then
    echo "Error: $JX not found. Run: cargo build --release"
    exit 1
fi

for tool in jq jaq gojq hyperfine; do
    if ! command -v "$tool" &>/dev/null; then
        echo "Warning: $tool not found, skipping it"
    fi
done

# Note: filters must not contain spaces since we use --shell=none
# (hyperfine splits on whitespace). Use e.g. .statuses[]|.user.name not
# .statuses[] | .user.name

run_bench() {
    local label="$1"
    shift
    echo "--- $label ---"
    hyperfine --warmup 3 --shell=none "$@" 2>&1
    echo ""
}

echo "=== Small file: twitter.json ($(wc -c < "$DATA/twitter.json" | tr -d ' ') bytes) ==="
echo ""

run_bench "Filter: -c '.' (identity compact)" \
    "$JX -c . $DATA/twitter.json" \
    "jq -c . $DATA/twitter.json" \
    "jaq -c . $DATA/twitter.json" \
    "gojq -c . $DATA/twitter.json"

run_bench "Filter: -c '.statuses' (field compact)" \
    "$JX -c .statuses $DATA/twitter.json" \
    "jq -c .statuses $DATA/twitter.json" \
    "jaq -c .statuses $DATA/twitter.json" \
    "gojq -c .statuses $DATA/twitter.json"

run_bench "Filter: '.statuses|length' (pipe+builtin)" \
    "$JX .statuses|length $DATA/twitter.json" \
    "jq .statuses|length $DATA/twitter.json" \
    "jaq .statuses|length $DATA/twitter.json" \
    "gojq .statuses|length $DATA/twitter.json"

run_bench "Filter: '.statuses[]|.user.name' (iterate+field)" \
    "$JX .statuses[]|.user.name $DATA/twitter.json" \
    "jq .statuses[]|.user.name $DATA/twitter.json" \
    "jaq .statuses[]|.user.name $DATA/twitter.json" \
    "gojq .statuses[]|.user.name $DATA/twitter.json"

if [ ! -f "$DATA/large_twitter.json" ]; then
    echo "Skipping large file benchmarks. Run: bash bench/gen_large.sh"
    exit 0
fi

echo "=== Large file: large_twitter.json ($(wc -c < "$DATA/large_twitter.json" | tr -d ' ') bytes) ==="
echo ""

run_bench "Filter: -c '.' (identity compact)" \
    "$JX -c . $DATA/large_twitter.json" \
    "jq -c . $DATA/large_twitter.json" \
    "jaq -c . $DATA/large_twitter.json" \
    "gojq -c . $DATA/large_twitter.json"

run_bench "Filter: -c '.statuses' (field compact)" \
    "$JX -c .statuses $DATA/large_twitter.json" \
    "jq -c .statuses $DATA/large_twitter.json" \
    "jaq -c .statuses $DATA/large_twitter.json" \
    "gojq -c .statuses $DATA/large_twitter.json"

run_bench "Filter: '.statuses|length' (pipe+builtin)" \
    "$JX .statuses|length $DATA/large_twitter.json" \
    "jq .statuses|length $DATA/large_twitter.json" \
    "jaq .statuses|length $DATA/large_twitter.json" \
    "gojq .statuses|length $DATA/large_twitter.json"

run_bench "Filter: '.statuses[]|.user.name' (iterate+field)" \
    "$JX .statuses[]|.user.name $DATA/large_twitter.json" \
    "jq .statuses[]|.user.name $DATA/large_twitter.json" \
    "jaq .statuses[]|.user.name $DATA/large_twitter.json" \
    "gojq .statuses[]|.user.name $DATA/large_twitter.json"

echo "=== Done ==="
