# qj Optimization Ideas

Performance optimization roadmap, ordered by expected impact.
Techniques drawn from simdjson internals, gigagrep (faster-than-ripgrep grep), and systems-level optimization research.

## Progress

| # | Optimization | Status | Impact |
|---|-------------|--------|--------|
| 1 | P-core-only threading | DONE | Avoids E-core contention on Apple Silicon |
| 2 | mmap for file I/O | DONE | **~23% faster** on 1.1GB NDJSON, ~1% on 49MB |
| 3 | Field-chain fast path | DONE | **~40% faster** `.field` on 1.1GB NDJSON |
| 4 | `select` fast path | TODO | Skip Value tree for non-matching NDJSON lines |
| 5 | Multi-field fast path | TODO | `{a: .x, b: .y}` without Value tree |
| 6 | `select` + field fast path | TODO | Combine filter + extract in one pass |
| 7 | `length`/`keys` NDJSON fast path | TODO | Extend existing single-doc passthrough |
| 8 | Streaming NDJSON | TODO | Enable >RAM files, reduce startup latency |
| 9 | Per-thread output buffers | TODO | Avoid final concatenation step |
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

### 4. `select(.field == literal)` fast path (TODO)

**Pattern:** `select(.type == "PushEvent")`, `select(.count > 100)`, `select(.active == true)`

**Approach:** No C++ bridge changes needed. Entirely in Rust:
1. Detect `Filter::Select(Compare(field_chain, op, Literal(val)))` in `detect_fast_path()`
2. Per NDJSON line: use existing `dom_find_field_raw()` to extract the field as raw bytes
3. Compare raw bytes against the serialized literal (e.g., `b"\"PushEvent\""` for strings, `b"true"` for bools)
4. If match: output the entire raw line from the mmap buffer — zero copy, no Value tree
5. If no match: skip the line entirely — no parse, no eval, no output

**Why this is huge:** Selective queries are the most common NDJSON workload. `select(.type == "PushEvent")` on gharchive matches ~25% of lines. The current path builds a full Value tree for every line just to check one field. The fast path skips Value construction for 100% of lines (matching lines output raw bytes too).

**Operators to support:** `==`, `!=` (string/number/bool/null comparison against literal). Numeric `<`, `>`, `<=`, `>=` require parsing the raw bytes as a number — slightly more work but still avoids Value tree.

**Complexity:** Low-moderate. Pattern detection is straightforward (match the AST). The raw-bytes comparison is ~30 lines. Reuses `dom_find_field_raw` and `prepare_padded`. ~4 files, similar effort to field-chain fast path.

**Expected impact:** High. On gharchive.ndjson, `select(.type=="PushEvent")` is currently ~565ms. With the fast path, non-matching lines (~75%) cost almost nothing (one field extraction), and matching lines output raw bytes. Estimate ~40-60% faster.

**Files:** `src/parallel/ndjson.rs` (detect + process), `src/filter/mod.rs` (AST pattern matching)

### 5. Multi-field extraction fast path (TODO)

**Pattern:** `{type, actor: .actor.login}`, `{id, name, email}`, `[.field1, .field2]`

**Approach:** Extract multiple fields via repeated `dom_find_field_raw()` calls, then construct the output JSON directly as bytes — no Value tree.
1. Detect `Filter::ObjectConstruct` where all values are field chains (or shorthand like `{type}` = `{type: .type}`)
2. Per line: extract each field as raw bytes, write `{"type":RAW,"actor":RAW}\n` directly to output buffer
3. Similarly for `ArrayConstruct` with field-chain elements: write `[RAW,RAW]\n`

**Why:** Object construction (`{type, actor: .actor.login}`) is one of the most common NDJSON patterns — reshape each line to a subset of fields. Currently builds the full Value tree, evaluates each field, constructs a new Value::Object, then serializes. The fast path does N field extractions and one formatted write.

**Complexity:** Moderate. Need to handle the `ObjKey` variants (literal string key vs field shorthand vs expression). Need to write valid JSON output (escaping keys, commas, braces). Array variant is simpler. ~3-4 files.

**Expected impact:** Medium-high. The README benchmark `{type, repo: .repo.name, actor: .actor.login}` at 505ms could see 30-50% improvement — 3 field extractions + byte formatting vs full Value tree construction.

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs`, possibly `src/output.rs`

### 6. `select` + field extraction combined (TODO)

**Pattern:** `select(.type == "PushEvent") | .actor.login`, `select(.active) | {id, name}`

**Approach:** Combine select fast path (#4) with field-chain (#3) or multi-field (#5).
1. Detect `Filter::Pipe(Select(...), field_or_object_construct)`
2. Per line: check predicate via raw bytes, if match extract field(s), else skip

**Why:** This is the full "filter then reshape" pipeline — probably the single most common NDJSON workflow. Without the combined fast path, select fast path outputs the full raw line, then you'd need another pass to extract fields.

**Complexity:** Low — if #4 and #5 are already done, this is mostly plumbing. Detect the combined pattern, run select check first, then dispatch to field extraction.

**Expected impact:** Additive — combines the wins of #4 and #5. On selective + extract queries, avoids Value tree for all lines.

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs`

### 7. `length`/`keys` NDJSON fast path (TODO)

**Pattern:** `length`, `keys`, `.field | length`, `.field | keys`

**Approach:** We already have `dom_field_length()` and `dom_field_keys()` as single-doc passthrough paths (`PassthroughPath` enum in `filter/mod.rs`). Extend these to work per-NDJSON-line:
1. Detect these patterns in `detect_fast_path()`
2. Per line: call existing C++ bridge functions, output result directly

**Why:** `length` on gharchive.ndjson is ~463ms with the normal path. The C++ bridge can compute length without building the Value tree.

**Complexity:** Low. The C++ functions already exist. Just need to add variants to `NdjsonFastPath` and call them in `process_line`. ~2 files, minimal new code.

**Expected impact:** Moderate. `length` and `keys` are common but the per-line simdjson DOM parse is still needed — the savings come from skipping `decode_value()` and eval. Estimate ~20-30% faster.

**Files:** `src/parallel/ndjson.rs`, `src/filter/mod.rs`

---

## Tier 2: Medium Impact

### 8. Streaming NDJSON

**Current:** Entire file loaded into memory (via mmap or heap) before processing. 10GB file = 10GB virtual address space.

**Proposed:** Stream fixed-size blocks (64MB), split at newline boundaries, process in parallel, advance. With mmap this becomes sliding window over mapped region.

**Why:** Enables processing files larger than RAM. Also improves startup latency — processing begins before the full file is paged in.

**Complexity:** Medium. Need to handle lines that span block boundaries. The mmap foundation makes this easier (sliding window over mapped region, kernel handles paging).

**Files:** `src/parallel/ndjson.rs`, `src/main.rs`

### 9. Per-thread output buffers with ordered flush

**Current:** Each Rayon chunk produces `Vec<u8>`, all collected into `Vec<Vec<u8>>`, then concatenated and written.

**Proposed:** Pre-allocated 64KB per-thread buffers, flush to shared `Mutex<BufWriter<Stdout>>` in chunk order.

**Why:** Avoids the final concatenation step and reduces peak memory (don't hold all output in memory at once).

**Complexity:** Low-moderate. Need ordered flushing (can't write chunk 3 before chunk 2).

**Files:** `src/parallel/ndjson.rs`

---

## Tier 3: Speculative / Low Impact

### 10. simdjson parse_many for NDJSON

**Current:** Rust splits at newlines via `memchr`, parses each line individually.

**Proposed:** Use simdjson's `document_stream` / `parse_many` which does SIMD document boundary detection.

**Caveat:** parse_many is single-threaded internally. Would need chunking on top. And memchr is already extremely fast for newline splitting. Unclear if this helps.

### 11. Lift 4GB single-document limit

For large arrays: treat `[\n{...}\n{...}\n]` as streaming docs. High complexity, rare use case.

### 12. NEON-specific output formatting

SIMD scan for chars needing escaping. Bulk-copy safe runs. Small impact since output is rarely the bottleneck.

### 13. Arena allocation (bumpalo)

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
