# qj

`qj` is Quick JSON, a `jq`-compatible processor with [simdjson](https://github.com/simdjson/simdjson) parsing and automatic parallelization across cores.

Benchmarked on an M4 MacBook Pro:

- **NDJSON (1.1 GB):** `select(.type == "PushEvent")` in 76 ms vs jq's 12.7 s (**166x faster**)
- **JSON (49 MB):** `.statuses | map({user, text})` in 58 ms vs jq's 695 ms (**12x faster**)

## qj vs jq

**Drop-in replacement.** qj is ~95% compatible with jq's syntax and flags, just faster. SIMD parsing makes even small files snappier.

**NDJSON / JSONL pipelines.** qj is 28-166x faster than jq by combining SIMD parsing, mmap, automatic parallelism (no `xargs` or `parallel` needed), and on-demand field extraction.

**Large JSON files.** qj is 2-12x faster than jq on a single file. Simple operations (`length`, `keys`, `map`) see the biggest gains; heavier transforms (`group_by`, `sort_by`) are ~2x faster.

**Where jq is better.** If you need jq modules (`import`/`include`) or arbitrary precision arithmetic. qj uses 64-bit integers and floats, so large numbers pass through unchanged but arithmetic may lose precision.

**Memory tradeoff.** qj trades memory for speed. It uses a sliding window so peak RSS is bounded regardless of file size (~174 MB on a 10-core machine), but jq streams one record at a time (~5 MB). For regular JSON files qj uses ~1.7x jq's RSS.

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
| `.actor.login` | **76 ms** | 355 ms | 7.3 s | 2.8 s | 6.7 s |
| `length` | **101 ms** | 590 ms | 7.1 s | 2.9 s | 7.2 s |
| `keys` | **123 ms** | 740 ms | 7.7 s | 2.8 s | 6.6 s |
| `select(.type == "PushEvent")` | **76 ms** | 346 ms | 12.7 s | 3.5 s | 7.5 s |
| `select(…) \| .payload.size` | **80 ms** | 426 ms | 7.1 s | 2.9 s | 6.6 s |
| `{type, repo, actor}` | **128 ms** | 751 ms | 7.8 s | 3.2 s | 6.7 s |
| `{type, commits: [….message]}` | **270 ms** | 1.63 s | 7.8 s | 3.1 s | 6.8 s |
| `{type, commits: (… \| length)}` | **264 ms** | 1.54 s | 7.5 s | 3.0 s | 6.8 s |

On single JSON files (49 MB) with no parallelism, qj is 2-25x faster than jq, 1-6x faster than jaq, and 2-10x faster than gojq.

## How it works

- **SIMD parsing.** C++ [simdjson](https://github.com/simdjson/simdjson) (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks, ordered output. Files are mmap'd with progressive munmap — the entire file is mapped for maximum kernel read-ahead, then each 128 MB window is unmapped after processing to bound RSS (~174 MB for a 1.1 GB file). Falls back to streaming read() for stdin/pipes. On Apple Silicon, uses only performance cores to avoid E-core contention.
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
