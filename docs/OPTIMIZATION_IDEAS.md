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

### Single-doc floor (currently 2.15x vs jq)

The single-doc floor is the weakest performance area. Current state on 49MB `large_twitter.json`:

| Filter | qj | jq | Speedup |
|--------|----|----|---------|
| `length` (passthrough) | 34ms | 361ms | 10.6x |
| `.statuses | map({user, text})` (passthrough) | 58ms | 751ms | 12.9x |
| `map({user, text})` (flat eval) | 94ms | 367ms | 3.9x |
| `reduce .[] as $x (0; .+1)` (flat eval) | 172ms | 369ms | 2.15x |

Time breakdown for the worst case (`reduce`, 172ms): I/O ~7ms, SIMD parse + flat buffer ~45ms, reduce loop (~2M iterations) ~120ms. The flat eval already has a zero-materialization optimization for this case (detects `$x` unused in `. + 1`, counts elements from flat buffer, loops without materializing any array elements). The 120ms is pure evaluator dispatch overhead.

**A. Parallel map/select on single-doc arrays.** Currently single-doc is always single-threaded, even for `map(transform)` on a large array. The NDJSON parallel infrastructure could apply to array elements: split array into chunks, process per-chunk, merge. Doesn't help `reduce` (inherently sequential) but helps the common `map`, `.[]`, `select` patterns. Medium complexity — reuse existing Rayon chunking from `src/parallel/ndjson.rs`. (`src/main.rs`, `src/parallel/`)

*Caveat:* Needs large arrays (10K+ elements) to amortize thread pool overhead. Won't show up on `large_twitter.json` where `.statuses` has ~100 tweets — passthroughs already catch those patterns at 10-13x. Would matter on bare arrays with 100K+ elements.

**B. Specialized reduce detection.** Detect common reduce idioms and execute natively without the eval loop:
- `reduce .[] as $x (0; . + 1)` → `length`
- `reduce .[] as $x (0; . + $x)` → sum elements
- `reduce .[] as $x (null; if . == null or $x > . then $x else . end)` → max

Limited coverage but these are textbook patterns. Low complexity — pattern-match on AST in `detect_fast_path()`. (`src/parallel/ndjson.rs`, `src/main.rs`)

**C. Bytecode VM for the evaluator.** Replace the tree-walking interpreter with a compiled bytecode loop. This is the only option that raises the floor for *all* filter types, not just recognized patterns.

**Current overhead per eval() call:** The evaluator (`src/filter/eval.rs`, 3600 lines) uses a callback-based tree walk. For `reduce .[] as $x (0; . + $x)` on a 1000-element array, each iteration involves:
- 6-8 pattern matches on the 35-variant `Filter` enum (Reduce → Iterate → Arith → Identity + Var)
- 2-3 nested closures (Arith creates `&mut |rval| { eval(left, &mut |lval| { ... }) }`)
- 1 thread-local `LAST_ERROR.with()` access per `eval()` call
- 1 `acc.clone()` (cheap for scalars, Arc bump for Array/Object)
- Environment lookup via `Rc<VarScope>` chain (pointer chasing)

**What bytecode eliminates:**
- Closure overhead → explicit value stack + program counter (biggest win)
- Pattern matching → computed goto / jump table
- TLS error checks → local error register
- Environment chain → indexed stack frame (array lookup instead of pointer chasing)

**Estimated impact:** 20-40% speedup on the eval loop portion. For `reduce` on 49MB: eval is ~120ms of 172ms, so 24-48ms saved → total ~124-148ms → **2.5-3.0x** vs jq (up from 2.15x). For NDJSON fallback filters, the same improvement applies per-core. The speedup compounds with parallelism: if per-core eval drops 30%, NDJSON fallback filters gain ~30% wall-time improvement too.

**The hard part:** jq's generator semantics — filters produce 0-N outputs per input. Current approach uses `&mut dyn FnMut(Value)` callbacks. Bytecode needs an explicit yield/resume mechanism (coroutine-style stack or iterator protocol). This is the bulk of the implementation complexity.

**Scope:** ~800-1200 lines for bytecode compiler + interpreter covering the core subset (Identity, Field, Pipe, Iterate, Arith, Compare, Select, Literal, Var, Reduce, Foreach, IfThenElse, ObjectConstruct, ArrayConstruct, builtins). Unsupported filters fall back to tree-walking eval. Can be done incrementally — compile what you can, fall back for the rest.

**Files:** new `src/filter/bytecode.rs` (compiler + VM), modified `src/filter/eval.rs` (dispatch to bytecode when available), `src/flat_eval.rs` (same).

**D. HashMap for object field lookup.** Currently `Value::Object(Arc<Vec<(String, Value)>>)` — `.field` access is O(n) linear scan (`src/filter/eval.rs`). For objects with 20-30 fields (typical GH Archive), every field access scans ~15 entries on average. A `HashMap` or sorted+binary-search layout would make this O(1) or O(log n). Independent of the bytecode question. Low-moderate complexity but changes Value's memory layout. (`src/value.rs`, `src/filter/eval.rs`)

*Caveat:* Won't show up on `large_twitter.json` benchmarks. For `map({user, text})` on ~100 tweets: 100 × 2 lookups × ~10 string comparisons ≈ 2000 comparisons at ~5-10ns each = ~10-20μs (negligible vs 94ms total). The reduce floor has zero field accesses. Also has a cost: HashMap construction is more expensive than Vec, so filters that build many objects could regress. More likely to show up on `gharchive.ndjson` (30-field objects, millions of lines) or deep pipelines with many `.field` accesses on wide objects.

### NDJSON infrastructure

- **Streaming NDJSON** — Replaced full-file mmap with 64 MB windowed reading. Each window's chunks processed in parallel (same Rayon approach), output written per-window, partial lines carried across boundaries. Peak RSS dropped from **2.6 GB → 109 MB** (multi-threaded) / **77 MB** (single-threaded) on 1.1 GB gharchive.ndjson. No speed regression (~54x vs jq). Detection uses `detect_ndjson_from_reader()` which reads in growing chunks (64 KB → 1 MB) to handle long first lines. (`src/parallel/ndjson.rs`, `src/main.rs`)
- **Per-thread output buffers** — Currently each Rayon chunk produces `Vec<u8>`, all collected then concatenated. Pre-allocated per-thread buffers with ordered flush to stdout would avoid the final concatenation and reduce peak memory. Low-moderate complexity. (`src/parallel/ndjson.rs`)

### Lazy evaluation

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
