# qj Optimization Ideas

Performance optimization roadmap, ordered by expected impact.
Techniques drawn from simdjson internals, gigagrep (faster-than-ripgrep grep), and systems-level optimization research.

## Progress

| # | Optimization | Status | Impact |
|---|-------------|--------|--------|
| 1 | P-core-only threading | DONE | Avoids E-core contention on Apple Silicon |
| 2 | mmap for file I/O | DONE | **~23% faster** on 1.1GB NDJSON, ~1% on 49MB |
| 3 | Field-chain fast path | DONE | **~40% faster** `.field` on 1.1GB NDJSON |
| 4 | `select` fast path | DONE | **~50% faster** `select(.type=="PushEvent")` on 1.1GB NDJSON |
| 5 | Multi-field fast path | REVERTED→DONE | Batch C++ extraction: **-54%** 3-field obj, **-38%** 2-field arr |
| 6 | `select` + extract fast path | DONE | **~35-42% faster** `select(.f==lit) \| .field` / `{...}` / `[...]` on 1.1GB NDJSON |
| 7 | `length`/`keys` NDJSON fast path | DONE | **~45% faster** `length`/`keys` on 1.1GB NDJSON |
| 8 | `select` ordering operators | DONE | Coverage: `>`, `<`, `>=`, `<=` now use fast path (same ~50% speedup as `==`) |
| 9 | DOM parser reuse | DONE | **~40% faster** — reuse parser across lines: 259ms→155ms on 3-field obj |
| 10 | Streaming NDJSON | TODO | Enable >RAM files, reduce startup latency |
| 11 | Per-thread output buffers | TODO | Avoid final concatenation step |
| — | Rc\<str\> for Value strings | REVERTED | Neutral — Rc indirection offsets clone savings |
| — | Arena allocation (bumpalo) | SKIPPED | Very high complexity, uncertain payoff |

---

## Tier 1: High Impact — NDJSON Fast Paths

The common theme: bypass the Rust Value tree entirely for NDJSON patterns.
Each line is independent, so we can detect the pattern once and apply a
specialized code path per line. The field-chain fast path (#3) proved this
approach gives ~40% improvement. The remaining patterns follow the same
strategy.

### 1. P-core-only threading on Apple Silicon (DONE)

**What:** Query `sysctl hw.perflevel0.logicalcpu` on macOS, configure Rayon's global thread pool with P-core count only.

**Why:** E-cores add lock contention without throughput benefit. gigagrep found `-j14` (all cores) was 1.6x *slower* than P-core-only on M4 Pro.

**Change:** `src/main.rs` — added `default_thread_count()` + `rayon::ThreadPoolBuilder` at start of main. Guarded with `cfg(all(target_os = "macos", target_arch = "aarch64"))` so Intel Macs skip the sysctl call entirely.

**Benchmarks (M4 Pro, 10P+4E):**

49MB large_twitter.json (single doc — no parallel processing, P-core change affects startup only):

| Query | Before (14 threads) | After (10 P-cores) | Delta |
|-------|---------------------|---------------------|-------|
| `-c "."` (identity compact) | 33.9ms ± 6.2 | 29.9ms ± 1.2 | -12% (likely noise — lower σ) |
| `.statuses[0].user.screen_name` | 183.7ms ± 23.7 | 166.0ms ± 4.9 | -10% (lower σ) |
| `.statuses \| length` | 54.5ms ± 42.3 | 44.0ms ± 1.4 | -19% (baseline was noisy) |
| `.statuses[] \| {id, text}` | 171.6ms ± 2.4 | 173.6ms ± 5.6 | ~0% (within noise) |

79MB 1m.ndjson (parallel — this is where P-core change matters):

| Query | Before (14 threads) | After (10 P-cores) | Delta |
|-------|---------------------|---------------------|-------|
| `-c "."` (identity compact) | 161.1ms ± 23.0 | 156.6ms ± 9.7 | -3% (much lower σ) |
| `.name` (field access) | 137.1ms ± 5.8 | 137.5ms ± 5.3 | ~0% |

**Verdict:** Single-doc numbers are noise (Rayon unused). NDJSON shows modest wall-time improvement but significantly lower variance — E-cores were adding jitter without helping throughput. Effect would be larger on bigger NDJSON files where contention matters more.

### 2. mmap for file I/O (DONE)

**What:** `libc::mmap(PROT_READ, MAP_PRIVATE)` + `MADV_SEQUENTIAL` for file I/O. Kernel pages data on demand — no heap allocation, no memcpy. Falls back to heap when mmap can't provide enough natural padding (file size within 63 bytes of a page boundary). `QJ_NO_MMAP=1` env var for benchmarking.

**Change:** `src/simdjson/types.rs` — new `PaddedFile` type (mmap or heap-backed), updated `read_padded_file` to try mmap first. Added `libc` dependency.

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson, sequential runs, 5s cooldown):**

| Query | Heap | mmap | Delta |
|-------|------|------|-------|
| `.type` | 548.9ms | 447.7ms | **-18%** |
| `.actor.login` | 546.5ms | 442.6ms | **-19%** |
| `length` | 553.3ms | 447.9ms | **-19%** |

49MB large_twitter.json: ~1% (within noise — file is too small for mmap to matter).

**Verdict:** Consistent ~19% wall-time improvement on GB-scale NDJSON. The win comes from avoiding a 1.1GB heap allocation + memcpy. System time drops from ~230ms to ~195ms (15% less kernel overhead).

**Files:** `src/simdjson/types.rs`, `Cargo.toml` (added `libc`)

### 3. Field-chain fast path (DONE)

**What:** For `.field` and `.field.nested.path` patterns on NDJSON, bypass the Rust Value tree entirely. Uses `dom_find_field_raw()` in C++ to extract raw JSON bytes directly from simdjson DOM, avoiding `decode_value()` and the full eval pipeline.

**Change:** `src/parallel/ndjson.rs` — added `NdjsonFastPath` enum, `detect_fast_path()`, `prepare_padded()` (scratch buffer reuse), `unescape_json_string()` (for raw output mode). Modified `process_chunk` and `process_line` to use the fast path. `QJ_NO_FIELD_FAST=1` env var to disable for benchmarking.

**Why:** Value tree construction (`decode_value()`) is the most expensive Rust-side operation. For `.type` on a 1GB file, we were building millions of full Value trees only to extract one string and discard the rest. The fast path skips: simdjson DOM → flat tokens → `decode_value()` → Value tree → eval → output. Instead: simdjson DOM → `dom_find_field_raw` → raw bytes → output.

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson, sequential runs, 5s cooldown):**

| Query | Without fast path | With fast path | Delta |
|-------|------------------|---------------|-------|
| `.type` | 476.9ms | 284.0ms | **-40%** |
| `.actor.login` | 443.0ms | 261.2ms | **-41%** |
| `length` | 463.2ms | 463.2ms | ~0% (control — not a field chain) |

**Verdict:** ~40% wall-time improvement on field-chain patterns. User time drops from ~3.8s to ~1.5s (62% less CPU work). The fast path avoids building the Value tree entirely — the dominant cost for simple extractions.

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs` (made `collect_field_chain` public), `src/output.rs` (added `PartialEq, Eq` to `OutputMode`)

### 4. `select(.field == literal)` fast path (DONE)

**Pattern:** `select(.type == "PushEvent")`, `select(.active == true)`, `select(.count == 42)`

**Approach:** No C++ bridge changes needed. Entirely in Rust:
1. Detect `Filter::Select(Compare(field_chain, op, Literal(val)))` in `detect_fast_path()`
2. Per NDJSON line: use existing `dom_find_field_raw()` to extract the field as raw bytes
3. Compare raw bytes against the serialized literal (e.g., `b"\"PushEvent\""` for strings, `b"true"` for bools)
4. If match: output the entire raw line from the mmap buffer — zero copy, no Value tree
5. If no match: skip the line entirely — no parse, no eval, no output

**Operators supported:** `==`, `!=` (string/number/bool/null comparison against literal). Both operand orientations handled: `(.field == lit)` and `(lit == .field)`.

**Change:** `src/parallel/ndjson.rs` — added `SelectEq` variant to `NdjsonFastPath`, `detect_select_fast_path()`, `serialize_literal()`, `process_line_select_eq()`. `QJ_NO_FAST_PATH=1` env var disables all fast paths for A/B benchmarking (renamed from `QJ_NO_FIELD_FAST`).

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson, sequential runs, 5s cooldown):**

| Query | Without fast path | With fast path | Delta |
|-------|------------------|---------------|-------|
| `select(.type=="PushEvent")` | 515.8ms | 258.0ms | **-50%** |

**Verdict:** ~50% wall-time improvement. User time drops from ~4.2s to ~1.4s (67% less CPU work). The fast path avoids building the Value tree for every line — matching lines output raw bytes, non-matching lines are skipped entirely after one field extraction.

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs` (`CmpOp` already had needed derives)

### 5. Multi-field extraction fast path (REVERTED → DONE with batch C++ extraction)

**Pattern:** `{type, actor: .actor.login}`, `{id, name, email}`, `[.field1, .field2]`

**First attempt (REVERTED):** N separate `dom_find_field_raw()` calls per line each re-parse the entire JSON through simdjson FFI. For ≥3 fields, the FFI round-trip overhead exceeded the cost of building the Value tree once.

| Query | Fast path | Normal path | Delta |
|-------|-----------|-------------|-------|
| `{type, repo: .repo.name, actor: .actor.login}` (3 fields) | 635ms | 470ms | **+35% REGRESSION** |

**Second attempt (DONE):** New batch C++ function `jx_dom_find_fields_raw` — parse once in C++, extract N field chains, return a single length-prefixed buffer. Eliminates N FFI round-trips. Added `jx_dom_find_fields_raw_reuse` variant for reusable DOM parser.

**Change:** `src/simdjson/bridge.cpp` — new `jx_dom_find_fields_raw` function that navigates multiple field chains per single DOM parse. `src/simdjson/bridge.rs` — Rust wrapper `dom_find_fields_raw()`. `src/parallel/ndjson.rs` — re-enabled `MultiFieldObj`, `MultiFieldArr`, `SelectEqObj`, `SelectEqArr` fast paths using batch extraction.

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson, with DOM parser reuse):**

| Query | Without fast path | With fast path | Delta |
|-------|------------------|---------------|-------|
| `select(.type=="PushEvent") \| {type, id, actor: .actor.login}` (3 fields) | 454ms | 155ms | **-66%** |
| `select(.type=="PushEvent") \| [.type, .actor.login]` (2 fields) | 472ms | 131ms | **-72%** |

**Verdict:** Batch extraction + DOM parser reuse makes multi-field fast paths profitable. The key insight: parse once in C++, extract N fields, return one buffer — no per-field FFI overhead.

### 6. `select` + extract fast path (DONE)

**Pattern:** `select(.type == "PushEvent") | .actor.login`, `select(.type == "PushEvent") | {type, actor: .actor.login}`, `select(.type == "PushEvent") | [.type, .id]`

**Approach:** Combine select fast path (#4) with field-chain (#3) or multi-field object/array construction.
1. Detect `Filter::Pipe(Select(Compare(...)), rhs)` where rhs is a field chain, ObjectConstruct, or ArrayConstruct
2. Per line: check predicate via raw byte comparison, if match extract field(s), else skip entirely
3. Three variants: `SelectEqField` (single field output), `SelectEqObj` (object construction), `SelectEqArr` (array construction)

**Why:** The select predicate filters out most lines (typically 80-95%), so only matching lines pay the multi-field extraction cost. This makes the N×FFI overhead acceptable — unlike bare multi-field (#5) where every line pays.

**Change:** `src/parallel/ndjson.rs` — added `SelectEqField`, `SelectEqObj`, `SelectEqArr` variants to `NdjsonFastPath`. Detection via `detect_select_extract_fast_path()` which reuses `try_field_literal()` for the predicate and `collect_field_chain`/`try_multi_field_obj`/`try_multi_field_arr` for the output side. Detection order: select+extract checked before bare select to avoid partial matches.

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson, sequential runs, 5s cooldown):**

| Query | Without fast path | With fast path | Delta |
|-------|------------------|---------------|-------|
| `select(.type=="PushEvent") \| .actor.login` | 444ms | 272ms | **-39%** |
| `select(.type=="PushEvent") \| {type, actor: .actor.login}` | 497ms | 290ms | **-42%** |
| `select(.type=="PushEvent") \| [.type, .actor.login]` | 443ms | 289ms | **-35%** |

**Verdict:** ~35-42% wall-time improvement across all select+extract variants. The predicate filters ~80% of lines, so matching lines pay N field extractions while non-matching lines are skipped after one field check.

**Files:** `src/parallel/ndjson.rs`, `tests/ndjson.rs`, `tests/e2e.rs`

### 7. `length`/`keys` NDJSON fast path (DONE)

**Pattern:** `length`, `keys`, `.field | length`, `.field | keys`

**Approach:** Uses existing `dom_field_length()` and `dom_field_keys()` C++ bridge functions per NDJSON line. Falls back to normal path for unsupported types (e.g. string length).

**Change:** `src/parallel/ndjson.rs` — added `Length` and `Keys` variants to `NdjsonFastPath`, `detect_length_keys_fast_path()`, `process_line_length()`, `process_line_keys()`. Also made `decompose_field_builtin` `pub(crate)` in `src/filter/mod.rs`.

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson, sequential runs, 5s cooldown):**

| Query | Without fast path | With fast path | Delta |
|-------|------------------|---------------|-------|
| `length` | 441.0ms | 249.5ms | **-43%** |
| `.actor \| length` | 437.9ms | 245.5ms | **-44%** |
| `keys` | 458.2ms | 251.2ms | **-45%** |

**Verdict:** ~43-45% wall-time improvement across all variants. User time drops from ~3.8s to ~1.4-1.6s. The C++ bridge computes length/keys directly from the simdjson DOM without constructing the Rust Value tree.

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs`

### 8. `select` with ordering operators (DONE)

**Pattern:** `select(.score > 90)`, `select(.ts >= 1234567890)`, `select(.name < "M")`

**Approach:** Extended the existing `SelectEq` fast path to support `>`, `<`, `>=`, `<=` operators. The detection functions (`detect_select_fast_path`, `detect_select_extract_fast_path`) previously rejected non-Eq/Ne operators — removed that restriction. Added `evaluate_select_predicate()` helper that handles all `CmpOp` variants:
- Eq/Ne: existing byte comparison with `bytes_mismatch_is_definitive` safety check
- Ordering ops: parse both sides as numbers (`parse_json_number`) and compare numerically, or compare unescaped string byte contents (UTF-8 preserves codepoint order for non-escaped strings)
- Falls back to full eval for ambiguous cases (escaped strings, type mismatches)

Propagated to all select variants: `SelectEq`, `SelectEqField`, `SelectEqObj`, `SelectEqArr`.

**Change:** `src/parallel/ndjson.rs` — removed Eq/Ne restriction in detection, added `evaluate_select_predicate()` + `parse_json_number()` helpers, refactored all 4 select processing functions. Added comprehensive unit tests for all ordering operators and predicate evaluation.

**Benchmarks:** No separate benchmark — this is a coverage extension, not a new mechanism. Ordering queries now get the same ~50% speedup as `==`/`!=` (optimization #4) instead of falling back to full Value tree eval.

**Files:** `src/parallel/ndjson.rs`, `tests/ndjson.rs`, `tests/e2e.rs`

### 9. DOM parser reuse across NDJSON lines (DONE)

**What:** Reusable `JxDomParser` handle that persists simdjson's `dom::parser` across lines within each chunk. Previously every fast-path FFI call constructed a fresh `dom::parser` on the stack — simdjson's `dom::parser` pre-allocates internal buffers, so creating one per line wasted that allocation.

**Approach:**
1. **C++ (`bridge.cpp`):** New `JxDomParser` struct holding a `dom::parser`. Added `_reuse` variants of all hot functions: `jx_dom_find_field_raw_reuse`, `jx_dom_find_fields_raw_reuse`, `jx_dom_field_length_reuse`, `jx_dom_field_keys_reuse`.
2. **Rust (`bridge.rs`):** `DomParser` wrapper type with `new()`, `Drop`, and methods mirroring the free functions. `Send` but not `Sync` (one parser per thread).
3. **NDJSON (`ndjson.rs`):** Create `DomParser` once per chunk in `process_chunk`. Thread through `process_line` to all fast-path processing functions. One parser per Rayon thread, reused across all lines.

**Benchmarks (M4 Pro, 1.1GB gharchive.ndjson):**

| Query | Before (per-line parser) | After (reused parser) | Delta |
|-------|-------------------------|----------------------|-------|
| `select(.type=="PushEvent") \| {type, id, actor: .actor.login}` | 259ms | 155ms | **-40%** |
| `select(.type=="PushEvent") \| .actor.login` | — | 131ms | — |
| `.type` | — | 111ms | — |
| `length` | — | 107ms | — |

**Verdict:** ~40% wall-time improvement. The savings come from avoiding repeated internal buffer allocation in simdjson's DOM parser. Biggest impact on queries that make multiple FFI calls per line (multi-field extraction).

**Files:** `src/simdjson/bridge.cpp`, `src/simdjson/ffi.rs`, `src/simdjson/bridge.rs`, `src/simdjson/mod.rs`, `src/parallel/ndjson.rs`, `tests/simdjson_ffi.rs`

---

## Tier 2: Medium Impact

### 10. Streaming NDJSON

**Current:** Entire file loaded into memory (via mmap or heap) before processing. 10GB file = 10GB virtual address space.

**Proposed:** Stream fixed-size blocks (64MB), split at newline boundaries, process in parallel, advance. With mmap this becomes sliding window over mapped region.

**Why:** Enables processing files larger than RAM. Also improves startup latency — processing begins before the full file is paged in.

**Complexity:** Medium. Need to handle lines that span block boundaries. The mmap foundation makes this easier (sliding window over mapped region, kernel handles paging).

**Files:** `src/parallel/ndjson.rs`, `src/main.rs`

### 11. Per-thread output buffers with ordered flush

**Current:** Each Rayon chunk produces `Vec<u8>`, all collected into `Vec<Vec<u8>>`, then concatenated and written.

**Proposed:** Pre-allocated 64KB per-thread buffers, flush to shared `Mutex<BufWriter<Stdout>>` in chunk order.

**Why:** Avoids the final concatenation step and reduces peak memory (don't hold all output in memory at once).

**Complexity:** Low-moderate. Need ordered flushing (can't write chunk 3 before chunk 2).

**Files:** `src/parallel/ndjson.rs`

---

## Tier 3: Speculative / Low Impact

### 12. simdjson parse_many for NDJSON

**Current:** Rust splits at newlines via `memchr`, parses each line individually.

**Proposed:** Use simdjson's `document_stream` / `parse_many` which does SIMD document boundary detection.

**Caveat:** parse_many is single-threaded internally. Would need chunking on top. And memchr is already extremely fast for newline splitting. Unclear if this helps.

### 13. Lift 4GB single-document limit

For large arrays: treat `[\n{...}\n{...}\n]` as streaming docs. High complexity, rare use case.

### 14. NEON-specific output formatting

SIMD scan for chars needing escaping. Bulk-copy safe runs. Small impact since output is rarely the bottleneck.

### 15. Arena allocation (bumpalo)

Per-document bump arena for Value tree construction. Theoretically cache-friendly and fast deallocation. In practice, the Rc\<str\> experiment showed allocation pressure isn't the dominant cost — fast-path avoidance of the Value tree entirely is a strictly better approach. Would be a massive refactor (lifetime parameter on Value, propagates everywhere). Not worth the complexity given the fast-path strategy.

---

## Rejected / Reverted

### Rc\<str\> for Value strings (REVERTED)

Changed `Value::String(String)` to `Value::String(Rc<str>)`. Benchmarked as neutral to slightly negative — Rc pointer indirection adds cache misses that offset the O(1) clone savings. Most strings are constructed once and output once. See commit 183f3b1 for the implementation and 3dbfa55 for the revert.

---

## What NOT to optimize (gigagrep learnings)

- **Custom SIMD JSON parsing** — simdjson already optimal
- **openat for file opens** — kernel dentry cache makes it free
- **inode sorting** — no benefit in warm cache
- **Async I/O (io_uring/kqueue)** — single-file processing, not many-file
- **Thread oversubscription** — causes regression on macOS (APFS B-tree mutex)
