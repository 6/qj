# qj

`qj` is Quick JSON, a `jq`-compatible processor. SIMD parsing and automatic parallelization across cores.

- **Single-threaded:** 4-30x faster on NDJSON, 2-10x on JSON.
- **Parallel:** 20-120x faster on NDJSON.

## When to use qj instead of jq

**NDJSON / JSONL pipelines.** qj auto-parallelizes across all cores. On 1.1 GB NDJSON: `select(.type == "PushEvent")` takes 104 ms vs jq's 12.6 s (121x). No `xargs` or `parallel` needed.

**Large JSON files (>10 MB).** qj parses with SIMD (simdjson via FFI). On a 49 MB file, `length` takes 34 ms vs jq's 361 ms (11x). Simple operations like `length` and `keys` are ~10x faster; complex filters like `map` and `reduce` are 2-4x.

**When jq is fine.** Small files (<1 MB), complex multi-page scripts, or when you need 100% jq compatibility. qj covers 98.5% of jq's feature surface but doesn't support modules or arbitrary precision arithmetic.

## Quick start

```bash
cargo build --release
./target/release/qj '.name' data.json
cat logs.jsonl | ./target/release/qj -c 'select(.level == "ERROR")'
./target/release/qj '.items[] | {id, name}' large.json
```

## Benchmarks

All benchmarks on M4 Pro MacBook Pro, 1.1 GB GitHub Archive NDJSON, 3 runs + 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine). Hyperfine discards stdout by default, so we measure compute + formatting, not terminal IO. See [benches/](benches/) for methodology.

Filters on the SIMD fast path show 59-121x gains. Evaluator-bound expressions show 27-28x. The single-threaded column shows qj's SIMD/fast-path gains without parallelism.

| Workload | qj (parallel) | qj (1 thread) | jq | Speedup |
|----------|----|----|---------|-----|
| `.actor.login` | **75 ms** | 347 ms | 7.2 s | **96x** |
| `length` | **91 ms** | 576 ms | 7.0 s | **77x** |
| `keys` | **120 ms** | 733 ms | 7.6 s | **64x** |
| `select(.type == "PushEvent")` | **104 ms** | 405 ms | 12.6 s | **121x** |
| `select(.type == "PushEvent") \| .payload.size` | **78 ms** | 426 ms | 7.2 s | **91x** |
| `{type, repo: .repo.name, actor: .actor.login}` | **132 ms** | 828 ms | 7.8 s | **59x** |
| `{type, commits: [.payload.commits[]?.message]}` | **295 ms** | 1.78 s | 7.9 s | **27x** |
| `{type, commits: (.payload.commits // [] \| length)}` | **266 ms** | 1.56 s | 7.5 s | **28x** |

[Full results with jaq and gojq](benches/results_large_only.md).

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
- NDJSON fast paths (e.g. `select`) output raw input bytes, so Unicode escapes like `\u000B` preserve their original hex casing. jq normalizes to lowercase (`\u000b`). Both are valid JSON.
