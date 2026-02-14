# jx — a faster jq for large JSON and JSONL

Fast jq-compatible JSON processor using SIMD parsing (C++ simdjson via FFI), parallel NDJSON processing, and streaming architecture. See PLAN.md for full design.

## After writing Rust code
```
cargo fmt
cargo clippy --release -- -D warnings
cargo test
```

## After writing shell scripts
```
shellcheck <file>.sh
```

## Testing
Run `cargo test` after any code change — it's fast.

- **Unit tests:** `#[cfg(test)]` modules alongside code.
- **Integration tests:** `tests/e2e.rs` — runs the `jx` binary against known JSON inputs.
  - Includes **jq conformance tests** (`assert_jq_compat`) that run both jx and jq and
    compare output. These run automatically when jq is installed, and are skipped otherwise.
  - Includes **number literal preservation tests** — verifies trailing zeros, scientific
    notation, and raw text are preserved from JSON input through output.
- **NDJSON tests:** `tests/ndjson.rs` — parallel NDJSON processing integration tests.
- **FFI tests:** `tests/simdjson_ffi.rs` — low-level simdjson bridge tests.
- **jq conformance suite:** `tests/jq_conformance.rs` — runs jq's official test suite
  (`tests/jq_compat/jq.test`, vendored from jqlang/jq) against jx and reports pass rate.
  ```
  cargo test jq_conformance -- --nocapture                              # summary
  cargo test jq_conformance_verbose -- --nocapture --ignored            # show failures
  ```
- **Cross-tool compat comparison:** `tests/jq_compat_runner.rs` — runs jq.test against
  jx, jq, jaq, and gojq (whichever are on `$PATH`), per-category breakdown.
  Results are also written to `tests/jq_compat/results.md`.
  ```
  cargo test jq_compat -- --nocapture              # summary + category table
  cargo test jq_compat_verbose -- --nocapture --ignored  # show failures
  ```
- **Updating the vendored test suite:** `tests/jq_compat/update_test_suite.sh` — downloads
  `jq.test` and test modules from a jq release tag and updates `mise.toml`.
  ```
  bash tests/jq_compat/update_test_suite.sh          # uses version from mise.toml
  bash tests/jq_compat/update_test_suite.sh 1.9.0    # upgrade to new version
  ```
- **Feature compatibility suite:** `tests/jq_compat/features.toml` — TOML-defined tests
  (2-3 per feature), runner produces per-feature Y/~/N matrix across jx, jq, jaq, gojq.
  ```
  cargo test feature_compat -- --nocapture              # summary + feature table
  cargo test feature_compat_verbose -- --nocapture --ignored  # show failures
  ```
- **When adding new jq builtins or language features**, always:
  1. Add corresponding e2e tests in `tests/e2e.rs` and `assert_jq_compat` checks
  2. Re-run `cargo test jq_compat -- --nocapture` and update jq compat % in `README.md`
- **Cache:** External tool results (jq, jaq, gojq) are cached in `tests/jq_compat/.cache/`.
  Cache auto-invalidates when test definitions or tool versions (`mise.toml`) change.
  Delete to force full re-run: `rm -rf tests/jq_compat/.cache/`
- **Conformance:** compare output against jq on real data.
```
diff <(./target/release/jx '.field' test.json) <(jq '.field' test.json)
```

## Fuzzing

Three fuzz targets exercise the C++/FFI boundary (`fuzz/`). Run after changing `src/simdjson/`. Requires nightly and `cargo-fuzz`.
```
cargo +nightly fuzz run fuzz_parse   -- -max_total_time=120
cargo +nightly fuzz run fuzz_dom     -- -max_total_time=120
cargo +nightly fuzz run fuzz_ndjson  -- -max_total_time=120
```

## Benchmarking

All benchmark scripts, data generators, and results live in `benches/`.

### Parse throughput (simdjson vs serde_json)
```
bash benches/download_testdata.sh   # twitter.json, citm_catalog.json, canada.json
bash benches/generate_ndjson.sh     # 100k.ndjson, 1m.ndjson
cargo bench --bench parse_throughput
```

### C++ baseline (no FFI, for overhead comparison)
```
bash benches/build_cpp_bench.sh
./benches/bench_cpp
```

### End-to-end tool comparison (jx vs jq vs jaq vs gojq)
```
bash benches/gen_large.sh           # ~49MB large_twitter.json, large.jsonl
bash benches/bench.sh               # defaults: 5 runs, 5s cooldown, writes benches/results.md
bash benches/bench.sh --runs 3 --cooldown 2   # faster run for quick checks
```

### Profiling a single run
```
./target/release/jx --debug-timing -c '.' benches/data/large_twitter.json > /dev/null
```

### Ad-hoc comparison
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
- `benches/` — all benchmark scripts, data generators, C++ baseline, and Cargo benchmarks
- `fuzz/` — cargo-fuzz targets for simdjson FFI boundary (requires nightly)
