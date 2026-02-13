# jx — a faster jq for large JSON and JSONL

Fast jq-compatible JSON processor using SIMD parsing (C++ simdjson via FFI), parallel NDJSON processing, and streaming architecture. See PLAN.md for full design.

## After writing Rust code
```
cargo fmt
cargo clippy --release -- -D warnings
cargo test
```

## Testing
Run `cargo test` after any code change — it's fast.

- **Unit tests:** `#[cfg(test)]` modules alongside code.
- **Integration tests:** `tests/` directory. Run the `jx` binary against known JSON inputs, compare output to `jq`.
- **Conformance:** compare output against jq on real data.
```
diff <(./target/release/jx '.field' test.json) <(jq '.field' test.json)
```

## Benchmarking

### Parse throughput (simdjson vs serde_json)
```
bash bench/download_testdata.sh   # twitter.json, citm_catalog.json, canada.json
bash bench/generate_ndjson.sh     # 100k.ndjson, 1m.ndjson
cargo bench --bench parse_throughput
```

### C++ baseline (no FFI, for overhead comparison)
```
bash bench/build_cpp_bench.sh
./bench/bench_cpp
```

### End-to-end tool comparison
Always warm cache with `--warmup 3`.
```
hyperfine --warmup 3 './target/release/jx ".field" test.json' 'jq ".field" test.json' 'jaq ".field" test.json'
```

## Architecture
- `src/simdjson/` — vendored simdjson.h/cpp + C-linkage bridge + safe Rust FFI wrapper
- `src/filter/` — jq filter lexer, parser, AST evaluator (On-Demand fast path + DOM fallback)
- `src/parallel/` — NDJSON chunk splitter + thread pool
- `src/output/` — pretty-print, compact, raw output formatters
- `src/io/` — mmap for files, streaming for stdin
