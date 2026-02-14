#!/usr/bin/env bash
# Run the feature compatibility test suite against multiple jq-compatible tools.
# Produces a per-feature Y/~/N matrix and overall compat percentages.
#
# Usage:
#   bash tests/jq_compat/run_features.sh           # summary + feature table
#   bash tests/jq_compat/run_features.sh -v         # verbose: show each failure
#
# Requires: jq (for JSON-aware output comparison), features.test in same directory.
# Builds jx from source if target/release/jx doesn't exist.

set -euo pipefail

# Find timeout command (gtimeout on macOS via coreutils, timeout on Linux)
if command -v gtimeout &>/dev/null; then
    TIMEOUT="gtimeout"
elif command -v timeout &>/dev/null; then
    TIMEOUT="timeout"
else
    echo "error: timeout command not found. Install coreutils: brew install coreutils" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TEST_FILE="$SCRIPT_DIR/features.test"
RESULTS_FILE="$SCRIPT_DIR/feature_results.md"
VERBOSE=false

[[ "${1:-}" == "-v" ]] && VERBOSE=true

if [[ ! -f "$TEST_FILE" ]]; then
    echo "error: $TEST_FILE not found" >&2
    exit 1
fi

if ! command -v jq &>/dev/null; then
    echo "error: jq is required for JSON-aware output comparison" >&2
    exit 1
fi

# Build jx if needed
if [[ ! -x "$PROJECT_ROOT/target/release/jx" ]]; then
    echo "Building jx (release)..."
    cargo build --release --manifest-path "$PROJECT_ROOT/Cargo.toml" 2>/dev/null
fi

# Discover available tools
tool_names=()
declare -A tool_paths

tool_paths[jx]="$PROJECT_ROOT/target/release/jx"
tool_names+=(jx)

for tool in jq jaq gojq; do
    if command -v "$tool" &>/dev/null; then
        tool_paths[$tool]="$(command -v "$tool")"
        tool_names+=("$tool")
    fi
done

echo "Tools: ${tool_names[*]}"
echo ""

# JSON-aware comparison (identical to run_compat.sh)
json_equal() {
    local a="$1" b="$2"
    [[ "$a" == "$b" ]] && return 0

    local -a a_lines b_lines
    IFS=$'\n' read -r -d '' -a a_lines <<< "$a" || true
    IFS=$'\n' read -r -d '' -a b_lines <<< "$b" || true

    [[ ${#a_lines[@]} -ne ${#b_lines[@]} ]] && return 1

    local i
    for (( i=0; i<${#a_lines[@]}; i++ )); do
        local al="${a_lines[$i]}" bl="${b_lines[$i]}"
        [[ "$al" == "$bl" ]] && continue
        if jq -e -n --argjson a "$al" --argjson b "$bl" '$a == $b' &>/dev/null; then
            continue
        fi
        return 1
    done
    return 0
}

# --- Parse test file ---

test_categories=()
test_features=()
test_filters=()
test_inputs=()
test_expected=()
test_flags=()
test_count=0

parse_test_file() {
    local current_category="Other"
    local current_feature="unknown"
    local current_flags=""
    local state="scan"
    local filter="" input="" expected=""

    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$state" in
            scan)
                # Skip blank lines
                [[ -z "${line// /}" ]] && continue
                # Metadata comments
                if [[ "$line" =~ ^#\ *category:\ *(.+) ]]; then
                    current_category="${BASH_REMATCH[1]}"
                    continue
                fi
                if [[ "$line" =~ ^#\ *feature:\ *(.+) ]]; then
                    current_feature="${BASH_REMATCH[1]}"
                    current_flags=""  # reset flags on new feature
                    continue
                fi
                if [[ "$line" =~ ^#\ *flags:\ *(.+) ]]; then
                    current_flags="${BASH_REMATCH[1]}"
                    continue
                fi
                # Skip other comments
                [[ "$line" == \#* ]] && continue
                filter="$line"
                state="input"
                ;;
            input)
                input="$line"
                expected=""
                state="expected"
                ;;
            expected)
                if [[ -z "${line// /}" ]] || [[ "$line" =~ ^#\  ]]; then
                    if [[ -n "$expected" ]]; then
                        test_categories+=("$current_category")
                        test_features+=("$current_feature")
                        test_filters+=("$filter")
                        test_inputs+=("$input")
                        test_expected+=("$expected")
                        test_flags+=("$current_flags")
                        test_count=$((test_count + 1))
                    fi
                    state="scan"
                    # Re-check metadata on this line
                    if [[ "$line" =~ ^#\ *category:\ *(.+) ]]; then
                        current_category="${BASH_REMATCH[1]}"
                    elif [[ "$line" =~ ^#\ *feature:\ *(.+) ]]; then
                        current_feature="${BASH_REMATCH[1]}"
                        current_flags=""
                    elif [[ "$line" =~ ^#\ *flags:\ *(.+) ]]; then
                        current_flags="${BASH_REMATCH[1]}"
                    fi
                else
                    [[ -n "$expected" ]] && expected="$expected"$'\n'"$line" || expected="$line"
                fi
                ;;
        esac
    done < "$TEST_FILE"

    # Flush last test case
    if [[ "$state" == "expected" && -n "$expected" ]]; then
        test_categories+=("$current_category")
        test_features+=("$current_feature")
        test_filters+=("$filter")
        test_inputs+=("$input")
        test_expected+=("$expected")
        test_flags+=("$current_flags")
        test_count=$((test_count + 1))
    fi
}

echo "Parsing features.test..."
parse_test_file
echo "Found $test_count tests"
echo ""

# --- Run tests for each tool ---

# Per-tool, per-test pass/fail: tool_results[tool|index] = "pass" or "fail"
declare -A tool_results

run_tool() {
    local tool_name="$1"
    local tool_path="$2"
    local passed=0 failed=0 total=0

    local i
    for (( i=0; i<test_count; i++ )); do
        total=$((total + 1))
        local filter="${test_filters[$i]}"
        local input="${test_inputs[$i]}"
        local expected="${test_expected[$i]}"
        local flags="${test_flags[$i]}"

        local actual
        if [[ -n "$flags" ]]; then
            # shellcheck disable=SC2086
            actual=$(printf '%b' "$input" | $TIMEOUT 5 "$tool_path" $flags "$filter" 2>/dev/null) || true
        else
            actual=$(printf '%s' "$input" | $TIMEOUT 5 "$tool_path" -c -- "$filter" 2>/dev/null) || true
        fi

        local actual_clean expected_clean
        actual_clean=$(printf '%s' "$actual" | sed '/^$/d')
        expected_clean=$(printf '%s' "$expected" | sed '/^$/d')

        if [[ -n "$actual_clean" ]] && json_equal "$actual_clean" "$expected_clean"; then
            tool_results["${tool_name}|${i}"]="pass"
            passed=$((passed + 1))
        else
            tool_results["${tool_name}|${i}"]="fail"
            failed=$((failed + 1))
            if $VERBOSE; then
                echo "  FAIL [$tool_name] ${test_features[$i]}: $filter | input: $input" >&2
                echo "    expected: $(echo "$expected_clean" | head -3)" >&2
                echo "    actual:   $(echo "$actual_clean" | head -3)" >&2
            fi
        fi
    done

    local pct
    pct=$(awk "BEGIN {printf \"%.1f\", $passed / $total * 100}")
    printf "  %-6s %3d/%d passed (%5s%%)\n" "$tool_name" "$passed" "$total" "$pct"
}

echo "Running tests..."
for name in "${tool_names[@]}"; do
    run_tool "$name" "${tool_paths[$name]}"
done
echo ""

# --- Aggregate per-feature ---

# Build ordered lists of unique features and their categories
ordered_features=()
ordered_categories=()
declare -A feature_seen

for (( i=0; i<test_count; i++ )); do
    f="${test_features[$i]}"
    if [[ -z "${feature_seen[$f]:-}" ]]; then
        feature_seen["$f"]=1
        ordered_features+=("$f")
        ordered_categories+=("${test_categories[$i]}")
    fi
done

feature_count=${#ordered_features[@]}

# Per-tool, per-feature: feature_pass[tool|feature] = count, feature_total[tool|feature] = count
declare -A feature_pass feature_total

for (( i=0; i<test_count; i++ )); do
    f="${test_features[$i]}"
    for tool in "${tool_names[@]}"; do
        key="${tool}|${f}"
        feature_total["$key"]=$(( ${feature_total["$key"]:-0} + 1 ))
        if [[ "${tool_results["${tool}|${i}"]}" == "pass" ]]; then
            feature_pass["$key"]=$(( ${feature_pass["$key"]:-0} + 1 ))
        fi
    done
done

# Compute Y/~/N per tool per feature
# feature_status[tool|feature] = "Y", "~", or "N"
declare -A feature_status

for tool in "${tool_names[@]}"; do
    for f in "${ordered_features[@]}"; do
        key="${tool}|${f}"
        p=${feature_pass["$key"]:-0}
        t=${feature_total["$key"]:-0}
        if (( p == t && t > 0 )); then
            feature_status["$key"]="Y"
        elif (( p > 0 )); then
            feature_status["$key"]="~"
        else
            feature_status["$key"]="N"
        fi
    done
done

# --- Output ---

write_output() {
    local out=""

    # Per-feature table grouped by category
    out+="## Feature compatibility matrix"$'\n'
    out+=""$'\n'
    out+="Status: **Y** = all tests pass, **~** = partial, **N** = none pass"$'\n'
    out+=""$'\n'

    local current_cat=""

    # Header
    local header="| Feature | Tests |"
    local sep="|---------|------:|"
    for tool in "${tool_names[@]}"; do
        if [[ "$tool" == "jx" ]]; then
            header+=" **jx** |"
        else
            header+=" $tool |"
        fi
        sep+="-----:|"
    done

    for (( fi=0; fi<feature_count; fi++ )); do
        local f="${ordered_features[$fi]}"
        local cat="${ordered_categories[$fi]}"

        if [[ "$cat" != "$current_cat" ]]; then
            current_cat="$cat"
            out+="### $cat"$'\n'
            out+=""$'\n'
            out+="$header"$'\n'
            out+="$sep"$'\n'
        fi

        # Use any tool's total (same for all)
        local t=${feature_total["${tool_names[0]}|${f}"]:-0}
        local row="| $f | $t |"

        for tool in "${tool_names[@]}"; do
            local key="${tool}|${f}"
            local p=${feature_pass["$key"]:-0}
            local total=${feature_total["$key"]:-0}
            local status="${feature_status["$key"]}"
            local cell="${p}/${total} ${status}"
            if [[ "$tool" == "jx" ]]; then
                row+=" **${cell}** |"
            else
                row+=" ${cell} |"
            fi
        done

        out+="$row"$'\n'
    done

    out+=""$'\n'

    # Summary table
    out+="## Summary"$'\n'
    out+=""$'\n'
    out+="| Tool | Y | ~ | N | Score |"$'\n'
    out+="|------|--:|--:|--:|------:|"$'\n'

    for tool in "${tool_names[@]}"; do
        local y_count=0 partial_count=0 n_count=0
        for f in "${ordered_features[@]}"; do
            local status="${feature_status["${tool}|${f}"]}"
            case "$status" in
                Y) y_count=$((y_count + 1)) ;;
                "~") partial_count=$((partial_count + 1)) ;;
                N) n_count=$((n_count + 1)) ;;
            esac
        done
        local score
        score=$(awk "BEGIN {printf \"%.1f\", ($y_count + 0.5 * $partial_count) / $feature_count * 100}")
        if [[ "$tool" == "jx" ]]; then
            out+="| **jx** | **$y_count** | **$partial_count** | **$n_count** | **${score}%** |"$'\n'
        else
            out+="| $tool | $y_count | $partial_count | $n_count | ${score}% |"$'\n'
        fi
    done

    out+=""$'\n'
    out+="Score = (Y + 0.5 × ~) / total × 100"$'\n'

    printf '%s' "$out"
}

output=$(write_output)

# Print to stdout
echo "$output"

# Write results file
{
    echo "# Feature compatibility results"
    echo ""
    echo "Generated by \`run_features.sh\` on $(date -u +%Y-%m-%d)."
    echo ""
    echo "$output"
} > "$RESULTS_FILE"

echo ""
echo "Results written to $RESULTS_FILE"
