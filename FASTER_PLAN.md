# Raising the NDJSON speedup floor

## Problem

qj's NDJSON speedup ranges from 2.8x to 127x vs jq. The floor (2.8x) occurs on evaluator-bound filters like `{type, commits: (.payload.commits // [] | length)}`. The goal is to raise this floor to at least 4x across the board — for ALL filters, not just ones that happen to touch few fields.

## Root cause

The current fallback path per NDJSON line:

1. simdjson parses JSON → C++ DOM (fast)
2. `jx_dom_to_flat` serializes entire DOM → flat token buffer (crosses FFI)
3. `decode_value` walks flat tokens → allocates full Rust `Value` tree (**bottleneck** — every field, string, nested object gets heap-allocated)
4. Rust evaluator runs filter on `Value` tree (moderate)
5. Output serialized from `Value` (fast)

Step 2-3 materializes the **entire** JSON object even if the filter only touches 2 fields. On GH Archive objects (~30 fields, 2-3 levels deep), this is massive waste.

**The per-core math:** qj uses ~12 cores to get 2.73s on the worst filter. jq does it single-threaded in 7.58s. That means qj's per-core CPU time is `2.73 × 12 ≈ 33s` — about **4x slower than jq per core**. Parallelism is masking the overhead. To hit an 8x floor with 12 cores, per-core needs to be at most 1.5x slower than jq (currently 4x slower). That's a ~2.7x improvement needed in per-core speed.

The SIMD fast paths (60-127x) skip steps 2-4 entirely, staying in C++ and extracting raw bytes.

## Current architecture (key files)

- **Value type:** `src/value.rs:1-23` — 7 variants (Null, Bool, Int, Double, String, Array, Object). Array/Object use `Rc<Vec<...>>` for O(1) clone.
- **DOM→Value conversion:** `src/simdjson/bridge.rs:26-47` — `dom_parse_to_value()` calls C++ `jx_dom_to_flat`, then Rust `decode_value` walks the flat buffer.
- **Flat token format:** `src/simdjson/bridge.rs:98-164` — tag-length-value encoding (TAG_NULL=0 through TAG_OBJECT_END=8).
- **FFI surface:** `src/simdjson/ffi.rs` — DOM conversion, On-Demand field extraction, raw field extraction, reusable DOM parsers.
- **Evaluator:** `src/filter/eval.rs` (~3600 lines) — 34 Filter variants, 164 builtins, generator-based output via `&mut dyn FnMut(Value)`.
- **Fast path detection:** `src/parallel/ndjson.rs:227-253` — pattern-matches filter AST against known shapes, routes to C++ extraction or Rust evaluator fallback.

---

## Approach A: Lazy Value

**Idea:** Instead of materializing the entire DOM into a Rust `Value` tree upfront, wrap simdjson DOM nodes in a `LazyValue` that only materializes children when the evaluator accesses them.

### How it works

Replace the current flow:
```
simdjson DOM → jx_dom_to_flat (all fields) → decode_value (all fields) → eval
```

With:
```
simdjson DOM → LazyValue (holds DOM pointer + position) → eval (materializes on access)
```

For `{type, commits: (.payload.commits // [] | length)}`:
- Current: materializes all ~30 fields, evaluator uses 2
- Lazy: materializes only `.type` (string) and `.payload.commits` (array, just need length)

### What changes

**New type** (in `src/value.rs` or new `src/lazy_value.rs`):
```rust
enum LazyValue<'a> {
    Eager(Value),
    DomNode { parser: &'a DomParser, element_id: u32 },
}
```

**New FFI functions** (in `src/simdjson/ffi.rs` + `bridge.cpp`):
- `jx_dom_parse_reuse(parser, buf, len) → element_handle`
- `jx_dom_element_type(parser, element) → type_tag`
- `jx_dom_element_get_field(parser, element, key) → element_handle`
- `jx_dom_element_get_index(parser, element, idx) → element_handle`
- `jx_dom_element_iterate(parser, element, callback)`
- `jx_dom_element_length(parser, element) → i64`
- `jx_dom_element_keys(parser, element, out_buf)`
- `jx_dom_element_to_raw(parser, element, out_buf)`
- `jx_dom_element_to_value(parser, element) → flat_tokens` (force materialization fallback)

**Evaluator changes** (in `src/filter/eval.rs`):
- Make evaluator generic over value type, or add `LazyValue` match arms
- Field access on DomNode → FFI call → return new DomNode
- `length` on DomNode → FFI call → return Int
- Operations that need full value → force materialization

### Pros
- Evaluator logic stays in Rust — no duplication
- Incremental: can start with just field access + length, expand over time
- Huge win for selective filters (skip materializing untouched fields)
- Falls back to eager materialization for unsupported operations

### Cons
- Many small FFI calls (each field access crosses boundary, no inlining)
- Lifetime management across FFI
- **Does NOT raise the floor for filters that touch every field** (`.`, `walk`, `[.[] | transform]`) — these still materialize everything and remain ~2.8x

### Honest assessment
Approach A helps selective filters a lot (2.8x → ~8-20x) but **does not raise the true floor**. Filters that iterate or transform every field in the document still hit the same per-core overhead. If "raise the floor across the board" means ALL filters, Approach A alone isn't enough.

### Effort
Medium. ~500-800 lines.

---

## Approach C: Mini C++ evaluator

**Idea:** Instead of pattern-matching specific AST shapes (current fast paths), build a small jq evaluator in C++ that handles a core subset of operations directly on simdjson's DOM. Returns raw JSON bytes. Falls back to Rust evaluator for complex features.

### What it covers

**In scope (~60-70% of real-world NDJSON filters):**
- Identity, Field access, Pipe, Iterate
- Object/Array construction
- Select with comparison predicates
- Alternative (`//`), Try (`.foo?`)
- If-then-else, Comparisons, Simple arithmetic
- Boolean operations (`and`, `or`, `not`)
- Core builtins: `length`, `keys`, `values`, `type`, `empty`, `not`, `add`, `map`, `select`, `first`, `last`, `reverse`, `sort`, `unique`, `flatten`, `tostring`, `tonumber`, `split`, `join`, `has`, `contains`, `startswith`, `endswith`, `floor`, `ceil`, `round`, `ascii_downcase`, `ascii_upcase`, `ltrimstr`, `rtrimstr`, `test` (basic regex)

**Out of scope (falls back to Rust):**
- Variable binding (`as` patterns), destructuring
- `reduce`, `foreach`, `label`/`break`
- User-defined functions (`def`)
- String interpolation
- Assignment operators (`|=`, `+=`, etc.)
- Path operations (`path`, `getpath`, `setpath`)
- Complex builtins: `walk`, `recurse`, `group_by`, `INDEX`, `JOIN`, `scan`, `gsub`
- Format strings (`@base64`, `@csv`, etc.)

### How it works

1. Rust parses jq filter → AST (unchanged)
2. `can_eval_in_cpp(filter: &Filter) -> bool` checks if entire filter tree is in the supported subset
3. If yes: serialize AST to compact binary format, pass to C++ `jx_eval_filter(ast_bytes, json_buf, json_len) → output_bytes`
4. C++ evaluator walks AST + simdjson DOM simultaneously, writes output JSON directly
5. If no: current path (DOM → Value → Rust eval)

```
                    ┌─ can_eval_in_cpp? ── YES ──→ C++ eval on DOM → raw bytes (60-127x)
NDJSON line ───→ ───┤
                    └─ NO ──→ dom_to_value → Rust eval → serialize (2.8x)
```

### Pros
- Zero FFI overhead during evaluation — everything stays in C++, compiler inlines
- Closest to what fast paths already do, just generalized
- All supported filters get near fast-path speed (60-100x+ range)
- Clear boundary: `can_eval_in_cpp` false → fall back cleanly

### Cons
- Two evaluators to maintain (C++ hot path, Rust fallback)
- jq generator semantics (0+ outputs per input) need careful C++ callback/buffer implementation
- AST serialization across FFI
- Risk of semantic drift between evaluators
- C++ memory safety risks

### Honest assessment
Approach C raises the floor dramatically for supported filters (~60-70% of real-world usage). But **unsupported filters still fall back to the 2.8x Rust path**. Complex filters using `reduce`, `def`, `foreach`, variable binding, string interpolation, etc. remain at the current floor. The true floor only rises if you can guarantee users never hit the Rust fallback on NDJSON, which isn't realistic.

### Effort
High. ~2000-3000 lines of new C++ code, ~300 lines of Rust glue. Significant testing for semantic equivalence.

---

## What would actually raise the floor across the board

Neither A nor C alone guarantees a higher floor for ALL filters. The true bottleneck is the per-core overhead of DOM → flat tokens → heap-allocated Value tree, which affects every filter that falls back to the Rust evaluator.

### Option: Faster Value construction (complements A or C)

The flat token buffer from C++ already contains all the data. Three ways to make Value construction cheaper:

**1. Arena-allocated Value tree.** Replace per-value heap allocations with a bump allocator per NDJSON line. Currently: ~60+ allocations per GH Archive line (strings, vecs, Rc wrappers). With arena: 1 allocation, everything bump-allocated. Bump allocation is essentially pointer increment — could be 3-5x faster for Value construction alone.

**2. Zero-copy strings from flat buffer.** The flat token buffer already contains string bytes. Instead of copying them into `String`, reference them with `&str` (the flat buffer outlives evaluation for a single line). Eliminates the largest allocation source.

**3. Combined: zero-copy FlatValue.** Don't decode the flat buffer into Value at all. Instead, a `FlatValue` type holds an offset into the flat buffer and navigates lazily:
```rust
struct FlatValue<'a> {
    buf: &'a [u8],
    offset: usize,
}
```
- `.type_tag()` → read tag byte
- `.get_field("name")` → scan key-value pairs, return FlatValue at value offset
- `.as_string()` → return &str into buffer (zero-copy)
- `.length()` → read count from array/object start tag
- `.to_value()` → materialize to owned Value only when needed

This eliminates the flat → Value step entirely for read-only evaluation. Combined with Approach A's lazy DOM access, you get: one FFI call (dom_to_flat), then zero-allocation navigation from Rust. Even filters touching every field avoid heap allocations.

**Expected impact on floor:** Arena or FlatValue could improve per-core speed by 2-4x. With 12 cores: floor rises from `12/4 = 3x` to `12/1.5 ≈ 8x` or better. This is the path to a guaranteed 4-8x floor for ALL filters.

**Effort:** Medium-high. Arena allocation is ~200-400 lines. FlatValue is ~400-600 lines plus evaluator refactor to handle the lifetime. Can be done incrementally.

---

## Won't Do: Full C++ evaluator

**Idea:** Move the entire jq evaluator to C++, making Rust just the CLI wrapper.

**Why not:**
- The Rust evaluator is 3600 lines covering 34 AST node types, 164 builtins, variable scoping with closures, generator semantics via callbacks, pattern matching with destructuring, label/break control flow, try/catch error propagation, and path-based structural updates. Reimplementing all of this in C++ is essentially writing a second jq.
- jq itself is written in C. If we wanted a C/C++ evaluator, we'd just be rebuilding jq with simdjson as the parser — and jq's evaluator isn't the bottleneck (the parser is).
- The Rust evaluator already passes 91% of jq's test suite and 98.5% feature coverage. Throwing that away to rewrite in C++ trades known-working code for a long development effort with no architectural advantage over Approach C.
- qj's value proposition is Rust + simdjson + parallelism. Going full C++ loses the safety benefits of Rust for the complex language features that benefit from it most.

---

## Recommendation

To raise the floor **across the board**:

1. **Start with faster Value construction** (arena allocation or FlatValue). This improves per-core speed for ALL filters regardless of complexity. Target: floor from 2.8x → 4-8x.

2. **Then add Approach A (Lazy Value)** on top. Selective filters that touch few fields jump further (8-20x+). Full-document filters stay at the improved 4-8x floor from step 1.

3. **Consider Approach C later** if the floor needs to go to 20x+ for common filters. The mini C++ evaluator gets supported filters to near fast-path speed (60-100x) but doesn't help the remaining Rust fallback cases.

Steps 1 and 2 are complementary and can be done incrementally. Step 3 is a larger investment with diminishing returns given 1+2.
