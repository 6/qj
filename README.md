# qj

`qj` is Quick JSON, a `jq`-compatible processor with [simdjson](https://github.com/simdjson/simdjson) parsing and automatic parallelization across cores.

Benchmarked on an M4 MacBook Pro:

- **NDJSON (1.1 GB):** `select(.type == "PushEvent")` in 101 ms vs jq's 13.5 s — **133x faster**
- **JSON (49 MB):** `.statuses | map({user, text})` in 60 ms vs jq's 706 ms — **12x faster**

## When to use qj instead of jq

**Any time you'd use jq.** ~95% compatible syntax and flags — just faster. SIMD parsing makes even small files snappier.

**Large JSON files.** 2-12x faster than jq on a single file. Simple operations (`length`, `keys`, `map`) see the biggest gains; heavier transforms (`group_by`, `sort_by`) are ~2x faster.

**NDJSON / JSONL pipelines.** 30-150x faster than jq — auto-parallelizes across cores. No `xargs` or `parallel` needed.

**When jq is fine.** If you need jq modules (`import`/`include`) or arbitrary precision arithmetic. qj uses i64/f64 internally — large numbers are preserved on passthrough but arithmetic loses precision beyond 53 bits.

**Memory tradeoff.** qj trades memory for speed (~64 MB for NDJSON vs jq's ~5 MB). If memory is tight, jq is the safer choice.

## Quick start

Work in progress. For now:

```bash
git clone https://github.com/6/qj
cd qj
cargo install --path .
```

Usage:

```bash
# Extract fields
qj '.name' data.json
qj '.items[] | {id, name}' large.json

# Extract from streaming logs
tail -f logs.jsonl | qj -c 'select(.level == "ERROR") | {ts: .timestamp, msg: .message}'

# Slurp NDJSON into array
qj -s 'sort_by(.age) | reverse | .[0]' users.jsonl
  
# Compressed files
qj '.actor.login' gharchive-*.json.gz
qj 'select(.type == "PushEvent")' 'data/*.ndjson.gz'
```

## Benchmarks

M4 MacBook Pro via [hyperfine](https://github.com/sharkdp/hyperfine). Compared against [jq](https://github.com/jqlang/jq) and two popular reimplementations: [jaq](https://github.com/01mf02/jaq) and [gojq](https://github.com/itchyny/gojq). See [benches/](benches/) for full results.

**NDJSON** (1.1 GB GitHub Archive, parallel by default):

| Workload | qj (parallel by default) | qj (1 thread) | jq | jaq | gojq |
|----------|---:|---------------:|---:|----:|----:|
| `.actor.login` | **66 ms** | 338 ms | 7.2 s | 2.9 s | 7.1 s |
| `length` | **108 ms** | 593 ms | 7.2 s | 2.7 s | 7.2 s |
| `keys` | **109 ms** | 737 ms | 7.7 s | 2.9 s | 6.8 s |
| `select(.type == "PushEvent")` | **101 ms** | 406 ms | 13.5 s | 3.5 s | 7.9 s |
| `select(…) \| .payload.size` | **77 ms** | 428 ms | 7.2 s | 2.9 s | 7.0 s |
| `{type, repo, actor}` | **116 ms** | 779 ms | 7.9 s | 3.3 s | 7.2 s |
| `{type, commits: [….message]}` | **268 ms** | 1.72 s | 8.0 s | 3.2 s | 7.0 s |
| `{type, commits: (… \| length)}` | **262 ms** | 1.54 s | 7.5 s | 3.1 s | 6.8 s |

On single JSON files (49 MB) with no parallelism, qj is 2-29x faster than jq, 1-8x faster than jaq, and 2-12x faster than gojq.

## How it works

- **SIMD parsing.** C++ [simdjson](https://github.com/simdjson/simdjson) (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks, ordered output. Streams in fixed-size windows (8–64 MB, scaled to core count) so memory stays flat regardless of file size. On Apple Silicon, uses only performance cores to avoid E-core contention.
- **Zero-copy I/O.** mmap for single-document JSON — no heap allocation or memcpy for the input file.
- **On-demand extraction.** Common NDJSON patterns (`.field`, `select`, `{...}` reshaping) extract raw bytes directly from simdjson's On-Demand API, bypassing Rust value tree construction entirely. Original number representation (scientific notation, trailing zeros) is preserved.
- **Transparent decompression.** `.gz` (gzip) and `.zst`/`.zstd` (zstd) files are decompressed automatically based on extension. Glob patterns in file arguments are expanded (quote them to bypass shell expansion: `'data/*.json.gz'`).

## Compatibility and limitations

**95%** pass rate on jq's official [497-test suite](https://github.com/jqlang/jq/blob/master/tests/jq.test).
**96%** feature coverage (169/176 features, [details](tests/jq_compat/feature_results.md)).

- No module system — `import`/`include` are not supported.
- No arbitrary precision arithmetic — i64/f64 internally. Large numbers are preserved on passthrough but arithmetic uses f64 precision.
- Some edge cases in `def` (def-inside-expressions, destructuring bind patterns).
- Single-document JSON >4 GB falls back to serde_json (simdjson's limit). Still faster than jq but ~3-6x slower than simdjson's fast path. **NDJSON (JSONL) is unaffected** since each line is parsed independently.
- NDJSON fast paths (e.g. `select`) output raw input bytes, so Unicode escapes like `\u000B` preserve their original hex casing. jq normalizes to lowercase (`\u000b`). Both are valid JSON per RFC 8259.
