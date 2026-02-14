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

|  | jq 1.8.1 | gojq 0.12.18 | jaq 2.3 | **jx** |
|--|--------|-----------|---------|--------|
| Throughput (49MB, `-c '.'`) | 42 MB/s | 110 MB/s | 190 MB/s | **2.4 GB/s** |
| Parallel NDJSON | — | — | — | **yes** |
| SIMD | — | — | — | **yes (NEON/AVX2)** |
| jq compat ([jq.test](tests/jq_compat/)) | 100% | 85% | 69% | **60%** |

Largest wins on parse-dominated workloads over large files; smallest on complex filters where evaluator cost dominates.

jq compat % = pass rate on [jq's official test suite](https://github.com/jqlang/jq/blob/jq-1.8.1/tests/jq.test) (497 tests, JSON-aware comparison). Run `cargo test jq_compat -- --nocapture` to reproduce.

See [benches/](benches/) for methodology, full results, and how to reproduce.
