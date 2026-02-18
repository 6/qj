# Test Suite Improvement Plan

> Prompted by the `null * number` bug ([610b937](https://github.com/6/qj/commit/610b937)): qj returned `null` for `null * 1` instead of erroring like jq. The bug lived in `arith_values()` in `src/filter/value_ops.rs` — an incorrect `(Value::Null, _) | (_, Value::Null) => Ok(Value::Null)` arm in the `Mul` match. No test caught it because no test ever compared `null * number` output between qj and jq.

## Why it was missed

The bug exposed several structural gaps in the test suite:

1. **jq's own test suite doesn't cover it.** The vendored `jq.test` (2500+ lines) has only 2 null arithmetic cases, both for addition (`null+.`, `.+null`). No `null *`, `null /`, `null -`, or `null %`. The conformance runner (`tests/jq_conformance.rs`) could not catch what jq.test doesn't test.

2. **E2e tests had no null arithmetic coverage.** 599 `assert_jq_compat` calls across `tests/e2e.rs`, ~110 involving null, but none for `null * number` or any other non-addition null arithmetic. The `jq_compat_arithmetic` test only checks `x + y`, `x + x`, and `x * 2` with numeric inputs.

3. **Zero unit tests for `arith_values()`.** The function has ~150 lines of match arms across 5 operators, handles type coercion for 7 value types, and had no direct unit tests. Arithmetic unit tests in `src/filter/eval.rs` only cover array subtraction, object merge, float modulo, and integer division — nothing with null.

4. **Feature compat suite (`features.toml`) skips type edges.** Arithmetic operator tests use clean numeric inputs only. No null, bool, string, array, or object operands.

5. **Fuzz targets can't practically reach it.** `fuzz_eval` splits random bytes into JSON + filter — the probability of generating valid `null` JSON + a valid `null * 1` filter from random bytes is vanishingly small. `fuzz_ndjson_diff` only tests field access, select, and type filters — no arithmetic.

The common thread: **the suite is good at testing deliberately-implemented features but bad at catching silent behavioral divergence from jq on type edge cases.**

---

## 1. Exhaustive type-pair arithmetic tests

**Status:** Done ([3a52294](https://github.com/6/qj/commit/3a52294))
**Priority:** Highest — directly prevents the class of bug that caused 610b937
**Effort:** Small (single test function)
**Files:** `tests/e2e.rs`

### What was done

Added `jq_compat_exhaustive_arithmetic_type_pairs` — tests 14 representative values × 14 values × 5 operators = 980 combinations against jq, comparing both stdout and exit codes. Also added `jq_compat_arithmetic_edge_cases` for string repetition, string split, division by zero, object merge, array operations, null identity, and overflow promotion.

**Found 4 bug classes on first run:**
- Negative zero (`0 * -1` → `0` instead of `-0`)
- Float modulo (`10.5 % 3` → `1.5` instead of `1`, jq truncates to int)
- String split edge cases (`"hello" / ""` included boundary empties)
- Float modulo div-by-zero (`1 % 0.5` didn't error, 0.5 truncates to 0)

All four fixed in the same commit.

---

## 2. Property-based differential testing (jq oracle)

**Status:** Done ([406bee4](https://github.com/6/qj/commit/406bee4))
**Priority:** High — catches divergences that humans wouldn't think to test
**Effort:** Medium (new test file + grammar-aware generator)
**Files:** `tests/jq_differential.rs`

### What was done

Implemented `tests/jq_differential.rs` with 4 proptest suites (2000 cases each, `#[ignore]`):
- `differential_filter_vs_jq` — composite filters with pipes, try, if/then/else, reduce, variable binding, string interpolation
- `differential_arithmetic_vs_jq` — arithmetic with random scalars
- `differential_builtins_vs_jq` — 35+ nullary builtins + unary builtins with random inputs
- `differential_formats_vs_jq` — all format strings (`@json`, `@html`, `@uri`, `@sh`, `@csv`, `@tsv`, `@base64`, `@base64d`)

Grammar-aware generators:
- `arb_json_input()` — recursive (depth 2): scalars, arrays, objects with keys a-d
- `arb_filter()` — recursive (depth 3): pipes, comma, arithmetic, comparison, boolean ops, try, if/then/else, alternative, array/object construction, variable binding, reduce, string interpolation
- Uses `(0-N)` instead of bare `-N` to avoid CLI flag parsing issues
- Avoids integer-valued floats (known filter literal preservation limitation)

**Found 4 divergences on first run:**
1. `flatten` on objects — qj produced nothing, jq extracts values first (fixed in [3152f98](https://github.com/6/qj/commit/3152f98))
2. Format strings on non-strings — `@html`, `@uri`, `@sh`, `@base64`, `@base64d`, `@urid` silently dropped non-string inputs instead of calling tostring first (fixed in [3152f98](https://github.com/6/qj/commit/3152f98))
3. `@sh` type-specific semantics — jq only quotes strings, passes numbers/bools/null bare, space-joins arrays recursively, errors on objects (fixed in [3152f98](https://github.com/6/qj/commit/3152f98))
4. Filter literal preservation (`null + 100.0` → qj outputs `100`, jq outputs `100.0`) — deferred, deeper issue

### Known limitation

Filter literal float preservation (e.g., `100.0` in a filter expression) is not yet tracked through evaluation. The proptest generators work around this by avoiding integer-valued floats. This is a known qj limitation, not a test gap.

---

## 3. Grammar-aware `fuzz_eval` rewrite

**Status:** Done
**Priority:** Medium — makes existing fuzzer dramatically more effective
**Effort:** Medium (rewrite one fuzz target)
**Files:** `fuzz/fuzz_targets/fuzz_eval.rs`, `fuzz/Cargo.toml`

### What was done

Rewrote `fuzz_eval.rs` with `arbitrary::Arbitrary`-derived structured inputs:

- **`FuzzValue`** — maps to `Value` with depth-bounded recursion (max 3). Scalars use lookup tables: 15 interesting doubles (0.0, -0.0, NAN, INFINITY, MIN, MAX, EPSILON, etc.), 8 strings, 4 object keys.
- **`FuzzFilter`** — maps to `Filter` with depth-bounded recursion (max 3). 6 leaf variants (Identity, Field, Iterate, Literal, Builtin, Var) and 17 recursive variants (Pipe, Comma, Arith, Compare, BoolOp, Not, Neg, Try, Alternative, IfThenElse, ArrayConstruct, ObjectConstruct, Select, Bind, Reduce, StringInterp, Index). Builtins use a table of ~70 expressions parsed via `filter::parse()`.
- **`FuzzInput`** — top-level struct with `FuzzValue` + `FuzzFilter`, derives `Arbitrary`.

Every fuzz iteration constructs a valid `Value` + `Filter` AST directly, bypassing parsing entirely. 100% of iterations reach the evaluator.

### Results

- **700K executions in 60s** (~10.8K exec/s) — vs the old approach which spent >99% of time in parser rejection
- **2925 code coverage edges** reached
- **Zero crashes or timeouts** on first run
- `Recurse` (`..`) excluded from filter generation to avoid combinatorial explosion with nested `Bind`

### Design notes

- Depth bounded to 3 to prevent exponential blowup from nested `Bind(Iterate, Iterate)` patterns
- Output capped at 500 values per evaluation to bound slow cases
- Per-case timeout of 10s recommended: `cargo +nightly fuzz run fuzz_eval -s none -- -timeout=10`

---

## 4. Arithmetic unit tests for `arith_values()`

**Status:** Largely superseded by #1
**Priority:** Low — the exhaustive e2e test (#1) already covers all type-pair combinations against jq, and the fixes it prompted updated the existing unit tests too.

### Remaining value

Unit tests in `value_ops.rs` would still be useful for:
- **Faster feedback** — unit tests run in <1ms vs ~10s for the exhaustive e2e test
- **Refactoring safety** — if someone restructures the match arms, unit tests catch breakage without needing jq installed

But this is incremental safety, not a gap. The e2e test is the real safety net.

---

## 5. Expand `features.toml` with type-edge variants

**Status:** Superseded by #1
**Priority:** Skip — the exhaustive e2e test covers all arithmetic type-edge cases more thoroughly than features.toml could. The feature matrix report would still show "Y" for operators, which is now actually accurate since the underlying bugs are fixed.

The only remaining value would be if the feature matrix report itself is a deliverable (e.g., for README documentation). Otherwise, not worth the effort.

---

## 6. flat_eval ↔ normal eval differential testing

**Status:** Done
**Priority:** Highest — structural gap, caused the `map` on objects bug ([3bd6b06](https://github.com/6/qj/commit/3bd6b06))
**Effort:** Small-medium (one test function)
**Files:** `src/flat_eval.rs` (unit tests)

### Problem

`src/flat_eval.rs` re-implements builtins independently from `src/filter/builtins/`. When a bug is fixed in one path, the other can still have the old behavior. No test systematically compared the two paths against each other across input types.

The `map` on objects bug is the proof: `arrays.rs` was fixed to handle `Value::Object`, but `flat_eval.rs` still silently dropped objects. The proptest differential suite (#2) couldn't catch it because it pipes single JSON docs through stdin, which uses the normal eval path — flat_eval is only reached via NDJSON processing.

### Builtins with independent flat_eval implementations

| Builtin | flat_eval handles | In `is_flat_safe`? |
|---|---|---|
| `map` | array, object | Yes |
| `map_values` | array, object, scalars | Yes |
| `length` | array, object, string, null | Yes |
| `type` | all types | Yes |
| `keys` | array, object | Yes |
| `tojson` | all types | Yes (catch-all) |
| `sort_by` | array only | No (catch-all) |
| `group_by` | array only | No (catch-all) |

Plus flat_eval handles `Iterate`, `Field`, `Select`, `Compare`, `Not`, `Neg`, `Arith`, `BoolOp`, `Alternative`, `IfThenElse`, `Reduce`, `Bind`, `StringInterp`, `ObjectConstruct`, `ArrayConstruct`, `PostfixSlice` — all independently from the normal eval path.

### What was done

Added 10 `differential_*` tests in `src/flat_eval.rs` that use the existing `assert_equiv` helper (which calls both `eval_flat` and `eval_filter_with_env` on the same input and compares outputs). Tests cover every builtin that flat_eval handles independently, across 15 diverse input types (null, bool, int, float, string, empty/non-empty arrays, mixed arrays, empty/non-empty objects, nested objects).

Tests: `differential_map`, `differential_map_values`, `differential_length`, `differential_type`, `differential_keys`, `differential_tojson`, `differential_sort_by`, `differential_group_by`, `differential_postfix_slice`, `differential_composite_pipes`.

**Found 2 divergences on first run:**
1. `map_values(.)` on null — flat_eval produced nothing, normal eval passed through `null` (fixed: added scalar passthrough)
2. `map_values(. + 1)` on objects with mixed types — flat_eval dropped entries where the inner filter errored, normal eval kept entries with `Null` default (fixed: changed flat_eval to use `let mut new_val = Null` + overwrite pattern)

### Why not just more proptest?

Proptest (#2) tests qj vs jq on single-doc input. It can't catch flat_eval divergences because single-doc input doesn't use flat_eval. The gap is specifically between qj's two internal eval paths. These unit tests run in <1ms and catch the exact class of bug.

---

## 7. Systemic silent error drops in builtins

**Status:** Done
**Priority:** Medium — causes exit code divergences from jq on invalid inputs
**Effort:** Medium-large (touches many builtins across multiple files)

### Problem

Many builtins in `src/filter/builtins/arrays.rs` use `if let Value::Array(arr) = input` patterns that silently produce no output when given non-array inputs. jq errors on these cases (exit code 5) with descriptive messages. qj produces empty stdout with exit code 0.

This is a systemic pattern affecting 20+ builtins. Examples:

- `sort` on an object → jq errors, qj silent
- `unique` on a string → jq errors, qj silent
- `group_by` on a number → jq errors, qj silent
- `@csv`/`@tsv` with nested arrays/objects → jq errors, qj silent

### What was done

Used TDD approach: wrote `jq_compat_builtin_type_errors` e2e test first (31 `assert_jq_compat` calls covering all affected builtins with non-array/non-iterable inputs), confirmed it failed, then fixed systematically.

**Files modified:**

1. **`src/filter/builtins/arrays.rs`** — Added `set_error()` calls to 16 builtins: `keys`/`keys_unsorted` (null passthrough), `sort`, `sort_by`, `group_by`, `unique`, `unique_by`, `flatten`, `first`, `last`, `reverse`, `min`, `max`, `min_by`, `max_by`, `transpose`, `add`. Error messages match jq's format.

2. **`src/filter/builtins/format.rs`** — Added `set_error()` for `@csv`/`@tsv` on non-array inputs and nested array/object elements.

3. **`src/flat_eval.rs`** — Added matching `set_last_error()` calls to `sort_by`, `group_by`, and `map` handlers (which are intercepted by flat_eval before reaching `arrays.rs`). Required because `main.rs` routes all single-doc JSON through flat_eval unconditionally.

**Bonus fixes:**
- `first`/`last` on empty arrays now return `null` (matching jq), previously returned nothing
- `first`/`last` on `null` now return `null` (matching jq), previously errored
- `reverse` on strings reverses characters (matching jq), previously errored
- `reverse` on `null` returns `[]` (matching jq), previously errored

---

---

## 8. Passthrough fast path differential testing

**Status:** Open
**Priority:** High — C++ reimplementation of evaluator logic with zero verification
**Effort:** Medium (new test function in `tests/e2e.rs`)

### Problem

Single-doc JSON files with simple filters (`.`, `.field`, `length`, `keys`, `type`, `has("x")`, `map(.field)`, etc.) are routed through passthrough fast paths in `src/main.rs` (lines ~442-455, ~800-890). These paths use C++ simdjson operations directly — `simdjson::minify()` for identity, C++ bridge functions for field extraction, length, keys, type — bypassing the Rust evaluator entirely.

No test verifies that passthrough output matches normal evaluator output. This is the same class of bug that produced divergences in flat_eval (#6).

### Passthrough patterns

| Pattern | C++ path | Rust equivalent |
|---|---|---|
| `.` (Identity) | `simdjson::minify()` | `eval_filter` → `write_compact` |
| `.field` | `field_raw()` | `eval_filter` → Field |
| `length` | `field_length()` | `eval_filter` → length builtin |
| `keys` | `field_keys()` | `eval_filter` → keys builtin |
| `type` | `field_type()` / tag check | `eval_filter` → type builtin |
| `has("x")` | `field_raw()` null check | `eval_filter` → has builtin |
| `.field.subfield` | chained `field_raw()` | `eval_filter` → Pipe(Field, Field) |

### Approach

Add `passthrough_matches_normal` e2e test that:
1. For each passthrough-eligible filter, processes diverse JSON inputs with passthrough enabled (default)
2. Processes the same inputs with `QJ_NO_FAST_PATH=1` (forces normal eval)
3. Asserts identical stdout

Diverse inputs should cover: objects with various value types, arrays, nested structures, strings, numbers, null, booleans, empty containers, unicode, large numbers, special floats.

---

## 9. Input parsing fallback differential testing

**Status:** Open
**Priority:** Medium — multiple parsing paths for same input
**Effort:** Small-medium (new test function)

### Problem

`src/input.rs` has three parsing paths:
1. **simdjson** (primary) — C++ FFI, fast, ~4GB capacity limit
2. **Line-by-line** (fallback) — tries each line as separate JSON via simdjson
3. **serde_json StreamDeserializer** (last resort) — pure Rust, handles multi-doc JSON without newline separators

No test verifies these produce identical `Value` representations for the same input. Differences in number precision, string escaping, error recovery, or object key ordering could cause silent divergences.

Additionally, `has_special_float_tokens()` + `preprocess_special_floats()` handles non-standard tokens (`NaN`, `Infinity`, `nan`, `inf`) with a preprocessing step that only applies in certain fallback paths.

### Approach

Add tests that:
1. Feed identical JSON through simdjson and serde_json paths, compare Values
2. Test inputs that trigger fallback (e.g., multi-doc without newlines, edge-case JSON)
3. Test special float token handling roundtrip
4. Verify number precision consistency across paths

---

## 10. Cross-mode routing differential testing

**Status:** Open
**Priority:** Medium — different routing produces different code paths for same logical operation
**Effort:** Medium (new test function in `tests/e2e.rs`)

### Problem

`main.rs` has multiple routing decisions that send the same logical input through different code paths:

1. **NDJSON vs single-doc**: A file with one JSON object per line can be processed as NDJSON (parallel, flat_eval per line) or as single-doc (flat_eval on whole file). `is_ndjson()` heuristic decides.
2. **Slurp mode**: `-s` collects all values into an array before eval. `echo '1\n2\n3' | qj -s '.'` vs `echo '[1,2,3]' | qj '.'` should produce identical output.
3. **Streaming vs buffered NDJSON**: `process_ndjson_streaming()` vs `process_ndjson()` — used depending on input source (pipe vs file). Different chunking and carry logic.
4. **mmap vs read()**: File I/O path differs; `QJ_NO_MMAP=1` forces read(). Both should produce identical output.
5. **Windowed NDJSON**: `process_ndjson_windowed()` for very large files — different chunking strategy.

### Approach

Add cross-mode tests that:
1. Process same NDJSON content via `--jsonl` flag (forced NDJSON) vs single-doc, compare output
2. Process same content with `-s` vs pre-wrapped in array, compare output
3. Process same file with `QJ_NO_MMAP=1` vs default, compare output
4. Generate NDJSON large enough to trigger windowed processing, compare against sequential

---

## 11. Decompression path differential testing

**Status:** Open
**Priority:** Low — standard library decompression, unlikely to diverge
**Effort:** Small (new test function)

### Problem

Compressed files (`.gz`, `.zst`/`.zstd`) are decompressed to memory, then processed through the normal pipeline. No test verifies that `qj '.field' data.json.gz` produces identical output to `qj '.field' data.json`.

Risk is low since decompression uses standard libraries (flate2, zstd), but the decompressed bytes follow a slightly different routing path in `process_file()` (lines 1006-1055) vs uncompressed files which may use mmap (lines 1057-1189).

### Approach

Add test that:
1. Creates a small JSON file and its gzip/zstd compressed version
2. Processes both with same filter
3. Asserts identical output

---

## 12. Output mode value identity testing

**Status:** Open
**Priority:** Low — shared code paths make divergence unlikely
**Effort:** Small

### Problem

Pretty, compact, and raw output modes use different formatting code paths. No test verifies that the underlying Value representation is preserved across modes — i.e., that `compact` and `pretty` output parse back to identical JSON values.

### Approach

Add test that processes same input with `-c` (compact) and default (pretty), parses both outputs back to Values, asserts equality. Also test `-r` (raw) on string values.

---

## Summary

| # | Improvement | Status | Catches future bugs? | Effort |
|---|---|---|---|---|
| 1 | Exhaustive type-pair arithmetic | **Done** | All arithmetic | Small |
| 2 | Property-based differential testing | **Done** | All features (single-doc path) | Medium |
| 3 | Grammar-aware fuzz_eval | **Done** | All features (crash + divergence) | Medium |
| 4 | Arithmetic unit tests | Superseded by #1 | Arithmetic only (faster feedback) | Small |
| 5 | features.toml type edges | Superseded by #1 | Tested features only | Small |
| 6 | flat_eval ↔ normal eval differential | **Done** | Dual-path divergences | Small-medium |
| 7 | Systemic silent error drops | **Done** | Error behavior fidelity | Medium-large |
| 8 | Passthrough fast path differential | Open | C++ vs Rust divergences | Medium |
| 9 | Input parsing fallback differential | Open | Parser divergences | Small-medium |
| 10 | Cross-mode routing differential | Open | Routing divergences | Medium |
| 11 | Decompression path differential | Open | Decompression routing | Small |
| 12 | Output mode value identity | Open | Output formatting | Small |

#1–#7 done. #8–#10 are the next priorities (parallel execution paths with independent implementations). #11–#12 are low risk.
