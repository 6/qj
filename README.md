# qj

`qj` is a fast, [`jq`](https://github.com/jqlang/jq)-compatible JSON processor powered by [simdjson](https://github.com/simdjson/simdjson).

> [!NOTE]
> Work in progress!

Benchmarked on M4 MacBook Pro:

- **NDJSON (3.4 GB, 1.2M records):** `qj 'select(.type == "PushEvent")'` runs in 190 ms vs jq's 36.4 s (**191x faster**)
- **JSON (49 MB):** `qj '.statuses | map({user, text})'` runs in 58 ms vs jq's 695 ms (**12x faster**)

## qj vs jq

**Drop-in replacement.** 95% pass rate on jq's official test suite, with full coverage of everyday filters, builtins, and flags — just faster.

**NDJSON / JSONL pipelines.** qj is 29-191x faster than jq by combining SIMD parsing, mmap, automatic parallelism across cores, and on-demand field extraction.

**Large JSON files.** qj is 2-12x faster than jq on a single file. Simple operations (`length`, `keys`, `map`) see the biggest gains; heavier transforms (`group_by`, `sort_by`) are ~2x faster.

**Where jq is better.** If you need jq modules (`import`/`include`) or arbitrary precision arithmetic. qj uses 64-bit integers and floats, so large numbers pass through unchanged but arithmetic may lose precision.

**Memory tradeoff.** qj trades memory for speed. It uses a sliding window so peak RSS stays well below file size (~300 MB for a 3.4 GB file), but jq streams one record at a time (~5 MB). For regular JSON files qj uses ~1.7x jq's RSS.

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

Benchmarked on M4 MacBook Pro [hyperfine](https://github.com/sharkdp/hyperfine) and compared against [jq](https://github.com/jqlang/jq) as well as two popular reimplementations: [jaq](https://github.com/01mf02/jaq) and [gojq](https://github.com/itchyny/gojq).

**NDJSON** (3.4 GB GitHub Archive, 1.2M records):

| Workload | qj (parallel by default) | qj (1 thread) | jq | jaq | gojq |
|----------|---:|---------------:|---:|----:|----:|
| `.actor.login` | **196 ms** | 1.02 s | 21.7 s | 8.2 s | 20.3 s |
| `select(.type == "PushEvent")` | **190 ms** | 1.03 s | 36.4 s | 10.4 s | 22.8 s |
| `{type, repo: .repo.name, actor: .actor.login}` | **332 ms** | 2.26 s | 23.4 s | 9.5 s | 20.7 s |
| `{type, commits: [.payload.commits[]?.message]}` | **801 ms** | 4.84 s | 23.8 s | 9.2 s | 20.9 s |

On single JSON files (49 MB) with no parallelism, qj is 2-25x faster than jq, 1-6x faster than jaq, and 2-10x faster than gojq. See [benches/](benches/) for full results.

## How it works

- **SIMD parsing.** C++ [simdjson](https://github.com/simdjson/simdjson) (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks. Output order always matches input order despite parallel processing. Files are mmap'd with progressive munmap: the entire file is mapped for maximum kernel read-ahead, then each 128 MB window is unmapped after processing to bound RSS (~300 MB for a 3.4 GB file). Falls back to streaming read() for stdin/pipes.
- **Apple Silicon tuning.** Uses only P-cores, avoiding E-cores whose ~3x slower throughput creates stragglers that bottleneck the parallel pipeline.
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
