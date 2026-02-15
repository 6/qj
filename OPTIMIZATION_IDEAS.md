# qj Optimization Ideas

Performance optimization roadmap, ordered by expected impact.
Techniques drawn from simdjson internals, gigagrep (faster-than-ripgrep grep), and systems-level optimization research.

## Progress

| # | Optimization | Status | Impact |
|---|-------------|--------|--------|
| 1 | P-core-only threading | DONE | Avoids E-core contention on Apple Silicon |
| 2 | mmap for file I/O | DONE | **~23% faster** on 1.1GB NDJSON, ~1% on 49MB |
| 3 | Expand On-Demand fast paths | DONE | **~40% faster** field-chain on 1.1GB NDJSON |
| 4 | Arena allocation (bumpalo) | TODO | Reduce malloc pressure in Value tree |
| 5 | Streaming NDJSON | TODO | Enable >RAM files, reduce startup latency |
| 6 | simdjson parse_many for NDJSON | TODO | Replace manual line splitting |
| 7 | Rc\<str\> for Value strings | TODO | O(1) string clones |
| 8 | Per-thread output buffers | TODO | Avoid final concatenation step |
| 9 | Lift 4GB single-doc limit | TODO | Rare but blocks large single-doc files |
| 10 | NEON output formatting | TODO | SIMD string escaping |

---

## Tier 1: High Impact

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

### 3. Expand On-Demand fast paths (DONE — field-chain)

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

**Still TODO:** More fast-path patterns:
- `.[] | .field` — iterate array in C++, extract per-element
- `select(.field == "value")` — evaluate predicate in C++
- `.field | values, has("k"), type` — extend C++ bridge

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs` (made `collect_field_chain` public), `src/output.rs` (added `PartialEq, Eq` to `OutputMode`)

### 4. Arena allocation (bumpalo)

**Current:** Every Value node allocates individually: `String::from()`, `Vec::new()`, `Rc::new()`.

**Proposed:** Per-document or per-chunk bump arena. All nodes from one arena, freed in one shot.

**Why:** O(1) bump vs O(log n) malloc. Cache-friendly (contiguous). Free deallocation.

**Trade-off:** Large refactor — Value must use arena-allocated types. Rc sharing model needs rethinking. Use `bumpalo-herd` for thread-local arenas in Rayon.

**Files:** `src/value.rs`, `src/filter/eval.rs`, `src/simdjson/bridge.rs`

---

## Tier 2: Medium Impact

### 5. Streaming NDJSON

**Current:** Entire file loaded into `Vec<u8>` before processing. 10GB file = 10GB RAM.

**Proposed:** Stream fixed-size blocks (64MB), split at newline boundaries, process in parallel, advance. With mmap this becomes sliding window over mapped region.

**Files:** `src/parallel/ndjson.rs`, `src/main.rs`

### 6. simdjson parse_many for NDJSON

**Current:** Rust splits at newlines via `memchr`, parses each line individually.

**Proposed:** Use simdjson's `document_stream` / `parse_many` which does SIMD document boundary detection. Already have FFI bindings but only used for benchmarking.

**Caveat:** parse_many is single-threaded internally. Combine with chunking: split into N chunks, each uses parse_many in its own thread.

**Files:** `src/simdjson/bridge.cpp`, `src/parallel/ndjson.rs`

### 7. Rc\<str\> for Value strings

**Current:** `Value::String(String)` — every clone allocates.

**Proposed:** `Value::String(Rc<str>)` — clones are O(1) reference bumps. Also consider `Arc<str>` if needed for Send across threads.

**Files:** `src/value.rs`, `src/filter/eval.rs`

### 8. Per-thread output buffers with ordered flush

**Current:** Each Rayon chunk produces `Vec<u8>`, all collected, then concatenated.

**Proposed:** Pre-allocated 64KB per-thread buffers, flush to shared `Mutex<BufWriter<Stdout>>` in chunk order.

**Files:** `src/parallel/ndjson.rs`

---

## Tier 3: Speculative

### 9. Lift 4GB single-document limit

For large arrays: treat `[\n{...}\n{...}\n]` as streaming docs. High complexity, rare use case.

### 10. NEON-specific output formatting

SIMD scan for chars needing escaping. Bulk-copy safe runs. Small impact since output is rarely the bottleneck.

### 11. Compile-time specialization for common filters

Generate optimized code paths for `.field`, `.[]`, `.[] | .field`. Marginal — eval dispatch is fast vs I/O.

---

## What NOT to optimize (gigagrep learnings)

- **Custom SIMD JSON parsing** — simdjson already optimal
- **openat for file opens** — kernel dentry cache makes it free
- **inode sorting** — no benefit in warm cache
- **Async I/O (io_uring/kqueue)** — single-file processing, not many-file
- **Thread oversubscription** — causes regression on macOS (APFS B-tree mutex)
