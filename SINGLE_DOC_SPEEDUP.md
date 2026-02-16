# Single-Document JSON Speedup Plan

## Current State (after Phase 2)

Phase 1 (flat eval for single-doc) and Phase 2 (DOM tape walk) are complete. Parse times are significantly reduced for all single-doc filters.

**Note:** `--debug-timing` uses the On-Demand parse path (`dom_parse_to_value`), not the production DOM tape walk path. It does NOT reflect actual production performance. Use `hyperfine` for accurate benchmarks.

Benchmarks on 49MB `large_twitter.json` (hyperfine, vs jq):

| Filter | qj | jq | Speedup |
|--------|----|----|---------|
| `length` (passthrough) | 34ms | 361ms | **10.6x** |
| `map({user, text})` | 94ms | 367ms | **3.9x** |
| `reduce .[] as $x (0; .+1)` | 172ms | 369ms | **2.15x** |

Before Phase 2 (On-Demand flat buffer):

| Filter | qj (before) | Speedup (before) |
|--------|-------------|-----------------|
| `map({user, text})` | 124ms | 2.55x |
| `reduce .[] as $x (0; .+1)` | 224ms | 1.65x |

**Phase 2 raised the floor:** map went from 2.55x to 3.9x, reduce from 1.65x to 2.15x.

## What "Parse" Actually Means

The flat buffer construction is the bottleneck. It has two stages:

### 1. SIMD parse (~28ms for 49MB)
simdjson tokenizes JSON bytes into its internal representation (DOM tape). This is what the `length` passthrough uses via `dom::parser`, and it runs at ~1.7 GB/s — near the theoretical limit for JSON validation + structural indexing.

### 2. Flat buffer construction
Recursively visits every value in the document and emits flat buffer tokens.

**On-Demand path** (`flatten_ondemand()`, ~78ms for 49MB): Uses simdjson On-Demand API which re-tokenizes, re-unescapes strings, and re-parses numbers — work the DOM parse already did.

**DOM tape walk** (`walk_element()`, ~45ms for 49MB): Uses pre-indexed DOM tape with strings already unescaped and numbers already parsed. A parallel cursor into the original JSON extracts raw number text for literal preservation (e.g., `75.80` stays `75.80`). ~1.7x faster than On-Demand.

### Why `length` is so much faster
The `length` passthrough calls `dom::parser` directly (28ms), navigates to the target field, calls `.size()`, and returns. It never builds a flat buffer or Value tree. This is the speed floor for any approach that avoids full-document traversal.

## Completed Phases

### ~~Phase 1: Use FlatValue for single-doc~~ ✅ Done

Eval time reduced from 135ms to 1-7ms. Parse became the bottleneck.

### ~~Phase 2: Faster flat buffer via DOM tape walk~~ ✅ Done

**Implementation:** New `jx_dom_to_flat_via_tape()` in `bridge.cpp` + `walk_element()` recursive tape walker. Uses DOM API's pre-indexed tape instead of On-Demand API for flat buffer construction. A parallel cursor advances through the original JSON in lockstep with the tape walk to extract raw number text.

**Results:** Parse time reduced ~30% for flat buffer path. Combined with faster Value tree construction via `dom_parse_to_value_fast()`, all single-doc filters benefit.

**Edge cases handled:**
- Big integers beyond u64 range: DOM parser returns NUMBER_ERROR/BIGINT_ERROR. Falls back to On-Demand path automatically.
- `fromjson` with arbitrary user strings: Uses the On-Demand path (`dom_parse_to_value`) since the DOM parser handles some malformed inputs differently.
- Number literal preservation: Parallel cursor extracts raw text from original JSON (e.g., `75.80`, `1e2`).

## Proposed Future Phases

### Phase 3: Lazy flat buffer — only flatten accessed subtrees

**Problem:** Both On-Demand and DOM tape walk visit every value in the document even when the filter only touches a few fields.

**Proposal:** Flatten lazily — emit structural tokens upfront, defer leaf values as byte offsets into the original JSON, flatten on access.

**Risk:** Per-value resolution cost via simdjson FFI (~5μs each) makes this SLOWER for materializing filters (map/reduce that touch all elements). Only helps selective filters like `select(.field == "val")`.

**Estimated improvement:** Proportional to filter selectivity. For `map({type, actor})` with 20 fields but only 2 accessed: 3-5x faster parse. For identity: no improvement.

### ~~Phase 4: DOM passthrough for `map(.field)` / `.[] | .field`~~ ✅ Done

**Implementation:** New `jx_dom_array_map_field()` in `bridge.cpp`. Parses with DOM, navigates an optional prefix field chain, verifies target is an array, then iterates elements extracting a per-element field chain via `at_key()` + `simdjson::to_string()`. Supports both `map` (wrap in `[...]`) and `.[]` (one value per line) modes.

**Patterns detected (single field):**
- `map(.field)` — bare map
- `.[] | .field` — bare iterate
- `.prefix | map(.field)` — prefixed map
- `.prefix[] | .field` — prefixed iterate

**Patterns detected (multi-field obj shorthand):**
- `map({f1, f2})` — bare map
- `.[] | {f1, f2}` — bare iterate
- `.prefix | map({f1, f2})` — prefixed map
- `.prefix[] | {f1, f2}` — prefixed iterate

Multi-field obj only supports simple shorthand `{f1, f2}` where key == field name, single-level.

**Results on 49MB `large_twitter.json`** (hyperfine, vs jq):

| Filter | qj (before) | qj (passthrough) | jq | Speedup |
|--------|-------------|-------------------|-----|---------|
| `.statuses \| map(.user)` | 153ms | **59ms** | 738ms | **12.5x** |
| `.statuses[] \| .user` | 203ms | **75ms** | 647ms | **11x** |
| `.statuses \| map({user, text})` | 162ms | **58ms** | 751ms | **12.9x** |

**On bare array (51MB):**

| Filter | qj (passthrough) | jq | Speedup |
|--------|-------------------|-----|---------|
| `map(.user)` | **54ms** | 652ms | **12x** |
| `.[] \| .user` | **55ms** | 648ms | **12x** |

Passthrough requires `-c` (compact output) and is disabled with `-r` (raw output).
Non-object/non-null elements cause fallback to normal evaluator (matches jq error semantics).

### ~~Phase 5: Scalar builtin passthroughs~~ ✅ Done

**Implementation:** Added `type`, `has("key")`, and `keys_unsorted` passthroughs.

- **`type` / `.field | type`:** First-byte inspection of raw JSON (no C++ function needed). `{` → object, `[` → array, `"` → string, `t`/`f` → boolean, `n` → null, digit/`-` → number.
- **`has("key")` / `.prefix | has("key")`:** New `jx_dom_field_has()` / `jx_dom_field_has_reuse()` C++ functions. Navigate prefix, check `at_key(key)`, return 1/0. Non-object → fallback (-2).
- **`keys_unsorted` / `.field | keys_unsorted`:** Added `int sorted` parameter to `jx_dom_field_keys()` / `jx_dom_field_keys_reuse()`. When `sorted == 0`, skip `std::sort()`.

**NDJSON fix:** `keys_unsorted` was previously mapped to the same `Keys` variant as `keys`, which always sorted. Fixed by adding `sorted: bool` to the `Keys` variant. Also added `Type` and `Has` NDJSON fast path variants.

### ~~Phase 6: Iterate + builtin passthroughs~~ ✅ Done

**Implementation:** New `jx_dom_array_map_builtin()` C++ function handles all iterate+builtin patterns via a single entry point with `int op` parameter:

- **op=0 (length):** Per-element `.size()` call. Object/array → count, string → `.get_string().length()`, null → 0. Bool/number → fallback (-2).
- **op=1 (keys):** Per-element key extraction + optional sort. Object → collect+sort, array → `[0,1,...N-1]`. Non-object/non-array → fallback (-2).
- **op=2 (type):** Per-element type string. Always succeeds (every DOM element has a type).
- **op=3 (has):** Per-element `at_key(arg)` probe. Object → true/false. Non-object → fallback (-2).

All support optional prefix (`.prefix | map(length)`, `.prefix[] | length`) and wrap_array semantics (map wraps in `[...]`, iterate emits one per line).

**Patterns detected:**
- `map(length)` / `.[] | length` / `.prefix | map(length)` / `.prefix[] | length`
- `map(keys)` / `.[] | keys` / `map(keys_unsorted)` / `.[] | keys_unsorted`
- `map(type)` / `.[] | type`
- `map(has("f"))` / `.[] | has("f")`

### ~~Phase 7: Syntactic variant detection~~ ✅ Done

**Implementation:** Detection-only changes in `passthrough_path()` — no new C++ functions.

- **`[.[] | .field]` → `ArrayMapField { wrap_array: true }`** — via `Filter::ArrayConstruct(Pipe(Iterate, field_chain))`
- **`[.[] | {f1, f2}]` → `ArrayMapFieldsObj { wrap_array: true }`** — via `Filter::ArrayConstruct(Pipe(Iterate, ObjectConstruct(...)))`
- **`[.[] | builtin]` → `ArrayMapBuiltin { wrap_array: true }`** — via `Filter::ArrayConstruct(Pipe(Iterate, Builtin(...)))`

All reuse existing C++ functions and dispatch paths.

### Possible Future Phases

**Lazy flatten (only accessed subtrees):** Both On-Demand and DOM tape walk visit every value even when the filter only touches a few fields. Lazy flattening would defer leaf values as byte offsets. Risk: per-value resolution cost via FFI makes this SLOWER for materializing filters. Only helps selective filters.

## Summary

| Phase | What | Floor impact | Effort |
|-------|------|-------------|--------|
| ~~1~~ | ✅ Flat eval for single-doc | Eval 135ms → 7ms | Done |
| ~~2~~ | ✅ DOM tape walk | map 2.55x → 3.9x, reduce 1.65x → 2.15x | Done |
| ~~4~~ | ✅ DOM passthrough for map/iterate + {f1, f2} | 12-13x vs jq | Done |
| ~~5~~ | ✅ Scalar builtins (type, has, keys_unsorted) | Avoids flat buffer for scalar results | Done |
| ~~6~~ | ✅ Iterate + builtins (map(length), map(type), etc.) | ~12x vs jq for iterate+builtin | Done |
| ~~7~~ | ✅ Syntactic variants ([.[]|.f] = map(.f)) | Catches more patterns, no new C++ | Done |
