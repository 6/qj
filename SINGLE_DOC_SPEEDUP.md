# Single-Document JSON Speedup Plan

## Current State (after Phase 1)

Phase 1 (flat eval for single-doc) is complete. Eval time dropped dramatically, but parse now dominates at 90-99% of total time for all filters.

Timing breakdown on 49MB `large_twitter.json` (`--debug-timing`):

| Filter | Parse | Eval | Output | Total | vs jq |
|--------|-------|------|--------|-------|-------|
| `length` (passthrough) | 28ms | — | — | 28ms | 10.6x |
| `map({user, text})` | 113ms | 7ms | 4ms | 124ms | 2.55x |
| `reduce .[] as $x (0; .+1)` | 105ms | 1ms | 0ms | 107ms | 1.65x |

On 1.1GB `gharchive.json`:

| Filter | Parse | Eval | Output | Total | vs jq |
|--------|-------|------|--------|-------|-------|
| `map({type, actor})` | 2452ms | 181ms | 29ms | 2668ms | 4.73x |
| `reduce .[] as $x (0; .+1)` | 2467ms | 61ms | 0ms | 2534ms | 1.65x |

**The evaluator is no longer the bottleneck. Parse is.**

## What "Parse" Actually Means

The 106ms "parse" for the flat buffer path (`dom_parse_to_flat_buf`) is really two things:

### 1. SIMD parse (~28ms for 49MB)
simdjson tokenizes JSON bytes into its internal representation. This is what the `length` passthrough uses via `dom::parser`, and it runs at ~1.7 GB/s — near the theoretical limit for JSON validation + structural indexing.

### 2. Flat buffer construction (~78ms for 49MB)
`flatten_ondemand()` in `bridge.cpp:335-397` recursively visits **every value** in the document:
- Uses simdjson On-Demand API (`ondemand::parser::iterate()`)
- For each value: determines type, extracts data (strings copied, numbers parsed + raw text preserved)
- Emits tokens to a `std::vector<uint8_t>` (TAG_STRING, TAG_INT, TAG_ARRAY_START, etc.)
- This is O(document) regardless of what the filter needs

The flat buffer construction is 2.8x more expensive than the SIMD parse itself. For `map({type, actor})` on 1.1GB, ~700ms of the 2.5s parse is SIMD and ~1.8s is flattening.

### Why `length` is so much faster
The `length` passthrough calls `dom::parser` directly (28ms), navigates to the target field, calls `.size()`, and returns. It never builds a flat buffer or Value tree. This is the speed floor for any approach that avoids full-document traversal.

## Proposed Changes (revised)

### ~~Phase 1: Use FlatValue for single-doc~~ ✅ Done

Eval time reduced from 135ms to 1-7ms. Parse is now the bottleneck.

### Phase 2: Faster flat buffer via DOM tape walk

**Problem:** `flatten_ondemand()` uses the On-Demand API which iterates tokens sequentially. The DOM API builds a pre-indexed tape in ~28ms that's cache-friendly and supports random access.

**Proposal:** Replace `jx_dom_to_flat()` to use DOM API instead of On-Demand:
1. Call `dom::parser::parse()` to build the tape (~28ms for 49MB)
2. Walk the DOM tape to emit flat buffer tokens
3. DOM tape is pre-indexed: arrays/objects have element counts, strings have lengths — no need to count during traversal
4. DOM tape is contiguous memory — better cache locality than On-Demand's token-by-token iteration

**Why this helps:** The DOM tape already stores type tags, string lengths, and structural boundaries. Converting tape → flat buffer should be mostly memcpy-like, much cheaper than On-Demand's per-value type dispatch + `get_string()` / `get_number()` calls.

**Risk:** DOM API doesn't preserve raw number text (it parses to double/int64). We currently preserve raw text for number literal fidelity (e.g., `75.80` stays `75.80`). Would need to either:
- Accept normalized numbers on this path (minor output difference)
- Hybrid: use DOM tape for structure + On-Demand for number raw text
- Store raw byte offsets from the original JSON during DOM walk

**Estimated improvement:** 1.5-2.5x faster flat buffer construction → total parse from 106ms to ~50-60ms for 49MB.

**Effort:** ~100-200 LOC in bridge.cpp.

### Phase 3: Lazy flat buffer — only flatten accessed subtrees

**Problem:** `flatten_ondemand()` visits every value in the document even when the filter only touches a few fields. For `map({type, actor})` on objects with 20+ fields, we flatten all fields but only read 2.

**Proposal:** Instead of flattening the entire document upfront, flatten lazily:
1. SIMD parse with On-Demand (keep current API)
2. For the top-level structure, emit structural tokens (ARRAY_START, OBJECT_START, counts)
3. For child values, store byte offsets into the original JSON instead of copying data
4. When `eval_flat` accesses a value, flatten just that subtree on demand

This is a deeper change to `FlatBuffer`/`FlatValue`:
- `FlatValue` would need a "deferred" token type that points back to the original padded JSON
- Accessing a deferred value triggers a targeted `flatten_ondemand()` on just that region
- Already-flattened values stay as-is (amortize cost over repeated access)

**Why this helps:** For `map({type, actor})` on an array of 500K objects with 20 fields each, current approach flattens 10M fields. Lazy approach flattens only 1M fields (2 per object). That's a 10x reduction in flattening work.

**Risk:** More complex FlatValue representation. FFI boundary changes. Need to keep the original padded buffer alive alongside the flat buffer.

**Estimated improvement:** Proportional to filter selectivity. For `map({type, actor})`: 3-5x faster parse. For `.` (identity): no improvement (must flatten everything).

**Effort:** ~300-500 LOC across bridge.cpp, flat_value.rs, flat_eval.rs.

### Phase 4: Extend passthrough/DOM fast paths

Reuse the existing DOM-based C++ functions for more single-doc patterns:
- `map(.field)` → DOM iterate array + extract field per element
- `.[] | {f1, f2}` → reuse `find_fields_raw` per array element
- `select(.field == "val")` → DOM navigate + compare

These skip the flat buffer entirely for recognized patterns (like `length` already does at 28ms).

**Expected:** 5-10x for matching patterns, but only raises the floor for those specific patterns.

**Effort:** ~100-200 LOC in bridge.cpp + main.rs.

## Summary

| Phase | What | Floor impact | Effort |
|-------|------|-------------|--------|
| ~~1~~ | ✅ Flat eval for single-doc | Eval 135ms → 7ms | Done |
| 2 | DOM tape → flat buffer | Parse 106ms → ~55ms (all filters) | Medium |
| 3 | Lazy flatten (only accessed subtrees) | Parse proportional to selectivity | High |
| 4 | More DOM passthrough patterns | Skip parse entirely for patterns | Medium |

Phase 2 raises the floor for everything. Phase 3 raises it further for selective filters. Phase 4 is pattern-specific but gives the biggest absolute wins where it applies.
