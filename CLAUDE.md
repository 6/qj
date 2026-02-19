# qj — a fast, jq-compatible JSON processor

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
cargo test --release jq_conformance_ndjson -- --ignored --nocapture     # jq.test via NDJSON path (single vs NDJSON diff)
cargo test --release jq_conformance_verbose -- --ignored --nocapture    # jq.test with failure details
cargo test --release conformance_gaps -- --ignored                      # gap tests by category
cargo test --release gap_label_break -- --ignored                       # run one category
cargo test --release jq_compat -- --ignored --nocapture                 # cross-tool comparison
cargo test --release feature_compat -- --ignored --nocapture            # feature matrix
cargo test --release jq_differential -- --ignored --nocapture          # proptest differential vs jq
cargo test --release differential_filter -- --ignored --nocapture      # differential: random filters
cargo test --release differential_arithmetic -- --ignored --nocapture  # differential: arithmetic focus
cargo test --release differential_builtins -- --ignored --nocapture    # differential: builtins focus
cargo test --release differential_formats -- --ignored --nocapture     # differential: format strings
```

**Note:** The conformance test prints its summary to stderr (visible without `--nocapture`).
Never pipe `--nocapture` output through `tail` — the verbose test produces 500+ lines which
can OOM `tail` on macOS. Use `grep` to filter if needed, or run the non-verbose test.

- **Unit tests:** `#[cfg(test)]` modules alongside code.
- **Integration tests:** `tests/e2e.rs` — runs the `qj` binary against known JSON inputs.
  - Includes **jq conformance tests** (`assert_jq_compat`) that run both qj and jq and
    compare output. These run automatically when jq is installed, and are skipped otherwise.
  - **Zero divergence policy:** Every e2e test that exercises jq-compatible behavior MUST
    use `assert_jq_compat` to verify output matches jq exactly. Never write tests that
    accept output differing from jq — if a fast path (passthrough, NDJSON, etc.) would
    produce different results, it must fall back to the normal evaluator.
  - Includes **number literal preservation tests** — verifies trailing zeros, scientific
    notation, and raw text are preserved from JSON input through output.
- **NDJSON tests:** `tests/ndjson.rs` — parallel NDJSON processing integration tests.
- **FFI tests:** `tests/simdjson_ffi.rs` — low-level simdjson bridge tests.
- **jq conformance suite** (`#[ignore]`): `tests/jq_conformance.rs` — runs jq's official test
  suite (`tests/jq_compat/jq.test`, vendored from jqlang/jq) against qj and reports pass rate.
  Also includes `jq_conformance_ndjson` which runs each object/array test case through both
  single-doc and NDJSON paths, asserting identical output (catches NDJSON path divergences).
- **Conformance gap tests** (`#[ignore]`): `tests/conformance_gaps.rs` — 9 tests for
  jq.test bignum/precision edge cases. All pass with `QJ_JQ_COMPAT=1` (497/497).
  See `docs/CONFORMANCE_100.md` for analysis.
- **Cross-tool compat comparison** (`#[ignore]`): `tests/jq_compat_runner.rs` — runs jq.test
  against qj, jq, jaq, and gojq. Writes `tests/jq_compat/results.md`.
- **Feature compatibility suite** (`#[ignore]`): `tests/jq_compat/features.toml` — TOML-defined
  tests, per-feature Y/~/N matrix. Writes `tests/jq_compat/feature_results.md`.
- **Differential testing** (`#[ignore]`): `tests/jq_differential.rs` — property-based tests using
  `proptest` that generate random (filter, input) pairs and compare qj vs jq output. Four focused
  tests: general filters, arithmetic, builtins, and format strings. 2000 cases each. Catches
  behavioral divergences that hand-written tests miss. Run iteratively: fix or exclude each
  failure, re-run to find the next.
- **Updating the vendored test suite:** `tests/jq_compat/update_test_suite.sh` — downloads
  `jq.test` and test modules from a jq release tag and updates `mise.toml`.
  ```
  bash tests/jq_compat/update_test_suite.sh          # uses version from mise.toml
  bash tests/jq_compat/update_test_suite.sh 1.9.0    # upgrade to new version
  ```
- **When adding new jq builtins or language features**, always:
  1. Add corresponding e2e tests in `tests/e2e.rs` and `assert_jq_compat` checks
  2. Run `cargo test --release jq_compat -- --ignored --nocapture` and update jq compat % in `README.md`
- **When adding or modifying NDJSON fast-path variants** (`NdjsonFastPath` enum in
  `src/parallel/ndjson.rs`), always:
  1. Add a filter for the new variant in `all_fast_path_test_filters()` (same file) —
     the exhaustive match will cause a compile error if you forget
  2. Add the filter to `FILTERS` in `fuzz/fuzz_targets/fuzz_ndjson_diff.rs`
  3. Run `cargo +nightly fuzz run fuzz_ndjson_diff -s none -- -max_total_time=120`
- **Cache:** External tool results (jq, jaq, gojq) are cached in `tests/jq_compat/.cache/`.
  Cache auto-invalidates when test definitions or tool versions (`mise.toml`) change.
  Delete to force full re-run: `rm -rf tests/jq_compat/.cache/`
- **Conformance:** compare output against jq on real data.
```
diff <(./target/release/qj '.field' test.json) <(jq '.field' test.json)
```

## Fuzzing

Ten fuzz targets in `fuzz/`. Requires nightly and `cargo-fuzz`.

Fuzz binaries use libfuzzer which runs indefinitely without `-max_total_time`.
All `[[bin]]` entries have `test = false` to prevent `cargo test` from picking them up.
Always run fuzz targets individually via `cargo +nightly fuzz run <target> -- -max_total_time=N`.

**ASan link error on macOS:** The C++ FFI objects (simdjson/bridge) are compiled with Apple
Clang, whose ASan runtime is incompatible with rustc nightly's. Use `-s none` to disable
sanitizers: `cargo +nightly fuzz run <target> -s none -- -max_total_time=N`.

**FFI boundary** (run after changing `src/simdjson/`):
```
cargo +nightly fuzz run fuzz_parse       -s none -- -max_total_time=120
cargo +nightly fuzz run fuzz_dom         -s none -- -max_total_time=120
cargo +nightly fuzz run fuzz_ndjson      -s none -- -max_total_time=120
cargo +nightly fuzz run fuzz_bridge_map  -s none -- -max_total_time=120
```

**Filter pipeline** (run after changing `src/filter/`):
```
cargo +nightly fuzz run fuzz_filter_parse -s none -- -max_total_time=120
cargo +nightly fuzz run fuzz_eval         -s none -- -max_total_time=120
```

**NDJSON fast-path differential** (run after changing `src/parallel/`):
```
cargo +nightly fuzz run fuzz_ndjson_diff  -s none -- -max_total_time=120
```

**flat_eval vs eval differential** (run after changing `src/flat_eval.rs`):
```
cargo +nightly fuzz run fuzz_flat_eval_diff -s none -- -max_total_time=120
```

**Output formatting** (run after changing `src/output.rs`):
```
cargo +nightly fuzz run fuzz_output        -s none -- -max_total_time=120
cargo +nightly fuzz run fuzz_double_format -s none -- -max_total_time=120
```

## Benchmarking

All benchmark scripts, data generators, and results live in `benches/`.

### Regression detection (iai-callgrind, requires valgrind)
```
cargo bench --bench eval_regression
```
Counts CPU instructions (deterministic, no wall-clock noise). Runs on CI for every PR (Ubuntu only).
Covers: SIMD parse, flat eval, standard eval, filter parsing.

### Parse throughput (simdjson vs serde_json)
```
bash benches/download_data.sh --json --gharchive  # twitter.json + gharchive.ndjson
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
cargo run --release --features bench --bin bench_tools -- --type json                    # JSON (large_twitter.json)
cargo run --release --features bench --bin bench_tools -- --type ndjson                  # NDJSON (gharchive_medium.ndjson, 3.4GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson --size small     # NDJSON (gharchive.ndjson, 1.1GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson --size large     # NDJSON (gharchive_large.ndjson, 6.2GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson-extended --size xsmall  # extended: streaming + stdin + complex + slurp
cargo run --release --features bench --bin bench_tools -- --type json --runs 3 --cooldown 2  # quick JSON run
```

### Memory usage comparison (qj vs jq vs jaq vs gojq)
```
cargo run --release --features bench --bin bench_mem -- --type json     # JSON (large_twitter.json)
cargo run --release --features bench --bin bench_mem -- --type ndjson    # NDJSON (gharchive.ndjson)
```
Measures peak RSS via `wait4()` rusage. No external tools needed (no hyperfine).
Results written to `benches/results_mem_json.md` / `benches/results_mem_ndjson.md`.

### GH Archive data (for NDJSON benchmarks)
```
bash benches/download_data.sh --gharchive           # gharchive.ndjson (~1.1GB) + .ndjson.gz
bash benches/download_data.sh --xsmall              # gharchive_xsmall.ndjson (~500MB)
bash benches/download_data.sh --medium              # gharchive_medium.ndjson (~3.4GB, ~1.2M records)
bash benches/download_data.sh --large               # gharchive_large.ndjson (~4.7GB)
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
Always warm cache with `--warmup 1` (sufficient for file I/O cache; higher values add time without improving accuracy).
```
hyperfine --warmup 1 './target/release/qj ".field" test.json' 'jq ".field" test.json' 'jaq ".field" test.json'
```

### Environment variables
- `QJ_WINDOW_SIZE=N` — NDJSON streaming window size in megabytes. Default is `num_cores × 2` MB
  (floor 8 MB). Larger values use more memory but may help on machines with many cores.
- `QJ_NO_MMAP=1` — Disable mmap for file I/O (use heap allocation instead).
- `QJ_NO_FAST_PATH=1` — Disable NDJSON fast paths (for A/B benchmarking).
- `QJ_JQ_COMPAT=1` — Match jq's precision behavior: arithmetic truncates to f64 for numbers
  > 2^53, extreme exponents preserved, `have_decnum=true`. Enables 497/497 (100%) conformance.
  See `docs/CONFORMANCE_100.md`.

### Important
Never run benchmarks concurrently with tests or other CPU-intensive processes.
Benchmarks require exclusive CPU access for reliable results.

## Architecture
- `src/simdjson/` — vendored simdjson.h/cpp + C-linkage bridge + safe Rust FFI wrapper
- `src/filter/` — jq filter lexer, parser, AST evaluator (On-Demand fast path + DOM fallback)
- `src/value.rs` — JSON value representation (Arc-based arrays/objects)
- `src/flat_value.rs` — zero-copy navigation of flat token buffer, avoids materializing full Value tree
- `src/flat_eval.rs` — lazy evaluator operating on FlatValue for NDJSON lines
- `src/parallel/` — NDJSON chunk splitter + thread pool
- `src/output.rs` — pretty-print, compact, raw output formatters
- `src/input.rs` — input preprocessing (BOM stripping, JSON/NDJSON parsing into Values)
- `src/decompress.rs` — transparent gzip (flate2) and zstd decompression, detected by file extension
- `benches/` — all benchmark scripts, data generators, C++ baseline, and Cargo benchmarks
- `fuzz/` — cargo-fuzz targets for simdjson FFI boundary (requires nightly)

## Compressed file support
Transparent decompression for `.gz` (gzip) and `.zst`/`.zstd` (zstd) files, detected by extension.
Glob patterns in file arguments are expanded (quote to bypass shell: `'data/*.json.gz'`).
```
qj '.actor.login' data/*.json.gz                      # shell-expanded
qj 'select(.type == "PushEvent")' 'data/*.ndjson.gz'  # qj-expanded glob
qj -s 'add' file1.json.zst file2.json                 # mixed compressed + plain
```
Compressed files are decompressed to memory, then processed through the normal NDJSON/JSON pipeline.
For benchmarking, `benches/download_data.sh --gharchive` produces a `.ndjson.gz` alongside the uncompressed files.
