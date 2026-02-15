# qj Optimization Ideas

Performance optimization roadmap, ordered by expected impact.
Techniques drawn from simdjson internals, gigagrep (faster-than-ripgrep grep), and systems-level optimization research.

## Progress

| # | Optimization | Status | Impact |
|---|-------------|--------|--------|
| 1 | P-core-only threading | DONE | Avoids E-core contention on Apple Silicon |
| 2 | mmap for file I/O | TODO | ~8% less system time (proven in gigagrep) |
| 3 | Expand On-Demand fast paths | TODO | 2-5x for simple filters on large JSON |
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

**Change:** `src/main.rs` — added `default_thread_count()` + `rayon::ThreadPoolBuilder` at start of main.

### 2. mmap for file I/O

**Current:** `read_padded_file()` does `vec![0u8; file_len + pad]` + `read_exact` — heap-allocates and copies entire file.

**Proposed:** `libc::mmap(PROT_READ, MAP_PRIVATE)` for files. Kernel pages data on demand. simdjson reads directly from mapped pages.

**Why:** Proven in gigagrep — 8% less system time. Eliminates the largest allocation in the program. simdjson accepts pointer+length, so mmap is near-drop-in.

**Caveat:** simdjson requires 64 bytes of zeroed padding after data. With mmap, the kernel zero-fills bytes between file end and page boundary. If `file_len % page_size` leaves >= 64 bytes of padding, no extra work needed. Otherwise need a small copy for the tail.

**Files:** `src/simdjson/types.rs`, `src/main.rs`

### 3. Expand On-Demand fast paths (bypass Value tree)

**Current:** Only 3 patterns bypass the Value tree: identity compact (`. -c`), `.field | length`, `.field | keys`.

**Proposed:** Evaluate common filters directly on simdjson without constructing Rust Value tree:
- `.field` — use existing `dom_find_field_raw()`
- `.field.nested.path` — chain On-Demand navigation in C++
- `.[] | .field` — iterate array in C++, extract per-element
- `select(.field == "value")` — evaluate predicate in C++
- `.field | values, has("k"), type` — extend C++ bridge

**Why:** Value tree construction (`decode_value()`) is the most expensive Rust-side operation. For `.user.name` on a 1GB file, we build millions of full Value trees to extract one string.

**Files:** `src/simdjson/bridge.cpp`, `src/simdjson/bridge.rs`, `src/filter/mod.rs`, `src/filter/eval.rs`

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
