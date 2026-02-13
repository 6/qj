#!/usr/bin/env bash
# Update the vendored jq test suite to match a specific jq release tag.
#
# Usage:
#   bash tests/jq_compat/update_test_suite.sh           # uses version from mise.toml
#   bash tests/jq_compat/update_test_suite.sh 1.9.0     # explicit version
#
# Requires: gh (GitHub CLI), jq

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
echo "Updating test suite to $TAG..."

# --- Download jq.test via base64 to preserve control characters ---
echo "Downloading jq.test..."
gh api "repos/jqlang/jq/contents/tests/jq.test?ref=$TAG" --jq '.content' \
    | base64 -d > "$SCRIPT_DIR/jq.test"
echo "  $(wc -l < "$SCRIPT_DIR/jq.test") lines"

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
echo "Done. Verify with:"
echo "  mise install"
echo "  bash tests/jq_compat/run_compat.sh"
