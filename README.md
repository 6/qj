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

See [BENCHMARKS.md](BENCHMARKS.md) for full results across filter tiers and file sizes.

To reproduce locally:

```bash
brew install hyperfine jq jaq gojq
bash bench/download_testdata.sh
bash bench/gen_large.sh
bash bench/bench.sh
```
