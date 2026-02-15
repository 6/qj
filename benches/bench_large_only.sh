#!/usr/bin/env bash
# Quick single-pass benchmark of the large GH Archive dataset across all 4 tools.
# Uses hyperfine with --runs 1, no warmup. Writes results to benches/results_large_only.md.
#
# Usage:
#   bash benches/bench_large_only.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
DATA="$DIR/data"
OUTPUT="$DIR/results_large_only.md"
RESULTS_DIR="$DIR/.bench_large_tmp"

NDJSON="$DATA/gharchive_large.ndjson"
JSON="$DATA/gharchive_large.json"

if [ ! -f "$NDJSON" ] || [ ! -f "$JSON" ]; then
    echo "Error: large GH Archive files not found. Run:"
    echo "  bash benches/download_gharchive.sh --large"
    exit 1
fi

NDJSON_SIZE=$(wc -c < "$NDJSON" | tr -d ' ')
JSON_SIZE=$(wc -c < "$JSON" | tr -d ' ')
NDJSON_MB=$(python3 -c "print(f'{$NDJSON_SIZE/1024/1024:.0f}')")
JSON_MB=$(python3 -c "print(f'{$JSON_SIZE/1024/1024:.0f}')")

# --- Discover tools ---
QJ="./target/release/qj"
if [ ! -x "$QJ" ]; then
    echo "Building qj in release mode..."
    cargo build --release
fi

declare -a TOOLS=("$QJ")
declare -a NAMES=("qj")

for tool in jq jaq gojq; do
    path=$(which "$tool" 2>/dev/null || true)
    if [ -n "$path" ]; then
        TOOLS+=("$path")
        NAMES+=("$tool")
    else
        echo "Note: $tool not found, skipping"
    fi
done

echo "Tools: ${NAMES[*]}"
echo "NDJSON: $NDJSON (${NDJSON_MB}MB)"
echo "JSON:   $JSON (${JSON_MB}MB)"
echo ""

mkdir -p "$RESULTS_DIR"

# --- Filters (2 per format) ---
NDJSON_FILTER_NAMES=("passthrough" "select + construct")
NDJSON_FILTER_FLAGS=("-c" "-c")
NDJSON_FILTER_EXPRS=(
    "."
    'select(.type == "PushEvent") | {actor: .actor.login, commits: (.payload.commits // [] | length)}'
)

JSON_FILTER_NAMES=("passthrough" "iterate + filter")
JSON_FILTER_FLAGS=("-c" "-c")
JSON_FILTER_EXPRS=(
    "."
    '[.[] | select(.type == "PushEvent")]'
)

# --- Run hyperfine for one filter across all tools ---
run_filter() {
    local section="$1"
    local idx="$2"
    local flags="$3"
    local expr="$4"
    local file="$5"

    local json_out="$RESULTS_DIR/${section}_${idx}.json"

    local hyp_args=(hyperfine --warmup 0 --runs 3 --ignore-failure --export-json "$json_out")
    for t in "${!TOOLS[@]}"; do
        local cmd="${TOOLS[$t]} $flags '$expr' '$file'"
        hyp_args+=("$cmd")
    done

    "${hyp_args[@]}"
    echo ""
}

# --- Parse median from hyperfine JSON ---
parse_median() {
    local json_file="$1"
    local tool_idx="$2"
    python3 -c "
import json
with open('$json_file') as f:
    data = json.load(f)
results = data.get('results', [])
idx = $tool_idx
if idx < len(results):
    print(f'{results[idx][\"median\"]:.4f}')
else:
    print('0')
"
}

format_time() {
    python3 -c "
t = $1
if t >= 1.0:
    print(f'{t:.2f}s')
elif t < 0.001:
    print('<1ms')
else:
    print(f'{t*1000:.0f}ms')
"
}

PLATFORM=$(uname -ms)
DATE=$(date +%Y-%m-%d)

# --- Run benchmarks ---
echo "=== NDJSON benchmarks (gharchive_large.ndjson, ${NDJSON_MB}MB) ==="
echo ""
for i in "${!NDJSON_FILTER_NAMES[@]}"; do
    echo "--- ${NDJSON_FILTER_NAMES[$i]} ---"
    run_filter "ndjson" "$i" "${NDJSON_FILTER_FLAGS[$i]}" "${NDJSON_FILTER_EXPRS[$i]}" "$NDJSON"
done

echo "=== JSON benchmarks (gharchive_large.json, ${JSON_MB}MB) ==="
echo ""
for i in "${!JSON_FILTER_NAMES[@]}"; do
    echo "--- ${JSON_FILTER_NAMES[$i]} ---"
    run_filter "json" "$i" "${JSON_FILTER_FLAGS[$i]}" "${JSON_FILTER_EXPRS[$i]}" "$JSON"
done

# --- Generate markdown ---
HEADER="| Filter | File |"
SEP="|--------|------|"
for name in "${NAMES[@]}"; do
    HEADER+=" $name |"
    SEP+="------|"
done

{
    echo "# Large GH Archive Benchmark"
    echo ""
    echo "> Benchmark on gharchive_large (24h of 2026-02-01, ${NDJSON_MB}MB)."
    echo "> Generated: $DATE on \`$PLATFORM\`"
    echo "> 3 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine)."
    echo ""

    echo "### NDJSON (${NDJSON_MB}MB, parallel processing)"
    echo ""
    echo "$HEADER"
    echo "$SEP"
    for i in "${!NDJSON_FILTER_NAMES[@]}"; do
        flags="${NDJSON_FILTER_FLAGS[$i]}"
        expr="${NDJSON_FILTER_EXPRS[$i]}"
        display="$flags '$expr'"
        row="| \`$display\` | gharchive_large.ndjson |"
        json_file="$RESULTS_DIR/ndjson_${i}.json"
        for t in "${!TOOLS[@]}"; do
            median=$(parse_median "$json_file" "$t")
            formatted=$(format_time "$median")
            if [ "${NAMES[$t]}" = "qj" ]; then
                row+=" **$formatted** |"
            else
                row+=" $formatted |"
            fi
        done
        echo "$row"
    done

    echo ""
    echo "### JSON (${JSON_MB}MB, single document)"
    echo ""
    echo "$HEADER"
    echo "$SEP"
    for i in "${!JSON_FILTER_NAMES[@]}"; do
        flags="${JSON_FILTER_FLAGS[$i]}"
        expr="${JSON_FILTER_EXPRS[$i]}"
        display="$flags '$expr'"
        row="| \`$display\` | gharchive_large.json |"
        json_file="$RESULTS_DIR/json_${i}.json"
        for t in "${!TOOLS[@]}"; do
            median=$(parse_median "$json_file" "$t")
            formatted=$(format_time "$median")
            if [ "${NAMES[$t]}" = "qj" ]; then
                row+=" **$formatted** |"
            else
                row+=" $formatted |"
            fi
        done
        echo "$row"
    done

    # Throughput from passthrough
    echo ""
    echo "### Throughput"
    echo ""
    echo "Peak throughput (\`-c '.'\`, single pass):"
    echo ""
    tp_header="| File |"
    tp_sep="|------|"
    for name in "${NAMES[@]}"; do
        tp_header+=" $name |"
        tp_sep+="------|"
    done
    echo "$tp_header"
    echo "$tp_sep"

    for section_info in "ndjson:gharchive_large.ndjson:$NDJSON_SIZE" "json:gharchive_large.json:$JSON_SIZE"; do
        IFS=: read -r section file bytes <<< "$section_info"
        json_file="$RESULTS_DIR/${section}_0.json"
        row="| $file |"
        for t in "${!TOOLS[@]}"; do
            median=$(parse_median "$json_file" "$t")
            tp=$(python3 -c "
bytes=$bytes; secs=$median
if secs <= 0:
    print('-')
else:
    mbps = bytes / secs / (1024*1024)
    if mbps >= 1024:
        print(f'{mbps/1024:.1f} GB/s')
    else:
        print(f'{mbps:.0f} MB/s')
")
            if [ "${NAMES[$t]}" = "qj" ]; then
                row+=" **$tp** |"
            else
                row+=" $tp |"
            fi
        done
        echo "$row"
    done
} > "$OUTPUT"

echo "=== Results written to $OUTPUT ==="
echo "Raw hyperfine JSON in $RESULTS_DIR/"
