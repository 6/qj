# qj

`qj` is a fast, [`jq`](https://github.com/jqlang/jq)-compatible JSON processor powered by [simdjson](https://github.com/simdjson/simdjson).

Benchmarked on M4 MacBook Pro:

- **NDJSON (3.4GB, 1.2M records):** `qj -c 'select(.type=="PushEvent")'` is 190ms vs `jq` 36.4s (**191x faster**)
- **JSON (49MB):** `qj -c '.statuses | map({user, text})'` is 58ms vs `jq` 695ms (**12x faster**)

## qj vs jq

**Near drop-in replacement.** 98% pass rate on jq's official test suite, broad coverage of everyday filters, builtins, and flags - optimized for speed.

**NDJSON / JSONL pipelines.** On file inputs, qj combines SIMD parsing, mmap, automatic parallelism across cores, and on-demand field extraction. It's often **~60–190x** faster than jq for common streaming filters, and **~25–30x** faster on complex filters. Stdin and slurp (`-s`) see smaller gains (no mmap / less parallelism - [see benchmarks](#benchmarks)).

**Large JSON files.** qj is 2-12x faster than jq on a single file. Simple operations (`length`, `keys`, `map`) see the biggest gains; heavier transforms (`group_by`, `sort_by`) are ~2x faster.

**Where jq is better.** `--stream` is not yet implemented. Memory — qj trades memory for speed, using a sliding window (~300 MB for a 3.4 GB file) vs jq's one-record-at-a-time streaming (~5 MB).

## Quick start

```bash
cargo install qj
```

Usage:

```bash
# Extract fields
qj '.name' data.json
qj '.items[] | {id, name}' large.json

# Extract from streaming logs
tail -f logs.jsonl | qj -c 'select(.level == "ERROR") | {ts: .timestamp, msg: .message}'

# Streaming aggregation (keeps parallelism)
qj -r '.actor.login' events.ndjson | sort | uniq -c | sort -rn | head -10

# Compressed files
qj '.actor.login' gharchive-*.json.gz
qj 'select(.type == "PushEvent")' 'data/*.ndjson.gz'
```

## Benchmarks

Benchmarked on M4 MacBook Pro with [hyperfine](https://github.com/sharkdp/hyperfine) and compared against jq as well as two popular reimplementations ([jaq](https://github.com/01mf02/jaq) & [gojq](https://github.com/itchyny/gojq)).

**NDJSON** (3.4 GB GitHub Archive, 1.2M records):

| Workload | qj (parallel) | qj (1 thread) | jq | jaq | gojq |
|----------|---:|---------------:|---:|----:|----:|
| `.actor.login` | **196 ms** | 1.02 s | 21.7 s | 8.2 s | 20.3 s |
| `select(.type == "PushEvent")` | **190 ms** | 1.03 s | 36.4 s | 10.4 s | 22.8 s |
| `{type,repo:.repo.name,actor:.actor.login}` | **332 ms** | 2.26 s | 23.4 s | 9.5 s | 20.7 s |

**Where the gap narrows:**

| Scenario | vs jq | Why? | Faster alternative |
|----------|------:|-----|-----|
| Stdin (`cat file \| qj`) | ~9-17x | No mmap | Pass filename directly (~10x faster than stdin) |
| Slurp mode (`-s`) | ~2-3x | No parallelism or on-demand fast paths | Prefer Unix pipelines (~4x faster), e.g. `qj '.field' \| sort \| uniq -c` |

On single JSON files (49 MB) with no parallelism, qj is 2-25x faster than jq, 1-6x faster than jaq, and 2-10x faster than gojq. See [benches/](benches/) for full results.

## How it works

- **SIMD parsing.** C++ [simdjson](https://github.com/simdjson/simdjson) (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Rayon work-stealing thread pool, ~1 MB chunks. Output order always matches input order despite parallel processing. Files are mmap'd with progressive munmap: the entire file is mapped for maximum kernel read-ahead, then each 128 MB window is unmapped after processing to bound RSS (~300 MB for a 3.4 GB file). Falls back to streaming read() for stdin/pipes.
- **Apple Silicon tuning.** Uses only P-cores, avoiding E-cores whose slower throughput creates stragglers that bottleneck the parallel pipeline.
- **Zero-copy I/O.** mmap for single-document JSON. No heap allocation or memcpy for the input file.
- **On-demand extraction.** Common NDJSON patterns (`.field`, `select`, `{...}` reshaping) extract raw bytes directly from simdjson's On-Demand API, bypassing Rust value tree construction entirely. Original number representation (scientific notation, trailing zeros) is preserved.
- **Transparent decompression.** `.gz` (gzip) and `.zst`/`.zstd` (zstd) files are decompressed automatically based on extension. Glob patterns in file arguments are expanded (quote them to bypass shell expansion: `'data/*.json.gz'`).

## Compatibility and limitations

**98%** pass rate on jq's official [497-test suite](https://github.com/jqlang/jq/blob/master/tests/jq.test) (488/497).
**98%** feature coverage (175/179 features, [details](tests/jq_compat/feature_results.md)).

Limitations vs jq:

- No arbitrary precision arithmetic: qj uses i64/f64 internally. Integers up to 2^63 are exact; beyond that, precision is lost. Set `QJ_JQ_COMPAT=1` to match jq's precision behavior — arithmetic truncates to f64 for numbers > 2^53, while display operations preserve precision (497/497 conformance, 100%).
- Single-document JSON >4 GB falls back to serde_json (simdjson's limit). Still faster than jq but ~3-6x slower than simdjson's fast path. **NDJSON (JSONL) is unaffected** since each line is parsed independently.

## Credits / Inspiration

thanks to [lemire](https://github.com/lemire)+team for the ultra-speedy simdjson library, [01mf02](https://github.com/01mf02) for pioneering Rust jq rewrite, and [aikoschurmann](https://github.com/aikoschurmann) for inspiring the raw byte-scan approach to NDJSON filtering.
