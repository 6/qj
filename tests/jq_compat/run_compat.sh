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

    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$state" in
            scan)
                [[ -z "${line// /}" ]] && continue
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
                            else
                                failed=$((failed + 1))
                                if $VERBOSE; then
                                    echo "  FAIL [$tool_name]: $filter | input: $input" >&2
                                    echo "    expected: $(echo "$expected_clean" | head -3)" >&2
                                    echo "    actual:   $(echo "$actual_clean" | head -3)" >&2
                                fi
                            fi
                        else
                            errored=$((errored + 1))
                            if $VERBOSE; then
                                echo "  ERROR [$tool_name]: $filter | input: $input" >&2
                            fi
                        fi
                    fi
                    state="scan"
                    [[ "$line" == \#* ]] && continue
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
            else
                failed=$((failed + 1))
            fi
        else
            errored=$((errored + 1))
        fi
    fi

    local pct
    pct=$(awk "BEGIN {printf \"%.1f\", $passed / $total * 100}")
    printf "  %-6s %3d/%d passed (%5s%%)\n" "$tool_name" "$passed" "$total" "$pct"
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
