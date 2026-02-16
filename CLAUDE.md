# qj — a faster jq for large JSON and JSONL

Fast jq-compatible JSON processor using SIMD parsing (C++ simdjson via FFI), parallel NDJSON processing, and streaming architecture.

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
`cargo test` runs the fast suite (unit + e2e + ndjson + ffi, ~5s).
Compat suites are `#[ignore]` — run them with `--release` after adding features.

```
cargo test                                                              # fast: unit + e2e (~5s)
cargo test --release -- --ignored --nocapture                           # all tests including compat (~50s)
cargo test --release jq_conformance -- --ignored                        # jq.test pass rate (summary on stderr)
cargo test --release jq_conformance_verbose -- --ignored --nocapture    # jq.test with failure details
cargo test --release conformance_gaps -- --ignored                      # gap tests by category
cargo test --release gap_label_break -- --ignored                       # run one category
cargo test --release jq_compat -- --ignored --nocapture                 # cross-tool comparison
cargo test --release feature_compat -- --ignored --nocapture            # feature matrix
```

**Note:** The conformance test prints its summary to stderr (visible without `--nocapture`).
Never pipe `--nocapture` output through `tail` — the verbose test produces 500+ lines which
can OOM `tail` on macOS. Use `grep` to filter if needed, or run the non-verbose test.

- **Unit tests:** `#[cfg(test)]` modules alongside code.
- **Integration tests:** `tests/e2e.rs` — runs the `qj` binary against known JSON inputs.
  - Includes **jq conformance tests** (`assert_jq_compat`) that run both qj and jq and
    compare output. These run automatically when jq is installed, and are skipped otherwise.
  - Includes **number literal preservation tests** — verifies trailing zeros, scientific
    notation, and raw text are preserved from JSON input through output.
- **NDJSON tests:** `tests/ndjson.rs` — parallel NDJSON processing integration tests.
- **FFI tests:** `tests/simdjson_ffi.rs` — low-level simdjson bridge tests.
- **jq conformance suite** (`#[ignore]`): `tests/jq_conformance.rs` — runs jq's official test
  suite (`tests/jq_compat/jq.test`, vendored from jqlang/jq) against qj and reports pass rate.
- **Conformance gap tests** (`#[ignore]`): `tests/conformance_gaps.rs` — 93 individual tests for
  currently-failing jq.test cases, categorized by feature (label/break, foreach, destructuring,
  modules, bignum, etc.) with fix suggestions in comments. Run by category to track progress.
- **Cross-tool compat comparison** (`#[ignore]`): `tests/jq_compat_runner.rs` — runs jq.test
  against qj, jq, jaq, and gojq. Writes `tests/jq_compat/results.md`.
- **Feature compatibility suite** (`#[ignore]`): `tests/jq_compat/features.toml` — TOML-defined
  tests, per-feature Y/~/N matrix. Writes `tests/jq_compat/feature_results.md`.
- **Updating the vendored test suite:** `tests/jq_compat/update_test_suite.sh` — downloads
  `jq.test` and test modules from a jq release tag and updates `mise.toml`.
  ```
  bash tests/jq_compat/update_test_suite.sh          # uses version from mise.toml
  bash tests/jq_compat/update_test_suite.sh 1.9.0    # upgrade to new version
  ```
- **When adding new jq builtins or language features**, always:
  1. Add corresponding e2e tests in `tests/e2e.rs` and `assert_jq_compat` checks
  2. Run `cargo test --release jq_compat -- --ignored --nocapture` and update jq compat % in `README.md`
- **Cache:** External tool results (jq, jaq, gojq) are cached in `tests/jq_compat/.cache/`.
  Cache auto-invalidates when test definitions or tool versions (`mise.toml`) change.
  Delete to force full re-run: `rm -rf tests/jq_compat/.cache/`
- **Conformance:** compare output against jq on real data.
```
diff <(./target/release/qj '.field' test.json) <(jq '.field' test.json)
```

## Fuzzing

Seven fuzz targets in `fuzz/`. Requires nightly and `cargo-fuzz`.

**FFI boundary** (run after changing `src/simdjson/`):
```
cargo +nightly fuzz run fuzz_parse   -- -max_total_time=120
cargo +nightly fuzz run fuzz_dom     -- -max_total_time=120
cargo +nightly fuzz run fuzz_ndjson  -- -max_total_time=120
```

**Filter pipeline** (run after changing `src/filter/`):
```
cargo +nightly fuzz run fuzz_filter_parse -- -max_total_time=120
cargo +nightly fuzz run fuzz_eval         -- -max_total_time=120
```

**NDJSON fast-path differential** (run after changing `src/parallel/`):
```
cargo +nightly fuzz run fuzz_ndjson_diff  -- -max_total_time=120
```

**Output formatting** (run after changing `src/output.rs`):
```
cargo +nightly fuzz run fuzz_output        -- -max_total_time=120
cargo +nightly fuzz run fuzz_double_format -- -max_total_time=120
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

### End-to-end tool comparison (qj vs jq vs jaq vs gojq)
```
bash benches/setup_bench_data.sh    # all test data (includes ~1GB GH Archive download)
cargo run --release --bin bench_tools                               # defaults: 5 runs, 5s cooldown
cargo run --release --bin bench_tools -- --runs 3 --cooldown 2      # faster run for quick checks
```

### GH Archive (real-world ~1.1GB / ~6.2GB)
```
bash benches/download_gharchive.sh           # gharchive.ndjson (~1.1GB), gharchive.json (~1.1GB)
bash benches/download_gharchive.sh --large   # gharchive_large.ndjson (~6.2GB), gharchive_large.json (~6.2GB)
```
Use `QJ_GHARCHIVE_HOURS=2` for quick testing with fewer hours of data.

### Profiling a single run
```
./target/release/qj --debug-timing -c '.' benches/data/large_twitter.json > /dev/null
```
**Caveat:** `--debug-timing` uses the On-Demand parse path (`dom_parse_to_value`), not the
production DOM tape walk path used by flat eval and the regular eval pipeline. Its parse times
are ~30% higher than actual production performance. Use `hyperfine` for accurate benchmarks.

### Ad-hoc comparison
Always warm cache with `--warmup 3`.
```
hyperfine --warmup 3 './target/release/qj ".field" test.json' 'jq ".field" test.json' 'jaq ".field" test.json'
```

### Important
Never run benchmarks concurrently with tests or other CPU-intensive processes.
Benchmarks require exclusive CPU access for reliable results.

## Architecture
- `src/simdjson/` — vendored simdjson.h/cpp + C-linkage bridge + safe Rust FFI wrapper
- `src/filter/` — jq filter lexer, parser, AST evaluator (On-Demand fast path + DOM fallback)
- `src/parallel/` — NDJSON chunk splitter + thread pool
- `src/output/` — pretty-print, compact, raw output formatters
- `src/input.rs` — input preprocessing (BOM stripping, JSON/NDJSON parsing into Values)
- `benches/` — all benchmark scripts, data generators, C++ baseline, and Cargo benchmarks
- `fuzz/` — cargo-fuzz targets for simdjson FFI boundary (requires nightly)
