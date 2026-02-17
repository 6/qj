# Benchmarks

## Quick start

Prerequisites:

```bash
mise install              # installs jq, jaq, gojq, hyperfine
brew install coreutils    # gtimeout (macOS only, needed by run_compat.sh)
```

Generate test data and run:

```bash
bash benches/setup_bench_data.sh    # all test data (includes ~1GB GH Archive download)
cargo run --release --features bench --bin bench_tools -- --type json
cargo run --release --features bench --bin bench_tools -- --type ndjson
```

## End-to-end tool comparison (qj vs jq vs jaq vs gojq)

Uses [hyperfine](https://github.com/sharkdp/hyperfine) for wall-clock measurement (process spawn + read + parse + filter + format + write to /dev/null).

### JSON

```bash
bash benches/download_data.sh --json
bash benches/generate_data.sh --json
cargo run --release --features bench --bin bench_tools -- --type json
cargo run --release --features bench --bin bench_tools -- --type json --runs 3 --cooldown 2
```

Results: `benches/results_json.md`

### NDJSON

```bash
bash benches/download_data.sh --medium              # 3.4GB, ~1.2M records (default for benchmarks)
cargo run --release --features bench --bin bench_tools -- --type ndjson                     # medium (3.4GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson --size small         # 1.1GB
cargo run --release --features bench --bin bench_tools -- --type ndjson --size large         # 4.7GB, 3 filters
```

Results: `benches/results_ndjson_{size}.md`

### Extended NDJSON (stdin, complex filters, slurp)

Tests scenarios where qj's speedup is smaller: stdin (no mmap), complex filters (no on-demand fast path), and slurp mode (no parallelism).

```bash
bash benches/download_data.sh --xsmall              # 500MB, 1 hour
cargo run --release --features bench --bin bench_tools -- --type ndjson-extended --size xsmall --runs 1 --cooldown 0
```

Results: `benches/results_ndjson_extended_{size}.md`

### GH Archive datasets

```bash
bash benches/download_data.sh --gharchive           # small: ~1.1GB (2 hours)
bash benches/download_data.sh --xsmall              # xsmall: ~500MB (1 hour)
bash benches/download_data.sh --medium              # medium: ~3.4GB (6 hours)
bash benches/download_data.sh --large               # large: ~4.7GB (24 hours)
```

All from [GH Archive](https://www.gharchive.org/), date: 2024-01-15 (except large: 2026-02-01).

## Memory usage comparison

Measures peak resident set size (RSS) via `wait4()` rusage:

```bash
cargo run --release --features bench --bin bench_mem -- --type json     # peak RSS on large_twitter.json
cargo run --release --features bench --bin bench_mem -- --type ndjson    # peak RSS on gharchive.ndjson
```

Results: `benches/results_mem_json.md` / `benches/results_mem_ndjson.md`

## Other benchmarks

### Regression detection (iai-callgrind, requires valgrind)

```bash
cargo bench --bench eval_regression
```

Counts CPU instructions (deterministic, no wall-clock noise). Runs on CI for every PR.

### Parse throughput (simdjson vs serde_json)

```bash
bash benches/download_data.sh --json
bash benches/generate_data.sh --ndjson
cargo bench --bench parse_throughput
```

### C++ baseline (no FFI overhead)

```bash
bash benches/build_cpp_bench.sh
./benches/bench_cpp
```

### Ad-hoc comparison

```bash
hyperfine --warmup 1 \
  './target/release/qj ".field" test.json' \
  'jq ".field" test.json' \
  'jaq ".field" test.json'
```
