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

**Status:** TODO — next up
**Priority:** High — catches divergences that humans wouldn't think to test
**Effort:** Medium (new test file + grammar-aware generator)
**Files:** new `tests/jq_differential.rs`

### Problem

Hand-written tests only cover cases someone thought of. The `null * number` bug persisted because nobody thought to write that specific test. The exhaustive arithmetic test (#1) locked down all type-pair arithmetic, but the rest of the evaluator — builtins, string operations, try/catch, if/then/else, reduce, path expressions — has the same "never compared against jq" exposure.

### Solution: proptest with grammar-aware generators

Generate random but **syntactically valid** `(filter, input)` pairs, run both qj and jq, assert identical stdout + exit code. Use `proptest` for deterministic seeds, reproducible failures, and automatic shrinking to minimal cases.

```rust
// tests/jq_differential.rs
use proptest::prelude::*;

fn arb_json_value() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("null".to_string()),
        Just("true".to_string()),
        Just("false".to_string()),
        (-1000i64..1000).prop_map(|n| n.to_string()),
        (-100.0f64..100.0).prop_map(|f| format!("{f}")),
        "[a-z]{0,10}".prop_map(|s| format!("\"{s}\"")),
        // arrays and objects of the above...
    ]
}

fn arb_filter() -> impl Strategy<Value = String> {
    prop_oneof![
        // Arithmetic
        (arb_json_value(), arb_op(), arb_json_value())
            .prop_map(|(a, op, b)| format!("{a} {op} {b}")),
        // Builtins
        Just("length".to_string()),
        Just("keys".to_string()),
        Just("type".to_string()),
        // Field access
        Just(".foo".to_string()),
        Just(".foo.bar".to_string()),
        // Pipes
        (arb_simple_filter(), arb_simple_filter())
            .prop_map(|(a, b)| format!("{a} | {b}")),
        // try
        arb_simple_filter().prop_map(|f| format!("try ({f})")),
    ]
}

proptest! {
    #[test]
    #[ignore]
    fn differential_vs_jq(
        filter in arb_filter(),
        input in arb_json_value(),
    ) {
        let (qj_out, qj_err, qj_ok) = run_qj(&filter, &input);
        let (jq_out, jq_err, jq_ok) = run_jq(&filter, &input);
        prop_assert_eq!(qj_out, jq_out, "stdout mismatch");
        prop_assert_eq!(qj_ok, jq_ok, "exit code mismatch");
    }
}
```

Advantages: reproducible seeds, shrinking to minimal failing case, runs in CI with `--ignored`.
Disadvantages: limited by the grammar strategies you write (but the grammar can grow incrementally).

### Grammar coverage targets

The generator should produce filters covering at minimum:
- All 5 arithmetic operators with all type combinations
- Comparison operators (`==`, `!=`, `<`, `>`, `<=`, `>=`)
- Boolean operators (`and`, `or`, `not`)
- String builtins (`length`, `ltrimstr`, `rtrimstr`, `split`, `join`, `test`, `match`, `ascii_downcase`, `ascii_upcase`, `startswith`, `endswith`, `contains`, `explode`, `implode`)
- Array builtins (`map`, `select`, `empty`, `add`, `any`, `all`, `flatten`, `group_by`, `sort_by`, `unique_by`, `min_by`, `max_by`, `first`, `last`, `nth`, `range`, `limit`, `until`, `repeat`, `indices`, `inside`, `getpath`, `setpath`, `delpaths`)
- Object builtins (`keys`, `values`, `has`, `in`, `to_entries`, `from_entries`, `with_entries`)
- Type/conversion builtins (`type`, `infinite`, `nan`, `isinfinite`, `isnan`, `isnormal`, `tostring`, `tonumber`, `ascii`, `null`)
- `try`/`catch`, `if`/`then`/`else`, `//` (alternative operator)
- Pipes and composition (`|`, `,`)
- Variable binding (`. as $x | ...`)
- `reduce`, `foreach`
- `@base64`, `@base64d`, `@uri`, `@csv`, `@tsv`, `@html`, `@json`, `@text`, `@sh`

---

## 3. Grammar-aware `fuzz_eval` rewrite

**Status:** TODO
**Priority:** Medium — makes existing fuzzer dramatically more effective
**Effort:** Medium (rewrite one fuzz target)
**Files:** `fuzz/fuzz_targets/fuzz_eval.rs`

### Problem

The current `fuzz_eval` splits random bytes at an arbitrary offset into "JSON" and "filter". Almost all generated inputs fail to parse (invalid UTF-8, invalid JSON, invalid filter syntax). The fuzzer spends >99% of its time in parser rejection paths and almost never reaches the evaluator.

It could theoretically generate `null * 1` — but the probability of random bytes producing both valid JSON (`null`) AND a valid filter (`1 * .`) is astronomically low. In practice, after 120 seconds of fuzzing, the evaluator code coverage is minimal.

### Solution

Use `libfuzzer_sys::arbitrary::Arbitrary` to derive structured inputs:

```rust
#[derive(Arbitrary)]
enum FuzzValue {
    Null,
    Bool(bool),
    Int(i16),          // small range to stay interesting
    Float(f32),        // small range
    Str(String),       // libfuzzer will generate varied strings
    Array(Vec<FuzzValue>),
    Object(Vec<(String, FuzzValue)>),
}

#[derive(Arbitrary)]
enum FuzzFilter {
    Identity,
    Field(String),
    Literal(FuzzValue),
    Arith(Box<FuzzFilter>, ArithOp, Box<FuzzFilter>),
    Pipe(Box<FuzzFilter>, Box<FuzzFilter>),
    Try(Box<FuzzFilter>),
    Builtin(BuiltinName),
    // ...
}
```

This guarantees every fuzz iteration produces a valid filter + valid input, so 100% of iterations reach the evaluator. Coverage-guided mutation then explores the evaluation logic space efficiently.

### Scope

This is a rewrite of `fuzz_eval.rs` only. The other fuzz targets (parse, DOM, NDJSON diff, output) are fine as-is — they're testing parser robustness where random bytes are the right approach.

### Relationship to #2

If #2 (proptest differential) is implemented first, the grammar generator code can be shared. The structured `FuzzFilter`/`FuzzValue` enums would be similar to the proptest strategies, just using `Arbitrary` instead of `Strategy`. Consider extracting the grammar into a shared crate or module.

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

## Summary

| # | Improvement | Status | Catches future bugs? | Effort |
|---|---|---|---|---|
| 1 | Exhaustive type-pair arithmetic | **Done** | All arithmetic | Small |
| 2 | Property-based differential testing | **Next** | All features | Medium |
| 3 | Grammar-aware fuzz_eval | TODO | All features (crash + divergence) | Medium |
| 4 | Arithmetic unit tests | Superseded by #1 | Arithmetic only (faster feedback) | Small |
| 5 | features.toml type edges | Superseded by #1 | Tested features only | Small |

Recommended order: **#2 → #3**. #1 is done. #4 and #5 are low priority given #1's coverage.
