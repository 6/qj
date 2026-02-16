# Plan: Phase 4 — DOM Passthrough for Array Iteration Patterns

## Context

After Phase 2 (DOM tape walk), single-doc JSON performance on 49MB `large_twitter.json`:
- `length` passthrough: **34ms** (DOM parse only, no flat buffer)
- `map({user, text})`: **94ms** (tape walk → flat buffer → Value tree → eval)
- `reduce .[] as $x (0; .+1)`: **172ms** (tape walk → Value tree → eval)

The gap between `length` (34ms) and `map` (94ms) is the flat buffer + Value tree construction (~60ms). Phase 4 adds DOM passthrough paths for common array iteration patterns, skipping flat buffer and Value tree entirely — same strategy `length` already uses.

**Target patterns:**
1. `map(.field)` / `.[] | .field` — iterate root array, extract field per element
2. `map({k: .f, ...})` / `.[] | {k: .f, ...}` — iterate root array, multi-field object construction
3. `select(.field == "val")` — check field equality, output root or nothing

**Expected:** ~35-40ms for array patterns (vs 94ms current = ~2.5x improvement on the filter).

## Files to Change

### 1. `src/filter/mod.rs` — New PassthroughPath variants + detection

Add three variants to `PassthroughPath` enum (line 234):

```rust
/// `map(.field)` or `.[] | .field` — iterate root array, extract field per element.
/// wrap_array: true for map (output JSON array), false for .[] (one per line).
ArrayMapField { fields: Vec<String>, wrap_array: bool },
/// `map({k: .f, ...})` or `.[] | {k: .f, ...}` — iterate root array, construct objects.
ArrayMapObj { entries: Vec<(String, Vec<String>)>, wrap_array: bool },
/// `select(.field == "val")` — check field equals literal, output root or nothing.
SelectEq { fields: Vec<String>, op: CmpOp, literal_json: Vec<u8> },
```

All three return `requires_compact() -> true` (produce raw JSON bytes, no formatter).

Extend `passthrough_path()` (line 292) with detection for:
- `Builtin("map", [field_chain])` → `ArrayMapField { wrap_array: true }`
- `Builtin("map", [ObjectConstruct([...])])` → `ArrayMapObj { wrap_array: true }`
- `Pipe(Iterate, field_chain)` → `ArrayMapField { wrap_array: false }`
- `Pipe(Iterate, ObjectConstruct([...]))` → `ArrayMapObj { wrap_array: false }`
- `Select(Compare(field_chain, Eq|Ne, Literal(...)))` → `SelectEq`

Reuse existing `collect_field_chain()` (line 259). Add new helpers:
- `detect_obj_construct_entries(filter) -> Option<Vec<(String, Vec<String>)>>` — decompose ObjectConstruct into (key_name, field_chain) pairs (adapted from `try_multi_field_obj` in ndjson.rs line 1217)
- `detect_select_passthrough(inner) -> Option<PassthroughPath>` — detect select + eq/ne patterns

Also add `serialize_literal()` here (currently private in `ndjson.rs` line 735). Make it `pub(crate)` for shared use by both passthrough detection and NDJSON fast path. Have ndjson.rs call the shared version.

### 2. `src/simdjson/bridge.cpp` — Three new C++ functions (~120 LOC)

**`jx_dom_array_map_field`** — Iterate root array, extract one field chain per element:
- Parse with `dom::parser`, verify root is ARRAY (return -2 if not)
- For each element: navigate field chain via `at_key()`, serialize with `simdjson::to_string()`
- Missing field → `"null"`
- `wrap_array=1`: output `[v1,v2,...,vN]`, `wrap_array=0`: output `v1\nv2\n...\n`

```cpp
int jx_dom_array_map_field(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    int wrap_array,
    char** out_ptr, size_t* out_len);
```

**`jx_dom_array_map_fields_obj`** — Iterate root array, batch-extract N fields, construct objects:
- Same parse/iterate pattern
- For each element and each key: navigate field chain, serialize value
- Construct `{"key1":val1,"key2":val2,...}` per element
- Key names are pre-JSON-escaped strings (passed with quotes from Rust)

```cpp
int jx_dom_array_map_fields_obj(
    const char* buf, size_t len,
    const char* const* keys, const size_t* key_lens, size_t num_keys,
    const char* const* const* chains,
    const size_t* const* chain_lens,
    const size_t* chain_counts,
    int wrap_array,
    char** out_ptr, size_t* out_len);
```

**`jx_dom_select_eq`** — Check field == literal, output minified root or nothing:
- Parse with `dom::parser`, navigate field chain
- Serialize field value with `to_string()`, compare bytes with literal
- Handle ambiguous cases (float normalization, escape differences): return -2 to fall back
- Match + `op==0` (Eq): output via `simdjson::minify()`
- No match: `out_len=0`, `out_ptr=nullptr`

```cpp
int jx_dom_select_eq(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    const char* literal, size_t literal_len,
    int op,  // 0=Eq, 1=Ne
    char** out_ptr, size_t* out_len);
```

All functions follow existing conventions: return 0=success, -1=error, -2=fallback. Heap-allocate output via `new char[]`, caller frees with `jx_minify_free`.

### 3. `src/simdjson/ffi.rs` — FFI declarations (~15 LOC)

Add `extern "C"` declarations for the three new functions.

### 4. `src/simdjson/bridge.rs` — Rust wrappers (~60 LOC)

Safe wrappers following existing patterns:
- `dom_array_map_field(buf, json_len, fields, wrap_array) -> Result<Option<Vec<u8>>>`
- `dom_array_map_fields_obj(buf, json_len, entries, wrap_array) -> Result<Option<Vec<u8>>>`
- `dom_select_eq(buf, json_len, fields, literal_json, is_eq) -> Result<Option<Option<Vec<u8>>>>`

Return `Ok(None)` on rc==-2 (fallback). Export from `src/simdjson/mod.rs`.

### 5. `src/main.rs` — Dispatch + guard update (~40 LOC)

Add match arms in `try_passthrough()` (line 654) for the three new variants.

Update passthrough guard (line 342) to disable passthrough when `-r`/`--raw-output0` is active:
```rust
let passthrough = if cli.slurp || cli.raw_input || cli.sort_keys || cli.join_output
    || use_color || cli.ascii_output || cli.raw || cli.raw_output0
{ None } else { ... };
```

This prevents passthrough from emitting JSON-quoted strings when `-r` should strip quotes. Affects all passthrough paths equally (simplest, safest approach).

Update debug-timing labels for the new variants.

## Edge Cases

| Scenario | Behavior |
|----------|----------|
| Root not array (map/iterate) | C++ returns -2, falls back to normal pipeline |
| Field not found in element | Emits `"null"` (jq semantics) |
| Empty root array | `map` outputs `[]`, iterate outputs nothing |
| Non-object element in array | Field access returns null (matches jq `map(.f)` behavior) |
| Select: ambiguous number comparison (1.0 vs 1) | Returns -2, falls back |
| Select: string escape ambiguity | Returns -2, falls back |
| Select: object/array equality | Returns -2, falls back |
| `-r` flag | Passthrough disabled, normal pipeline handles raw output |
| `--sort-keys` | Passthrough disabled (existing guard) |
| Number normalization | `to_string()` normalizes (75.80→75.8), matches jq compact output |

## Implementation Order

1. **`serialize_literal` extraction** — Move to `src/filter/mod.rs` as `pub(crate)`, have ndjson.rs import it
2. **PassthroughPath variants + detection** — Enum, `requires_compact()`, detection in `passthrough_path()`. Unit tests.
3. **`jx_dom_array_map_field`** — C++ + FFI + Rust wrapper + dispatch + e2e tests for `map(.field)` and `.[] | .field`
4. **`jx_dom_array_map_fields_obj`** — C++ + FFI + Rust wrapper + dispatch + e2e tests for `map({k:.f})` and `.[] | {k:.f}`
5. **`jx_dom_select_eq`** — C++ + FFI + Rust wrapper + dispatch + e2e tests
6. **Passthrough guard update** — Add `cli.raw || cli.raw_output0`. Update debug-timing labels.

## Verification

1. `cargo fmt && cargo clippy --release -- -D warnings && cargo test` — all pass
2. Output equivalence against jq for each pattern (via `assert_jq_compat` in e2e tests)
3. `hyperfine --warmup 3` benchmarks on `large_twitter.json`:
   - `map(.user)` — expect ~35ms (vs current ~80ms)
   - `map({user: .user, text: .text})` — expect ~35-40ms (vs current 94ms)
   - `select(.type == "PushEvent")` — expect ~30ms (vs current ~70ms)
4. Verify fallback works: `map(.field)` on non-array input, `select` with float comparison
5. Verify `-r` flag disables passthrough and output is correct
