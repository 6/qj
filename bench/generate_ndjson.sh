#!/usr/bin/env bash
# Generate synthetic NDJSON benchmark files.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)/data"
mkdir -p "$DIR"

# Build gen_ndjson if not already built
cargo build --release --bin gen_ndjson 2>/dev/null

GEN="$(cd "$(dirname "$0")/.." && pwd)/target/release/gen_ndjson"

if [ ! -f "$DIR/100k.ndjson" ]; then
    echo "Generating 100k.ndjson (100K lines)..."
    "$GEN" 100000 > "$DIR/100k.ndjson"
    echo "  $(wc -c < "$DIR/100k.ndjson" | tr -d ' ') bytes"
else
    echo "100k.ndjson already exists"
fi

if [ ! -f "$DIR/1m.ndjson" ]; then
    echo "Generating 1m.ndjson (1M lines)..."
    "$GEN" 1000000 > "$DIR/1m.ndjson"
    echo "  $(wc -c < "$DIR/1m.ndjson" | tr -d ' ') bytes"
else
    echo "1m.ndjson already exists"
fi

echo "Done. NDJSON data in $DIR/"
