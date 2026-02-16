# qj

Quick JSON. A jq-compatible processor, 3-100x faster on large inputs.

## When to use qj instead of jq

**Large JSON files (>10 MB).** qj parses with SIMD (simdjson via FFI). On a 49 MB file, `length` takes 33 ms vs jq's 395 ms (12x). Field extraction and `keys` are 5-15x faster than jq.

**NDJSON / JSONL pipelines.** qj auto-parallelizes across all cores. On 1.1 GB NDJSON: `select(.type == "PushEvent")` takes 119 ms vs jq's 12.8 s (108x). No `xargs` or `parallel` needed.

**When jq is fine.** Small files (<1 MB), complex multi-page scripts, or when you need 100% jq compatibility. qj covers 98.5% of jq's feature surface but doesn't support modules or arbitrary precision arithmetic.

## Quick start

```bash
cargo build --release
./target/release/qj '.name' data.json
cat logs.jsonl | ./target/release/qj -c 'select(.level == "ERROR")'
./target/release/qj '.items[] | {id, name}' large.json
```

## Benchmarks

All benchmarks on M4 Pro MacBook Pro. See [benches/](benches/) for methodology.

49 MB JSON (large_twitter.json):

| Workload | qj | jq | jaq | gojq |
|----------|----|----|-----|------|
| `.statuses \| length` | **33 ms** | 395 ms | 172 ms | 291 ms |
| `.statuses[] \| .user.name` | **153 ms** | 407 ms | 172 ms | 301 ms |
| `walk(if type == "boolean" then not else . end)` | **373 ms** | 3.57 s | 2.43 s | 1.60 s |

Field extraction and simple operations show the largest wins. Complex filter workloads where the evaluator dominates are closer to 2-3x over jq and roughly even with jaq.

GB-scale NDJSON (1.1 GB GitHub Archive, parallel processing):

| Workload | qj | jq | jaq | gojq |
|----------|----|----|-----|------|
| `-c '.'` (passthrough) | **772 ms** | 27.5 s | 5.0 s | 9.9 s |
| `length` | **101 ms** | 7.2 s | 2.7 s | 6.7 s |
| `select(.type == "PushEvent")` | **119 ms** | 12.8 s | 3.5 s | 7.8 s |
| `{type, repo: .repo.name, actor: .actor.login}` | **134 ms** | 7.9 s | 3.3 s | 6.9 s |
| `select(.type == "PushEvent") \| {actor, commits}` | **2.76 s** | 7.5 s | 3.1 s | 6.9 s |

Throughput: **1.4 GB/s** (vs jq 41 MB/s, jaq 225 MB/s, gojq 114 MB/s). Scales linearly: 4.8 GB NDJSON shows the same ratios ([full results](benches/results_large_only.md)). See also [tool comparison data](benches/results.md).

## How it works

- **SIMD parsing.** C++ simdjson (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks, ordered output. On Apple Silicon, uses only performance cores to avoid E-core contention.
- **Zero-copy I/O.** mmap — no heap allocation or memcpy for the input file.
- **On-demand extraction.** Common NDJSON patterns (`.field`, `select`, `{...}` reshaping) extract raw bytes directly from simdjson's On-Demand API, bypassing Rust value tree construction entirely. Original number representation (scientific notation, trailing zeros) is preserved.

## Compatibility

**98.5%** feature coverage (163/166 features passing, [details](tests/jq_compat/feature_results.md)).
**91%** pass rate on jq's official 497-test suite.

What's missing: module system (`import`/`include`), arbitrary precision arithmetic (qj uses i64/f64, large numbers preserved on passthrough).

## Known limitations

- No module system — `import`/`include` are not supported.
- No arbitrary precision arithmetic — i64/f64 internally. Large numbers are preserved on passthrough but arithmetic uses f64 precision.
- Some edge cases in `def` (def-inside-expressions, destructuring bind patterns).
- Single-document JSON >4 GB falls back to serde_json (simdjson's limit). Still faster than jq but ~3-6x slower than simdjson's fast path. **NDJSON (JSONL) is unaffected** since each line is parsed independently.
