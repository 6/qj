# jx

A jq-compatible JSON processor that aims to be 10-50x faster on large inputs. Work in progress.

Uses C++ simdjson (SIMD parsing, On-Demand API) via FFI and built-in parallel NDJSON processing to outperform jq and jaq on large JSON files and JSONL streams.

## Setup

```bash
brew install mise
mise install
cargo build --release
```

## Benchmarking

```bash
brew install hyperfine jq
hyperfine --warmup 3 \
  './target/release/jx ".field" large.json' \
  'jq ".field" large.json'
```
