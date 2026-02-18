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

**Status:** TODO
**Priority:** Highest — directly prevents the class of bug that caused 610b937
**Effort:** Small (single test function)
**Files:** `tests/e2e.rs`

### Problem

`arith_values(left, op, right)` dispatches on `(Value, ArithOp, Value)`. There are 7 value types (`null`, `bool`, `int`, `float`, `string`, `array`, `object`) and 5 operators (`+`, `-`, `*`, `/`, `%`). That's 7×7×5 = 245 type-pair/operator combinations. Most should error, but the exact behavior (error vs. result, error message wording) must match jq.

No test currently covers more than a handful of these combinations. The `null * number` bug was one cell in this 245-cell matrix that nobody checked.

### Solution

A single `#[test]` that exhaustively enumerates representative values for each type, pairs them with each operator, runs the expression through both qj and jq, and compares stdout + exit code:

```rust
#[test]
fn exhaustive_arithmetic_type_pairs() {
    let values = [
        "null",
        "true",
        "false",
        "0",
        "1",
        "1.5",
        "\"hello\"",
        "[]",
        "[1,2]",
        "{}",
        "{\"a\":1}",
    ];
    let ops = ["+", "-", "*", "/", "%"];
    for a in &values {
        for b in &values {
            for op in &ops {
                let filter = format!("{a} {op} {b}");
                assert_jq_compat_full(&filter, "null");
            }
        }
    }
}
```

11 values × 11 values × 5 ops = 605 comparisons. Uses `assert_jq_compat_full` (comparing both stdout and exit code, not just stdout) so error-producing combinations are verified too.

This would have caught the `null * number` bug immediately — it's literally one cell in the matrix.

### Edge cases to include

Beyond the basic matrix, add targeted tests for:
- Division by zero: `1 / 0`, `1.0 / 0`, `1 % 0`
- String repetition: `"ab" * 3`, `"ab" * -1`, `"ab" * 0`
- String split: `"a,b,c" / ","`
- Object merge via `*`: `{a:1} * {b:2}`, `{a:{x:1}} * {a:{y:2}}`
- Array concatenation via `+`: `[1] + [2]`
- Array subtraction via `-`: `[1,2,3] - [2]`
- `null` as addition identity: `null + 1`, `1 + null`, `null + "s"`, `null + []`, `null + {}`
- Overflow: `9223372036854775807 + 1`, `9223372036854775807 * 2`

---

## 2. Property-based differential testing (jq oracle)

**Status:** TODO
**Priority:** High — catches divergences that humans wouldn't think to test
**Effort:** Medium (new test file + grammar-aware generator)
**Files:** new `tests/jq_differential.rs` or new fuzz target `fuzz/fuzz_targets/fuzz_jq_diff.rs`

### Problem

Hand-written tests only cover cases someone thought of. The `null * number` bug persisted because nobody thought to write that specific test. We need a way to explore the space of `(filter, input)` pairs automatically and flag any case where qj and jq disagree.

### Solution: grammar-aware differential fuzzer

Generate random but **syntactically valid** `(filter, input)` pairs, run both qj and jq, assert identical stdout + exit code.

Two possible implementations:

#### Option A: `proptest`/`quickcheck` in a `#[test] #[ignore]` (deterministic, reproducible)

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

Advantages: reproducible seeds, shrinking to minimal failing case, runs in CI.
Disadvantages: limited by the grammar strategies you write.

#### Option B: cargo-fuzz target (`fuzz_jq_diff.rs`)

```rust
// fuzz/fuzz_targets/fuzz_jq_diff.rs
fuzz_target!(|data: &[u8]| {
    let (filter, input) = grammar_decode(data);
    let qj_result = run_qj_binary(&filter, &input);
    let jq_result = run_jq_binary(&filter, &input);
    if qj_result != jq_result {
        panic!("divergence: filter={filter:?} input={input:?}");
    }
});
```

Advantages: coverage-guided, finds surprising edge cases.
Disadvantages: requires jq installed, slower per iteration (process spawning), harder to reproduce.

#### Recommendation

Start with **Option A** (`proptest` in `tests/jq_differential.rs`). It integrates with `cargo test --ignored`, produces reproducible regressions, and the grammar can grow incrementally. If/when the grammar stops finding new bugs, consider Option B for deeper exploration.

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

---

## 4. Arithmetic unit tests for `arith_values()`

**Status:** TODO
**Priority:** Medium — catches regressions from refactoring the match arms
**Effort:** Small
**Files:** `src/filter/value_ops.rs` (add to existing `#[cfg(test)]` module)

### Problem

The `arith_values()` function (~150 lines, 5 operators, dozens of match arms) is the single dispatch point for all arithmetic in the evaluator. It currently has **zero unit tests**. The existing `#[cfg(test)]` module in `value_ops.rs` tests `to_f64`, `frexp`, `todate`, `set_path`, `del_path`, etc. — but not arithmetic.

This means any refactoring of the match arms (reordering, adding new type combinations, changing error messages) has no safety net beyond e2e tests, which are slower and less targeted.

### Solution

Add direct unit tests for each match arm, focusing on:

1. **Null behavior per operator:**
   - `Add`: null is identity (`null + x = x`, `x + null = x`)
   - `Sub`, `Mul`, `Div`, `Mod`: null always errors

2. **Overflow promotion:**
   - `i64::MAX + 1` → f64
   - `i64::MAX * 2` → f64
   - `i64::MIN / -1` → f64

3. **Division by zero:**
   - `int / 0` → error
   - `float / 0.0` → error
   - `int % 0` → error

4. **String operations:**
   - `"ab" * 3` → `"ababab"`
   - `"ab" * -1` → `null`
   - `"a,b" / ","` → `["a","b"]`

5. **Collection operations:**
   - `[1,2] + [3]` → `[1,2,3]`
   - `[1,2,3] - [2]` → `[1,3]`
   - `{a:1} * {b:2}` → `{a:1,b:2}` (shallow merge)
   - `{a:{x:1}} * {a:{y:2}}` → recursive merge

6. **Type mismatch errors:**
   - `"s" - 1` → error
   - `[] * 2` → error
   - `{} / 2` → error

```rust
#[test]
fn arith_null_add_identity() {
    let one = Value::Int(1);
    assert_eq!(arith_values(&Value::Null, &ArithOp::Add, &one), Ok(Value::Int(1)));
    assert_eq!(arith_values(&one, &ArithOp::Add, &Value::Null), Ok(Value::Int(1)));
}

#[test]
fn arith_null_mul_errors() {
    let one = Value::Int(1);
    assert!(arith_values(&Value::Null, &ArithOp::Mul, &one).is_err());
    assert!(arith_values(&one, &ArithOp::Mul, &Value::Null).is_err());
}
```

---

## 5. Expand `features.toml` with type-edge variants

**Status:** TODO
**Priority:** Low — mostly redundant if #1 is implemented, but useful for the feature matrix report
**Effort:** Small
**Files:** `tests/jq_compat/features.toml`

### Problem

The feature compat suite tests each operator with one or two clean cases:
- Addition: `2 + 1`, `"hello" + " world"`, `[1,2] + [3,4]`
- Multiplication: `7 * 3`, `{a:{b:1}} * {a:{c:2}}`
- etc.

None include null, bool, or mixed-type operands. The feature matrix report shows "Y" for all operators, giving a false sense of completeness.

### Solution

Add type-edge variants to each operator section:

```toml
[[features]]
category = "Operators"
name = "Addition (null identity)"
tests = [
  { filter = 'null + 1', input = 'null', expected = '1' },
  { filter = '1 + null', input = 'null', expected = '1' },
  { filter = 'null + "s"', input = 'null', expected = '"s"' },
  { filter = 'null + [1]', input = 'null', expected = '[1]' },
  { filter = 'null + {a:1}', input = 'null', expected = '{"a":1}' },
]

[[features]]
category = "Operators"
name = "Multiplication (type errors)"
tests = [
  { filter = 'null * 1', input = 'null', expected_error = true },
  { filter = '1 * null', input = 'null', expected_error = true },
  { filter = '"s" * 3', input = 'null', expected = '"sss"' },
  { filter = '"s" * -1', input = 'null', expected = 'null' },
]
```

**Note:** This requires adding `expected_error` support to the feature compat runner if it doesn't already exist.

---

## Summary

| # | Improvement | Catches `null*number`? | Catches future type bugs? | Effort |
|---|---|---|---|---|
| 1 | Exhaustive type-pair arithmetic | Yes (directly) | Yes (all arithmetic) | Small |
| 2 | Property-based differential testing | Yes (probabilistically) | Yes (all features) | Medium |
| 3 | Grammar-aware fuzz_eval | Yes (probabilistically) | Yes (all features) | Medium |
| 4 | Arithmetic unit tests | Yes (directly) | Partial (arithmetic only) | Small |
| 5 | features.toml type edges | Yes (directly) | Partial (tested features) | Small |

Recommended order: **1 → 4 → 2 → 5 → 3**. Items 1 and 4 are quick wins that cover the immediate gap. Item 2 is the highest long-term value. Items 3 and 5 are incremental improvements.
