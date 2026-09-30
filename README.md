# qj

`qj` is a fast, [`jq`](https://github.com/jqlang/jq)-compatible JSON processor powered by [simdjson](https://github.com/simdjson/simdjson).

Benchmarked on M4 MacBook Pro, with qj's previous core (being re-measured on the jq port):

- **NDJSON (3.4GB):** `qj -c 'select(.type=="PushEvent")'` is 190ms vs `jq` 36.4s (**191x faster**)
- **JSON (49MB):** `qj -c '.statuses | map({user, text})'` is 58ms vs `jq` 695ms (**12x faster**)

## qj vs jq

**Drop-in replacement.** qj runs a port of jq 1.8.1's own implementation, so it matches jq byte for byte: the same stdout, exit codes, and error messages. A differential harness runs 21,505 cases against jq 1.8.1, including jq's own test suites in each input and output mode (2,903 cases, all byte-exact). qj matches 21,494 of them; 7 of the rest are qj's own help and version text, and 4 are programs jq never finishes. A differential fuzzer also generates random programs, inputs and flags and compares qj with jq. ([details](docs/COMPATIBILITY.md))

**NDJSON / JSONL pipelines.** On file inputs, qj combines SIMD parsing, mmap, and automatic parallelism across cores. It's often **~60–190x** faster than jq for common streaming filters, and **~25–30x** faster on complex filters (being re-measured: these numbers came from the previous core's NDJSON fast paths). Stdin and slurp (`-s`) see smaller gains (no mmap / less parallelism - [see benchmarks](#benchmarks)).

**Large JSON files.** qj is 2-12x faster than jq on a single file (being re-measured). Simple operations (`length`, `keys`, `map`) see the biggest gains; heavier transforms (`group_by`, `sort_by`) are ~2x faster.

**Memory usage.** qj trades memory for speed, using a ~300 MB sliding window for a 3.4 GB file vs jq's ~5 MB (being re-measured).

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

These numbers were measured with qj's previous core, whose NDJSON fast paths and single-file passthroughs answered common filters straight from simdjson, before qj became a port of jq. They're being re-measured on the port.

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

- **A port of jq itself.** qj's core is jq 1.8.1's implementation ported to Rust: the value and number model, the parser (driven by jq's own bison tables), the compiler and bytecode VM, the C builtins, jq's `builtin.jq` (run verbatim), and `main.c`/`util.c`. qj behaves like jq because it runs jq's code, and a differential harness checks every change against the jq binary.
- **SIMD parsing.** C++ [simdjson](https://github.com/simdjson/simdjson) (NEON/AVX2) via FFI. Single-file vendored build, no cmake. jq values are built straight from simdjson's tape, with number literals kept exactly as jq keeps them. Anything simdjson rejects (`nan`, invalid UTF-8, huge numbers, malformed text) goes through the ported jq parser, so values and error messages are always jq's.
- **mmap and streaming input.** Files are memory-mapped: no heap copy of the input. Stdin and pipes are streamed, and each record is processed as soon as its line is complete, so `tail -f logs.jsonl | qj ...` keeps up.
- **Ordered parallel NDJSON.** Independent records are processed on worker threads, each running its own compiled copy of the program, and their output is written in input order. Programs that carry state from one input to the next (`input`, `halt`, `$__loc__`, ...) run on one thread.
- **Apple Silicon tuning.** Uses every non-efficiency core (P-cores, or Super + Performance cores on M5 Pro/Max), avoiding E-cores whose slower throughput creates stragglers that bottleneck the parallel pipeline.
- **Transparent decompression.** `.gz` (gzip) and `.zst`/`.zstd` (zstd) files are decompressed as they're read, based on extension. Glob patterns in file arguments are expanded (quote them to bypass shell expansion: `'data/*.json.gz'`).

## Compatibility and limitations

See [compatibility details](docs/COMPATIBILITY.md) for how conformance is measured, the test results, what's exempt, and the feature matrix.

Numbers behave exactly as in jq 1.8.1, which uses decNumber. Number literals keep their exact decimal value: `100000000000000000001` prints as written, and `1e2` prints in canonical form as `1E+2`. Arithmetic is f64: `13911860366432393 - 10` is `13911860366432382`. qj no longer does i64 arithmetic; this is how it behaves in every mode.

Set `QJ_JQ_COMPAT=1` to make qj a drop-in jq 1.8.1, bugs included: it reproduces the few places where jq crashes or hangs and qj returns a sane answer instead (a value nested deeper than jq's C stack allows, a module that imports itself, `delpaths` with a `nan` in a path), and it turns off qj's own additions, so glob patterns, `.gz`/`.zst` names and `--threads`/`--jsonl`/`--debug-timing` behave as they do in jq. Parallel processing stays on — it isn't observable — and qj's help, version and program name stay qj's. See [Being exactly jq](docs/COMPATIBILITY.md#being-exactly-jq).

Limitations vs jq:

- Help, version, and build-configuration text are qj's own, and messages name `qj` where jq's name `jq` (`qj: error: ...`).
- Single-document JSON >4 GB is beyond simdjson's limit, so it's parsed by qj's port of jq's parser: same result, but slower. **NDJSON (JSONL) is unaffected** since each line is parsed independently.
- qj builds for macOS and Linux. More in [docs/LIMITATIONS.md](docs/LIMITATIONS.md).

## Credits / Inspiration

qj's core is a port of [jq](https://github.com/jqlang/jq) 1.8.1 (MIT licensed). [LICENSE-jq](LICENSE-jq) has jq's license and the notices it carries.

thanks to [lemire](https://github.com/lemire)+team for the ultra-speedy simdjson library, [01mf02](https://github.com/01mf02) for pioneering Rust jq rewrite, and [aikoschurmann](https://github.com/aikoschurmann) for inspiring the raw byte-scan approach to NDJSON filtering.
