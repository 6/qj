#!/usr/bin/env bash
# Compile the standalone C++ benchmark.
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
SRC="$DIR/../src/simdjson"

echo "Compiling bench_cpp..."
c++ -std=c++17 -O3 -DNDEBUG \
    -I"$SRC" \
    "$DIR/bench_cpp.cpp" \
    "$SRC/simdjson.cpp" \
    -o "$DIR/bench_cpp"

echo "Done: $DIR/bench_cpp"
