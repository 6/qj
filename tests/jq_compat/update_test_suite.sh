#!/usr/bin/env bash
# Update the vendored jq test suites to match a specific jq release tag.
#
# Usage:
#   bash tests/jq_compat/update_test_suite.sh           # uses version from mise.toml
#   bash tests/jq_compat/update_test_suite.sh 1.9.0     # explicit version
#
# Vendors, mirroring jqlang/jq's tests/ directory:
#   tests/jq_compat/*.test     jq.test, man.test, manonig.test, onig.test,
#                              base64.test, uri.test, optional.test
#   tests/jq_compat/modules/   tests/modules (for import/include tests)
#   tests/jq_compat/shtest/    shtest + setup and the fixtures shtest reads
#                              (reference only; nothing runs them yet)
#
# After upgrading, regenerate the jq_diff baseline and the builtin matrix
# (see CLAUDE.md, "Testing").
#
# Requires: gh (GitHub CLI)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# Determine version: argument > mise.toml
if [[ -n "${1:-}" ]]; then
    VERSION="$1"
else
    VERSION=$(grep '^jq' "$PROJECT_ROOT/mise.toml" | sed 's/.*= *"\(.*\)"/\1/')
    if [[ -z "$VERSION" ]]; then
        echo "error: no version argument and no jq entry in mise.toml" >&2
        exit 1
    fi
fi

TAG="jq-${VERSION}"
echo "Updating test suites to $TAG..."

# Download one file via base64 to preserve control characters and invalid UTF-8.
fetch() {
    local repo_path="$1"
    local dest="$2"
    mkdir -p "$(dirname "$dest")"
    gh api "repos/jqlang/jq/contents/${repo_path}?ref=$TAG" --jq '.content' \
        | base64 -d > "$dest"
    echo "  $dest ($(wc -l < "$dest" | tr -d ' ') lines)"
}

# --- Upstream .test suites (jq's --run-tests format) ---
echo "Downloading .test suites..."
for suite in jq man manonig onig base64 uri optional; do
    fetch "tests/${suite}.test" "$SCRIPT_DIR/${suite}.test"
done

# --- shtest and its fixtures (reference for porting jq's CLI tests) ---
# jq-f-test.sh is left out on purpose: CI shellchecks every tracked *.sh file.
echo "Downloading shtest..."
rm -rf "$SCRIPT_DIR/shtest"
for f in shtest setup no-main-program.jq yes-main-program.jq utf8-truncate.jq \
    torture/input0.json; do
    fetch "tests/$f" "$SCRIPT_DIR/shtest/$f"
done

# --- Download test modules ---
echo "Downloading test modules..."
rm -rf "$SCRIPT_DIR/modules"
mkdir -p "$SCRIPT_DIR/modules"

download_dir() {
    local api_path="$1"
    local local_dir="$2"

    local entries
    entries=$(gh api "repos/jqlang/jq/contents/${api_path}?ref=$TAG" \
        --jq '.[] | "\(.type)\t\(.path)\t\(.name)"')

    while IFS=$'\t' read -r type path name; do
        if [[ "$type" == "file" ]]; then
            gh api "repos/jqlang/jq/contents/${path}?ref=$TAG" --jq '.content' \
                | base64 -d > "$local_dir/$name"
            echo "  $local_dir/$name"
        elif [[ "$type" == "dir" ]]; then
            mkdir -p "$local_dir/$name"
            download_dir "$path" "$local_dir/$name"
        fi
    done <<< "$entries"
}

download_dir "tests/modules" "$SCRIPT_DIR/modules"

# --- Update mise.toml ---
if grep -q '^jq' "$PROJECT_ROOT/mise.toml"; then
    sed -i '' "s/^jq = .*/jq = \"$VERSION\"/" "$PROJECT_ROOT/mise.toml"
else
    # Append under [tools]
    sed -i '' "/^\[tools\]/a\\
jq = \"$VERSION\"
" "$PROJECT_ROOT/mise.toml"
fi
echo "Updated mise.toml to jq $VERSION"

# --- Verify ---
echo ""
echo "Done. Next:"
echo "  mise install"
echo "  python3 tests/jq_compat/corpus/gen_builtin_matrix.py   # regenerate the builtin matrix"
echo "  JQ_DIFF_UPDATE_BASELINE=1 cargo test --release jq_diff -- --ignored"
