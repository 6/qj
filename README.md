# qj

Quick JSON. A jq-compatible processor, 3-60x faster on large inputs.

## When to use qj instead of jq

**Large JSON files (>10 MB).** qj parses with SIMD (simdjson via FFI) at 2.2 GB/s. On a 49 MB file, `qj -c '.'` finishes in 22 ms vs jq's 1.2 s — 53x faster. Field extraction, `length`, and `keys` are 5-15x faster than jq.

**NDJSON / JSONL pipelines.** qj auto-parallelizes across all cores. On 1M lines: `qj '.name'` takes 117 ms vs jq's 1.3 s (11x) and jaq's 715 ms (6x). No `xargs` or `parallel` needed.

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
| `-c '.'` | **22 ms** | 1.16 s | 258 ms | 444 ms |
| `.statuses \| length` | **33 ms** | 395 ms | 172 ms | 291 ms |
| `.statuses[] \| .user.name` | **153 ms** | 407 ms | 172 ms | 301 ms |
| `walk(if type == "boolean" then not else . end)` | **373 ms** | 3.57 s | 2.43 s | 1.60 s |

Parse-dominated workloads (identity, field extraction) show the largest wins. Complex filter workloads where the evaluator dominates are closer to 2-3x over jq and roughly even with jaq.

GB-scale (GH Archive):

| File | Workload | qj | jq | Speedup |
|------|----------|----|----|---------|
| 1.1 GB NDJSON | `-c '.'` | 822ms | 28.3s | **34x** |
| 1.1 GB NDJSON | filter | 3.05s | 7.51s | **2.5x** |
| 1.1 GB JSON | `-c '.'` | 474ms | 29.1s | **61x** |
| 1.1 GB JSON | filter | 3.89s | 13.6s | **3.5x** |
| 4.8 GB NDJSON | `-c '.'` | 3.52s | 120.7s | **34x** |
| 4.8 GB JSON† | filter | 22.5s | 62.1s | **2.8x** |

†serde_json fallback — simdjson has a 4 GB single-document limit, so qj falls back to serde_json for larger JSON files. **NDJSON (JSONL) is unaffected** (each line is parsed independently).

See [full GB-scale results](benches/results_large_only.md) and [tool comparison data](benches/results.md).

## How it works

- **SIMD parsing.** C++ simdjson (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks, ordered output.
- **Fast-path passthrough.** Identity compact uses `simdjson::minify()` at ~10 GB/s, bypassing the value tree entirely.

## Compatibility

**98.5%** feature coverage (163/166 features passing, [details](tests/jq_compat/feature_results.md)).
**91%** pass rate on jq's official 497-test suite.

What's missing: module system (`import`/`include`), arbitrary precision arithmetic (qj uses i64/f64, large numbers preserved on passthrough).

## Known limitations

- No module system — `import`/`include` are not supported.
- No arbitrary precision arithmetic — i64/f64 internally. Large numbers are preserved on passthrough but arithmetic uses f64 precision.
- Some edge cases in `def` (def-inside-expressions, destructuring bind patterns).
- Single-document JSON >4 GB falls back to serde_json (simdjson's limit). Still faster than jq but ~3-6x slower than simdjson's fast path. **NDJSON (JSONL) is unaffected** since each line is parsed independently.
