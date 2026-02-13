# jx

A jq-compatible JSON processor that aims to be 10-50x faster on large inputs. Work in progress.

Uses C++ simdjson (SIMD parsing, On-Demand API) via FFI and built-in parallel NDJSON processing to outperform jq and jaq on large JSON files and JSONL streams.

## Setup

```bash
brew install mise
mise install
cargo build --release
```

## Benchmarks

**2-65x faster than jq** depending on workload. Largest wins on parse-dominated filters (identity, field extraction) over large files; smallest on complex filters where evaluator cost dominates.

See [benches/results.md](benches/results.md) for full results from local dedicated hardware. CI also produces [directional results](benches/results_ci.md) on shared runners.

To reproduce locally:

```bash
brew install hyperfine jq jaq gojq
bash benches/download_testdata.sh
bash benches/gen_large.sh
bash benches/bench.sh
```
