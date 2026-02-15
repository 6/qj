# qj — A faster jq for large JSON and JSONL

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

qj aims to beat everything on *parsing throughput* and *parallel
processing*, while matching jaq on *filter evaluation speed*. The
performance story is SIMD parsing + parallelism, not a faster evaluator.

---

## Competitive landscape

| Tool | Parsing (measured) | E2E (measured) | Parallel | SIMD | On-Demand | Streaming | jq compat | Platform |
|------|-------------------|----------------|----------|------|-----------|-----------|-----------|----------|
| jq 1.7 | 23-62 MB/s e2e | baseline | No | No | No | Yes (`--stream`)* | 100% | All |
| jaq 2.3 | 93-187 MB/s e2e | 1.3-2x jq | No | No | No | No | ~90% | All |
| gojq 0.12 | 47-122 MB/s e2e | 0.8-2.5x jq | No | No | No | No | ~85% | All |
| **qj 0.1** | **7-9 GB/s parse** | **3-63x jq** | **Planned** | **Yes (NEON/AVX2)** | **Passthrough** | **Planned** | **~60%** | **macOS/Linux** |

\* jq's `--stream` mode parses incrementally, emitting `[[path], value]`
pairs without loading the full tree. It works for constant-memory
processing of large files, but requires a completely different programming
model — you work with path arrays instead of normal jq filters (e.g.,
`jq --stream 'select(.[0] == ["name"]) | .[1]'` instead of `jq '.name'`).
It's also significantly slower than normal mode because every value gets
wrapped in a path tuple. qj's streaming (Phase 4) uses normal jq filter
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
qj/
├── src/
│   ├── main.rs               # CLI (clap derive), orchestration
│   ├── lib.rs                 # Module declarations
│   ├── value.rs               # Value enum (Null/Bool/Int/Double/String/Array/Object)
│   ├── output.rs              # JSON output: compact, pretty, raw (itoa + ryu)
│   ├── simdjson/
│   │   ├── mod.rs             # Re-exports
│   │   ├── simdjson.h         # Vendored C++ header (v4.2.4)
│   │   ├── simdjson.cpp       # Vendored C++ implementation
│   │   ├── bridge.cpp         # C-linkage FFI: On-Demand + DOM flat token buffer
│   │   └── bridge.rs          # Safe Rust wrapper: parse, extract, dom_parse_to_value
│   └── filter/
│       ├── mod.rs             # Filter AST, CmpOp, ArithOp, BoolOp, ObjKey, parse()
│       ├── lexer.rs           # Tokenizer (47 token types)
│       ├── parser.rs          # Recursive descent parser → Filter AST
│       └── eval.rs            # Generator evaluator, 30+ builtins
├── benches/
│   ├── data/                  # twitter.json, canada.json, citm_catalog.json, NDJSON
│   ├── gen_ndjson.rs          # NDJSON test data generator
│   └── parse_throughput.rs    # Criterion benchmarks (simdjson vs serde)
├── tests/
│   ├── simdjson_ffi.rs        # simdjson FFI integration tests (15 tests)
│   └── e2e.rs                 # End-to-end CLI tests (56 tests)
├── build.rs                   # Compiles simdjson.cpp via cc crate
├── Cargo.toml
├── CLAUDE.md
└── PLAN.md

# Planned (not yet created)
#   src/parallel/              # Phase 2: NDJSON chunk splitter + thread pool
#   src/io/                    # Phase 2: mmap for files, streaming for stdin
#   src/filter/eval_ondemand.rs  # Phase 1.5: On-Demand fast path
```

---

## Platform strategy — why cross-platform

Unlike gg, qj's advantages are **not platform-specific**:

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

**What to build:** `benches/parse-throughput/`

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

### Phase 0 results (Apple Silicon M-series, 2025-02)

**Status: COMPLETE — thesis validated.**

Measured throughput on Apple Silicon (NEON) using simdjson v4.2.4 via
Rust FFI (`cc` crate, C++17, -O3). jq 1.7.1, jaq 2.3.0, gojq 0.12.18
via Homebrew.

Single-file parsing (identity filter `.`):

| File | jq | gojq | jaq | serde_json | simdjson (FFI) |
|------|-----|------|------|-----------|----------------|
| twitter.json (631KB) | 32 MB/s | 76 MB/s | 93 MB/s | 558 MB/s | 8,577 MB/s |
| citm_catalog.json (1.7MB) | 44 MB/s | 122 MB/s | 187 MB/s | 814 MB/s | 9,320 MB/s |
| canada.json (2.2MB) | 23 MB/s | 122 MB/s | 114 MB/s | 599 MB/s | 6,992 MB/s |

NDJSON field extraction (`.name`):

| File | jq | gojq | jaq | serde_json | simdjson iterate_many (FFI) |
|------|-----|------|------|-----------|----------------------------|
| 100k.ndjson (8MB) | 62 MB/s | 47 MB/s | 108 MB/s | 242 MB/s | 2,601 MB/s |
| 1m.ndjson (82MB) | 64 MB/s | 50 MB/s | 114 MB/s | 245 MB/s | 2,663 MB/s |

Speedup ratios (simdjson On-Demand via FFI vs each tool):

| File | vs jq | vs gojq | vs jaq | vs serde_json |
|------|-------|---------|--------|---------------|
| twitter.json | 268x | 113x | 92x | 15x |
| citm_catalog.json | 212x | 76x | 50x | 11x |
| canada.json | 304x | 57x | 61x | 12x |
| 100k.ndjson | 42x | 55x | 24x | 11x |
| 1m.ndjson | 42x | 54x | 23x | 11x |

Note: jq/jaq/gojq numbers are end-to-end (process spawn + read + parse +
filter + format + write to /dev/null), while simdjson/serde numbers are
pure in-process parse throughput. The apples-to-apples comparison is
against serde_json (same measurement method). External tool numbers
include ~1-5ms process startup overhead which penalizes them on small
files but is negligible on large ones. gojq is notably slower than jaq
on NDJSON but competitive on single-file float-heavy data (canada.json).

FFI overhead (C++ direct vs Rust FFI):

| Benchmark | C++ direct | Rust FFI | Overhead |
|-----------|-----------|----------|----------|
| twitter.json | 8,116 MB/s | 8,577 MB/s | <1% (noise) |
| canada.json | 6,989 MB/s | 6,992 MB/s | <1% (noise) |
| 1m.ndjson count | 3,088 MB/s | 2,943 MB/s | ~4.7% |

Results exceed expectations:
- On-Demand throughput 7,000-9,300 MB/s (expected 5,000-7,000)
- ≥11x over serde_json across all files (target was ≥5x) ✓
- ≥42x over jq across all files (target was ≥10x) ✓
- ≥23x over jaq on NDJSON, ≥50x on single files ✓
- FFI overhead 0-5% (target was <5%) ✓

---

## Phase 1: Filter parser, evaluator, and output

**Goal:** `qj '.field' file.json` works end-to-end, producing identical
output to jq. Throughput ≥5x jq on large files.

### Phase 0 lessons applied

Phase 0 revealed several things that change the Phase 1 approach:

1. **Output formatting is the new bottleneck.** At 8+ GB/s parsing, even
   a moderately fast JSON serializer (~1 GB/s) becomes the dominant cost.
   Output formatting is not a nice-to-have — it's the critical path.
   Elevated from "1e afterthought" to core Phase 1 deliverable.

2. **DOM-only first, On-Demand fast path later.** The On-Demand API is
   forward-only with strict access ordering constraints (`select` needs
   re-access, `{b: .b, a: .a}` breaks field order). The simdjson DOM
   path is still ~2-3 GB/s — already 30-90x faster than jq end-to-end.
   Ship Phase 1 with DOM-only evaluation. Add On-Demand fast path as
   Phase 1.5 optimization once the evaluator is solid and we can profile
   what actually matters.

3. **End-to-end benchmarks from day one.** Phase 0 numbers compare
   in-process parse throughput vs external tool end-to-end, which inflates
   the ratios. Phase 1 must include honest end-to-end benchmarks:
   `qj '.name' file > /dev/null` vs `jq '.name' file > /dev/null`.
   The real advantage will be ~10-30x vs jq (not 264x), and that's the
   number we should report.

4. **NDJSON iterate_many ceiling is ~2,800 MB/s per thread**, not
   ~8,000 MB/s. Phase 2 parallelism estimates are adjusted accordingly.

### 1a. simdjson FFI bridge — DONE (Phase 0)

Completed in Phase 0. The bridge supports On-Demand parse, field
extraction (string/int64/double), document type checking, and
iterate_many for NDJSON. Safe Rust wrapper with padding enforcement
and lifetime tracking.

### 1b. Filter language parser

Build a jq filter parser that handles Tier 1 syntax. Use a recursive
descent parser (jq's grammar is simple enough).

The AST should be compact — it's evaluated millions of times for NDJSON.
Represent it as a flat Vec of nodes with index references (arena
allocation) rather than heap-allocated tree nodes, to avoid pointer
chasing and improve cache locality.

### 1c. DOM-based evaluator

**DOM path only for Phase 1.** Parse via simdjson DOM API (full tree),
convert to Rust-owned `Value` enum, evaluate filters against that.
This path is ~2-3 GB/s parse throughput, which is already 30-90x
faster than jq end-to-end.

All Tier 1 filters work against the DOM:
- Field access chains: `.a.b.c`
- Array iteration: `.[]`, `.items[]`
- Pipe: `.[] | .name`
- Select: `select(.age > 30)`
- Object construction: `{name: .name, id: .id}`

**Key design choices:**
- Native i64 for integers (not f64 like jq)
- Arena-allocated AST with index references (cache-friendly)
- Per-thread reusable output buffers
- Avoid unnecessary allocations in the hot loop — reuse Value storage

### 1d. Output formatting — core deliverable

Three modes: pretty-print (default TTY), compact (`-c`), raw (`-r`).
Pretty-print with optional ANSI color (same as jq).

**This is performance-critical.** At 8+ GB/s parse throughput, output
formatting becomes the bottleneck immediately. We use a tiered
serialization strategy:

#### Tier 0 — Passthrough (zero-copy)

**Note:** This was the original plan. The revised approach (see "Next
steps" Step 2) uses `Value::RawBytes` + `simdjson::minify()` instead of
`raw_json()` pointers, which composes better with the evaluator.

For filters that return original sub-objects (`.`, `.field`, `.[0]`),
skip Value construction and output serialization. For identity compact,
use `simdjson::minify()` (~10 GB/s SIMD-accelerated). For field access,
DOM parse + field lookup produces `Value::RawBytes` containing minified
JSON bytes that flow through the evaluator and output formatter
naturally. Only the output formatter needs to handle the new variant
(compact: `write_all`, pretty: parse back to Value then format).

#### Tier 1 — Direct-to-buffer writing

For transformed/constructed values, write directly into `BufWriter<Stdout>`
with 128KB buffer. Use `itoa` for integers, `ryu` for floats (Ryu
algorithm — fastest float-to-string, 5-10x faster than `sprintf`).
Tight loop for string escaping: scan for special chars (`"`, `\`,
control chars), memcpy safe runs in bulk. Never allocate intermediate
`String` — all formatting writes directly to the output buffer.

#### Tier 2 — Pretty-print

Same as Tier 1 but with indentation tracking. Default for TTY output,
disabled with `-c`. Two spaces per level (jq default), or tabs with
`--tab`.

#### Future: SIMD string escaping

For Phase 2+, investigate SIMD-accelerated scanning for escape
characters (16-byte NEON / 32-byte AVX2 chunks). If a chunk has no
special chars, bulk-copy. This closes the gap between "passthrough"
and "must-serialize" paths. Could live in bridge.cpp leveraging
simdjson's internal SIMD infrastructure or adapt from `simdjson::minify`.

#### Future: Per-thread scratchpads (Phase 2)

For parallel NDJSON, each thread writes to its own `Vec<u8>` buffer.
After processing a chunk, pass the whole buffer to the main thread for
ordered `write_all`. Avoids contention on stdout.

**Implementation order:** Start with Tier 1 (direct-to-buffer) for all
output. Add Tier 0 passthrough as a follow-up optimization once the
basic pipeline works.

**Success criterion:** `qj '.field' file.json` produces identical output
to `jq '.field' file.json` for all Tier 1 filters. End-to-end throughput
≥5x jq, ≥2x jaq on large files (measured with real output, not
parse-only).

### 1.5 (future): On-Demand fast path

Deferred from Phase 1. Once the DOM evaluator and output formatting are
solid, add an On-Demand fast path for pure path navigation filters.
This skips DOM construction entirely for `.field`, `.a.b.c`,
`.[] | .name` — navigating the SIMD structural index directly at
~7 GB/s. Only add this if profiling shows DOM construction is actually
a bottleneck for common filters (it may not be, since output formatting
likely dominates anyway).

### 1e. Filter evaluation performance

**Honest assessment:** jaq is already a well-optimized Rust implementation
by someone who has spent years on it. It uses native integers, efficient
memory management, and a clean evaluator. We will not beat jaq on pure
filter evaluation speed — we'd be doing essentially the same things in
the same language. The target is to **match** jaq on eval, and win on
everything else (parsing, parallelism, streaming).

Where the performance advantage is real:
- **Parsing**: simdjson DOM at ~2-3 GB/s vs jaq's hifijson at ~500 MB/s (4-6x)
- **Parallelism**: 10 threads on independent NDJSON lines (~8-9x scaling)
- **End-to-end large NDJSON**: Parsing dominates, so SIMD + threading = 20-40x

Where we're at parity with jaq:
- **Pure eval on same parsed data**: `map(select(.x > 0) | {a: .a, b: .b})`
  on an already-parsed DOM — roughly same speed, maybe 10-20% either way
- **Small inputs**: Parsing takes microseconds regardless. Startup dominates
- **Complex filters on small data**: Pure eval speed. jaq is already good

### Phase 1 results (Apple Silicon M-series, 2025-02)

**Status: COMPLETE — `qj '.field' file.json` works end-to-end.**

All Phase 1 slices implemented: Value type, output formatter (Tier 1
direct-to-buffer with itoa/ryu, Tier 2 pretty-print), simdjson DOM bridge
(flat token buffer protocol), filter lexer, recursive descent parser,
generator-based evaluator, CLI with clap. 217 tests (146 unit + 56 e2e + 15 FFI).

#### What was built

**Filter language** — covers all of Tier 1, most of Tier 2, and a
significant portion of Tier 3:

| Category | Implemented |
|----------|-------------|
| Core | `.field`, `.[]`, `\|`, `select()`, `{...}`, `[...]`, `,` (comma) |
| Navigation | `.[n]`, `.[-n]`, `..` (recurse) |
| Arithmetic | `+`, `-`, `*`, `/`, `%` (polymorphic: numbers, strings, arrays, objects) |
| Comparison | `==`, `!=`, `<`, `<=`, `>`, `>=` |
| Boolean | `and`, `or`, `not` |
| Control | `if-then-else-end`, `//` (alternative), `?` (try) |
| Builtins | `length`, `keys`, `keys_unsorted`, `values`, `type`, `empty` |
| Array ops | `sort`, `sort_by()`, `group_by()`, `unique`, `unique_by()`, `flatten`, `reverse`, `first`, `last`, `min`, `max`, `min_by()`, `max_by()` |
| Transforms | `map()`, `select()`, `add`, `any`, `all`, `has()`, `del()`, `contains()` |
| Object ops | `to_entries`, `from_entries` |
| String ops | `split()`, `join()`, `ascii_downcase`, `ascii_upcase`, `ltrimstr()`, `rtrimstr()`, `startswith()`, `endswith()` |
| Type conversion | `tostring`, `tonumber` |
| String interp | `"hello \(.name)"` |
| Unary | `-expr` (negation) |
| Literals | `null`, `true`, `false`, integers, floats, strings |

**CLI flags:** `-c` (compact), `-r` (raw), `--tab`, `--indent N`,
`-e` (exit status), `-n` (null input).

**Architecture:**
- `Value` enum with `Int(i64)` / `Double(f64)` distinction (unlike jq's all-f64)
- `Object` as `Vec<(String, Value)>` preserving insertion order
- Generator-based evaluator: `fn eval(filter, input, &mut dyn FnMut(Value))`
- simdjson DOM → flat token buffer → `Value` tree (single FFI call per doc)
- `BufWriter` with 128KB buffer, `itoa` for ints, `ryu` for floats
- Output includes trailing newline per value (matches jq)

#### End-to-end benchmarks

Measured with hyperfine (warmup 3, shell=none). jq 1.7.1, jaq 2.3.0,
gojq 0.12.18 via Homebrew. Apple Silicon.

| Benchmark | qj | jq | jaq | qj vs jq | qj vs jaq |
|-----------|-----|-----|------|----------|-----------|
| `.statuses[0].user.screen_name` twitter.json (631KB) | 4.0ms | 8.9ms | 4.9ms | **2.25x** | 1.25x |
| `-c '.'` canada.json (2.2MB) | 10.4ms | 40.7ms | 13.2ms | **3.9x** | 1.27x |
| `-c '.performances \| keys \| length'` citm_catalog.json (1.7MB) | 5.3ms | 18.7ms | 6.8ms | **3.5x** | 1.29x |

Output verified byte-identical to jq on all test files:
- `diff <(qj -c '.' twitter.json) <(jq -c '.' twitter.json)` → match
- `diff <(qj -c '.statuses[] | .user.screen_name' twitter.json) <(jq -c ...)` → match
- `diff <(qj -c '.statuses[] | select(.retweet_count > 0) | {user: .user.screen_name, retweets: .retweet_count}' twitter.json) <(jq -c ...)` → match

#### Assessment vs success criteria

| Criterion | Target | Actual | Status |
|-----------|--------|--------|--------|
| Identical output to jq | All Tier 1 filters | All tested filters | ✓ |
| Throughput vs jq | ≥5x | 2.25-3.9x | **Partial** |
| Throughput vs jaq | ≥2x | 1.25-1.29x | **Not yet** |

The throughput targets are not met yet. Analysis:

1. **These files are small** (0.6-2.2MB). Process startup (~2-3ms) and
   output formatting dominate at these sizes. The 7-9 GB/s simdjson
   parse advantage is mostly hidden — parsing a 2.2MB file takes <1ms
   regardless of parser. The real advantage shows on large files (Phase 2
   NDJSON with parallelism).

2. **DOM construction overhead.** We build a full `Value` tree from the
   flat token buffer, then clone values during evaluation. This is the
   expected DOM path — the On-Demand fast path (Phase 1.5) would skip
   tree construction for simple field access.

3. **Output is not yet optimized.** We write value-by-value through
   `write_value()`. Tier 0 passthrough (zero-copy from input buffer) is
   not implemented yet — it would eliminate output serialization entirely
   for identity and simple extraction filters.

4. **Still significantly faster than jq** — 2-4x across all benchmarks.
   On larger files (100MB+ NDJSON), where parsing dominates, the
   advantage will be much higher.

#### File inventory

| File | Purpose |
|------|---------|
| `src/value.rs` | `Value` enum, `type_name()`, `is_truthy()` |
| `src/output.rs` | JSON output: compact, pretty, raw modes (itoa + ryu) |
| `src/filter/mod.rs` | `Filter` AST, `CmpOp`, `ArithOp`, `BoolOp`, `ObjKey`, `StringPart` |
| `src/filter/lexer.rs` | Tokenizer (47 token types) |
| `src/filter/parser.rs` | Recursive descent parser |
| `src/filter/eval.rs` | Generator-based evaluator, 30+ builtins |
| `src/simdjson/bridge.cpp` | Extended with `qj_dom_to_flat()` (flat token buffer) |
| `src/simdjson/bridge.rs` | Extended with `dom_parse_to_value()` |
| `src/main.rs` | CLI (clap derive), stdin/file input, filter → eval → output |
| `tests/e2e.rs` | 42 end-to-end tests |

---

## Next steps

Based on Phase 1 results, the core problem is clear: on small files
(0.6-2.2MB), we're only 1.25-1.3x faster than jaq. The 7-9 GB/s
simdjson parse advantage is invisible because **parsing is <1ms
regardless of parser at this size**. The time goes to:

- Process startup: ~2-3ms (both tools)
- DOM→Value tree construction + cloning during eval: ~3-4ms (qj-specific overhead)
- Output serialization: ~2-3ms (both tools, fundamentally same speed)

We *think* the bottleneck is DOM→Value construction + output
serialization, but we haven't validated this on large files — which
is qj's actual value proposition. **Revised approach:** benchmark large
files first, profile to understand where time actually goes, then
implement passthrough if the data justifies it.

### Step 1: Large-file benchmarks + profiling

**Status: COMPLETE.**

Generated ~49MB test files, benchmarked all tools, profiled qj internals.
Also fixed double-allocation in file reading (`read_padded_file()` in
`bridge.rs` — single alloc, no copy). Added `--debug-timing` flag for
profiling breakdown.

**Benchmark results** (Apple Silicon, hyperfine --warmup 3):

Small file — twitter.json (631KB):

| Filter | qj | jq | jaq | gojq | qj vs jq | qj vs jaq |
|--------|----|----|-----|------|----------|-----------|
| `-c '.'` | 4.1ms | 16.5ms | 5.8ms | 8.2ms | **4.0x** | **1.4x** |
| `-c '.statuses'` | 4.2ms | 16.7ms | 5.7ms | 8.2ms | **4.0x** | **1.4x** |
| `.statuses\|length` | 3.8ms | 9.7ms | 5.5ms | 7.2ms | **2.5x** | **1.4x** |
| `.statuses[]\|.user.name` | 5.0ms | 10.1ms | 5.5ms | 7.2ms | **2.0x** | **1.1x** |

Large file — large_twitter.json (49MB):

| Filter | qj | jq | jaq | gojq | qj vs jq | qj vs jaq |
|--------|----|----|-----|------|----------|-----------|
| `-c '.'` | 260ms | 1178ms | 259ms | 453ms | **4.5x** | **1.0x** |
| `-c '.statuses'` | 261ms | 1173ms | 258ms | 448ms | **4.5x** | **1.0x** |
| `.statuses\|length` | 208ms | 391ms | 165ms | 293ms | **1.9x** | 0.79x |
| `.statuses[]\|.user.name` | 256ms | 398ms | 172ms | 299ms | **1.6x** | 0.67x |

**Profiling breakdown** (large_twitter.json, `-c '.'`, `--debug-timing`):

| Phase | Time | % of total |
|-------|------|------------|
| File read | 12ms | 6% |
| Parse (DOM→flat + flat→Value) | 114ms | 53% |
| Filter eval | 33ms | 15% |
| Output serialization | 56ms | 26% |
| **Total** | **216ms** | **227 MB/s** |

**Key findings:**
1. Parse + output = **79% of total time** on identity compact. Passthrough
   (skip Value construction + output serialization) would eliminate most
   of this. **Decision: proceed with passthrough (Step 2).**
2. On large files, jaq is neck-and-neck with qj on identity/field compact
   (~259ms vs ~260ms). jaq pulls ahead on iterate+field (172ms vs 256ms)
   — likely because jaq's evaluator avoids cloning Values.
3. qj is consistently 4-4.5x faster than jq on identity/field compact
   (both small and large), but only 1.6-2x on iterate+field.
4. The 7-9 GB/s simdjson advantage is hidden behind DOM→Value construction
   (114ms) and output serialization (56ms). With passthrough, identity
   compact should drop to ~15-20ms (parse only, no Value tree, no
   serialization) — that's **~60x jq, ~13x jaq**.

### Step 2: Rc-wrap Value containers — fix eval cloning overhead

**Status: COMPLETE.**

Wrapped `Array(Vec<Value>)` → `Array(Rc<Vec<Value>>)` and
`Object(Vec<(String, Value)>)` → `Object(Rc<Vec<(String, Value)>>)`.
Value::clone() for containers is now an Rc refcount bump (~1ns) instead
of a deep copy. Also added `Rc::ptr_eq` fast-path in `values_equal`.

**Files changed:** `value.rs`, `bridge.rs`, `eval.rs`, `output.rs`,
`parser.rs` (all pattern matches updated to use `.iter()` for
IntoIterator, `Rc::new()` for construction).

**Profiling results** (large_twitter.json, `--debug-timing`):

`.statuses[]|.user.name` (iterate+field) — the eval-heavy workload:

| Phase | Before Rc | After Rc |
|-------|-----------|----------|
| Parse | 114ms | 112ms |
| Eval | 33ms | **5ms** |
| Output | ~1ms | ~0.3ms |
| **Total** | **~150ms** | **~129ms** |

`-c '.'` (identity compact) — the parse+output workload:

| Phase | Before Rc | After Rc |
|-------|-----------|----------|
| Parse | 114ms | 115ms |
| Eval | 0ms | 0ms |
| Output | 56ms | 45ms |
| **Total** | **~216ms** | **~172ms** |

**Benchmark results** (iterate+field, large file):

| Tool | Before Rc | After Rc |
|------|-----------|----------|
| qj | 256ms | **157ms** |
| jaq | 172ms | 169ms |
| jq | 398ms | 390ms |
| qj vs jaq | **0.67x** (jaq wins) | **1.08x** (qj wins) |

**Key outcome:** qj now beats jaq on iterate+field — was 1.5x slower,
now 1.08x faster. Eval dropped from 33ms to 5ms (6.6x improvement).
Total improved from 256ms to 157ms (1.6x).

### Step 3: Tier 0 passthrough — identity compact fast path

**Status: COMPLETE.**

Identity compact (`. -c`) now calls `simdjson::minify()` directly on the
raw input bytes, bypassing DOM parse, Value tree, eval, and output
serialization entirely. This is a pre-check in `main.rs` — if the
filter is `Filter::Identity` and the output mode is compact, write the
minified bytes + newline and skip the entire Value pipeline.

**Implementation:**
- `bridge.cpp`: Added `qj_minify()` (wraps `simdjson::minify()`) and
  `qj_minify_free()`.
- `bridge.rs`: Added FFI declarations + safe `pub fn minify()` wrapper.
- `filter/mod.rs`: Added `PassthroughPath` enum + `passthrough_path()`
  to detect eligible filters (currently only `Filter::Identity`).
- `main.rs`: Pre-check before `process_padded()` — identity + compact →
  minify fast path. Works for both file and stdin inputs. Added
  `minify_timed()` for `--debug-timing` support.
- 170 tests, all passing (pre-field-passthrough count).

**Profiling** (large_twitter.json, 49MB, `--debug-timing`):

| Phase | Before (Step 2) | After (Step 3) |
|-------|-----------------|----------------|
| Read | 12ms | 5ms |
| Parse + Eval + Output | 160ms | — (skipped) |
| Minify | — | 10ms |
| Write | — | 1ms |
| **Total** | **172ms** | **16ms** |

**Benchmark results** (Apple Silicon, hyperfine --warmup 3, 49MB file):

| Tool | Time | qj speedup |
|------|------|------------|
| qj `-c .` | **18.2ms** | — |
| jaq `-c .` | 253ms | **13.9x** |
| jq `-c .` | 1,157ms | **63.5x** |

**Verification:** Output byte-identical to `jq -c .` on twitter.json
(631KB), large_twitter.json (49MB), and via stdin.

**What this does NOT include:**
- `Value::RawBytes` variant — not needed for identity compact (we bypass
  the Value pipeline entirely). Useful later for field passthrough.
- Field passthrough (`.field` + `-c`) — future step, needs DOM parse +
  field extraction + raw serialization via `to_json_string()`.
- Any changes to the evaluator or output formatter.

**Success criteria:**

| Criterion | Target | Actual | Status |
|-----------|--------|--------|--------|
| Output byte-identical to jq `-c` | Yes | Yes | ✓ |
| Identity compact >=10x faster than jq | >=10x | **63.5x** | ✓ |
| Identity compact >=10x faster than jaq | >=10x | **13.9x** | ✓ |

### Step 3b: Field compact passthrough — `.field` + `-c` fast path

**Status: COMPLETE.**

Field compact (`.field` + `-c`, `.a.b.c` + `-c`) now uses a dedicated
C++ FFI function that DOM parses, navigates to the target field via
`at_key()`, and serializes just that sub-tree via `simdjson::to_string()`.
This bypasses Value construction, eval, and Rust output serialization.

**Implementation:**
- `bridge.cpp`: Added `qj_dom_find_field_raw()` — DOM parse + nested
  field navigation + `to_string()` serialization. Handles missing fields
  (returns `"null"`) and non-object inputs (returns `"null"`).
- `bridge.rs`: Added FFI declaration + safe `pub fn dom_find_field_raw()`
  wrapper. Reuses `qj_minify_free()` for deallocation.
- `filter/mod.rs`: Extended `PassthroughPath` with `Field(Vec<String>)`.
  Added `collect_field_chain()` to detect `.a.b.c` pipe chains.
- `main.rs`: Added field match arm in passthrough pre-check for both
  file and stdin paths. Added `field_raw_timed()` for `--debug-timing`.
- 217 tests (146 unit + 56 e2e + 15 FFI), all passing.

**Profiling** (large_twitter.json, 49MB, `--debug-timing`):

| Phase | Time | % of total |
|-------|------|------------|
| Read | 4ms | 6% |
| DOM parse + find + to_string | 67ms | 92% |
| Write | 1ms | 2% |
| **Total** | **72ms** | **676 MB/s** |

**Benchmark results** (Apple Silicon, hyperfine --warmup 3, 49MB file):

| Tool | Time | qj speedup |
|------|------|------------|
| qj `-c .statuses` | **74ms** | — |
| jaq `-c .statuses` | 246ms | **3.3x** |
| jq `-c .statuses` | 1,132ms | **15.3x** |

Small file (twitter.json, 631KB):

| Tool | Time | qj speedup |
|------|------|------------|
| qj `-c .statuses` | **2.4ms** | — |
| jaq `-c .statuses` | 6.4ms | **2.7x** |
| jq `-c .statuses` | 16.7ms | **6.9x** |

**Before vs after** (49MB file, `-c .statuses`):

| | Before | After | Improvement |
|---|--------|-------|-------------|
| qj | 261ms | **74ms** | **3.5x** |
| qj vs jaq | 1.0x (tied) | **3.3x** |
| qj vs jq | 4.5x | **15.3x** |

**Verification:** Output byte-identical to `jq -c .statuses` on both
twitter.json and large_twitter.json.

**Note:** The 74ms is dominated by DOM parse (~65ms). The `to_string()`
serialization of the sub-tree is very fast. The original plan estimated
25-30ms, but that underestimated the DOM parse cost on 49MB. The minify
passthrough (18ms) is faster because it skips DOM entirely. Further
improvement would require On-Demand parsing or a sub-tree minify approach.

**Updated performance table** (49MB large_twitter.json):

| Filter | qj | jq | jaq | qj vs jq | qj vs jaq |
|--------|----|----|-----|----------|-----------|
| `-c '.'` | **18ms** | 1,157ms | 253ms | 63x | 14x |
| `-c '.statuses'` | **74ms** | 1,132ms | 246ms | 15x | 3.3x |

### Step 3c: Length/keys passthrough — `.field | length`, `.field | keys`

**Status: COMPLETE.**

`.field | length` and `.field | keys` (plus bare `length` / `keys`) now
use dedicated C++ functions that DOM parse, navigate to the target field,
and compute length/keys directly — bypassing Value construction, eval,
and Rust output serialization.

**Implementation:**
- `bridge.cpp`: Factored out `navigate_fields()` shared helper (dedup
  from `qj_dom_find_field_raw`). Added `json_escape()` for key
  serialization. New `qj_dom_field_length()` (array/object→size,
  string→byte length, null→0, other→fallback signal) and
  `qj_dom_field_keys()` (object→sorted keys, array→indices, other→fallback).
- `bridge.rs`: FFI declarations + safe `dom_field_length()` /
  `dom_field_keys()` wrappers returning `Result<Option<Vec<u8>>>`.
  `None` = unsupported type (caller falls back to normal pipeline).
- `filter/mod.rs`: Extended `PassthroughPath` with `FieldLength(Vec<String>)`
  and `FieldKeys(Vec<String>)`. Added `requires_compact()` method — returns
  `false` for length/keys (scalar/array output is mode-independent).
  Detection via `decompose_field_builtin()` for `Pipe(field_chain, Builtin)`
  and bare `Builtin("length"|"keys", [])`.
- `main.rs`: Relaxed passthrough check to `filter(|p| !p.requires_compact() || cli.compact)`.
  New match arms for both stdin and file paths with fallback on unsupported types.
  Added `field_length_timed()` for `--debug-timing`.
- 217 tests (146 unit + 56 e2e + 15 FFI), all passing.

**Profiling** (large_twitter.json, 49MB, `--debug-timing`):

`.statuses | length`:

| Phase | Time | % of total |
|-------|------|------------|
| Read | 4ms | 16% |
| DOM parse + navigate + length | 23ms | 84% |
| Write | 0ms | 0% |
| **Total** | **28ms** | **1,765 MB/s** |

**Benchmark results** (Apple Silicon, hyperfine --warmup 3, 49MB file):

| Filter | qj | jq | jaq | qj vs jq | qj vs jaq |
|--------|----|----|-----|----------|-----------|
| `.statuses \| length` | **33ms** | 398ms | 167ms | **12.2x** | **5.1x** |
| `-c '.statuses \| keys'` | **31ms** | 393ms | 165ms | **12.6x** | **5.3x** |
| `length` (bare) | **32ms** | 391ms | 166ms | **12.3x** | **5.2x** |

Small file (twitter.json, 631KB):

| Filter | qj | jq | jaq | qj vs jq | qj vs jaq |
|--------|----|----|-----|----------|-----------|
| `.statuses \| length` | **2.0ms** | 9.3ms | 5.3ms | **4.6x** | **2.6x** |

**Before vs after** (49MB file, `.statuses | length`):

| | Before (Step 1) | After (Step 3c) | Improvement |
|---|-----------------|-----------------|-------------|
| qj | 208ms | **33ms** | **6.3x** |
| qj vs jaq | 0.79x (jaq wins) | **5.1x** |
| qj vs jq | 1.9x | **12.2x** |

**Verification:** Output byte-identical to jq for `.statuses | length`
and `.statuses | keys` on both twitter.json and large_twitter.json.

**Updated cumulative performance table** (49MB large_twitter.json):

| Filter | qj | jq | jaq | qj vs jq | qj vs jaq |
|--------|----|----|-----|----------|-----------|
| `-c '.'` | **18ms** | 1,157ms | 253ms | 63x | 14x |
| `-c '.statuses'` | **74ms** | 1,132ms | 246ms | 15x | 3.3x |
| `.statuses \| length` | **33ms** | 398ms | 167ms | 12x | 5.1x |
| `-c '.statuses \| keys'` | **31ms** | 393ms | 165ms | 13x | 5.3x |

### Step 4: NDJSON end-to-end benchmarks + parallel NDJSON

The 82MB NDJSON file from Phase 0 (`1m.ndjson`) already exists but
hasn't been benchmarked end-to-end. This is qj's strongest positioning
— SIMD parse + parallelism on independent lines.

**Phase 1: benchmark single-threaded NDJSON.** Add 1m.ndjson to
`benches/run_bench.sh`. Measure qj vs jq vs jaq vs gojq. This gives
the single-threaded baseline before parallelism.

**Phase 2: parallel NDJSON.** Design from Phase 2 section holds:
chunk-split at newline boundaries, per-thread simdjson parser, ordered
merge of output buffers.

**Expected impact:** ~8x additional speedup on 8+ cores. Combined with
SIMD parsing + Rc eval + passthrough: 50-100x over jq on large NDJSON.

### Step 5: Missing core filters

The most impactful gaps for real-world jq scripts. Not on the critical
path for performance — do this when the speed story is solid.

| Priority | Feature | Why it matters |
|----------|---------|----------------|
| High | `--slurp` / `-s` | Very common flag, blocks entire categories of usage |
| High | `--arg name val` / `--argjson` | CLI variable injection — any script using `$TOKEN` etc. can't use qj without this |
| High | `.[2:5]` (array slicing) | Common, simple to implement (index works, range doesn't) |
| Medium | `. as $x \| ...` (variable binding) | Required for complex jq idioms, fewer users |
| Medium | `reduce .[] as $x (init; update)` | Aggregation — needs variable binding first |
| Medium | `with_entries(f)` | Desugars to `to_entries \| map(f) \| from_entries` |
| Medium | `@base64` / `@csv` / `@tsv` / `@json` / `@uri` | Format strings |
| Medium | `test()` / `match()` / `capture()` | Regex (add `regex` crate) |
| Medium | `--raw-input` / `-R` | Treat input lines as strings |
| Medium | `def name: body;` | User-defined functions |

### Deferred: On-Demand fast path (Phase 1.5)

Skip DOM construction entirely for simple path-only filters. Navigate
the SIMD structural index directly at ~7 GB/s.

**When to do this:** Only if profiling after Steps 2-3 shows DOM
construction is still a bottleneck. With Rc + passthrough, simple
filters are already fast. On-Demand would shave a few more
milliseconds off the DOM parse step — diminishing returns.

**Eligible filters:** `.field`, `.a.b.c`, `.[n]` — pure path
navigation with no branching or transformation. Falls back to DOM
for anything requiring random access.

---

## Phase 2: Parallel NDJSON

**Goal:** `cat huge.jsonl | qj '.field'` uses all cores and achieves
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

**Success criterion:** `qj '.field' 1gb.jsonl` is ≥5x faster than
`jq '.field' 1gb.jsonl` on an 8+ core machine. ≥30x faster with
10 cores + SIMD combined.

### Phase 2 results (Apple Silicon M-series, 2025-02)

**Status: COMPLETE.**

Implementation uses rayon work-stealing thread pool (not hand-rolled).
Each ~1MB chunk is processed independently: memchr line splitting →
simdjson DOM parse per line → eval filter → format to per-chunk
`Vec<u8>`. Chunks collected in order via `par_iter()` and concatenated.

NDJSON auto-detected via heuristic (first line is complete JSON value,
second line starts with `{`/`[`), or forced with `--jsonl` flag. Both
stdin and file paths supported.

Thread safety: `Filter` contains `Rc`-based `Value` literals (making it
`!Send + !Sync`). Solved with `SharedFilter` raw-pointer wrapper +
`filter_is_parallel_safe()` runtime check that falls back to sequential
processing for filters containing `Rc`-based array/object literals.

**Benchmarks (1M-line NDJSON, `.name` field extraction):**

| Tool | Wall time | User time | Notes |
|------|-----------|-----------|-------|
| qj   | 120ms     | 1,327ms   | Multi-core (rayon) |
| jq   | 1,230ms   | 1,210ms   | Single-threaded |
| jaq  | 670ms     | 650ms     | Single-threaded |

~10x faster than jq, ~5.6x faster than jaq. User time > wall time
confirms rayon is using multiple cores effectively.

**Key files:**
- `src/parallel/ndjson.rs` — detection, chunking, parallel processing
- `src/parallel/mod.rs` — module root
- `src/main.rs` — `--jsonl` flag, NDJSON detection in stdin + file paths
- `tests/ndjson.rs` — 18 integration tests
- `benches/bench.sh` — NDJSON benchmark sections

**Deviations from plan:**
- Used DOM parse per line (not `iterate_many`) for the general case.
  `iterate_many` FFI only supports counting and single-field extraction;
  the general filter path needs full Value trees. Parallelism comes from
  processing chunks simultaneously, not simdjson's batch mode.
- Stdin path reads all input then processes in parallel (not true
  streaming dispatch). Still gets full threading speedup.

---

## Phase 3: Tier 2+3 filter support

**Goal:** Cover the "weekly ten" and "monthly rest" filters. At this
point qj is usable for most real-world tasks.

**Status: MOSTLY COMPLETE.** Phase 1 already implemented 14 of the 15
Tier 2 filters and many Tier 3 filters. See Phase 1 results for the
full list.

### Already implemented (in Phase 1)

1. ~~**`map()`**~~ ✓
2. ~~**`length`**~~ ✓
3. ~~**`keys` / `values`**~~ ✓ (including `keys_unsorted`)
4. ~~**`sort_by()` / `group_by()` / `unique_by()`**~~ ✓
5. ~~**`first` / `last`**~~ ✓
6. **Array slicing** — `.[n]` and `.[-n]` work, `.[2:5]` range slicing NOT YET
7. ~~**`+` operator**~~ ✓ (numbers, strings, arrays, objects)
8. ~~**`type`**~~ ✓
9. ~~**`add`**~~ ✓
10. ~~**`not` / `and` / `or`**~~ ✓
11. ~~**`has()`**~~ ✓ (`in()` not yet)
12. ~~**`if-then-else`**~~ ✓
13. ~~**`//` (alternative)**~~ ✓
14. ~~**`?` (try)**~~ ✓
15. **Variable binding** — `. as $x | ...` NOT YET

Also already done from Tier 3: `to_entries`, `from_entries`, `sort`,
`unique`, `flatten`, `reverse`, `min`, `max`, `min_by()`, `max_by()`,
`del()`, `contains()`, `split()`, `join()`, `ascii_downcase`,
`ascii_upcase`, `ltrimstr()`, `rtrimstr()`, `startswith()`,
`endswith()`, `tostring`, `tonumber`, `any`, `all`, `empty`, `..`
(recurse), string interpolation, `not`.

### Remaining to implement

High priority (unblock real-world scripts):
1. **`--slurp` / `-s`** — Very common CLI flag, blocks entire categories of usage
2. **`--arg` / `--argjson`** — CLI variable injection — scripts using `$TOKEN` etc. can't use qj without this
3. **Array slicing** — `.[2:5]`, `.[:-1]` — common, simple to implement

Medium priority (complex jq idioms):
4. **`. as $x \| ...`** (variable binding) — required for advanced jq patterns
5. **`reduce`** — `reduce .[] as $x (0; . + $x)` — needs variable binding first
6. **`with_entries`** — desugar to `to_entries | map(f) | from_entries`
7. **`in()`** — key membership (inverse of `has`)

Medium priority:
8. **`@base64` / `@csv` / `@tsv` / `@json` / `@uri`** — format strings
9. **`test()` / `match()` / `capture()` / `scan()`** — regex
10. **`gsub()` / `sub()`** — regex replacement
11. **`def`** — user function definitions
12. **`walk()`** — recursive transform
13. **`limit()` / `until()` / `while()` / `repeat()`** — loop constructs
14. **`range()`** — sequence generation
15. **`--raw-input` / `-R`** — treat input lines as strings
16. **`input` / `inputs`** — multi-input processing

Low priority:
17. **`foreach`** — streaming fold
18. **`try-catch`** — error handling with catch clause
19. **`path()` / `getpath()` / `setpath()` / `delpaths()`** — path ops
20. **`env` / `$ENV`** — environment variables
21. **`--slurpfile` / `-f`** — CLI features
22. **`debug`** — debug output to stderr
23. **Date/time** — `now`, `todate`, `strftime`, etc.
24. **Math** — `floor`, `ceil`, `round`, `sqrt`, etc.
25. **`tojson` / `fromjson`** — JSON encode/decode

### Conformance testing

Build a test harness that:
1. Runs every filter expression through both `qj` and `jq`
2. Compares output byte-for-byte
3. Tests against a corpus of real-world JSON (GitHub API responses,
   npm package.json, AWS CloudTrail logs, Kubernetes manifests)

Any difference is a bug. Import jaq's test suite as additional coverage.

**Success criterion:** All Tier 2+3 filters produce identical output to
jq. Performance regression ≤5% vs Phase 1 for Tier 1 filters.

---

## Phase 4: Streaming large single-file JSON

**Goal:** `qj '.items[]' 5gb_array.json` works without loading 5GB
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

**Success criterion:** `qj '.[]' 1gb_array.json` completes in <10s
with <100MB RSS. `jq '.[]' 1gb_array.json` uses >5GB RSS.

---

## Phase 5: Remaining filters and polish

**Goal:** Cover the remaining filter gaps. Full CLI compatibility
with jq for common flags. Optimize filter evaluation if needed.

**Note:** Many Tier 3 filters originally planned for Phase 5 were
implemented early in Phase 1. See Phase 3 section for the remaining
list. This phase now focuses on the long-tail features.

### Filters still to add (not covered by Phase 3)

- `reduce` / `foreach` — streaming fold operations (Phase 3 high priority)
- `try-catch` — error handling with catch clause
- `@csv` / `@tsv` / `@base64` / `@uri` / `@json` — format strings
- `test()` / `match()` / `capture()` / `scan()` / `splits()` — regex
- `gsub` / `sub` — regex replacement
- `tojson` / `fromjson` — JSON encode/decode within filters
- `env` / `$ENV` — environment variable access
- `input` / `inputs` — multi-input processing
- `--arg` / `--argjson` / `--slurpfile` — CLI variable injection
- `def` — user-defined functions
- `walk()` — recursive transform
- `path()` / `getpath()` / `setpath()` / `delpaths()` — path operations
- `debug` — debug output to stderr
- `limit()` / `until()` / `while()` / `repeat()` — loop constructs
- `range()` — sequence generation
- `error` / `halt` / `halt_error` — control flow
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

## Estimated performance targets (updated with Phase 0 actuals)

### Parsing throughput (Phase 0) — MEASURED

See "Phase 0 results" section above for full measured data. Summary:
simdjson On-Demand delivers 6,700-9,200 MB/s on Apple Silicon.
11-15x over serde_json, 41-286x over jq end-to-end, 24-90x over jaq.

Original estimates vs actuals:
- simdjson On-Demand: predicted 5,000-7,000 → **actual 6,700-9,200** MB/s
- serde_json: predicted 330-560 → **actual 245-810** MB/s (varies by file)
- jq end-to-end: predicted ~200-400 → **actual 24-64** MB/s (includes output)
- canada.json was predicted as worst-case 1,500 → **actual 6,748** (NEON handles floats better than expected)

### End-to-end targets (Phase 1+2, revised)

These account for output formatting overhead and measured iterate_many
throughput (~2,800 MB/s per thread, not ~8,000).

| Workload | jq | qj (1 thread) | qj (10 threads) | Speedup |
|----------|-----|---------------|-----------------|---------|
| `'.field' 100mb.jsonl` | ~1.5s | ~0.1s | ~0.02s | **75x** |
| `'.field' 1gb.jsonl` | ~15s | ~1s | ~0.15s | **100x** |
| `'.' 100mb.json` (pretty-print) | ~3s | ~0.3s | — | **10x** |
| `'.[]' 5gb_array.json` | OOM | — | ~5s, <100MB RSS | **∞** |

Note: jq end-to-end is much slower than "parse throughput" estimates
because it includes output formatting (pretty-print identity = read +
parse + serialize + write). The single-threaded qj advantage is ~10-15x
for end-to-end workloads. Parallelism on NDJSON adds another ~8x.

### Filter evaluation (Phase 1)

| Benchmark | jq | jaq | qj target |
|-----------|-----|------|-----------|
| Startup (empty filter) | ~5ms | ~1ms | ≤1ms |
| Simple field access | baseline | ~2x jq | ~2x jq (match jaq) |
| map/select | baseline | ~2-3x jq | ~2-3x jq (match jaq) |
| reduce/fold | baseline | ~2-5x jq | ~2-5x jq (match jaq) |

Filter evaluation speed should match jaq — not beat it. Both are Rust,
both use native integers, both can use efficient memory management. jaq
has had years of optimization; claiming we'd beat it on eval is not
credible. The real win is in parsing and parallelism, not the evaluator.

### Honest performance comparison: qj vs jaq vs jq (revised)

| Scenario | vs jq | vs jaq | Why |
|----------|-------|--------|-----|
| Simple filter, large file (1 thread) | 10-20x | 3-8x | SIMD DOM parsing, fast output |
| Simple filter, large NDJSON (10 threads) | 50-100x | 20-40x | SIMD + parallelism |
| Complex filter, large file | 5-10x | 2-4x | SIMD parsing, similar eval |
| Complex filter, small file | 2-3x | ~1x | Eval-dominated, similar speed |
| Small file, simple filter | 2-5x | ~1x | Startup-dominated |

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
eval-dominated workloads (complex filters, small inputs), qj and jaq
are roughly equivalent. The performance story is parsing + parallelism.

**Primary audience:** Developers processing large JSON. Log pipelines,
NDJSON datasets, large API dumps. Specifically: anyone who has added
`parallel | jq` to a pipeline, hit OOM on a large JSON file, or waited
more than a second for jq to finish.

**Competitive positioning vs jaq:** jaq is "jq but correct and clean."
qj is "jq but fast on large data." Different niches. jaq is better for
people who want a drop-in jq replacement with maximum compatibility.
qj is better for people processing >10MB of JSON at a time. They
complement rather than compete — though if qj matches jaq on eval speed
AND adds SIMD parsing + parallelism, the "why not just use qj" argument
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

**Mitigation:** Implement fast-path output for common cases. For `qj -c`
(compact output), the input bytes can often be copied directly to output
without re-serialization (if the filter is identity or simple field
access). For pretty-print, use SIMD-accelerated string escaping.

### Risk: People just use jaq

jaq already exists, is well-maintained, and handles most use cases well
enough. qj's niche (large files, parallelism) may be too narrow.

**Mitigation:** The combination of SIMD On-Demand parsing + parallel
NDJSON + streaming large files is unique — no other jq-like tool does
all three. If qj also matches jaq's eval speed, the only advantage jaq
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

---

## Launch readiness

Practical requirements and risks for a successful public release.

### Distribution

Homebrew formula and prebuilt binaries from day one. If users can't
install in under 30 seconds, adoption stalls. Needs:
- Homebrew tap with formula (compile from source via `cc` crate)
- GitHub Releases with prebuilt binaries for macOS ARM, macOS x86,
  Linux x86, Linux ARM
- `cargo install qj` support (already works, but slow — C++ compile)

### First impressions and compatibility

At ~60% jq compatibility, someone will try `--slurp`, `$var`, or
`.[2:5]` in the first 30 seconds and it will fail. This is the biggest
adoption risk. Requirements:

- **≥80% jq compatibility before release.** At minimum: `--slurp`,
  `--arg`/`--argjson`, array slicing, variable binding, `reduce`.
- **Clear error messages for unsupported features.** "qj does not yet
  support `def`. See https://github.com/.../issues/..." is much better
  than a cryptic parse error.
- **Document incompatibilities explicitly** rather than pretending to be
  a drop-in replacement. A `COMPAT.md` listing what works and what
  doesn't saves users time and sets honest expectations.

### The "just use jaq" question

This will be the first question from anyone who knows the space. Have a
clear, honest answer: qj wins on large data (SIMD parsing + automatic
parallelism), jaq wins on compatibility (~90% vs ~80%). They're
complementary, not competing. qj is for people processing >10MB of
JSON; jaq is for people who want maximum jq compatibility.

If qj reaches ~85% compat AND has parallel NDJSON, the argument shifts:
"why not just use qj" becomes strong for anyone with large-data
workloads, and qj is no worse than jaq for small-data use.

### Real-world demo

Microbenchmarks alone aren't convincing. Need at least one concrete
real-world story alongside the numbers:
- "1GB CloudTrail log: jq takes 2 minutes, qj takes 2 seconds"
- "NDJSON pipeline: replaced `parallel -j8 | jq` with `qj`, same
  result, zero configuration"
- Show a real log processing or data pipeline task, not synthetic data

### jq compatibility long tail

The first 80% of jq's filter language is straightforward. 80→90% is
where subtle edge cases live (generator semantics, backtracking, error
propagation). The last 10% (full module system, `label`/`break`, TCO)
requires reimplementing jq's entire VM and is not worth the effort.

**Strategy:** Target 80-85% compatibility. Document what's missing.
Accept that some complex jq scripts won't work and be upfront about it.
The target user has simple-to-moderate filters on large data, not
complex 50-line jq programs.

### Maintenance burden

jq's language surface area is large. The risk is building 80% compat,
launching, then facing a steady stream of "qj doesn't support X" issues
that slowly consume all development time.

**Mitigation:** Be explicit about scope. Core filters are maintained;
niche features (module system, `label`/`break`, `$__loc__`) are
documented as out of scope. Focus ongoing effort on performance
(parallelism, streaming) rather than chasing 100% compat.

---

## Small-file performance optimization

qj already beats jaq ~2x on small files (2ms vs 5ms on 631KB
twitter.json). These optimizations target widening that to 3-4x, which
matters for the "why not just use qj for everything" argument — making
qj dominant at all file sizes, not just large ones.

### Where time goes (631KB twitter.json, ~3ms total)

| Component | Time | % | Opportunity |
|-----------|------|---|-------------|
| File I/O | 0.2ms | 7% | Optimal |
| simdjson DOM parse | 0.8ms | 27% | Optimal (C++ baseline) |
| Flat buffer serialize (C++) | 0.3ms | 10% | Avoidable for some filters |
| Flat buffer deserialize (Rust) | 0.6ms | 20% | String alloc heavy |
| Eval + output | 0.5ms | 17% | Cloning overhead |
| Process startup + clap | 0.6ms | 19% | Fixed cost |

The flat buffer roundtrip (serialize in C++ → deserialize in Rust)
takes ~0.9ms — 30% of total time. String allocation during
deserialization is the single largest source of overhead after simdjson
parsing itself.

### Optimization ideas (priority order)

**1. Compact string representation (SmallString)**

Object keys and short strings dominate allocation count. A small-string
optimization (inline ≤24 bytes on the stack, heap-allocate only longer
strings) would eliminate most heap allocations during Value construction.
Most JSON keys ("name", "id", "status", "user") are well under 24
bytes. The `compact_str` or `smol_str` crates provide this.

Expected impact: ~0.2-0.3ms saved on string-heavy JSON.

**2. On-Demand evaluator for `.[] | .field` patterns**

This is the most common non-passthrough filter. Currently builds the
full DOM→Value tree, then iterates. An On-Demand path could:
- Use simdjson On-Demand to iterate array elements
- For each element, navigate directly to the target field
- Materialize only the accessed value, not the entire element
- Skip building the full Value tree entirely

This is essentially the Phase 1.5 On-Demand fast path, but scoped to
the highest-value pattern. Would bring `.[] | .field` close to
passthrough speed.

Expected impact: 2-3x faster for iterate+field patterns.

**3. Direct DOM→Value without flat buffer**

The two-pass approach (C++ → flat tokens → Rust Values) exists to
minimize FFI crossings. But for small files, the serialization +
deserialization overhead (~0.9ms) is 30% of total time. A tighter FFI
that walks the simdjson DOM and constructs Rust Values directly (more
FFI calls, but no intermediate buffer) could be faster. Trade FFI call
count for elimination of the intermediate copy.

Expected impact: ~0.3-0.5ms saved.

**4. Arena allocation for Values**

Instead of individual heap allocations per Value, use a bump allocator
(`bumpalo` crate) for the entire Value tree of a single document. All
Values are freed together after output. Eliminates per-allocation
overhead and improves cache locality.

Expected impact: ~0.1-0.2ms saved, better cache behavior.

**5. String interning for object keys**

JSON documents reuse the same key names across array elements ("name",
"id", "type" repeated in every object). Intern these during DOM→Value
conversion so repeated keys share a single allocation. A simple HashMap
during decoding would deduplicate ~80% of key strings in typical JSON.

Expected impact: ~0.1-0.2ms saved on key-heavy JSON.

### Priority assessment

Items 1-2 have the best effort-to-impact ratio. SmallString is a
crate swap + Value type change. On-Demand iterate+field is more
complex but addresses the single most common non-passthrough pattern.
Items 3-5 are diminishing returns and should only be pursued if
profiling shows they matter after 1-2 are implemented.

**None of these block launch.** qj is already faster than jaq on small
files. These are post-launch optimizations that widen the gap.
