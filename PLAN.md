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

| Tool | Parsing (measured) | E2E (measured) | Parallel | SIMD | On-Demand | Streaming | jq compat | Platform |
|------|-------------------|----------------|----------|------|-----------|-----------|-----------|----------|
| jq 1.7 | 23-62 MB/s e2e | baseline | No | No | No | Yes (`--stream`)* | 100% | All |
| jaq 2.3 | 93-187 MB/s e2e | 1.3-2x jq | No | No | No | No | ~90% | All |
| gojq 0.12 | 47-122 MB/s e2e | 0.8-2.5x jq | No | No | No | No | ~85% | All |
| **jx 0.1** | **7-9 GB/s parse** | **2-4x jq** | **Planned** | **Yes (NEON/AVX2)** | **Planned** | **Planned** | **~60%** | **macOS/Linux** |

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
├── bench/
│   ├── data/                  # twitter.json, canada.json, citm_catalog.json, NDJSON
│   ├── gen_ndjson.rs          # NDJSON test data generator
│   └── parse_throughput.rs    # Criterion benchmarks (simdjson vs serde)
├── tests/
│   ├── simdjson_ffi.rs        # simdjson FFI integration tests (15 tests)
│   └── e2e.rs                 # End-to-end CLI tests (29 tests)
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

**Goal:** `jx '.field' file.json` works end-to-end, producing identical
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
   `jx '.name' file > /dev/null` vs `jq '.name' file > /dev/null`.
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

For filters that return original sub-objects (`.`, `.field`, `.[0]`),
simdjson's `raw_json()` provides a pointer+length directly into the
input buffer. Write to stdout via `write_all` — no escaping, no
formatting, no allocation. This is `memcpy` speed (~GB/s), preserving
the SIMD parse throughput story for compact output (`-c`).

This is the "golden path" for identity and simple extraction filters.
Requires `raw_json()` FFI support and filter analysis to detect
passthrough-eligible expressions. Only works for compact output — pretty-
print requires re-formatting.

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

**Success criterion:** `jx '.field' file.json` produces identical output
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

**Status: COMPLETE — `jx '.field' file.json` works end-to-end.**

All Phase 1 slices implemented: Value type, output formatter (Tier 1
direct-to-buffer with itoa/ryu, Tier 2 pretty-print), simdjson DOM bridge
(flat token buffer protocol), filter lexer, recursive descent parser,
generator-based evaluator, CLI with clap. 146 tests (117 unit + 29 e2e).

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

| Benchmark | jx | jq | jaq | jx vs jq | jx vs jaq |
|-----------|-----|-----|------|----------|-----------|
| `.statuses[0].user.screen_name` twitter.json (631KB) | 4.0ms | 8.9ms | 4.9ms | **2.25x** | 1.25x |
| `-c '.'` canada.json (2.2MB) | 10.4ms | 40.7ms | 13.2ms | **3.9x** | 1.27x |
| `-c '.performances \| keys \| length'` citm_catalog.json (1.7MB) | 5.3ms | 18.7ms | 6.8ms | **3.5x** | 1.29x |

Output verified byte-identical to jq on all test files:
- `diff <(jx -c '.' twitter.json) <(jq -c '.' twitter.json)` → match
- `diff <(jx -c '.statuses[] | .user.screen_name' twitter.json) <(jq -c ...)` → match
- `diff <(jx -c '.statuses[] | select(.retweet_count > 0) | {user: .user.screen_name, retweets: .retweet_count}' twitter.json) <(jq -c ...)` → match

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
| `src/simdjson/bridge.cpp` | Extended with `jx_dom_to_flat()` (flat token buffer) |
| `src/simdjson/bridge.rs` | Extended with `dom_parse_to_value()` |
| `src/main.rs` | CLI (clap derive), stdin/file input, filter → eval → output |
| `tests/e2e.rs` | 29 end-to-end tests |

---

## Next steps (proposed)

Based on Phase 1 results, here's the priority order for closing the
performance gap and making jx genuinely useful:

### Priority 1: Large-file benchmarks and mmap (immediate)

Generate larger test files (100MB+ single JSON, 100MB+ NDJSON) and
benchmark. The current small-file benchmarks (0.6-2.2MB) hide jx's
parsing advantage. On large files, simdjson's 7+ GB/s should dominate
the end-to-end time, showing the real 5-10x advantage over jq.

Add mmap-based file reading (via `libc::mmap`) to avoid the
`std::fs::read` + `pad_buffer` copy. For large files, mmap provides
zero-copy access and lets the OS page in data on demand.

### Priority 2: Missing core filters (variable binding, reduce, slurp)

The most impactful missing features for real-world usage:

1. **Variable binding** — `. as $x | ...`, needed for many jq idioms
2. **`--slurp` / `-s`** — read all inputs into array, very common flag
3. **`reduce`** — `reduce .[] as $x (0; . + $x)`, needed for aggregation
4. **`--arg` / `--argjson`** — CLI variable injection, critical for scripts
5. **Array slicing** — `.[2:5]`, `.[:-1]`
6. **`with_entries`** — desugars to `to_entries | map(f) | from_entries`
7. **`@base64` / `@csv` / `@tsv` / `@json` / `@uri`** — format strings
8. **`test()` / `match()`** — regex support (use `regex` crate)
9. **`--raw-input` / `-R`** — treat input lines as strings

### Priority 3: Parallel NDJSON (Phase 2)

This is the biggest performance multiplier. Current plan holds:
chunk-split NDJSON at newline boundaries, distribute chunks to thread
pool, each thread has its own simdjson parser, ordered merge of output.
Expected ~8x additional speedup on multi-core, for a combined 20-40x
over jq on large NDJSON.

### Priority 4: Output optimization (Tier 0 passthrough)

Add `raw_json()` FFI support so identity and simple extraction filters
can copy the original input bytes directly to stdout. This turns output
from the bottleneck into effectively free for `-c` mode on passthrough-
eligible filters. Expected to close the gap to the ≥5x target on
single-file benchmarks.

### Priority 5: On-Demand fast path (Phase 1.5)

For simple path-only filters (`.field`, `.a.b.c`, `.[0]`), skip DOM
construction entirely and navigate the SIMD structural index directly.
This provides 7+ GB/s for the most common use case. Requires filter
analysis to detect eligible expressions and fallback to DOM for anything
that needs random access.

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

## Phase 3: Tier 2+3 filter support

**Goal:** Cover the "weekly ten" and "monthly rest" filters. At this
point jx is usable for most real-world tasks.

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

High priority (needed for common jq scripts):
1. **Variable binding** — `. as $x | ...`
2. **Array slicing** — `.[2:5]`, `.[:-1]`
3. **`reduce`** — `reduce .[] as $x (0; . + $x)`
4. **`with_entries`** — desugar to `to_entries | map(f) | from_entries`
5. **`in()`** — key membership (inverse of `has`)
6. **`--slurp` / `-s`** — CLI flag
7. **`--arg` / `--argjson`** — CLI variable injection

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
1. Runs every filter expression through both `jx` and `jq`
2. Compares output byte-for-byte
3. Tests against a corpus of real-world JSON (GitHub API responses,
   npm package.json, AWS CloudTrail logs, Kubernetes manifests)

Any difference is a bug. Import jaq's test suite as additional coverage.

**Success criterion:** All Tier 2+3 filters produce identical output to
jq. Performance regression ≤5% vs Phase 1 for Tier 1 filters.

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

| Workload | jq | jx (1 thread) | jx (10 threads) | Speedup |
|----------|-----|---------------|-----------------|---------|
| `'.field' 100mb.jsonl` | ~1.5s | ~0.1s | ~0.02s | **75x** |
| `'.field' 1gb.jsonl` | ~15s | ~1s | ~0.15s | **100x** |
| `'.' 100mb.json` (pretty-print) | ~3s | ~0.3s | — | **10x** |
| `'.[]' 5gb_array.json` | OOM | — | ~5s, <100MB RSS | **∞** |

Note: jq end-to-end is much slower than "parse throughput" estimates
because it includes output formatting (pretty-print identity = read +
parse + serialize + write). The single-threaded jx advantage is ~10-15x
for end-to-end workloads. Parallelism on NDJSON adds another ~8x.

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

### Honest performance comparison: jx vs jaq vs jq (revised)

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
