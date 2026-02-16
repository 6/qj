#!/usr/bin/env bash
# Benchmark qj/jq/jaq/gojq on GH Archive NDJSON (~1.1GB).
# Eight workloads from fast-path to evaluator-bound.
#
# Features:
#   - Non-zero exit code detection (appends * to times, with footnote)
#   - "vs jq" speedup column per tool
#
# Note: hyperfine redirects stdout to /dev/null by default (--output=null),
# so benchmarks measure compute + formatting, not terminal/pipe IO.
#
# Usage:
#   bash benches/bench_large_only.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
DATA="$DIR/data"
OUTPUT="$DIR/results_large_only.md"
RESULTS_DIR="$DIR/.bench_large_tmp"

# --- Discover tools ---
QJ="./target/release/qj"
if [ ! -x "$QJ" ]; then
    echo "Building qj in release mode..."
    cargo build --release
fi

declare -a TOOLS=("$QJ" "$QJ --threads 1")
declare -a NAMES=("qj" "qj (1T)")

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
echo ""

mkdir -p "$RESULTS_DIR"

# --- Data file ---
NDJSON="$DATA/gharchive.ndjson"
if [ ! -f "$NDJSON" ]; then
    echo "Error: $NDJSON not found. Run:"
    echo "  bash benches/download_gharchive.sh"
    exit 1
fi

# --- Filters (fast-path spectrum → evaluator-bound) ---
FILTER_NAMES=("field" "length" "keys" "type" "has" "select" "select+field" "reshape" "evaluator" "evaluator (complex)")
FILTER_FLAGS=("" "-c" "-c" "-c" "-c" "-c" "-c" "-c" "-c" "-c")
FILTER_EXPRS=(
    '.actor.login'
    'length'
    'keys'
    'type'
    'has("actor")'
    'select(.type == "PushEvent")'
    'select(.type == "PushEvent") | .payload.size'
    '{type, repo: .repo.name, actor: .actor.login}'
    '{type, commits: [.payload.commits[]?.message]}'
    '{type, commits: (.payload.commits // [] | length)}'
)

# --- Run hyperfine for one filter across all tools ---
run_filter() {
    local section="$1"
    local idx="$2"
    local flags="$3"
    local expr="$4"
    local file="$5"

    local json_out="$RESULTS_DIR/${section}_${idx}.json"

    local hyp_args=(caffeinate -dims hyperfine --warmup 1 --runs 3 --ignore-failure --export-json "$json_out")
    for t in "${!TOOLS[@]}"; do
        local cmd="${TOOLS[$t]} $flags '$expr' '$file'"
        hyp_args+=("$cmd")
    done

    "${hyp_args[@]}"
    echo ""
}

# --- Parse median and exit code status from hyperfine JSON ---
# Returns "median,failed" where failed is "1" or "0"
parse_result() {
    local json_file="$1"
    local tool_idx="$2"
    python3 -c "
import json
with open('$json_file') as f:
    data = json.load(f)
results = data.get('results', [])
idx = $tool_idx
if idx < len(results):
    median = results[idx]['median']
    exit_codes = results[idx].get('exit_codes', [0])
    failed = 1 if any(c != 0 for c in exit_codes) else 0
    print(f'{median:.4f},{failed}')
else:
    print('0,0')
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

format_speedup() {
    python3 -c "
jq_time = $1
tool_time = $2
if tool_time > 0 and jq_time > 0:
    ratio = jq_time / tool_time
    print(f'{ratio:.1f}x')
else:
    print('-')
"
}

if [[ "$(uname -s)" == "Darwin" ]]; then
    CHIP=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "Apple Silicon")
    RAM=$(sysctl -n hw.memsize 2>/dev/null | awk '{printf "%d GB", $1/1073741824}')
    PLATFORM="$CHIP ($RAM)"
else
    PLATFORM=$(uname -ms)
fi
DATE=$(date +%Y-%m-%d)

# --- Run all benchmarks ---
NDJSON_SIZE=$(wc -c < "$NDJSON" | tr -d ' ')
NDJSON_MB=$(python3 -c "print(f'{$NDJSON_SIZE/1024/1024:.0f}')")
NDJSON_BASENAME=$(basename "$NDJSON")

echo "=== NDJSON ($NDJSON_BASENAME, ${NDJSON_MB}MB) ==="
echo ""
for i in "${!FILTER_NAMES[@]}"; do
    echo "--- ${FILTER_NAMES[$i]} ---"
    run_filter "ndjson" "$i" "${FILTER_FLAGS[$i]}" "${FILTER_EXPRS[$i]}" "$NDJSON"
done

# --- Generate markdown ---
HAS_FAILURES=0

{
    echo "# GH Archive Benchmark"
    echo ""
    echo "> Generated: $DATE on \`$PLATFORM\`"
    echo "> 3 runs, 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine)."
    echo ""
    echo "### NDJSON (${NDJSON_BASENAME}, ${NDJSON_MB}MB, parallel processing)"
    echo ""

    # Build header with vs jq columns (only for qj variants)
    HEADER="| Filter |"
    SEP="|--------|"
    for name in "${NAMES[@]}"; do
        if [ "$name" = "qj" ]; then
            HEADER+=" **qj** |"
        else
            HEADER+=" $name |"
        fi
        SEP+="------:|"
        if [[ "$name" == qj* ]]; then
            HEADER+=" vs jq |"
            SEP+="------:|"
        fi
    done
    echo "$HEADER"
    echo "$SEP"

    # Find jq index
    jq_idx=-1
    for t in "${!NAMES[@]}"; do
        if [ "${NAMES[$t]}" = "jq" ]; then
            jq_idx=$t
            break
        fi
    done

    for i in "${!FILTER_NAMES[@]}"; do
        flags="${FILTER_FLAGS[$i]}"
        expr="${FILTER_EXPRS[$i]}"
        display="$flags '$expr'"
        row="| \`$display\` |"
        json_file="$RESULTS_DIR/ndjson_${i}.json"

        # Get jq median for speedup calculation
        jq_median=0
        if [ "$jq_idx" -ge 0 ]; then
            jq_result=$(parse_result "$json_file" "$jq_idx")
            jq_median=$(echo "$jq_result" | cut -d, -f1)
        fi

        for t in "${!TOOLS[@]}"; do
            result=$(parse_result "$json_file" "$t")
            median=$(echo "$result" | cut -d, -f1)
            failed=$(echo "$result" | cut -d, -f2)
            formatted=$(format_time "$median")
            if [ "$failed" = "1" ]; then
                formatted="${formatted}*"
                HAS_FAILURES=1
            fi
            if [ "${NAMES[$t]}" = "qj" ]; then
                row+=" **$formatted** |"
            else
                row+=" $formatted |"
            fi
            # Add vs jq column (only for qj variants)
            if [[ "${NAMES[$t]}" == qj* ]]; then
                speedup=$(format_speedup "$jq_median" "$median")
                if [ "${NAMES[$t]}" = "qj" ]; then
                    row+=" **$speedup** |"
                else
                    row+=" $speedup |"
                fi
            fi
        done
        echo "$row"
    done

    echo ""

    # --- Footnotes ---
    if [ "$HAS_FAILURES" = "1" ]; then
        echo "\\*non-zero exit code (tool crashed or returned an error)"
        echo ""
    fi
} > "$OUTPUT"

echo "=== Results written to $OUTPUT ==="
echo "Raw hyperfine JSON in $RESULTS_DIR/"
