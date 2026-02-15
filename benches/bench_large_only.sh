#!/usr/bin/env bash
# Benchmark qj/jq/jaq/gojq on large GH Archive datasets at two tiers:
#   ~1GB tier: gharchive.ndjson + gharchive.json  (2h of 2024-01-15)
#   ~5GB tier: gharchive_large.ndjson + gharchive_large.json  (24h of 2026-02-01)
#
# Features:
#   - Non-zero exit code detection (appends * to times, with footnote)
#   - "vs jq" speedup column per tool
#   - Backend note: qj (simdjson) vs qj (serde_json†) for >4GB JSON
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
echo ""

mkdir -p "$RESULTS_DIR"

# --- Tiers ---
# Each tier: label, ndjson_file, json_file
declare -a TIER_LABELS=()
declare -a TIER_NDJSON=()
declare -a TIER_JSON=()

# ~1GB tier
if [ -f "$DATA/gharchive.ndjson" ] && [ -f "$DATA/gharchive.json" ]; then
    TIER_LABELS+=("~1GB")
    TIER_NDJSON+=("$DATA/gharchive.ndjson")
    TIER_JSON+=("$DATA/gharchive.json")
fi

# ~5GB tier
if [ -f "$DATA/gharchive_large.ndjson" ] && [ -f "$DATA/gharchive_large.json" ]; then
    TIER_LABELS+=("~5GB")
    TIER_NDJSON+=("$DATA/gharchive_large.ndjson")
    TIER_JSON+=("$DATA/gharchive_large.json")
fi

if [ ${#TIER_LABELS[@]} -eq 0 ]; then
    echo "Error: no GH Archive files found. Run one of:"
    echo "  bash benches/download_gharchive.sh          # ~1GB tier"
    echo "  bash benches/download_gharchive.sh --large   # ~5GB tier"
    exit 1
fi

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

    local hyp_args=(caffeinate -dims hyperfine --warmup 0 --runs 2 --ignore-failure --export-json "$json_out")
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

PLATFORM=$(uname -ms)
DATE=$(date +%Y-%m-%d)

# --- Run all benchmarks ---
for tier_idx in "${!TIER_LABELS[@]}"; do
    tier="${TIER_LABELS[$tier_idx]}"
    ndjson="${TIER_NDJSON[$tier_idx]}"
    json="${TIER_JSON[$tier_idx]}"

    ndjson_size=$(wc -c < "$ndjson" | tr -d ' ')
    json_size=$(wc -c < "$json" | tr -d ' ')
    ndjson_mb=$(python3 -c "print(f'{$ndjson_size/1024/1024:.0f}')")
    json_mb=$(python3 -c "print(f'{$json_size/1024/1024:.0f}')")
    ndjson_basename=$(basename "$ndjson")
    json_basename=$(basename "$json")

    echo "=== Tier $tier: NDJSON ($ndjson_basename, ${ndjson_mb}MB) ==="
    echo ""
    for i in "${!NDJSON_FILTER_NAMES[@]}"; do
        echo "--- ${NDJSON_FILTER_NAMES[$i]} ---"
        run_filter "tier${tier_idx}_ndjson" "$i" "${NDJSON_FILTER_FLAGS[$i]}" "${NDJSON_FILTER_EXPRS[$i]}" "$ndjson"
    done

    echo "=== Tier $tier: JSON ($json_basename, ${json_mb}MB) ==="
    echo ""
    for i in "${!JSON_FILTER_NAMES[@]}"; do
        echo "--- ${JSON_FILTER_NAMES[$i]} ---"
        run_filter "tier${tier_idx}_json" "$i" "${JSON_FILTER_FLAGS[$i]}" "${JSON_FILTER_EXPRS[$i]}" "$json"
    done
done

# --- Generate markdown ---
# Track whether we need the exit-code footnote
HAS_FAILURES=0
# 4GB threshold in bytes
FOUR_GB=$((4 * 1024 * 1024 * 1024))

{
    echo "# Large GH Archive Benchmark"
    echo ""
    echo "> Generated: $DATE on \`$PLATFORM\`"
    echo "> 2 runs, no warmup via [hyperfine](https://github.com/sharkdp/hyperfine)."
    echo ""

    for tier_idx in "${!TIER_LABELS[@]}"; do
        tier="${TIER_LABELS[$tier_idx]}"
        ndjson="${TIER_NDJSON[$tier_idx]}"
        json="${TIER_JSON[$tier_idx]}"

        ndjson_size=$(wc -c < "$ndjson" | tr -d ' ')
        json_size=$(wc -c < "$json" | tr -d ' ')
        ndjson_mb=$(python3 -c "print(f'{$ndjson_size/1024/1024:.0f}')")
        json_mb=$(python3 -c "print(f'{$json_size/1024/1024:.0f}')")
        ndjson_basename=$(basename "$ndjson")
        json_basename=$(basename "$json")

        # Determine qj backend label for JSON tier
        if [ "$json_size" -gt "$FOUR_GB" ]; then
            qj_label="qj (serde_json†)"
            needs_serde_footnote=1
        else
            qj_label="qj (simdjson)"
            needs_serde_footnote=0
        fi

        echo "## Tier: $tier"
        echo ""

        # --- NDJSON section ---
        echo "### NDJSON (${ndjson_basename}, ${ndjson_mb}MB, parallel processing)"
        echo ""

        # Build header with vs jq columns
        HEADER="| Filter |"
        SEP="|--------|"
        for name in "${NAMES[@]}"; do
            if [ "$name" = "qj" ]; then
                HEADER+=" **qj (simdjson)** |"
            else
                HEADER+=" $name |"
            fi
            SEP+="------:|"
            if [ "$name" != "jq" ]; then
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

        for i in "${!NDJSON_FILTER_NAMES[@]}"; do
            flags="${NDJSON_FILTER_FLAGS[$i]}"
            expr="${NDJSON_FILTER_EXPRS[$i]}"
            display="$flags '$expr'"
            row="| \`$display\` |"
            json_file="$RESULTS_DIR/tier${tier_idx}_ndjson_${i}.json"

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
                # Add vs jq column (skip for jq itself)
                if [ "${NAMES[$t]}" != "jq" ]; then
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

        # --- JSON section ---
        echo "### JSON (${json_basename}, ${json_mb}MB, single document)"
        echo ""

        # Build header with backend-aware qj label
        HEADER="| Filter |"
        SEP="|--------|"
        for name in "${NAMES[@]}"; do
            if [ "$name" = "qj" ]; then
                HEADER+=" **${qj_label}** |"
            else
                HEADER+=" $name |"
            fi
            SEP+="------:|"
            if [ "$name" != "jq" ]; then
                HEADER+=" vs jq |"
                SEP+="------:|"
            fi
        done
        echo "$HEADER"
        echo "$SEP"

        for i in "${!JSON_FILTER_NAMES[@]}"; do
            flags="${JSON_FILTER_FLAGS[$i]}"
            expr="${JSON_FILTER_EXPRS[$i]}"
            display="$flags '$expr'"
            row="| \`$display\` |"
            json_file="$RESULTS_DIR/tier${tier_idx}_json_${i}.json"

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
                if [ "${NAMES[$t]}" != "jq" ]; then
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

        # --- Throughput ---
        echo "### Throughput (\`-c '.'\`, single pass)"
        echo ""
        tp_header="| File |"
        tp_sep="|------|"
        for name in "${NAMES[@]}"; do
            tp_header+=" $name |"
            tp_sep+="------:|"
        done
        echo "$tp_header"
        echo "$tp_sep"

        for section_info in "tier${tier_idx}_ndjson:${ndjson_basename}:$ndjson_size" "tier${tier_idx}_json:${json_basename}:$json_size"; do
            IFS=: read -r section file bytes <<< "$section_info"
            json_file="$RESULTS_DIR/${section}_0.json"
            row="| $file |"
            for t in "${!TOOLS[@]}"; do
                result=$(parse_result "$json_file" "$t")
                median=$(echo "$result" | cut -d, -f1)
                failed=$(echo "$result" | cut -d, -f2)
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
                suffix=""
                if [ "$failed" = "1" ]; then
                    suffix="*"
                    HAS_FAILURES=1
                fi
                if [ "${NAMES[$t]}" = "qj" ]; then
                    row+=" **${tp}${suffix}** |"
                else
                    row+=" ${tp}${suffix} |"
                fi
            done
            echo "$row"
        done

        echo ""
    done

    # --- Footnotes ---
    if [ "$HAS_FAILURES" = "1" ]; then
        echo "\\*non-zero exit code (tool crashed or returned an error)"
        echo ""
    fi
    if [ "${needs_serde_footnote:-0}" = "1" ]; then
        echo "†serde_json fallback for >4GB single-document JSON (simdjson 4GB limit)"
        echo ""
    fi
} > "$OUTPUT"

echo "=== Results written to $OUTPUT ==="
echo "Raw hyperfine JSON in $RESULTS_DIR/"
