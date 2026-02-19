# Path to 100% jq.test Conformance

Current: **488/497 (98.2%)**. 9 tests remain.

## Remaining Failures

### NaN/Infinity modulo (2 tests) — FIXED

Lines 689, 693. Fixed by propagating NaN in modulo and using i64 truncation
for infinity operands.

### Bignum precision (8 tests)

These tests use numbers beyond 2^53 (e.g., `13911860366432393`) and are
conditional on `have_decnum`. jq has `have_decnum=true` (arbitrary precision
via libdecnumber). The "else" branch expects f64 precision loss. qj uses i64
which is *more accurate* than jq-without-decnum but doesn't match either
branch of the conditional.

| Line | Filter | What happens |
|------|--------|--------------|
| 2154 | `.[0] \| tostring` on `13911860366432393` | qj: "...393" (exact i64). Test expects "...392" (f64 rounded). |
| 2158 | `.x \| tojson` on same number | Same mismatch. |
| 2162 | `13911860366432393 == 13911860366432392` | qj: `false` (different i64s). Test expects `true` (same f64). |
| 2169 | `. - 10` on `13911860366432393` | qj: `383` (correct). Test expects `382` (f64 precision loss). |
| 2173 | `.[0] - 10` on same | Same. |
| 2177 | `.x - 10` on same | Same. |
| 2182 | `-. \| tojson` on same | Same precision mismatch. |
| 2199 | `.[] as $n \| $n+0` on array of large ints | Multiple precision mismatches. |

### Extreme exponents (1 test)

| Line | Filter | What happens |
|------|--------|--------------|
| 661 | `9E999999999, 9999999999E999999990, 1E-999999999, ...` | Exponents overflow/underflow f64. jq handles via decnum. qj: infinity/0. |

## Options for the remaining 9

### Option A: Accept the gap (recommended)

qj's i64 representation is a deliberate design choice that makes it *more
accurate* than jq for integers in the i64 range. The failing tests are all
conditional on `have_decnum` — they test jq's arbitrary precision behavior,
not core jq semantics. Accepting 488/497 (98.2%) with a clear explanation is
reasonable.

### Option B: Truncate large integers to f64

Parse JSON integers > 2^53 as f64 (losing precision deliberately) to match
jq-without-decnum behavior. This would pass the "else" branch of the
conditional tests. Downside: makes qj *less accurate* for large integers
that fit in i64. Would break the number-literal-preservation property.

### Option C: Implement arbitrary precision

Add a `BigNum` variant to `Value` using a decimal library (e.g., `rust_decimal`
or `bigdecimal`). Set `have_decnum=true`. This would match jq exactly for all
9 tests. Downside: significant architectural change — new `Value` variant
affects every match arm in eval, output, value_ops. Performance impact on all
number operations due to wider enum. Would need careful benchmarking.

### Option D: Hybrid approach

Keep i64/f64 for normal operations. Add string-based passthrough for number
literals > 2^53 that appear in JSON input but aren't used in arithmetic.
Only lose precision when arithmetic is performed. This matches what users
actually want (preserve numbers on passthrough, accept f64 limits for math)
but wouldn't pass the conformance tests since they specifically test
arithmetic on large numbers.
