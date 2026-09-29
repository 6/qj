#!/usr/bin/env bash
# Download the jq release source tarball used as the porting reference for src/jq/.
#
# Usage:
#   bash tests/jq_compat/fetch_jq_source.sh           # uses version from mise.toml
#   bash tests/jq_compat/fetch_jq_source.sh 1.8.1     # explicit version
#
# Extracts to target/jq-src/jq-<version>/ (gitignored). Prints the path.
# Requires: gh (GitHub CLI)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

if [[ -n "${1:-}" ]]; then
    VERSION="$1"
else
    VERSION=$(grep '^jq' "$PROJECT_ROOT/mise.toml" | sed 's/.*= *"\(.*\)"/\1/')
fi

DEST="$PROJECT_ROOT/target/jq-src"
if [[ ! -d "$DEST/jq-$VERSION/src" ]]; then
    mkdir -p "$DEST"
    gh release download "jq-$VERSION" -R jqlang/jq -p "jq-$VERSION.tar.gz" -D "$DEST" --clobber >&2
    tar -xzf "$DEST/jq-$VERSION.tar.gz" -C "$DEST"
    rm -f "$DEST/jq-$VERSION.tar.gz"
fi
echo "$DEST/jq-$VERSION"
