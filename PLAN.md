# jx — A faster jq for large JSON and JSONL

A jq-compatible JSON processor that uses SIMD parsing (C++ simdjson via
FFI), parallel NDJSON processing, and streaming architecture to be
10-50x faster than jq on large inputs. Cross-platform (macOS ARM, macOS
x86, Linux ARM, Linux x86).

**Target audience:** Anyone who has typed `jq` and waited. Developers
processing API responses, log pipelines, JSONL datasets. LLM agents
(Claude Code, Cursor, aider) that parse JSON tool outputs thousands of
times per session.

---

## Why not just use jaq?

jaq is good. It wins 23/29 benchmarks vs jq on filter evaluation speed
and has near-zero startup time. But jaq doesn't address the three biggest
performance problems:

1. **No SIMD parsing.** jaq uses `hifijson` (its own parser) or serde_json.
   Both process byte-by-byte at ~400-600 MB/s. C++ simdjson's On-Demand
   API reaches 7 GB/s — a 10-15x gap. For simple filters (`.field`,
   `.[] | .name`), parsing dominates runtime. jaq is faster at
   *evaluating filters* than jq, but still bottlenecked on *parsing*.

2. **No On-Demand parsing.** jaq (and the Rust simd-json port) always
   builds a full DOM tree regardless of what the filter accesses. For
   `jq '.name' huge.json`, the entire document is materialized just to
   extract one field. simdjson's On-Demand API navigates the SIMD
   structural index directly — only the accessed fields are materialized.
   This is the difference between 1.4 GB/s (full DOM) and 7 GB/s
   (On-Demand) for simple path queries.

3. **No parallelism.** jaq processes input sequentially. For NDJSON (one
   JSON object per line), each line is independent — trivially
   parallelizable. People currently work around this with
   `GNU parallel | jq`, spawning dozens of processes with startup/IPC
   overhead. Built-in parallel NDJSON processing gives 8-10x speedup
   on multi-core machines with zero user configuration. simdjson's
   `iterate_many` already does 3.5 GB/s on NDJSON — we build on it.

jx aims to beat everything on *parsing throughput* and *parallel
processing*, while matching jaq on *filter evaluation speed*. The
performance story is SIMD parsing + parallelism, not a faster evaluator.

---

## Competitive landscape

| Tool | Parsing | Parallel | SIMD | On-Demand | Streaming | Memory model | jq compat | Platform |
|------|---------|----------|------|-----------|-----------|--------------|-----------|----------|
| jq 1.8 | ~300 MB/s | No | No | No | Yes (`--stream`)* | Full DOM, O(n)+ | 100% | All |
| jaq 2.3 | ~500 MB/s | No | No | No | No | Full DOM, O(n) | ~90% | All |
| gojq | ~400 MB/s | No | No | No | No | Full DOM, O(n) | ~85% | All |
| **jx** | **3-7 GB/s** | **Yes (NDJSON)** | **Yes (NEON/AVX2)** | **Yes** | **Yes (transparent)** | **Streaming** | **~80%** | **All** |

\* jq's `--stream` mode parses incrementally, emitting `[[path], value]`
pairs without loading the full tree. It works for constant-memory
processing of large files, but requires a completely different programming
model — you work with path arrays instead of normal jq filters (e.g.,
`jq --stream 'select(.[0] == ["name"]) | .[1]'` instead of `jq '.name'`).
It's also significantly slower than normal mode because every value gets
wrapped in a path tuple. jx's streaming (Phase 4) uses normal jq filter
syntax with automatic streaming behavior — that's the real differentiator.

---

## Why C++ simdjson, not the Rust port

The Rust `simd-json` crate is a port that explicitly trades performance
for ergonomics. Three problems make it insufficient:

1. **No On-Demand API.** The Rust port only offers DOM parsing (full tree
   build) and tape parsing (flat index). C++ simdjson's On-Demand API —
   the one that navigates directly to requested fields at 7 GB/s without
   building a tree — has no Rust port equivalent. This is the single most
   important feature for a jq replacement.

2. **2-2.5x slower on DOM parsing.** Published benchmarks show simd-json
   (Rust) at ~1,200-1,400 MB/s on twitter.json vs simdjson-rust (C++
   bindings) at 3,160 MB/s on the same file. The gap comes from Rust's
   stricter aliasing rules, different memory allocation patterns, and the
   port tracking an older version of the C++ library.

3. **No built-in NDJSON batch parsing.** C++ simdjson has `iterate_many`
   which parses a stream of JSON documents at 3.5 GB/s. The Rust port
   doesn't expose this.

### How we use it

simdjson ships as two files: `simdjson.h` and `simdjson.cpp`. We vendor
them directly into the repo and compile via the `cc` crate — no cmake,
no vcpkg, no system dependency. This is the same pattern as gg's use of
`libc::opendir`/`readdir`: wrap a C/C++ API in a safe Rust interface.

```
src/
  simdjson/
    simdjson.h          # Vendored from simdjson release
    simdjson.cpp        # Vendored from simdjson release
    bridge.cpp          # Our C-linkage wrapper functions
    bridge.rs           # Safe Rust FFI wrapper
```

The FFI surface is small and well-defined:
- `parse(buf, len) → document`
- `doc.get_field(key) → value`
- `doc.get_array() → iterator`
- `value.get_string() → &str`
- `value.get_number() → f64 | i64`
- `value.get_bool() → bool`
- `value.type() → JsonType`
- `iterate_many(buf, len) → document_stream` (NDJSON)

Each function is `unsafe` at the FFI boundary, wrapped in safe Rust.
The bridge layer handles lifetime management (simdjson reuses internal
buffers between parses — the Rust wrapper enforces this with borrowing).

### Build complexity

**Cost:** Requires a C++ compiler (clang or gcc) at build time. The `cc`
crate handles this automatically on macOS (Xcode clang) and Linux
(system gcc/clang). Cross-compilation is slightly harder but simdjson
supports all our target triples natively.

**Mitigation:** simdjson is a single-compilation-unit library. `build.rs`
compiles one file. No cmake, no configure, no external dependencies.
Binary size impact is ~500KB.

---

## Architecture

```
jx/
├── src/
│   ├── main.rs               # CLI parsing (clap), orchestration
│   ├── cli.rs                 # Argument definitions
│   ├── simdjson/
│   │   ├── simdjson.h         # Vendored C++ header
│   │   ├── simdjson.cpp       # Vendored C++ implementation
│   │   ├── bridge.cpp         # C-linkage FFI functions
│   │   └── bridge.rs          # Safe Rust wrapper over FFI
│   ├── filter/
│   │   ├── mod.rs             # Filter AST and evaluation
│   │   ├── lexer.rs           # jq filter tokenizer
│   │   ├── parser.rs          # jq filter parser → AST
│   │   ├── eval.rs            # AST evaluator against simdjson values
│   │   ├── eval_ondemand.rs   # Fast-path: On-Demand for simple queries
│   │   └── builtins.rs        # Built-in functions (length, keys, etc.)
│   ├── parallel/
│   │   ├── mod.rs             # NDJSON parallel processor
│   │   ├── splitter.rs        # Chunk input at newline boundaries
│   │   └── pool.rs            # Thread pool (rayon or manual)
│   ├── output/
│   │   ├── mod.rs             # Output formatting dispatch
│   │   ├── pretty.rs          # Pretty-print with optional color
│   │   ├── compact.rs         # Compact single-line output (-c)
│   │   └── raw.rs             # Raw string output (-r)
│   └── io/
│       ├── mod.rs             # Input reading dispatch
│       ├── mmap.rs            # mmap for file inputs
│       └── stream.rs          # Streaming stdin reader
├── bench/
│   ├── parse-throughput/      # Phase 0: raw parsing benchmarks
│   ├── filter-eval/           # Phase 1: filter evaluation benchmarks
│   ├── parallel-ndjson/       # Phase 2: parallel NDJSON benchmarks
│   └── e2e/                   # Phase 3: end-to-end vs jq/jaq
├── tests/
│   ├── conformance/           # Output diff vs jq on real data
│   └── filters/               # Per-filter correctness tests
├── build.rs                   # Compiles simdjson.cpp via cc crate
├── Cargo.toml
└── PLAN.md
```

---

## Platform strategy — why cross-platform

Unlike gg, jx's advantages are **not platform-specific**:

- SIMD parsing: simdjson auto-selects NEON (ARM), AVX2 (x86), or SSE4.2
  at runtime. Single binary, all platforms.
- Parallel NDJSON: standard threading, no OS-specific APIs.
- Streaming/mmap: POSIX on macOS and Linux. Windows via standard APIs.

Apple Silicon is the **primary dev and benchmark target** (NEON is
excellent, M-series gives clean perf measurement), but shipping
Linux x86 costs almost nothing and dramatically expands the audience.
Log processing pipelines (the biggest NDJSON workload) run on Linux.

**Build targets (in priority order):**
1. aarch64-apple-darwin (macOS ARM — your dev machine)
2. x86_64-unknown-linux-gnu (Linux x86 — where log pipelines run)
3. aarch64-unknown-linux-gnu (Linux ARM — AWS Graviton)
4. x86_64-apple-darwin (macOS Intel — still common)

---

## The 95% filter coverage

Based on real-world jq usage (tutorials, cheat sheets, Stack Overflow,
CI/CD scripts, agent tool calls), these filters cover ~95% of what
people actually type:

### Tier 1: "The daily five" (~70% of all usage)

These are what people type thousands of times a day. They must be fast
and correct from day one.

| Filter | Example | Notes |
|--------|---------|-------|
| `.field` | `.name`, `.commit.author.email` | Object field access, nested via dot |
| `.[]` | `.items[]` | Array/object iteration |
| `\|` (pipe) | `.[] \| .name` | Compose filters |
| `select()` | `select(.age > 30)` | Filter/predicate |
| Object construction | `{name: .name, id: .id}` | Build new objects |

### Tier 2: "The weekly ten" (~20% of all usage)

Common in scripts, CI/CD, and slightly more complex one-liners.

| Filter | Example | Notes |
|--------|---------|-------|
| `map()` | `map(.price * 1.1)` | Transform array elements |
| `length` | `.items \| length` | Array/string/object length |
| `keys` / `values` | `.config \| keys` | Object introspection |
| `sort_by()` | `sort_by(.date)` | Sort arrays |
| `group_by()` | `group_by(.category)` | Group array elements |
| `unique` / `unique_by()` | `unique_by(.id)` | Deduplicate |
| `first` / `last` | `first(.[] \| select(...))` | Short-circuit |
| Array slicing | `.[0:5]`, `.[-1]` | Index and slice |
| `+` (addition/merge) | `. + {"new": "field"}` | Object merge, array concat, string concat, arithmetic |
| `type` | `select(type == "string")` | Type checking |

### Tier 3: "The monthly rest" (~5% of all usage)

Used in more complex scripts. Important for compatibility but not the
performance-critical path.

| Filter | Example | Notes |
|--------|---------|-------|
| `reduce` | `reduce .[] as $x (0; . + $x)` | Fold/accumulate |
| `if-then-else` | `if .x then .y else .z end` | Conditional |
| `try-catch` | `try .foo catch "default"` | Error handling |
| `@csv` / `@tsv` / `@base64` / `@uri` / `@json` | `@csv` | Format strings |
| `test()` / `match()` / `capture()` | `select(.name \| test("^foo"))` | Regex |
| `split` / `join` / `gsub` / `sub` | `.tags \| join(",")` | String ops |
| `to_entries` / `from_entries` / `with_entries` | `with_entries(select(.value != null))` | Object↔array |
| `not` / `and` / `or` | `select(.a and .b)` | Boolean logic |
| `min_by` / `max_by` | `max_by(.score)` | Min/max |
| `flatten` | `flatten(1)` | Flatten nested arrays |
| `has()` / `in()` | `select(has("name"))` | Key existence |
| `tostring` / `tonumber` | `.count \| tonumber` | Type conversion |
| `empty` | `if .x then . else empty end` | Suppress output |
| `env` / `$ENV` | `$ENV.HOME` | Environment access |
| `ascii_downcase` / `ascii_upcase` | `.name \| ascii_downcase` | Case conversion |
| `ltrimstr` / `rtrimstr` | `.path \| ltrimstr("/")` | String trimming |
| `startswith` / `endswith` / `contains` | `select(.url \| startswith("https"))` | String predicates |
| `add` | `[.prices[]] \| add` | Sum/concat |
| `any` / `all` | `any(.[]; . > 10)` | Quantifiers |
| `range()` | `[range(5)]` | Generate sequences |
| `recurse` / `..` | `.. \| .name? // empty` | Recursive descent |
| `del()` | `del(.unwanted)` | Remove field |
| `//` (alternative) | `.x // "default"` | Alternative operator |
| `?` operator | `.foo?` | Suppress errors |
| Variable binding | `. as $x \| ...` | Variable assignment |
| `def` | `def double: . * 2;` | Function definitions |
| String interpolation | `"Hello \(.name)"` | Embedded expressions |
| `limit()` / `until()` / `while()` / `repeat()` | Loop constructs | Loops |
| `foreach` | `foreach .[] as $x (0; . + $x)` | Streaming fold |
| `input` / `inputs` | `[inputs \| select(.level == "ERROR")]` | Multi-input |
| `indices` / `index` / `rindex` | String search | Search |
| `walk()` | `walk(if type == "string" then ascii_downcase else . end)` | Recursive transform |
| `path()` / `getpath()` / `setpath()` / `delpaths()` | Path manipulation | Path ops |
| `debug` | `.x \| debug \| .y` | Debug output |
| `null`, `true`, `false` | Literal construction | Literals |
| `--arg` / `--argjson` | CLI variable injection | CLI args |
| `--slurp` / `-s` | Slurp all inputs into array | CLI mode |
| `--raw-input` / `-R` | Treat input as strings | CLI mode |
| `--null-input` / `-n` | No input | CLI mode |
| Date/time: `now`, `todate`, `fromdate`, `strftime` | Date handling | Dates |
| Math: `floor`, `ceil`, `round`, `sqrt`, `pow`, `log`, `exp`, `fabs` | Math functions | Math |
| `tojson` / `fromjson` | JSON encode/decode within filters | Encode |
| `ascii` / `explode` / `implode` | Codepoint ops | String |
| `scan` / `splits` | `[scan("[0-9]+")]` | Regex extraction |
| `transpose` / `combinations` / `nth` / `isempty` | Utilities | Utility |
| `builtins` | `builtins \| length` | Introspection |
| `halt` / `halt_error` / `error` | Program termination / errors | Control |

### What we're NOT implementing (initially)

- Full module system (`import`/`include`/`modulemeta`)
- `$__loc__` (source location debugging)
- `@sh` (shell escaping — niche)
- `@html` (HTML escaping — niche)
- `@base32` (uncommon format)
- `label`/`break` (advanced loop control)
- TCO / tail recursion optimization

### CLI flags (MVP)

| Flag | Description |
|------|-------------|
| `-r` / `--raw-output` | Output raw strings (no quotes) |
| `-c` / `--compact-output` | Compact (one line per value) |
| `-s` / `--slurp` | Read all input into array |
| `-S` / `--sort-keys` | Sort object keys |
| `-e` / `--exit-status` | Exit 1 if last output is false/null |
| `-R` / `--raw-input` | Treat each line as string |
| `-n` / `--null-input` | Don't read input |
| `--arg name value` | Bind $name to string value |
| `--argjson name value` | Bind $name to JSON value |
| `--jsonargs` | Remaining args are JSON |
| `--slurpfile name file` | Bind $name to file contents |
| `-f` / `--from-file` | Read filter from file |
| `--jsonl` / `--ndjson` | Force NDJSON mode (auto-detected) |
| `-j` / `--threads` | Thread count (default: auto) |
| `--tab` | Use tabs for indentation |
| `--indent n` | Set indentation level |
| `-C` / `--color-output` | Force color |
| `-M` / `--monochrome-output` | Disable color |

---

## Phase 0: Parsing benchmark (validate the premise)

**Goal:** Confirm that C++ simdjson on Apple Silicon actually delivers the
expected throughput advantage over serde_json, simd-json (Rust port), and
jq's parser. This is the entire thesis — if simdjson's On-Demand API isn't
5x+ faster than serde_json, the project doesn't have a compelling story.

**What to build:** `bench/parse-throughput/`

Two benchmark binaries:
1. **Rust benchmark** (`bench.rs`): Parses test files via serde_json and
   simd-json (Rust port). Reports throughput in MB/s.
2. **C++ benchmark** (`bench.cpp`): Parses same files via C++ simdjson
   DOM API and On-Demand API. Reports throughput in MB/s.
3. **Shell benchmark**: Times `jq '.' file > /dev/null` and
   `jaq '.' file > /dev/null` via hyperfine.

**Test files:**
- twitter.json (631KB) — mixed strings/numbers/nesting
- citm_catalog.json (1.7MB) — deep nesting, many keys
- canada.json (2.2MB) — dense floating-point coordinates
- synthetic 100MB NDJSON (100K lines of ~1KB objects)
- synthetic 1GB NDJSON (1M lines)

**Key measurements:**
- Full DOM parse throughput (MB/s) — simdjson DOM vs simd-json vs serde
- On-Demand field access throughput — simdjson `.field` extraction at speed
- NDJSON batch throughput — simdjson `iterate_many` vs line-at-a-time
- FFI overhead — same simdjson calls via Rust FFI vs direct C++

**Expected results (from published benchmarks):**
- simdjson On-Demand: 5,000-7,000 MB/s (simple field access)
- simdjson DOM: 2,000-3,500 MB/s
- simd-json (Rust): 1,200-1,400 MB/s
- serde_json: 330-560 MB/s
- jq: 200-400 MB/s

**Platform test:** Run on Apple Silicon (NEON) and x86 Linux (AVX2) to
confirm cross-platform story holds.

**Kill criterion:** If simdjson On-Demand on Apple Silicon delivers <3x
over serde_json on typical JSON files, the SIMD angle isn't compelling
enough. Pivot to pure parallelism story only.

**Success criterion:** ≥5x throughput over serde_json, ≥10x over jq.
FFI overhead <5% vs direct C++ calls.

---

## Phase 1: Filter parser, evaluator, and FFI bridge

**Goal:** Parse and evaluate the Tier 1 filters (`.field`, `.[]`, `|`,
`select()`, object construction) against simdjson-parsed data. Match
jaq's filter evaluation speed (the win is in parsing, not eval).

### 1a. simdjson FFI bridge

Build the Rust ↔ C++ bridge:

```cpp
// bridge.cpp — C-linkage functions callable from Rust
extern "C" {
    // Parser lifecycle
    void* jx_parser_new();
    void  jx_parser_free(void* parser);

    // Parse document (On-Demand)
    int jx_parse(void* parser, const char* buf, size_t len, void** doc);

    // Navigate On-Demand document
    int jx_doc_get_field(void* doc, const char* key, size_t key_len, void** value);
    int jx_doc_get_array(void* doc, void** iter);
    int jx_iter_next(void* iter, void** value);

    // Extract values
    int jx_value_get_string(void* value, const char** out, size_t* len);
    int jx_value_get_int64(void* value, int64_t* out);
    int jx_value_get_double(void* value, double* out);
    int jx_value_get_bool(void* value, bool* out);
    int jx_value_type(void* value);  // 0=null,1=bool,2=int,3=float,4=string,5=array,6=object

    // DOM fallback (for complex filters that need full tree)
    int jx_parse_dom(void* parser, char* buf, size_t len, void** dom);
    // ... DOM navigation functions ...

    // NDJSON batch parsing
    void* jx_iterate_many(void* parser, const char* buf, size_t len, size_t batch_size);
    int jx_iterate_many_next(void* stream, void** doc);
}
```

```rust
// bridge.rs — safe Rust wrapper
pub struct Parser { ptr: *mut c_void }
pub struct Document<'a> { ptr: *mut c_void, _marker: PhantomData<&'a Parser> }
pub struct Value<'a> { ptr: *mut c_void, _marker: PhantomData<&'a Document<'a>> }

impl Parser {
    pub fn new() -> Self { ... }
    pub fn parse<'a>(&'a mut self, data: &'a [u8]) -> Result<Document<'a>> { ... }
    pub fn iterate_many<'a>(&'a mut self, data: &'a [u8]) -> DocumentStream<'a> { ... }
}

impl<'a> Document<'a> {
    pub fn get_field(&mut self, key: &str) -> Result<Value<'a>> { ... }
    pub fn get_array(&mut self) -> Result<ArrayIter<'a>> { ... }
}
```

Key design: Rust lifetimes enforce simdjson's buffer reuse model — a
`Document` borrows the `Parser` (which owns internal buffers), and
`Value`s borrow the `Document`. This prevents use-after-reparse bugs
at compile time.

### 1b. Filter language parser

Build a jq filter parser that handles Tier 1 syntax. Use a recursive
descent parser (jq's grammar is simple enough).

The AST should be compact — it's evaluated millions of times for NDJSON.
Represent it as a flat Vec of nodes with index references (arena
allocation) rather than heap-allocated tree nodes, to avoid pointer
chasing and improve cache locality.

### 1c. Two-tier evaluator

**On-Demand fast path (eval_ondemand.rs):** For filters that are pure
path navigation — `.field`, `.a.b.c`, `.items[].name`, `.[] | .id` —
drive simdjson's On-Demand API directly. The filter AST is "compiled"
into a sequence of On-Demand navigation calls. No DOM is built. This
is the 7 GB/s path.

Filters eligible for the fast path:
- Field access chains: `.a.b.c`
- Array iteration with field access: `.[] | .name`
- Simple select with comparison: `select(.age > 30)`
- Object construction from fields: `{name: .name, id: .id}`
- Pipe chains of the above

**DOM fallback path (eval.rs):** For anything the On-Demand path can't
handle (map, sort_by, reduce, variable binding, etc.), fall back to full
DOM parsing via simdjson's DOM API. The DOM is materialized as Rust-owned
values (our own `Value` enum, converted from simdjson DOM). This path is
still fast (~2-3 GB/s parse) but allocates more memory.

The evaluator dispatches at filter compile time: analyze the AST, and if
it fits the On-Demand pattern, use the fast path. Otherwise, fall back.

### 1d. Filter evaluation performance

**Honest assessment:** jaq is already a well-optimized Rust implementation
by someone who has spent years on it. It uses native integers, efficient
memory management, and a clean evaluator. We will not beat jaq on pure
filter evaluation speed — we'd be doing essentially the same things in
the same language. The target is to **match** jaq on eval, and win on
everything else (parsing, parallelism, streaming).

Where the performance advantage is real:
- **Parsing**: simdjson On-Demand at 5-7 GB/s vs jaq's hifijson at ~500 MB/s (10-14x)
- **Parallelism**: 10 threads on independent NDJSON lines (~8-9x scaling)
- **End-to-end large NDJSON**: Parsing dominates, so SIMD + threading = 30-50x

Where we're at parity with jaq:
- **Pure eval on same parsed data**: `map(select(.x > 0) | {a: .a, b: .b})`
  on an already-parsed DOM — roughly same speed, maybe 10-20% either way
- **Small inputs**: Parsing takes microseconds regardless. Startup dominates
- **Complex filters on small data**: Pure eval speed. jaq is already good

**Key design choices for competitive evaluation:**
- Native i64 for integers (not f64 like jq)
- Arena-allocated AST with index references (cache-friendly)
- Per-thread reusable output buffers (same pattern as gg's WorkerState)
- Small-string optimization for common short strings (field names, "null", etc.)
- Avoid unnecessary allocations in the hot loop — reuse Value storage

If profiling shows AST-walk overhead matters for complex filters on small
inputs, consider a simple bytecode compiler as a Phase 5 optimization.
For Tier 1 filters on large inputs (the target workload), parsing
dominates and evaluation overhead is negligible.

### 1e. Output formatting

Three modes: pretty-print (default TTY), compact (`-c`), raw (`-r`).
Pretty-print with optional ANSI color (same as jq).

**Fast-path output:** For On-Demand simple field access with `-c` or
`-r`, we can often write simdjson's raw bytes directly to output without
re-serialization. simdjson gives us pointers into the original input
buffer — for string values, we can write the raw bytes (with unescaping
if needed) instead of building a Value and re-serializing.

**Success criterion:** `jx '.field' file.json` produces identical output
to `jq '.field' file.json` for all Tier 1 filters. Throughput ≥5x jq
on a 100MB file. Filter evaluation speed comparable to jaq (within ±20%)
on jaq's own benchmark suite.

---

## Phase 2: Parallel NDJSON

**Goal:** `cat huge.jsonl | jx '.field'` uses all cores and achieves
near-linear speedup.

### 2a. NDJSON detection

Auto-detect NDJSON input: if the first N bytes contain multiple
top-level JSON values separated by newlines, switch to parallel mode.
Also triggered by `--jsonl` flag.

Heuristic: read first 16KB. If it contains ≥2 newlines that each
precede a `{` or `[`, treat as NDJSON.

### 2b. Parallel processing pipeline

```
Input (stdin or mmap'd file)
  → Split into chunks at newline boundaries (splitter.rs)
    → Each chunk: ~1MB of NDJSON lines
    → Distribute chunks to thread pool
      → Per-thread simdjson parser (reused across lines)
      → Per-thread: iterate_many(chunk) → evaluate filter → format output
    → Ordered merge of output (preserve input order)
  → Write to stdout
```

**Two parsing strategies:**

For mmap'd files: split at ~1MB boundaries using memchr to find newlines.
Each thread gets a chunk and uses `iterate_many` (simdjson's built-in
NDJSON batch parser, already 3.5 GB/s) to parse all documents in its
chunk. This is the optimal path — simdjson handles the line splitting
and parsing internally.

For stdin: read into a growing buffer, split at newlines, distribute
complete lines to workers. Can't use iterate_many as easily (need
complete chunks), but still parallelize parsing + filter evaluation.

**Thread pool:** Each thread owns its own `simdjson::parser` instance
(simdjson parsers are not thread-safe but are reusable). Default thread
count: `std::thread::available_parallelism()`. On Apple Silicon this
respects QoS and returns P-core count.

**Output ordering:** Each thread writes to a per-thread buffer. Flush
buffers to stdout in chunk order (not completion order) to preserve
input ordering. Same pattern as gg's per-thread out_buf.

**Edge case: --slurp with NDJSON.** `--slurp` collects all inputs into
a single array before filtering. Still benefit from parallel *parsing*,
just defer filter evaluation until all lines are parsed and collected.

### 2c. Stdin streaming

For piped input (not seekable), we can't mmap. Instead:
1. Read stdin into a growing buffer
2. When buffer has ~1MB of complete lines, dispatch to a worker
3. Workers parse + filter + buffer output
4. Main thread collects output in order

This allows streaming — output appears as input arrives, with ~1MB
latency for chunk accumulation.

**Success criterion:** `jx '.field' 1gb.jsonl` is ≥5x faster than
`jq '.field' 1gb.jsonl` on an 8+ core machine. ≥30x faster with
10 cores + SIMD combined.

---

## Phase 3: Tier 2 filter support

**Goal:** Cover the "weekly ten" filters. At this point jx is usable
for most real-world tasks.

### Implementation order (by user impact)

1. **`map()`** — desugars to `[.[] | f]`. Once `.[]` and pipe work,
   this is straightforward.
2. **`length`** — array length, string length, object key count, null → 0.
3. **`keys` / `values`** — return array of keys or values from object.
4. **`sort_by()` / `group_by()` / `unique_by()`** — sort/group array
   by expression result. Requires collecting into array, evaluating
   expression per element, then sorting.
5. **`first` / `last`** — `first(expr)` takes first output of generator.
   `last(expr)` takes last. Important for short-circuit optimization.
6. **Array slicing** — `.[2:5]`, `.[-1]`, `.[0]`. Already partially
   covered by Phase 1 indexing.
7. **`+` operator** — polymorphic: number addition, string concatenation,
   array concatenation, object merge. Object merge is shallow.
8. **`type`** — returns type name as string.
9. **`add`** — reduce array via `+`. `[1,2,3] | add` → 6.
10. **`not` / `and` / `or`** — boolean operations.
11. **`has()` / `in()`** — key existence check.
12. **`if-then-else`** — conditional expression.
13. **`//` (alternative)** — `.x // "default"`.
14. **`?` (try)** — `.foo?` suppresses errors.
15. **Variable binding** — `. as $x | ...`.

Note: Tier 2 filters require the DOM fallback path (they need the full
tree structure). The On-Demand fast path only applies to Tier 1. This is
fine — the DOM path is still 2-3 GB/s via simdjson, which is 5-10x
faster than jq's parser.

### Conformance testing

Build a test harness that:
1. Runs every filter expression through both `jx` and `jq`
2. Compares output byte-for-byte
3. Tests against a corpus of real-world JSON (GitHub API responses,
   npm package.json, AWS CloudTrail logs, Kubernetes manifests)

Any difference is a bug. Import jaq's test suite as additional coverage.

**Success criterion:** All Tier 2 filters produce identical output to jq.
Performance regression ≤5% vs Phase 1 for Tier 1 filters (no impact on
the fast path).

---

## Phase 4: Streaming large single-file JSON

**Goal:** `jx '.items[]' 5gb_array.json` works without loading 5GB
into memory.

### The problem

A 5GB JSON file like `[{...}, {...}, ..., {...}]` with millions of
array elements. jq loads the entire thing into memory (uses ~5x the
file size in RAM).

### Prior art: jq `--stream`

jq already has a streaming mode: `jq --stream` parses incrementally,
emitting `[[path], value]` pairs without loading the full tree. So
`jq --stream 'select(.[0] == ["name"]) | .[1]' huge.json` works on a
5GB file with constant memory.

The problems with `--stream`:
- **Different programming model.** You work with path arrays instead of
  normal jq filters. `select(.[0] == ["name"]) | .[1]` vs just `.name`.
  It's a different language, not just a flag.
- **Significantly slower.** Every value gets wrapped in a `[[path], value]`
  tuple, adding allocation and GC overhead. Typical slowdown is 2-5x vs
  normal mode.
- **Painful for anything beyond trivial extraction.** Reconstructing
  objects, filtering nested structures, or doing any real transformation
  in streaming mode requires writing convoluted path-matching expressions.

Our differentiator is **transparent streaming**: write normal jq filter
syntax, get streaming memory behavior automatically. The user shouldn't
need to know or care that streaming is happening.

### Approach: simdjson iterate_many + structural scanning

simdjson's `iterate_many` is designed for document streams but can also
be used with creative input preparation. For a large array:

1. mmap the entire file (virtual memory, not physical)
2. Use simdjson's SIMD structural character scanner to identify
   top-level array element boundaries without parsing
3. For each element: parse only that element via On-Demand, evaluate
   filter, output if matches
4. Never hold more than one element's DOM in memory at a time

**Parallelizable:** Once element boundaries are known, elements can be
parsed + filtered in parallel (same as NDJSON chunks). Distribute
element ranges to threads.

**Complication:** Nested arrays. If the filter is `.items[].tags[]`,
we need to identify top-level elements of `.items`, then for each,
identify elements of `.tags`. The structural scanner needs to track
nesting depth.

**Success criterion:** `jx '.[]' 1gb_array.json` completes in <10s
with <100MB RSS. `jq '.[]' 1gb_array.json` uses >5GB RSS.

---

## Phase 5: Tier 3 filters and polish

**Goal:** Cover the remaining ~5% of usage. Full CLI compatibility
with jq for common flags. Optimize filter evaluation if needed.

### Filters to add

- `reduce` / `foreach` — streaming fold operations
- `try-catch` — error handling
- `@csv` / `@tsv` / `@base64` / `@uri` / `@json` — format strings
- `test()` / `match()` / `capture()` / `scan()` / `splits()` — regex (use `regex` crate)
- `split` / `join` / `gsub` / `sub` — string operations
- `to_entries` / `from_entries` / `with_entries` — object↔array conversion
- `min_by` / `max_by` / `flatten` — array operations
- `tostring` / `tonumber` / `tojson` / `fromjson` — type/format conversion
- `ascii_downcase` / `ascii_upcase` — case conversion
- `ltrimstr` / `rtrimstr` / `startswith` / `endswith` / `contains` — string predicates
- `del()` — field deletion
- `env` / `$ENV` — environment variable access
- `input` / `inputs` — multi-input processing
- `--arg` / `--argjson` / `--slurpfile` — CLI variable injection
- String interpolation: `"Hello \(.name)"`
- `def` — user-defined functions
- `walk()` — recursive transform
- `path()` / `getpath()` / `setpath()` / `delpaths()` — path operations
- `debug` — debug output to stderr
- `limit()` / `until()` / `while()` / `repeat()` — loop constructs
- `any` / `all` — quantifiers
- `range()` — sequence generation
- `recurse` / `..` — recursive descent
- `empty` / `error` / `halt` / `halt_error` — control flow
- `//` alternative, `?` try, `?//` try-alternative
- Date/time: `now`, `strftime`, `strptime`, `todate`, `fromdate`, `gmtime`, `mktime`
- Math: `floor`, `ceil`, `round`, `sqrt`, `pow`, `log`, `exp`, `fabs`, `nan`, `infinite`
- `builtins` — list available built-ins
- `transpose` / `combinations` / `nth` / `isempty` — utility functions
- `ascii` / `explode` / `implode` — codepoint operations

### Evaluation optimization (if needed)

If profiling shows filter evaluation is a bottleneck for complex Tier 3
filters on small inputs (where parsing doesn't dominate), consider:

1. **Simple bytecode compiler:** Compile AST to flat bytecode with a
   register-based VM. Eliminates tree-walk overhead. jq does this but
   their VM is poorly optimized; a clean Rust VM would be faster.
2. **Specialize common patterns:** Hard-code `map(f)` as a single
   operation rather than desugaring to `[.[] | f]`. Same for `select`,
   `any`, `all`.
3. **Avoid intermediate allocations:** For `map(f)`, allocate the result
   array once at the known size rather than pushing to a Vec.

### Polish

- Color output matching jq's color scheme
- Error messages matching jq's format (so scripts that parse errors work)
- `--tab` and `--indent` flags
- Bash/zsh completion
- Man page
- Homebrew formula

---

## Dependencies

| Crate | Purpose | Status |
|-------|---------|--------|
| `cc` | Compile vendored simdjson.cpp at build time | Build dependency |
| `clap` | CLI parsing | Standard |
| `rayon` | Parallel iterators for NDJSON | Core dependency |
| `memchr` | Fast newline scanning for NDJSON splitting | Core dependency |
| `regex` | Regex for `test()`, `match()`, `capture()`, `gsub()`, `sub()` | Phase 5 |
| `libc` | mmap for file I/O | Platform I/O |

**Vendored (not crate dependencies):**
| Library | Purpose |
|---------|---------|
| `simdjson.h` + `simdjson.cpp` | SIMD JSON parser — On-Demand API, DOM API, iterate_many |

**What we're NOT depending on:**
- `simd-json` (Rust port) — no On-Demand API, 2-2.5x slower DOM parsing
- `serde` / `serde_json` — not needed; we go directly from simdjson
  values to our own Value type. May add as optional feature for
  compatibility if there's demand.
- `jaq-core` — tightly coupled to jaq's own value type; can't use
  simdjson On-Demand without a conversion layer that defeats the purpose

---

## Estimated performance targets

### Parsing throughput (Phase 0)

| Input | jq | jaq | simd-json (Rust) | simdjson On-Demand | Speedup vs jq |
|-------|-----|------|------------------|--------------------|---------------|
| twitter.json (631KB) | ~400 MB/s | ~500 MB/s | ~1,300 MB/s | ~5,000 MB/s | **12x** |
| citm_catalog.json (1.7MB) | ~300 MB/s | ~450 MB/s | ~1,400 MB/s | ~6,000 MB/s | **20x** |
| canada.json (2.2MB, float-heavy) | ~200 MB/s | ~300 MB/s | ~450 MB/s | ~1,500 MB/s | **7x** |
| 100MB NDJSON | ~300 MB/s | ~450 MB/s | ~1,200 MB/s | ~3,500 MB/s | **12x** |

Note: canada.json is worst-case (dense floats). simdjson acknowledges
float parsing can't reach GB/s speeds. Typical JSON (mixed types) is
the best case.

### End-to-end with parallelism (Phase 2)

| Workload | jq | jx (1 thread) | jx (10 threads) | Speedup |
|----------|-----|---------------|-----------------|---------|
| `'.field' 100mb.jsonl` | 0.33s | 0.03s | 0.005s | **66x** |
| `'.field' 1gb.jsonl` | 3.3s | 0.3s | 0.05s | **66x** |
| `'select(.x > 0)' 1gb.jsonl` | 5s | 0.5s | 0.08s | **62x** |
| `'.' 100mb.json` (pretty-print) | 1.5s | 0.15s | — | **10x** |
| `'.[]' 5gb_array.json` | OOM | — | 2s, <100MB RSS | **∞** |

These are optimistic estimates. Real numbers will be lower due to output
formatting, filter evaluation overhead, thread synchronization, and I/O.
Even achieving half these targets gives a compelling story: 5x
single-threaded, 30x multi-threaded for NDJSON.

### Filter evaluation (Phase 1)

| Benchmark | jq | jaq | jx target |
|-----------|-----|------|-----------|
| Startup (empty filter) | ~5ms | ~1ms | ≤1ms |
| Simple field access | baseline | ~2x jq | ~2x jq (match jaq) |
| map/select | baseline | ~2-3x jq | ~2-3x jq (match jaq) |
| reduce/fold | baseline | ~2-5x jq | ~2-5x jq (match jaq) |

Filter evaluation speed should match jaq — not beat it. Both are Rust,
both use native integers, both can use efficient memory management. jaq
has had years of optimization; claiming we'd beat it on eval is not
credible. The real win is in parsing and parallelism, not the evaluator.

For simple path queries on the On-Demand fast path, filter "evaluation"
is essentially free — it's just navigating the SIMD structural index.
This is where the end-to-end numbers get exciting, but it's a parsing
win, not an eval win.

### Honest performance comparison: jx vs jaq vs jq

| Scenario | vs jq | vs jaq | Why |
|----------|-------|--------|-----|
| Simple filter, large file (1 thread) | 10-15x | 5-10x | SIMD On-Demand parsing |
| Simple filter, large NDJSON (10 threads) | 50-100x | 30-60x | SIMD + parallelism |
| Complex filter, large file | 5-8x | 2-4x | SIMD parsing, similar eval |
| Complex filter, small file | 2-3x | ~1x | Eval-dominated, similar speed |
| Small file, simple filter | 2-3x | ~1x | Startup-dominated |

The win over jaq is almost entirely in the parser and threading, not
the evaluator. On eval-dominated workloads (complex filters, small files),
we're at parity. That's fine — nobody runs `reduce` on a 100-byte JSON
file and complains about speed. The people who need "faster jq" have
large inputs, and that's where the 10-50x advantage lives.

---

## Positioning

**"jq, but fast on large data."** Two concrete claims backed by hardware:

1. **10x faster parsing** via SIMD (simdjson On-Demand, NEON/AVX2)
2. **10x faster NDJSON** via built-in parallel processing

And one architectural advantage:

3. **Transparent streaming** for large files — normal jq syntax, constant
   memory (jq's `--stream` exists but requires a different programming model)

**What we're NOT claiming:** Faster filter evaluation than jaq. On
eval-dominated workloads (complex filters, small inputs), jx and jaq
are roughly equivalent. The performance story is parsing + parallelism.

**Primary audience:** Developers processing large JSON. Log pipelines,
NDJSON datasets, large API dumps. Specifically: anyone who has added
`parallel | jq` to a pipeline, hit OOM on a large JSON file, or waited
more than a second for jq to finish.

**Competitive positioning vs jaq:** jaq is "jq but correct and clean."
jx is "jq but fast on large data." Different niches. jaq is better for
people who want a drop-in jq replacement with maximum compatibility.
jx is better for people processing >10MB of JSON at a time. They
complement rather than compete — though if jx matches jaq on eval speed
AND adds SIMD parsing + parallelism, the "why not just use jx" argument
gets strong for large-data workflows.

---

## Risks and mitigations

### Risk: jq language is more complex than it looks

jq's filter language has subtle semantics around generators, backtracking,
and multiple outputs that are hard to get right. `map(select(.x))` looks
simple but involves generator semantics.

**Mitigation:** Start with the simplest possible filter subset. Build
a comprehensive conformance test suite from day one. Accept ~80% jq
compatibility as the target, not 100%. Document incompatibilities clearly.

### Risk: C++ FFI adds build complexity

Vendoring simdjson.cpp requires a C++ compiler at build time.

**Mitigation:** simdjson is a single-compilation-unit library (one .h,
one .cpp). The `cc` crate handles C++ compilation transparently on all
major platforms. No cmake, no configure, no pkg-config. Cross-compilation
is supported — simdjson's autodetection works at runtime, so a single
binary handles NEON/AVX2/SSE4.2 automatically. The FFI surface is small
(~20 functions) and well-tested.

### Risk: simdjson's On-Demand API has usage constraints

On-Demand documents are forward-only iterators — you can't access a field
twice, and you must access fields in order. This is by design (it's what
enables the speed), but it constrains the evaluator.

**Mitigation:** The On-Demand fast path only handles filters where field
access order is known at compile time (the common case: `.a.b.c`,
`.[] | .name`). For anything that needs random access (e.g.,
`{b: .b, a: .a}` — accessing fields out of order), fall back to DOM.
The filter compiler can detect this statically.

### Risk: Output formatting becomes the bottleneck

At 5 GB/s parsing throughput, the bottleneck shifts to JSON serialization
for output. Pretty-printing with color is much slower than parsing.

**Mitigation:** Implement fast-path output for common cases. For `jx -c`
(compact output), the input bytes can often be copied directly to output
without re-serialization (if the filter is identity or simple field
access). For pretty-print, use SIMD-accelerated string escaping.

### Risk: People just use jaq

jaq already exists, is well-maintained, and handles most use cases well
enough. jx's niche (large files, parallelism) may be too narrow.

**Mitigation:** The combination of SIMD On-Demand parsing + parallel
NDJSON + streaming large files is unique — no other jq-like tool does
all three. If jx also matches jaq's eval speed, the only advantage jaq
retains is higher jq compatibility (~90% vs ~80%) and no C++ build
dependency. Focus marketing on concrete benchmarks: "process 1GB of
JSONL in 50ms" is a number that gets attention.

---

## What we're NOT building (scope control)

- No full jq language compatibility. ~80% of filters, the ones people
  actually use.
- No module system (`import`/`include`). Niche usage, complex to implement.
- No custom format strings beyond @csv/@tsv/@base64/@uri/@json.
- No interactive mode / REPL (jnv, jless exist for this).
- No YAML/TOML/XML input (jaq does this; differentiate on speed, not
  format support).
- No SQL-like query language (different paradigm entirely).
- No GUI / TUI. Pipe-friendly CLI only.
- No Windows support in MVP (add later if there's demand).
