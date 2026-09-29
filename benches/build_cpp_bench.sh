#!/usr/bin/env bash
# Compile the standalone C++ benchmark (simdjson's DOM parse without FFI).
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
SIMDJSON="$DIR/../simdjson"

echo "Compiling bench_cpp..."
c++ -std=c++17 -O3 -DNDEBUG \
    -I"$SIMDJSON" \
    "$DIR/bench_cpp.cpp" \
    "$SIMDJSON/simdjson.cpp" \
    -o "$DIR/bench_cpp"

echo "Done: $DIR/bench_cpp"
