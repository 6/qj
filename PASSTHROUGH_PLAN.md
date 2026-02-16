# Plan: Phase 4 — DOM Passthrough for `map(.field)` / `.[] | .field`

## Context

After Phase 2 + flat eval reduce + Env scope chain, benchmarks on 49MB `large_twitter.json` (hyperfine, vs jq):

| Filter | qj | jq | Speedup |
|--------|----|----|---------|
| `length` (passthrough) | 34ms | 361ms | **10.6x** |
| `reduce .[] as $x (0; .+1)` | 88ms | 366ms | **4.1x** |
| `map({user, text})` | 88ms | 367ms | **4.2x** |
| `reduce .statuses[] as $x (0; . + $x.id)` | 157ms | 370ms | **2.4x** |

The `length` passthrough achieves 34ms by skipping flat buffer + Value tree entirely. Phase 4 extends this to the most common array iteration pattern: extracting a single field per element.

**Target:** `map(.field)` and `.[] | .field` via DOM passthrough. Expected ~35ms (vs 88ms).

## Files to Change

### 1. `src/filter/mod.rs` — New variant + detection (~30 LOC)

Add to `PassthroughPath` enum (line 234):
```rust
/// `map(.field)` or `.[] | .field` — iterate root array, extract field per element.
/// wrap_array: true for map (output `[v1,v2,...]`), false for .[] (output `v1\nv2\n...`).
ArrayMapField { fields: Vec<String>, wrap_array: bool },
```

`requires_compact() -> true` for this variant.

Extend `passthrough_path()` (line 292):
- `Builtin("map", [arg])` where `collect_field_chain(arg)` succeeds → `ArrayMapField { wrap_array: true }`
- `Pipe(Iterate, rhs)` where `collect_field_chain(rhs)` succeeds → `ArrayMapField { wrap_array: false }`

### 2. `src/simdjson/bridge.cpp` — One new C++ function (~40 LOC)

```cpp
int jx_dom_array_map_field(
    const char* buf, size_t len,
    const char** fields, const size_t* field_lens, size_t field_count,
    int wrap_array,
    char** out_ptr, size_t* out_len);
```

- Parse with `dom::parser`, verify root is ARRAY (return -2 if not)
- For each element: navigate field chain via `at_key()`, serialize with `simdjson::to_string()`
- Missing field → `"null"`
- `wrap_array=1`: output `[v1,v2,...,vN]`; `wrap_array=0`: output `v1\nv2\n...\n`
- Return 0=success, -1=error, -2=not-an-array (fallback)
- Output freed via `jx_minify_free`

### 3. `src/simdjson/ffi.rs` — FFI declaration (~5 LOC)

### 4. `src/simdjson/bridge.rs` + `mod.rs` — Rust wrapper (~15 LOC)

`dom_array_map_field(buf, json_len, fields, wrap_array) -> Result<Option<Vec<u8>>>`

Returns `Ok(None)` on rc==-2 (not an array, fall back).

### 5. `src/main.rs` — Dispatch + guard (~15 LOC)

Add match arm in `try_passthrough()` (line 654).

Add `cli.raw || cli.raw_output0` to the passthrough guard (line 342) so `-r` disables passthrough (it emits JSON-quoted strings).

## Edge Cases

- Root not array → C++ returns -2, falls back to normal pipeline
- Field not found → `"null"` (jq semantics)
- Non-object element → field access returns null (matches `jq map(.f)` on non-objects)
- Empty array → `map` outputs `[]`, iterate outputs nothing
- Number normalization → `to_string()` normalizes (matches jq `-c` output)

## Verification

1. `cargo fmt && cargo clippy --release -- -D warnings && cargo test`
2. e2e tests with `assert_jq_compat` for: `map(.name)`, `map(.a.b)`, `.[] | .name`, empty array, missing field, non-array fallback
3. `hyperfine --warmup 3` on `large_twitter.json`: `map(.user)`, `.[] | .user`
