# Speedup Ideas

Performance optimization history and roadmap. Benchmarks on Apple M4 Pro (10P+4E) unless noted.

## What was done

### NDJSON fast paths

These bypass the Rust Value tree entirely for common NDJSON patterns. Each line is independent, so we detect the pattern once and apply a specialized C++ extraction path per line.

- **P-core-only threading** — Restrict Rayon to P-cores on Apple Silicon (`sysctl hw.perflevel0.logicalcpu`). E-cores added jitter without throughput benefit. Lower variance, modest wall-time improvement on large NDJSON. (`src/main.rs`)
- **mmap for file I/O** — `mmap(PROT_READ, MAP_PRIVATE)` + `MADV_SEQUENTIAL`. Avoids heap alloc + memcpy for large files. **~23% faster** on 1.1GB NDJSON, negligible on small files. Falls back to heap when file size is within 63 bytes of a page boundary. `QJ_NO_MMAP=1` env var. (`src/simdjson/types.rs`)
- **Field-chain fast path** — `.field` and `.field.nested.path`: extract raw JSON bytes via `dom_find_field_raw()` in C++, skip `decode_value()` entirely. **~40% faster** on 1.1GB NDJSON. (`src/parallel/ndjson.rs`)
- **`select(.field == literal)` fast path** — Extract field as raw bytes, compare against serialized literal. Match → output raw line (zero copy). No match → skip entirely. Supports `==`, `!=`, `>`, `<`, `>=`, `<=`. **~50% faster**. (`src/parallel/ndjson.rs`)
- **Multi-field extraction (batch C++)** — `{f1, f2: .f2.nested}` and `[.f1, .f2]`: new `jx_dom_find_fields_raw` C++ function parses once, extracts N field chains, returns length-prefixed buffer. First attempt (N separate FFI calls) was **+35% regression** on 3 fields and was reverted. Batch version: **-54%** on 3-field obj, **-38%** on 2-field arr. (`src/simdjson/bridge.cpp`, `src/parallel/ndjson.rs`)
- **`select` + extract combined** — `select(.f==lit) | .field` / `{...}` / `[...]`: predicate filters ~80% of lines, only matching lines pay extraction cost. **~35-42% faster**. (`src/parallel/ndjson.rs`)
- **`length`/`keys` fast path** — C++ bridge computes directly from simdjson DOM. **~45% faster**. (`src/parallel/ndjson.rs`)
- **DOM parser reuse** — Reusable `JxDomParser` handle persists simdjson's `dom::parser` across lines within each chunk (one per Rayon thread). Avoids repeated internal buffer allocation. **~40% faster**, biggest impact on multi-field extraction. (`src/simdjson/bridge.cpp`, `src/parallel/ndjson.rs`)
- **On-Demand raw field extraction** — Switched from simdjson DOM `to_string()` to On-Demand `raw_json()`. Zero-copy pointer into source bytes, preserves exact number representation (`1.5e10` stays `1.5e10`). **~26-31% faster** single-field, **~12%** 2-field, neutral 3-field. (`src/simdjson/bridge.cpp`)

### NDJSON evaluator floor

The fast paths above only help recognized patterns. The "floor" is the slowdown for arbitrary filters that fall back to the Rust evaluator.

- **FlatValue** — Zero-copy `FlatValue<'a>` view into the flat token buffer. Navigates objects/arrays without heap allocation; `to_value()` materializes only when needed. New `eval_flat()` handles Field, Pipe, ObjectConstruct, ArrayConstruct, Iterate, Select, Alternative, Try, Comma, Literal, and builtins (length, type, keys, not). Falls back to regular evaluator for unsupported filters. Worst-case filter went from **2.8x → 4.8x** vs jq. (`src/flat_value.rs`, `src/flat_eval.rs`, `src/parallel/ndjson.rs`)
- **Rc → Arc (parallelism fix)** — `Value::Array`/`Object` used `Rc<Vec<...>>` (not Send), so filters containing literals like `// []` forced single-threaded execution. Switched to `Arc`. The worst-case filter went from **4.8x → 26.5x** vs jq (parallelism now enabled). (`src/value.rs`, `src/filter/mod.rs`, 16 files total)

### Single-document speedups

- **Phase 1: FlatValue for single-doc** — Same FlatValue used in NDJSON fallback, applied to single-doc eval. Eval time 135ms → 1-7ms. Parse became the bottleneck. (`src/flat_eval.rs`)
- **Phase 2: DOM tape walk** — New `jx_dom_to_flat_via_tape()` uses DOM API's pre-indexed tape instead of On-Demand API for flat buffer construction. ~1.7x faster than On-Demand for this step. `map` went from **2.55x → 3.9x**, `reduce` from **1.65x → 2.15x** vs jq on 49MB JSON. Falls back to On-Demand for big integers beyond u64 range and `fromjson`. (`src/simdjson/bridge.cpp`)
- **Phase 4: DOM passthrough for map/iterate** — `map(.field)`, `.[] | .field`, `map({f1, f2})`, `.[] | {f1, f2}` (with optional prefix). New `jx_dom_array_map_field()` C++ function parses with DOM, iterates array, extracts per-element field chain. **~12x** vs jq on 49MB JSON. Requires `-c` (compact output). (`src/simdjson/bridge.cpp`)
- **Phase 5: Scalar builtin passthroughs** — `type` (first-byte inspection, no C++ needed), `has("key")` (new `jx_dom_field_has()`), `keys_unsorted` (added `sorted` param to existing `jx_dom_field_keys()`). Also added `Type` and `Has` NDJSON fast path variants. (`src/simdjson/bridge.cpp`, `src/parallel/ndjson.rs`)
- **Phase 6: Iterate + builtin passthroughs** — `map(length)`, `map(keys)`, `map(type)`, `map(has("f"))` and `.[]` equivalents. New `jx_dom_array_map_builtin()` C++ function with `int op` parameter. **~12x** vs jq. (`src/simdjson/bridge.cpp`)
- **Phase 7: Syntactic variant detection** — `[.[] | .field]` detected as `map(.field)`, `[.[] | {f1, f2}]` as `map({f1, f2})`, `[.[] | builtin]` as `map(builtin)`. Detection-only, no new C++. (`src/parallel/ndjson.rs`)

### Reverted / bad ideas

- **Rc\<str\> for Value strings** — Changed `Value::String(String)` to `Value::String(Rc<str>)`. Benchmarked neutral to slightly negative — Rc pointer indirection adds cache misses that offset O(1) clone savings. Most strings are constructed once and output once. Reverted.
- **Multi-field extraction via N separate FFI calls** — First attempt at multi-field fast path made N separate `dom_find_field_raw()` calls per line. Each re-parsed the entire JSON. **+35% regression** on 3 fields. Replaced by batch C++ extraction (see above).
- **Arena allocation (bumpalo)** — Investigated but skipped. Very high complexity (lifetime parameter on Value propagates everywhere). The fast-path strategy of avoiding the Value tree entirely proved strictly better. FlatValue achieves similar benefits without the refactor.

## Remaining to explore

- **Streaming NDJSON** — Currently entire file loaded via mmap before processing. Streaming fixed-size blocks (64MB) would enable >RAM files and reduce startup latency. Medium complexity: need to handle lines spanning block boundaries. (`src/parallel/ndjson.rs`, `src/main.rs`)
- **Per-thread output buffers** — Currently each Rayon chunk produces `Vec<u8>`, all collected then concatenated. Pre-allocated per-thread buffers with ordered flush to stdout would avoid the final concatenation and reduce peak memory. Low-moderate complexity. (`src/parallel/ndjson.rs`)
- **Lazy flat buffer (single-doc)** — Only flatten accessed subtrees instead of the whole document. Risk: per-value resolution cost via simdjson FFI (~5us each) makes it slower for materializing filters (map/reduce that touch all elements). Only helps selective filters.
- **Lazy Value (NDJSON fallback)** — Wrap simdjson DOM nodes in `LazyValue` that materializes children on access. Helps selective filters (touch few fields) but doesn't raise the floor for full-document filters. Medium effort, ~500-800 lines. Complementary to FlatValue.

## Won't do / Deferred

- **Full C++ evaluator** — The Rust evaluator is 3600 lines, 34 AST nodes, 164 builtins, variable scoping, generators, pattern matching, label/break, try/catch, path updates. Reimplementing in C++ is essentially writing a second jq with no architectural advantage.
- **Mini C++ evaluator** — Subset evaluator in C++ for ~60-70% of real-world filters. Would get supported filters to fast-path speed (60-100x) but unsupported filters still fall back to Rust. High effort (~2000-3000 lines C++, ~300 lines Rust glue). Diminishing returns given FlatValue + Arc already raised the floor to 26.5x.
- **simdjson parse_many** — simdjson's `document_stream` for NDJSON boundary detection. Single-threaded internally, would need chunking on top. `memchr` is already extremely fast for newline splitting. Unclear benefit.
- **4GB single-document limit** — Treat `[\n{...}\n{...}\n]` as streaming docs. High complexity, rare use case.
- **NEON-specific output formatting** — SIMD scan for chars needing escaping, bulk-copy safe runs. Small impact since output is rarely the bottleneck.
- **Custom SIMD JSON parsing** — simdjson already optimal.
- **openat for file opens** — Kernel dentry cache makes it free.
- **Inode sorting** — No benefit in warm cache.
- **Async I/O (io_uring/kqueue)** — Single-file processing, not many-file.
- **Thread oversubscription** — Causes regression on macOS (APFS B-tree mutex).
