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

## 6. Systemic silent error drops in builtins

**Status:** Open
**Priority:** Medium — causes exit code divergences from jq on invalid inputs
**Effort:** Medium-large (touches many builtins across multiple files)

### Problem

Many builtins in `src/filter/builtins/arrays.rs` use `if let Value::Array(arr) = input` patterns that silently produce no output when given non-array inputs. jq errors on these cases (exit code 5) with descriptive messages. qj produces empty stdout with exit code 0.

This is a systemic pattern affecting 20+ builtins. Examples:

- `sort` on an object → jq errors, qj silent
- `unique` on a string → jq errors, qj silent
- `group_by` on a number → jq errors, qj silent
- `@csv`/`@tsv` with nested arrays/objects → jq errors, qj silent

### Why it matters

Silent errors are worse than wrong output — the user gets no indication that their filter didn't apply. The proptest differential suite (#2) catches these as exit code mismatches, but fixing them requires an error propagation mechanism that qj currently lacks for builtin evaluation.

### Approach

1. Add an error/diagnostic callback to the builtin eval signature (or return `Result`)
2. Replace `if let Value::Array` early-returns with explicit type-check errors
3. Match jq's error messages where possible (users may parse stderr)

### Builtins affected (non-exhaustive)

`sort`, `sort_by`, `group_by`, `unique`, `unique_by`, `flatten`, `first`, `last`, `reverse`, `min`, `max`, `min_by`, `max_by`, `add`, `transpose`, `limit`, `skip`, `until`, `while`, `repeat`, `nth`, `combinations`, `@csv`, `@tsv`

---

## Summary

| # | Improvement | Status | Catches future bugs? | Effort |
|---|---|---|---|---|
| 1 | Exhaustive type-pair arithmetic | **Done** | All arithmetic | Small |
| 2 | Property-based differential testing | **Done** | All features | Medium |
| 3 | Grammar-aware fuzz_eval | **Done** | All features (crash + divergence) | Medium |
| 4 | Arithmetic unit tests | Superseded by #1 | Arithmetic only (faster feedback) | Small |
| 5 | features.toml type edges | Superseded by #1 | Tested features only | Small |
| 6 | Systemic silent error drops | Open | Error behavior fidelity | Medium-large |

All high-priority items (#1, #2, #3) are done. #4 and #5 are low priority given #1's coverage. #6 is a new finding from the proptest differential suite.
