#!/usr/bin/env bash
# Run jq's official test suite against multiple jq-compatible tools.
# Reports pass/fail/error counts for each tool found on $PATH.
#
# Usage:
#   bash tests/jq_compat/run_compat.sh           # test all available tools
#   bash tests/jq_compat/run_compat.sh -v         # verbose: show each failure
#
# Requires: jq (for JSON-aware output comparison), jq.test in same directory.
# Builds jx from source if target/release/jx doesn't exist.
# Requires: coreutils (brew install coreutils) for gtimeout on macOS.

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
TEST_FILE="$SCRIPT_DIR/jq.test"
MODULES_DIR="$SCRIPT_DIR/modules"
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

# Discover available tools — ordered for display
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

# Known section headers in jq.test: "match_prefix=Display Name"
# The match prefix is checked against comment text; the display name is used in output.
section_headers=(
    "Simple value tests=Simple value tests"
    "Dictionary construction syntax=Dictionary construction syntax"
    "Field access, piping=Field access, piping"
    "Negative array indices=Negative array indices"
    "Multiple outputs, iteration=Multiple outputs, iteration"
    "Slices=Slices"
    "Variables=Variables"
    "Builtin functions=Builtin functions"
    "User-defined functions=User-defined functions"
    "Paths=Paths"
    "Assignment=Assignment"
    "Conditionals=Conditionals"
    "string operations=string operations"
    "module system=module system"
    "Basic numbers tests=Basic numbers tests"
    "Tests to cover the new toliteral number=toliteral number"
    "explode/implode=explode/implode"
    "walk=walk"
)

# Per-category tracking (global, populated by run_tool)
declare -A cat_passed cat_total
categories=()       # ordered list of categories seen
categories_seen=()  # dedup tracker

# JSON-aware comparison: compare each line using jq's == operator.
# This handles differences like 2 vs 2.0, whitespace, key ordering, etc.
json_equal() {
    local a="$1" b="$2"
    # Fast path: exact string match
    [[ "$a" == "$b" ]] && return 0

    # Split into arrays of lines
    local -a a_lines b_lines
    IFS=$'\n' read -r -d '' -a a_lines <<< "$a" || true
    IFS=$'\n' read -r -d '' -a b_lines <<< "$b" || true

    [[ ${#a_lines[@]} -ne ${#b_lines[@]} ]] && return 1

    local i
    for (( i=0; i<${#a_lines[@]}; i++ )); do
        local al="${a_lines[$i]}" bl="${b_lines[$i]}"
        [[ "$al" == "$bl" ]] && continue
        # Try JSON-aware comparison using jq's == operator
        if jq -e -n --argjson a "$al" --argjson b "$bl" '$a == $b' &>/dev/null; then
            continue
        fi
        return 1
    done
    return 0
}

# Check if a comment line is a known section header; if so, echo the display name.
match_section_header() {
    local text="$1"
    for entry in "${section_headers[@]}"; do
        local prefix="${entry%%=*}"
        local display="${entry#*=}"
        if [[ "$text" == "$prefix"* ]]; then
            echo "$display"
            return 0
        fi
    done
    return 1
}

# Record a category if not already seen (preserves order)
record_category() {
    local cat="$1"
    local key
    for key in "${categories_seen[@]+"${categories_seen[@]}"}"; do
        [[ "$key" == "$cat" ]] && return
    done
    categories+=("$cat")
    categories_seen+=("$cat")
}

# Run all tests for a given tool, reading jq.test inline
# $1 = tool name, $2 = tool path, remaining args = extra flags (e.g. -L path)
run_tool() {
    local tool_name="$1"
    local tool_path="$2"
    shift 2
    local extra_args=()
    [[ $# -gt 0 ]] && extra_args=("$@")
    local passed=0 failed=0 errored=0 total=0
    local state="scan" filter="" input="" expected=""
    local current_category="Other"

    # Helper: record a test result into per-category tracking
    record_result() {
        local result="$1"  # "pass", "fail", or "error"
        local key="${tool_name}|${current_category}"
        cat_total["$key"]=$(( ${cat_total["$key"]:-0} + 1 ))
        if [[ "$result" == "pass" ]]; then
            cat_passed["$key"]=$(( ${cat_passed["$key"]:-0} + 1 ))
        fi
        record_category "$current_category"
    }

    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$state" in
            scan)
                [[ -z "${line// /}" ]] && continue
                if [[ "$line" =~ ^#\ (.+) ]]; then
                    local comment_text="${BASH_REMATCH[1]}"
                    local matched
                    matched=$(match_section_header "$comment_text") && current_category="$matched"
                    continue
                fi
                [[ "$line" == \#* ]] && continue
                if [[ "$line" == "%%FAIL"* ]]; then state="skip_fail"; continue; fi
                filter="$line"
                state="input"
                ;;
            input)
                input="$line"
                expected=""
                state="expected"
                ;;
            expected)
                if [[ -z "${line// /}" ]] || [[ "$line" == \#* ]]; then
                    if [[ -n "$expected" ]]; then
                        total=$((total + 1))
                        local actual
                        actual=$(printf '%s' "$input" | $TIMEOUT 5 "$tool_path" ${extra_args[@]+"${extra_args[@]}"} -c -- "$filter" 2>/dev/null) || true
                        if [[ -n "$actual" ]]; then
                            local actual_clean expected_clean
                            actual_clean=$(printf '%s' "$actual" | sed '/^$/d')
                            expected_clean=$(printf '%s' "$expected" | sed '/^$/d')
                            if json_equal "$actual_clean" "$expected_clean"; then
                                passed=$((passed + 1))
                                record_result pass
                            else
                                failed=$((failed + 1))
                                record_result fail
                                if $VERBOSE; then
                                    echo "  FAIL [$tool_name]: $filter | input: $input" >&2
                                    echo "    expected: $(echo "$expected_clean" | head -3)" >&2
                                    echo "    actual:   $(echo "$actual_clean" | head -3)" >&2
                                fi
                            fi
                        else
                            errored=$((errored + 1))
                            record_result error
                            if $VERBOSE; then
                                echo "  ERROR [$tool_name]: $filter | input: $input" >&2
                            fi
                        fi
                    fi
                    state="scan"
                    # Re-check for section header on this comment line
                    if [[ "$line" =~ ^#\ (.+) ]]; then
                        local comment_text="${BASH_REMATCH[1]}"
                        local matched
                        matched=$(match_section_header "$comment_text") && current_category="$matched"
                    fi
                else
                    [[ -n "$expected" ]] && expected="$expected"$'\n'"$line" || expected="$line"
                fi
                ;;
            skip_fail)
                [[ -z "${line// /}" ]] && state="scan"
                ;;
        esac
    done < "$TEST_FILE"

    # Flush last test case
    if [[ "$state" == "expected" && -n "$expected" ]]; then
        total=$((total + 1))
        local actual
        actual=$(printf '%s' "$input" | $TIMEOUT 5 "$tool_path" ${extra_args[@]+"${extra_args[@]}"} -c -- "$filter" 2>/dev/null) || true
        if [[ -n "$actual" ]]; then
            local actual_clean expected_clean
            actual_clean=$(printf '%s' "$actual" | sed '/^$/d')
            expected_clean=$(printf '%s' "$expected" | sed '/^$/d')
            if json_equal "$actual_clean" "$expected_clean"; then
                passed=$((passed + 1))
                record_result pass
            else
                failed=$((failed + 1))
                record_result fail
            fi
        else
            errored=$((errored + 1))
            record_result error
        fi
    fi

    local pct
    pct=$(awk "BEGIN {printf \"%.1f\", $passed / $total * 100}")
    printf "  %-6s %3d/%d passed (%5s%%)\n" "$tool_name" "$passed" "$total" "$pct"
}

# Write per-category breakdown as a markdown table.
# Output goes to both stdout and the results file.
write_category_table() {
    local results_file="$SCRIPT_DIR/results.md"

    # Determine column widths
    local max_cat_len=10
    for cat in "${categories[@]}"; do
        (( ${#cat} > max_cat_len )) && max_cat_len=${#cat}
    done
    # Account for bold markers on Total row
    local cat_width=$((max_cat_len + 2))

    local tool_col_width=10

    # Build table into a variable
    local table=""

    # Header row
    local row
    row=$(printf "| %-${cat_width}s |" "Category")
    for tool in "${tool_names[@]}"; do
        row+=$(printf " %-${tool_col_width}s |" "$tool")
    done
    table+="$row"$'\n'

    # Separator row
    local sep="|"
    sep+="$(printf '%*s' "$((cat_width + 2))" '' | tr ' ' '-')|"
    for _ in "${tool_names[@]}"; do
        sep+="$(printf '%*s' "$((tool_col_width + 2))" '' | tr ' ' '-')|"
    done
    table+="$sep"$'\n'

    # Data rows
    declare -A grand_passed grand_total
    for tool in "${tool_names[@]}"; do
        grand_passed["$tool"]=0
        grand_total["$tool"]=0
    done

    for cat in "${categories[@]}"; do
        row=$(printf "| %-${cat_width}s |" "$cat")
        for tool in "${tool_names[@]}"; do
            local key="${tool}|${cat}"
            local p=${cat_passed["$key"]:-0}
            local t=${cat_total["$key"]:-0}
            grand_passed["$tool"]=$(( ${grand_passed["$tool"]} + p ))
            grand_total["$tool"]=$(( ${grand_total["$tool"]} + t ))
            if (( t == 0 )); then
                row+=$(printf " %-${tool_col_width}s |" "-")
            else
                row+=$(printf " %-${tool_col_width}s |" "${p}/${t}")
            fi
        done
        table+="$row"$'\n'
    done

    # Total row (bold)
    row=$(printf "| %-${cat_width}s |" "**Total**")
    for tool in "${tool_names[@]}"; do
        local p=${grand_passed["$tool"]}
        local t=${grand_total["$tool"]}
        row+=$(printf " %-${tool_col_width}s |" "**${p}/${t}**")
    done
    table+="$row"$'\n'

    # Print to stdout
    echo ""
    echo "Per-category breakdown:"
    echo ""
    printf '%s' "$table"

    # Write markdown results file
    {
        echo "# jq compatibility results"
        echo ""
        echo "Generated by \`run_compat.sh\` on $(date -u +%Y-%m-%d)."
        echo ""
        echo "## Summary"
        echo ""
        for tool in "${tool_names[@]}"; do
            local p=${grand_passed["$tool"]}
            local t=${grand_total["$tool"]}
            local pct
            pct=$(awk "BEGIN {printf \"%.1f\", $p / $t * 100}")
            echo "- **${tool}**: ${p}/${t} (${pct}%)"
        done
        echo ""
        echo "## Per-category breakdown"
        echo ""
        printf '%s' "$table"
    } > "$results_file"

    echo ""
    echo "Results written to $results_file"
}

echo "jq compat (jq.test):"
for name in "${tool_names[@]}"; do
    case "$name" in
        jq|jaq|gojq)
            run_tool "$name" "${tool_paths[$name]}" -L "$MODULES_DIR"
            ;;
        *)
            run_tool "$name" "${tool_paths[$name]}"
            ;;
    esac
done

write_category_table
