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

### Phase 4: Extend passthrough/DOM fast paths

Reuse the existing DOM-based C++ functions for more single-doc patterns:
- `map(.field)` → DOM iterate array + extract field per element
- `.[] | {f1, f2}` → reuse `find_fields_raw` per array element
- `select(.field == "val")` → DOM navigate + compare

These skip the flat buffer entirely for recognized patterns (like `length` already does at 28ms).

**Expected:** 5-10x for matching patterns, but only raises the floor for those specific patterns.

## Summary

| Phase | What | Floor impact | Effort |
|-------|------|-------------|--------|
| ~~1~~ | ✅ Flat eval for single-doc | Eval 135ms → 7ms | Done |
| ~~2~~ | ✅ DOM tape walk | map 2.55x → 3.9x, reduce 1.65x → 2.15x | Done |
| 3 | Lazy flatten (only accessed subtrees) | Only helps selective filters | High |
| 4 | More DOM passthrough patterns | Skip parse entirely for patterns | Medium |

Phase 2 raised the floor for ALL single-doc filters. Phase 3 only helps selective filters and may hurt materializing ones. Phase 4 is pattern-specific but gives the biggest absolute wins where it applies.
