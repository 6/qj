# Single-Document JSON Speedup Plan

## Problem

On evaluator-bound single-file JSON workloads, qj is only 2-2.5x faster than jq. The SIMD parsing advantage (12x) is eaten by a slower evaluator.

Timing breakdown on 49MB `large_twitter.json`:

| Filter | qj parse | qj eval | jq total | qj speedup |
|--------|----------|---------|----------|------------|
| `length` | ~31ms | ~0ms | 370ms | 12x |
| `map({user, text})` | ~31ms | ~135ms | 416ms | 2.5x |
| `reduce` | ~31ms | ~135ms | 376ms | 2.3x |

The evaluator is the bottleneck. NDJSON doesn't have this problem because it uses fast paths and FlatValue — single-doc JSON uses neither.

## Root Cause

Single-doc JSON (`src/main.rs:810-813`) always:
1. `dom_parse_to_value()` — builds complete Rust Value tree (all fields heap-allocated)
2. `eval_filter_with_env()` — evaluates on heap-allocated tree
3. `write_value()` — serializes output

NDJSON has three faster paths that single-doc doesn't use:

### 1. C++ Fast Paths (used by NDJSON, not single-doc)
- `FieldChain`: extract raw bytes via C++ DOM, no Value tree
- `SelectEq`: byte-compare on extracted field, raw line passthrough
- `MultiFieldObj`: batch field extraction, manual JSON construction
- Detected in `src/parallel/ndjson.rs:225-251`

### 2. FlatValue Lazy Evaluation (used by NDJSON fallback, not single-doc)
- `dom_parse_to_flat_buf()` — parse to flat token buffer (no Value tree)
- `eval_flat()` — lazy evaluator, only materializes accessed fields
- Zero-copy navigation of objects/arrays
- Code: `src/flat_eval.rs`, `src/flat_value.rs`
- Already proven: moved NDJSON evaluator-bound floor from 2.8x to 26.5x

### 3. PassthroughPath (partially used by single-doc)
- Single-doc only detects 3 patterns: Identity, FieldLength, FieldKeys
- NDJSON detects 10+ patterns
- Code: `src/filter/mod.rs:291-314`

## Proposed Changes (ranked by effort/impact)

### Phase 1: Use FlatValue for single-doc (50-100 LOC)

Change `process_padded()` in `src/main.rs` to use `dom_parse_to_flat_buf` + `eval_flat` instead of `dom_parse_to_value` + `eval_filter_with_env`.

FlatValue already handles: Field, Pipe, ObjectConstruct, ArrayConstruct, Iterate, Select, and builtins (length, type, keys, not). Falls back to regular eval for unsupported filters.

**Expected: 1.5-3x speedup on evaluator-bound single-doc filters.**

### Phase 2: Extend passthrough detection (100-200 LOC)

Reuse NDJSON fast-path patterns (SelectEq, MultiFieldObj, etc.) for single-doc JSON. Emit raw JSON bytes instead of Value tree.

**Expected: 5-20x for selective single-doc filters on large objects.**

### Phase 3: Array element fast-paths (300-500 LOC)

For `map(f)` or `[.[] | f]` on large arrays:
1. Parse once with simdjson DOM
2. Iterate array elements via C++ handle
3. Apply fast-path extraction per element (reuse DomParser::find_fields_raw)
4. Emit results wrapped in `[...]`

**Expected: 10-50x for array iteration with simple field extraction.**

## Architecture Notes

- `FlatBuffer` owns C++-allocated memory, freed on Drop. For single-doc, one buffer for whole file — must outlive evaluation (same as NDJSON per-line).
- `eval_flat()` returns `Value` on fallback, so it's always correct.
- Array iteration would need output buffering for `[elem, elem, ...]` format (NDJSON emits one per line).
