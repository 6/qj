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

|  | jq 1.7 | gojq 0.12 | jaq 2.0 | **jx** |
|--|--------|-----------|---------|--------|
| Throughput (49MB, `-c '.'`) | 42 MB/s | 110 MB/s | 194 MB/s | **2.7 GB/s** |
| Parallel NDJSON | — | — | — | **yes** |
| SIMD | — | — | — | **yes (NEON/AVX2)** |
| jq compat | 100% | ~85% | ~90% | **~60%** |

Largest wins on parse-dominated workloads over large files; smallest on complex filters where evaluator cost dominates.

See [benches/](benches/) for methodology, full results, and how to reproduce.
