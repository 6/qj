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

| Tool | Speed vs jq | Parallel NDJSON | SIMD | jq compat |
|------|-------------|-----------------|------|-----------|
| jq 1.7 | baseline | — | — | 100% |
| jaq 2.0 | 1.3–2x | — | — | ~90% |
| gojq 0.12 | 0.8–2.5x | — | — | ~85% |
| **jx** | **2–65x** | **yes** | **yes (NEON/AVX2)** | **~60%** |

Largest wins on parse-dominated workloads over large files; smallest on complex filters where evaluator cost dominates.

See [benches/](benches/) for methodology, full results, and how to reproduce.
