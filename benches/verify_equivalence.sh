#!/usr/bin/env bash
# Verify qj output matches jq for each benchmarked filter on GH Archive data.
# Reports per-filter pass/fail and diff line counts on mismatch.
#
# Usage:
#   bash benches/verify_equivalence.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
DATA="$DIR/data"
NDJSON="$DATA/gharchive.ndjson"
QJ="./target/release/qj"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

if [ ! -x "$QJ" ]; then
    echo "Building qj in release mode..."
    cargo build --release
fi

if ! command -v jq &>/dev/null; then
    echo "Error: jq not found"
    exit 1
fi

if [ ! -f "$NDJSON" ]; then
    echo "Error: $NDJSON not found. Run:"
    echo "  bash benches/download_gharchive.sh"
    exit 1
fi

# Same filters as bench_tools.rs NDJSON_FILTERS
FILTER_NAMES=("field" "length" "keys" "select" "select+field" "reshape" "evaluator" "evaluator (complex)")
FILTER_FLAGS=("" "-c" "-c" "-c" "-c" "-c" "-c" "-c")
FILTER_EXPRS=(
    '.actor.login'
    'length'
    'keys'
    'select(.type == "PushEvent")'
    'select(.type == "PushEvent") | .payload.size'
    '{type, repo: .repo.name, actor: .actor.login}'
    '{type, commits: [.payload.commits[]?.message]}'
    '{type, commits: (.payload.commits // [] | length)}'
)

PASS=0
FAIL=0

echo "Verifying qj vs jq output equivalence on $(basename "$NDJSON")"
echo ""

for i in "${!FILTER_NAMES[@]}"; do
    name="${FILTER_NAMES[$i]}"
    flags="${FILTER_FLAGS[$i]}"
    expr="${FILTER_EXPRS[$i]}"

    qj_out="$TMP_DIR/qj_${i}.out"
    jq_out="$TMP_DIR/jq_${i}.out"

    printf "%-25s" "$name"

    # shellcheck disable=SC2086
    "$QJ" $flags "$expr" "$NDJSON" > "$qj_out" 2>/dev/null || true
    # shellcheck disable=SC2086
    jq $flags "$expr" "$NDJSON" > "$jq_out" 2>/dev/null || true

    if cmp -s "$qj_out" "$jq_out"; then
        echo "PASS (byte-identical)"
        PASS=$((PASS + 1))
    else
        diff_lines=$(diff "$qj_out" "$jq_out" | grep -c '^[<>]' || true)
        echo "DIFF ($diff_lines lines differ)"
        FAIL=$((FAIL + 1))
    fi
done

echo ""
echo "--- Summary: $PASS passed, $FAIL differ ---"

if [ "$FAIL" -gt 0 ]; then
    echo ""
    echo "To inspect differences:"
    echo "  diff <(qj -c 'FILTER' $NDJSON) <(jq -c 'FILTER' $NDJSON) | head -20"
fi
