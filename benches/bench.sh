#!/usr/bin/env bash
# Benchmark jx vs jq vs jaq vs gojq. Writes results to $OUTPUT and prints results.
# Requires: hyperfine, jq, and a release build of jx. jaq/gojq are optional.
#
# Usage: bash benches/bench.sh [--cooldown SECS] [--runs N] [--output PATH]
set -euo pipefail

# --- Parse CLI args ---
COOLDOWN=5
RUNS=5
OUTPUT="benches/results.md"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --cooldown) COOLDOWN="$2"; shift 2 ;;
        --runs)     RUNS="$2";     shift 2 ;;
        --output)   OUTPUT="$2";   shift 2 ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

JX="./target/release/jx"
DATA="benches/data"
RESULTS_DIR="benches/results"
mkdir -p "$RESULTS_DIR"

# --- Preflight checks ---

if [ ! -f "$JX" ]; then
    echo "Error: $JX not found. Run: cargo build --release"
    exit 1
fi

if ! command -v hyperfine &>/dev/null; then
    echo "Error: hyperfine not found."
    exit 1
fi

if ! command -v jq &>/dev/null; then
    echo "Error: jq not found."
    exit 1
fi

# Detect available tools
TOOLS=("jx" "jq")
TOOL_CMDS=("$JX" "jq")
for tool in jaq gojq; do
    if command -v "$tool" &>/dev/null; then
        TOOLS+=("$tool")
        TOOL_CMDS+=("$tool")
    else
        echo "Note: $tool not found, skipping"
    fi
done

# Display settings
echo "Settings: --cooldown $COOLDOWN --runs $RUNS --output $OUTPUT"

# --- Platform info ---

PLATFORM="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
DATE="$(date -u +%Y-%m-%d)"
echo "Platform: $PLATFORM"
echo "Date: $DATE"
echo ""

# --- Filter definitions ---
# Each filter has: display name, extra flags (may be empty), and the filter expression.
# Flags and filter are kept separate so we can quote the filter properly.

FILTER_NAMES=(
    "identity compact"
    "field extraction"
    "pipe + builtin"
    "iterate + field"
    "select + construct"
    "math (floor)"
    "string ops (split+join)"
    "unique + sort"
    "paths(scalars)"
    "map_values + tojson"
)
# Extra flags for each filter (e.g. -c for compact output)
FILTER_FLAGS=(
    "-c"
    "-c"
    ""
    ""
    ""
    ""
    ""
    ""
    ""
    ""
)
# The jq filter expression itself
FILTER_EXPRS=(
    "."
    ".statuses"
    ".statuses|length"
    ".statuses[]|.user.name"
    '.statuses[]|select(.retweet_count>0)|{user:.user.screen_name,n:.retweet_count}'
    '[.statuses[]|.retweet_count|floor]'
    '[.statuses[]|.user.screen_name|split("_")|join("-")]'
    '[.statuses[]|.user.screen_name]|unique|length'
    '[paths(scalars)]|length'
    '.statuses[0]|map_values(tojson)'
)

# Files to benchmark (small first, then large if available)
FILES=("twitter.json")
if [ -f "$DATA/large_twitter.json" ]; then
    FILES+=("large_twitter.json")
fi

# --- NDJSON filter definitions ---

NDJSON_FILTER_NAMES=(
    "ndjson field"
    "ndjson identity compact"
    "ndjson select + construct"
)
NDJSON_FILTER_FLAGS=(
    ""
    "-c"
    "-c"
)
NDJSON_FILTER_EXPRS=(
    ".name"
    "."
    'select(.score>50)|{name,score}'
)

NDJSON_FILES=()
if [ -f "$DATA/100k.ndjson" ]; then
    NDJSON_FILES+=("100k.ndjson")
fi
if [ -f "$DATA/1m.ndjson" ]; then
    NDJSON_FILES+=("1m.ndjson")
fi

# --- Helper to build a command string for a tool ---
# Returns a shell command string suitable for hyperfine (with shell).
build_cmd() {
    local cmd="$1" flags="$2" filter="$3" file="$4"
    if [ -n "$flags" ]; then
        echo "$cmd $flags '$filter' '$file'"
    else
        echo "$cmd '$filter' '$file'"
    fi
}

# --- Correctness check ---
# Verify jx matches jq output for every filter+file combo before timing.

echo "=== Correctness check ==="
CORRECT=true
for file in "${FILES[@]}"; do
    for i in "${!FILTER_NAMES[@]}"; do
        flags="${FILTER_FLAGS[$i]}"
        filter="${FILTER_EXPRS[$i]}"
        if [ -n "$flags" ]; then
            jx_out=$("$JX" "$flags" "$filter" "$DATA/$file" 2>&1) || true
            jq_out=$(jq "$flags" "$filter" "$DATA/$file" 2>&1) || true
        else
            jx_out=$("$JX" "$filter" "$DATA/$file" 2>&1) || true
            jq_out=$(jq "$filter" "$DATA/$file" 2>&1) || true
        fi
        if [ "$jx_out" != "$jq_out" ]; then
            echo "MISMATCH: ${FILTER_NAMES[$i]} on $file"
            echo "  jx: $(echo "$jx_out" | head -3)"
            echo "  jq: $(echo "$jq_out" | head -3)"
            CORRECT=false
        else
            echo "  OK: ${FILTER_NAMES[$i]} on $file"
        fi
    done
done
echo ""

if [ "$CORRECT" != "true" ]; then
    echo "WARNING: Output mismatches detected. Benchmarking anyway."
    echo ""
fi

# --- Run benchmarks ---
# Collect median times from hyperfine JSON export.

# Associative array: results[filter_idx,file,tool] = median_seconds
declare -A RESULTS

for file in "${FILES[@]}"; do
    echo "=== $file ($(wc -c < "$DATA/$file" | tr -d ' ') bytes) ==="
    echo ""
    for i in "${!FILTER_NAMES[@]}"; do
        label="${FILTER_NAMES[$i]}"
        flags="${FILTER_FLAGS[$i]}"
        filter="${FILTER_EXPRS[$i]}"
        json_file="$RESULTS_DIR/run-${i}-${file}.json"

        # Build hyperfine command list (using shell mode for proper quoting)
        cmds=()
        cmd_tools=()
        for t in "${!TOOLS[@]}"; do
            tool="${TOOLS[$t]}"
            cmd="${TOOL_CMDS[$t]}"
            # Test that this tool can run the filter
            if [ -n "$flags" ]; then
                test_ok=$($cmd "$flags" "$filter" "$DATA/$file" >/dev/null 2>&1 && echo yes || echo no)
            else
                test_ok=$($cmd "$filter" "$DATA/$file" >/dev/null 2>&1 && echo yes || echo no)
            fi
            if [ "$test_ok" = "yes" ]; then
                cmds+=("$(build_cmd "$cmd" "$flags" "$filter" "$DATA/$file")")
                cmd_tools+=("$tool")
            else
                echo "  Skip $tool for '$label' (unsupported)"
            fi
        done

        if [ ${#cmds[@]} -lt 2 ]; then
            echo "  Skipping '$label' — not enough tools support it"
            echo ""
            continue
        fi

        echo "--- $label ---"
        hyperfine --warmup 3 --runs "$RUNS" --export-json "$json_file" "${cmds[@]}" 2>&1
        echo ""

        # Parse median times from JSON
        for t in "${!cmd_tools[@]}"; do
            median=$(jq ".results[$t].median" "$json_file")
            RESULTS["$i,$file,${cmd_tools[$t]}"]="$median"
        done

        # Cooldown between groups to mitigate thermal throttling
        sleep "$COOLDOWN"
    done
done

# --- Run NDJSON benchmarks ---

if [ ${#NDJSON_FILES[@]} -gt 0 ]; then
    for file in "${NDJSON_FILES[@]}"; do
        echo "=== NDJSON: $file ($(wc -c < "$DATA/$file" | tr -d ' ') bytes) ==="
        echo ""
        for i in "${!NDJSON_FILTER_NAMES[@]}"; do
            label="${NDJSON_FILTER_NAMES[$i]}"
            flags="${NDJSON_FILTER_FLAGS[$i]}"
            filter="${NDJSON_FILTER_EXPRS[$i]}"
            json_file="$RESULTS_DIR/ndjson-run-${i}-${file}.json"

            cmds=()
            cmd_tools=()
            for t in "${!TOOLS[@]}"; do
                tool="${TOOLS[$t]}"
                cmd="${TOOL_CMDS[$t]}"
                if [ -n "$flags" ]; then
                    test_ok=$($cmd "$flags" "$filter" "$DATA/$file" >/dev/null 2>&1 && echo yes || echo no)
                else
                    test_ok=$($cmd "$filter" "$DATA/$file" >/dev/null 2>&1 && echo yes || echo no)
                fi
                if [ "$test_ok" = "yes" ]; then
                    cmds+=("$(build_cmd "$cmd" "$flags" "$filter" "$DATA/$file")")
                    cmd_tools+=("$tool")
                else
                    echo "  Skip $tool for '$label' (unsupported)"
                fi
            done

            if [ ${#cmds[@]} -lt 2 ]; then
                echo "  Skipping '$label' — not enough tools support it"
                echo ""
                continue
            fi

            echo "--- $label ---"
            hyperfine --warmup 3 --runs "$RUNS" --export-json "$json_file" "${cmds[@]}" 2>&1
            echo ""

            for t in "${!cmd_tools[@]}"; do
                median=$(jq ".results[$t].median" "$json_file")
                RESULTS["ndjson_$i,$file,${cmd_tools[$t]}"]="$median"
            done

            sleep "$COOLDOWN"
        done
    done
fi

# --- Format time values ---

format_time() {
    local seconds="$1"
    if [ -z "$seconds" ] || [ "$seconds" = "null" ]; then
        echo "-"
        return
    fi
    local ms
    ms=$(echo "$seconds" | awk '{printf "%.1f", $1 * 1000}')
    # If >= 1000ms, show as seconds
    if echo "$ms" | awk '{exit ($1 >= 1000) ? 0 : 1}'; then
        echo "$seconds" | awk '{printf "%.2fs", $1}'
    else
        echo "${ms}ms"
    fi
}

# --- Generate BENCHMARKS.md ---

# Column display labels for the filter
filter_display() {
    local flags="$1" expr="$2"
    if [ -n "$flags" ]; then
        echo "$flags '$expr'"
    else
        echo "'$expr'"
    fi
}

{
    echo "# Benchmarks"
    echo ""
    echo "> Auto-generated by \`bash benches/bench.sh\`. Do not edit manually."
    echo "> Last updated: $DATE on \`$PLATFORM\`"
    echo ""
    echo "All benchmarks: warm cache (\`--warmup 3\`), $RUNS runs, output to pipe. NDJSON uses parallel processing."
    echo "Median of $RUNS runs via [hyperfine](https://github.com/sharkdp/hyperfine)."
    echo ""

    # Build header from available tools
    header="| Filter | File |"
    separator="|--------|------|"
    for tool in "${TOOLS[@]}"; do
        if [ "$tool" = "jx" ]; then
            header="$header **$tool** |"
        else
            header="$header $tool |"
        fi
        separator="$separator------|"
    done

    echo "$header"
    echo "$separator"

    for file in "${FILES[@]}"; do
        for i in "${!FILTER_NAMES[@]}"; do
            display=$(filter_display "${FILTER_FLAGS[$i]}" "${FILTER_EXPRS[$i]}")
            row="| \`$display\` | $file |"
            for tool in "${TOOLS[@]}"; do
                val="${RESULTS["$i,$file,$tool"]:-}"
                formatted=$(format_time "$val")
                if [ "$tool" = "jx" ]; then
                    row="$row **$formatted** |"
                else
                    row="$row $formatted |"
                fi
            done
            echo "$row"
        done
    done

    # NDJSON results
    if [ ${#NDJSON_FILES[@]} -gt 0 ]; then
        echo ""
        echo "### NDJSON (parallel processing)"
        echo ""
        echo "jx processes NDJSON in parallel across all cores using rayon."
        echo ""
        echo "$header"
        echo "$separator"

        for file in "${NDJSON_FILES[@]}"; do
            for i in "${!NDJSON_FILTER_NAMES[@]}"; do
                display=$(filter_display "${NDJSON_FILTER_FLAGS[$i]}" "${NDJSON_FILTER_EXPRS[$i]}")
                row="| \`$display\` | $file |"
                for tool in "${TOOLS[@]}"; do
                    val="${RESULTS["ndjson_$i,$file,$tool"]:-}"
                    formatted=$(format_time "$val")
                    if [ "$tool" = "jx" ]; then
                        row="$row **$formatted** |"
                    else
                        row="$row $formatted |"
                    fi
                done
                echo "$row"
            done
        done
    fi

    # --- Throughput ---
    # Compute throughput for identity compact on the largest JSON file.
    largest_file="${FILES[${#FILES[@]}-1]}"
    largest_bytes=$(wc -c < "$DATA/$largest_file" | tr -d ' ')
    jx_identity="${RESULTS["0,$largest_file,jx"]:-}"
    if [ -n "$jx_identity" ] && [ "$jx_identity" != "null" ]; then
        throughput=$(echo "$largest_bytes $jx_identity" | awk '{
            mbps = ($1 / $2) / (1024*1024)
            if (mbps >= 1024) printf "%.1f GB/s", mbps/1024
            else printf "%.0f MB/s", mbps
        }')
        echo ""
        echo "### Throughput"
        echo ""
        echo "Peak throughput (\`-c '.'\` on ${largest_file}, $(echo "$largest_bytes" | awk '{printf "%.0fMB", $1/(1024*1024)}')): **$throughput**"
    fi

    # --- Summary: geometric-mean speedup vs jq ---
    # Categories: JSON (large file only if available, else small), NDJSON
    echo ""
    echo "### Summary (times faster than jq)"
    echo ""

    # Build summary header
    sum_header="| Category |"
    sum_sep="|----------|"
    for tool in "${TOOLS[@]}"; do
        if [ "$tool" = "jq" ]; then continue; fi
        if [ "$tool" = "jx" ]; then
            sum_header="$sum_header **$tool** |"
        else
            sum_header="$sum_header $tool |"
        fi
        sum_sep="$sum_sep------|"
    done
    echo "$sum_header"
    echo "$sum_sep"

    # Geometric mean helper: reads lines of "jq_time tool_time" pairs, outputs geomean ratio
    geomean_ratio() {
        awk 'BEGIN { sum=0; n=0 }
        {
            if ($1 > 0 && $2 > 0) { sum += log($1/$2); n++ }
        }
        END {
            if (n > 0) printf "%.1fx", exp(sum/n)
            else printf "-"
        }'
    }

    # JSON category: use large file if available, else small
    json_file="${FILES[${#FILES[@]}-1]}"
    for tool in "${TOOLS[@]}"; do
        if [ "$tool" = "jq" ]; then continue; fi
        pairs=""
        for i in "${!FILTER_NAMES[@]}"; do
            jq_val="${RESULTS["$i,$json_file,jq"]:-}"
            tool_val="${RESULTS["$i,$json_file,$tool"]:-}"
            if [ -n "$jq_val" ] && [ "$jq_val" != "null" ] && [ -n "$tool_val" ] && [ "$tool_val" != "null" ]; then
                pairs="${pairs}${jq_val} ${tool_val}\n"
            fi
        done
        eval "json_speedup_${tool//-/_}=$(echo -e "$pairs" | geomean_ratio)"
    done

    json_row="| JSON (${json_file}) |"
    for tool in "${TOOLS[@]}"; do
        if [ "$tool" = "jq" ]; then continue; fi
        varname="json_speedup_${tool//-/_}"
        val="${!varname}"
        if [ "$tool" = "jx" ]; then
            json_row="$json_row **$val** |"
        else
            json_row="$json_row $val |"
        fi
    done
    echo "$json_row"

    # NDJSON category
    if [ ${#NDJSON_FILES[@]} -gt 0 ]; then
        ndjson_file="${NDJSON_FILES[${#NDJSON_FILES[@]}-1]}"
        for tool in "${TOOLS[@]}"; do
            if [ "$tool" = "jq" ]; then continue; fi
            pairs=""
            for i in "${!NDJSON_FILTER_NAMES[@]}"; do
                jq_val="${RESULTS["ndjson_$i,$ndjson_file,jq"]:-}"
                tool_val="${RESULTS["ndjson_$i,$ndjson_file,$tool"]:-}"
                if [ -n "$jq_val" ] && [ "$jq_val" != "null" ] && [ -n "$tool_val" ] && [ "$tool_val" != "null" ]; then
                    pairs="${pairs}${jq_val} ${tool_val}\n"
                fi
            done
            eval "ndjson_speedup_${tool//-/_}=$(echo -e "$pairs" | geomean_ratio)"
        done

        ndjson_row="| NDJSON (${ndjson_file}) |"
        for tool in "${TOOLS[@]}"; do
            if [ "$tool" = "jq" ]; then continue; fi
            varname="ndjson_speedup_${tool//-/_}"
            val="${!varname}"
            if [ "$tool" = "jx" ]; then
                ndjson_row="$ndjson_row **$val** |"
            else
                ndjson_row="$ndjson_row $val |"
            fi
        done
        echo "$ndjson_row"
    fi

    echo ""
    echo "Geometric mean of per-filter speedups (median time). Higher is better."
} > "$OUTPUT"

echo "=== Done ==="
echo "Wrote $OUTPUT"
