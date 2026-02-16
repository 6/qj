# qj

`qj` is Quick JSON, a `jq`-compatible processor. SIMD parsing and automatic parallelization across cores.

- **Single-threaded:** 4-60x faster on NDJSON, 2-13x on JSON.
- **Parallel:** 28-150x faster on NDJSON.

## When to use qj instead of jq

**NDJSON / JSONL pipelines.** qj auto-parallelizes across all cores. On 1.1 GB NDJSON: `select(.type == "PushEvent")` takes 101 ms vs jq's 13.5 s (133x). No `xargs` or `parallel` needed.

**Large JSON files (>10 MB).** qj parses with SIMD (simdjson via FFI). On a 49 MB file, `length` takes 34 ms vs jq's 361 ms (11x). Simple operations like `length`, `keys`, and `map` are 10-12x faster; `group_by` and `sort_by` are the slowest at ~2x.

**When jq is fine.** Small files (<1 MB), complex multi-page scripts, or when you need 100% jq compatibility. qj covers 98.5% of jq's feature surface but doesn't support modules or arbitrary precision arithmetic.

**Memory tradeoff.** qj trades memory for speed. jq streams one line at a time (~5 MB for any size NDJSON). qj streams in parallel windows — ~64 MB for NDJSON regardless of file size, or ~19 MB single-threaded. For single-document JSON, all tools load the full file. If memory is tight (small containers, embedded), jq is the safer choice.

## Quick start

```bash
cargo build --release
./target/release/qj '.name' data.json
cat logs.jsonl | ./target/release/qj -c 'select(.level == "ERROR")'
./target/release/qj '.items[] | {id, name}' large.json

# Compressed files — transparent gzip/zstd decompression
./target/release/qj '.actor.login' gharchive-2024-01-15-*.json.gz

# Glob patterns (quote to let qj expand instead of shell)
./target/release/qj 'select(.type == "PushEvent")' 'data/*.ndjson.gz'
```

## Benchmarks

All benchmarks on M4 Pro MacBook Pro, 1.1 GB GitHub Archive NDJSON, 3 runs + 1 warmup via [hyperfine](https://github.com/sharkdp/hyperfine). Hyperfine discards stdout by default, so we measure compute + formatting, not terminal IO. See [benches/](benches/) for methodology.

Filters on the SIMD fast path show 66-150x gains. Evaluator-bound expressions show 28-29x. The single-threaded column shows qj's SIMD/fast-path gains without parallelism.

| Workload | qj (parallel) | qj (1 thread) | jq | Speedup |
|----------|----|----|---------|-----|
| `.actor.login` | **66 ms** | 338 ms | 7.2 s | **109x** |
| `length` | **108 ms** | 593 ms | 7.2 s | **66x** |
| `keys` | **109 ms** | 737 ms | 7.7 s | **70x** |
| `select(.type == "PushEvent")` | **101 ms** | 406 ms | 13.5 s | **133x** |
| `select(.type == "PushEvent") \| .payload.size` | **77 ms** | 428 ms | 7.2 s | **94x** |
| `{type, repo: .repo.name, actor: .actor.login}` | **116 ms** | 779 ms | 7.9 s | **68x** |
| `{type, commits: [.payload.commits[]?.message]}` | **268 ms** | 1.72 s | 8.0 s | **29x** |
| `{type, commits: (.payload.commits // [] \| length)}` | **262 ms** | 1.54 s | 7.5 s | **28x** |

[Full results with jaq and gojq](benches/results_ndjson.md).

## How it works

- **SIMD parsing.** C++ simdjson (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks, ordered output. Streams in fixed-size windows (8–64 MB, scaled to core count) so memory stays flat regardless of file size. On Apple Silicon, uses only performance cores to avoid E-core contention.
- **Zero-copy I/O.** mmap for single-document JSON — no heap allocation or memcpy for the input file.
- **On-demand extraction.** Common NDJSON patterns (`.field`, `select`, `{...}` reshaping) extract raw bytes directly from simdjson's On-Demand API, bypassing Rust value tree construction entirely. Original number representation (scientific notation, trailing zeros) is preserved.
- **Transparent decompression.** `.gz` (gzip) and `.zst`/`.zstd` (zstd) files are decompressed automatically based on extension. Glob patterns in file arguments are expanded (quote them to bypass shell expansion: `'data/*.json.gz'`).

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
