# qj

`qj` is a fast, [`jq`](https://github.com/jqlang/jq)-compatible JSON processor powered by [simdjson](https://github.com/simdjson/simdjson).

Benchmarked on M4 MacBook Pro:

- **NDJSON (3.4GB):** `qj -c 'select(.type=="PushEvent")'` is 190ms vs `jq` 36.4s (**191x faster**)
- **JSON (49MB):** `qj -c '.statuses | map({user, text})'` is 58ms vs `jq` 695ms (**12x faster**)

## qj vs jq

**Drop-in replacement.** qj is a port of jq 1.8.1 itself, so output, errors and exit codes match jq byte for byte: 100% feature coverage (181/181) and 100% pass rate on all of jq's official test suites. All filters, builtins, and flags. ([details](docs/COMPATIBILITY.md))

**NDJSON / JSONL pipelines.** On file inputs, qj combines SIMD parsing, mmap, automatic parallelism across cores, and direct evaluation of simple filters on simdjson's tape. It's often **~60–190x** faster than jq for common streaming filters, and **~25–30x** faster on complex filters. Stdin and slurp (`-s`) see smaller gains (no mmap / less parallelism - [see benchmarks](#benchmarks)).

**Large JSON files.** qj is 2-12x faster than jq on a single file. Simple operations (`length`, `keys`, `map`) see the biggest gains; heavier transforms (`group_by`, `sort_by`) are ~2x faster.

**Memory usage.** qj trades memory for speed, using a ~300 MB sliding window for a 3.4 GB file vs jq's ~5 MB.

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
| Slurp mode (`-s`) | ~2-3x | No parallelism | Prefer Unix pipelines (~4x faster), e.g. `qj '.field' \| sort \| uniq -c` |

On single JSON files (49 MB) with no parallelism, qj is 2-25x faster than jq, 1-6x faster than jaq, and 2-10x faster than gojq. See [benches/](benches/) for full results.

## How it works

- **A port of jq itself.** qj's core is jq 1.8.1 ported to Rust: its value and number model, parser, compiler, bytecode VM, builtins and `builtin.jq`, checked change by change against the jq binary by a differential harness.
- **SIMD parsing.** C++ [simdjson](https://github.com/simdjson/simdjson) (NEON/AVX2) via FFI. Single-file vendored build, no cmake.
- **Parallel NDJSON.** Worker threads, ~1 MB chunks, each thread with its own compiled program. Output order always matches input order despite parallel processing. Files are mmap'd. Falls back to streaming read() for stdin/pipes, processing each record as soon as it arrives.
- **Apple Silicon tuning.** Uses every non-efficiency core (P-cores, or Super + Performance cores on M5 Pro/Max), avoiding E-cores whose slower throughput creates stragglers that bottleneck the parallel pipeline.
- **Zero-copy I/O.** mmap for single-document JSON. No heap allocation or memcpy for the input file.
- **Tape evaluation.** Simple filters (`.field`, `.[]`, `select`, `{...}` reshaping, `length`, `keys`) run directly on simdjson's tape, bypassing value construction, and print exactly what jq prints.
- **Transparent decompression.** `.gz` (gzip) and `.zst`/`.zstd` (zstd) files are decompressed automatically based on extension. Glob patterns in file arguments are expanded (quote them to bypass shell expansion: `'data/*.json.gz'`).

## Compatibility and limitations

See [compatibility details](docs/COMPATIBILITY.md) for the full feature matrix, test suite results, and `QJ_JQ_COMPAT=1` mode.

Limitations vs jq:

- Help and version text, and the program name in messages (`qj: error: ...`), are qj's own. Where jq crashes or hangs, qj returns a sane answer; set `QJ_JQ_COMPAT=1` to reproduce jq exactly, bugs included, and to turn off qj's extras (glob expansion, decompression, `--threads`).
- Single-document JSON >4 GB falls back to qj's port of jq's own parser (simdjson's limit). Same result, but slower than simdjson's fast path. **NDJSON (JSONL) is unaffected** since each line is parsed independently.

## Credits / Inspiration

qj's core is a port of [jq](https://github.com/jqlang/jq) 1.8.1 (MIT licensed); see [LICENSE-jq](LICENSE-jq).

thanks to [lemire](https://github.com/lemire)+team for the ultra-speedy simdjson library, [01mf02](https://github.com/01mf02) for pioneering Rust jq rewrite, and [aikoschurmann](https://github.com/aikoschurmann) for inspiring the raw byte-scan approach to NDJSON filtering.
