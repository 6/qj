# qj

Quick JSON. A jq-compatible processor, 2-100x faster on large inputs.

## When to use qj instead of jq

**Large JSON files (>10 MB).** qj parses with SIMD (simdjson via FFI). On a 49 MB file, `length` takes 34 ms vs jq's 361 ms (11x). Parse-heavy operations like `length` and `keys` are ~10x faster; evaluator-bound filters 2-4x.

**NDJSON / JSONL pipelines.** qj auto-parallelizes across all cores. On 1.1 GB NDJSON: `select(.type == "PushEvent")` takes 106 ms vs jq's 13.5 s (127x). No `xargs` or `parallel` needed.

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
| `.statuses \| length` | **34 ms** | 361 ms | 157 ms | 299 ms |
| `keys` | **36 ms** | 364 ms | 152 ms | 294 ms |
| `.statuses[] \| .user.name` | **152 ms** | 367 ms | 165 ms | 316 ms |
| `walk(if type == "boolean" then not else . end)` | **374 ms** | 3.47 s | 2.10 s | 1.61 s |

Filters on the SIMD fast path show 60-127x gains. Evaluator-bound expressions narrow to 2-16x.

GB-scale NDJSON (1.1 GB GitHub Archive, parallel processing):

| Workload | qj | jq | Speedup | Why |
|----------|----|----|---------|-----|
| `.actor.login` | **77 ms** | 7.2 s | **94x** | direct byte extraction |
| `length` | **108 ms** | 7.2 s | **66x** | SIMD parse, trivial eval |
| `keys` | **126 ms** | 7.7 s | **61x** | SIMD parse, trivial eval |
| `select(.type == "PushEvent")` | **106 ms** | 13.5 s | **127x** | SIMD filter + extract |
| `select(.type == "PushEvent") \| .payload.size` | **80 ms** | 7.3 s | **91x** | SIMD filter + extract |
| `{type, repo: .repo.name, actor: .actor.login}` | **134 ms** | 8.1 s | **60x** | SIMD reshape |
| `{type, commits: [.payload.commits[]?.message]}` | **494 ms** | 7.9 s | **16x** | mixed SIMD + evaluator |
| `{type, commits: (.payload.commits // [] \| length)}` | **2.73 s** | 7.6 s | **2.8x** | evaluator-bound |

[Full results](benches/results_large_only.md) and [tool comparison data](benches/results.md).

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
