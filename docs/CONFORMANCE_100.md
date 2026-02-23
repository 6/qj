# jq.test Conformance

## Current Status

- **Default:** 488/497 (98.2%)
- **`QJ_JQ_COMPAT=1`:** 497/497 (100.0%)

## Why not 100% by default?

qj uses i64 for integers (exact up to 2^63) while jq uses f64 for arithmetic on large numbers (exact only up to 2^53). For integers between 2^53 and 2^63, qj actually gives **more precise** results than jq. The 9 "failing" tests are cases where jq's test suite checks for jq's specific precision-loss behavior — qj doesn't match because it gives the mathematically correct answer.

For example: `13911860366432393 - 10`
- **qj (default):** `13911860366432383` (correct)
- **jq:** `13911860366432382` (f64 precision loss)

No real-world jq script depends on getting the wrong answer, so this is safe for drop-in use. But if you need byte-identical output with jq (e.g., for checksumming or diff-testing), set `QJ_JQ_COMPAT=1`:

```bash
export QJ_JQ_COMPAT=1   # match jq's precision behavior exactly
```

This makes qj deliberately truncate arithmetic to f64 for numbers above 2^53, matching jq's behavior. All 497 tests pass.

## Fixed

### NaN/Infinity modulo (2 tests)

Lines 689, 693. Fixed by propagating NaN in modulo and using i64 saturating
cast for infinity operands.

### Extreme exponents (1 test, compat mode)

Line 661. Filter literals like `9E999999999` now normalize and preserve their
text representation in compat mode (e.g., `9999999999E999999990` becomes
`9.999999999E+999999999`). The f64 approximation (infinity/0) handles
comparisons; the raw text handles output.

### have_decnum conditional (4 tests, compat mode)

Lines 2154, 2158, 2162, 2182. In compat mode, `have_decnum` returns `true`
since qj preserves i64 precision for display and extreme exponent text,
matching jq-with-decnum behavior for non-arithmetic operations.

### Arithmetic truncation (4 tests, compat mode)

Lines 2169, 2173, 2177, 2199. In compat mode, binary arithmetic (+, -, *, /,
%) converts operands to f64 when either exceeds 2^53, matching jq's
truncation behavior. Precision is preserved for non-arithmetic operations
(tostring, tojson, equality).

### Decimal precision preservation (1 test, compat mode)

Line 2186. Unary negation preserves raw JSON text in compat mode, so
`-. | tojson` on `-0.12345678901234567890123456789` produces the full 30-digit
representation.

## Remaining Default-Mode Gaps (9 tests)

All 9 remaining gaps in default mode are bignum/precision related. qj uses i64
(exact for integers up to 2^63) while jq uses arbitrary precision. The gaps are
cases where qj is "too accurate" — tests that check jq's precision-loss
behavior, not core jq semantics.

These are fully resolved by `QJ_JQ_COMPAT=1`.

## How QJ_JQ_COMPAT=1 Works

In compat mode, qj emulates jq-with-decnum behavior:

1. **`have_decnum` = true** — matches the decnum branch of conditional tests
2. **i64 storage preserved** — large integers stay as i64 for display/comparison
3. **Arithmetic truncation** — binary ops convert operands > 2^53 to f64 first
4. **Extreme exponent preservation** — filter literals beyond f64 range preserve text
5. **Raw text through negation** — unary minus preserves JSON input precision
6. **Raw text through abs/length** — abs/length preserve extreme exponent text
