# Path to 100% jq.test Conformance

Current: **488/497 (98.2%)**. 9 tests remain — all bignum/precision related.

## Fixed

### NaN/Infinity modulo (2 tests)

Lines 689, 693. Fixed by propagating NaN in modulo and using i64 saturating
cast for infinity operands.

## Remaining Failures

The 9 remaining tests fall into 3 subcategories with distinct root causes.

### A. `have_decnum` conditional mismatch (4 tests)

Lines 2154, 2158, 2162, 2182. These use `if have_decnum then X else Y end`.
jq has `have_decnum=true` (arbitrary precision via libdecnumber). qj returns
`false`, so the test takes the "else" branch — which expects f64 precision
loss. But qj uses i64, which preserves these numbers exactly.

| Line | Filter | qj (correct) | Test expects (f64 loss) |
|------|--------|---------------|-------------------------|
| 2154 | `.[0] \| tostring` on `13911860366432393` | `"...393"` | `"...392"` |
| 2158 | `.x \| tojson` on same | `"...393"` | `"...392"` |
| 2162 | `13911860366432393 == 13911860366432392` | `false` | `true` |
| 2182 | `-. \| tojson` on same | `"-...393"` | `"-...392"` |

qj falls between the two branches: more accurate than jq-without-decnum
(f64), but doesn't have actual arbitrary precision (decnum).

**Why not just set `have_decnum=true`?** It's a net zero. It would fix these
4 tests but break 4 currently-passing tests (2186, 2190, 2229, 2233) that also
use `have_decnum` conditionals involving `1E+1000` and extreme decimals, where
the `true` branch expects literal preservation that qj can't provide (it sees
infinity/f64 truncation).

### B. Arithmetic truncation (4 tests)

Lines 2169, 2173, 2177, 2199. These are **not** conditional on `have_decnum`.
The jq.test comment reads: *"Applying arithmetic to the value will truncate
the result to double"*. Even jq-with-decnum truncates to f64 for arithmetic.

| Line | Filter | qj (exact i64) | jq (f64 truncated) |
|------|--------|-----------------|---------------------|
| 2169 | `. - 10` on `13911860366432393` | `13911860366432383` | `13911860366432382` |
| 2173 | `.[0] - 10` on same | same | same |
| 2177 | `.x - 10` on same | same | same |
| 2199 | `.[] as $n \| $n+0` on large int array | exact values | f64-rounded values |

This is a real behavioral difference — not a test-conditional mismatch. Users
running arithmetic on numbers > 2^53 will get different (more accurate)
results from qj than from jq. The qj result is mathematically correct; the jq
result reflects f64 precision limits.

### C. Extreme exponents (1 test)

| Line | Filter | qj | jq (decnum) |
|------|--------|----|-------------|
| 661 | `9E999999999, 9999999999E999999990, ...` | infinity/0 | exact |

Exponents beyond f64 range. Genuinely requires a different number system.

## Decision: Accept the gap (default), offer compat mode

### Default behavior: i64 precision (488/497)

qj's i64 arithmetic is a deliberate design choice. It gives mathematically
correct results for integers up to 2^63. The 9 failing tests are all cases
where qj is "too accurate" — they test jq's precision-loss behavior, not
core jq semantics.

### `QJ_JQ_COMPAT=1`: f64-compatible mode (496/497)

For users who need exact jq behavioral parity (e.g., migration validation,
pipeline certification), an environment variable `QJ_JQ_COMPAT=1` opts into
jq-compatible f64 semantics: integers > 2^53 are stored as f64, losing
precision to match jq-without-decnum behavior.

This fixes 8 of 9 remaining tests (groups A and B). The conditional tests
match the `have_decnum=false` branch, and arithmetic naturally truncates since
values are already f64. Only the extreme exponent test (661) remains, as it
requires a number system beyond both i64 and f64.

**Implementation:** In the JSON parser and filter literal parser, when
`QJ_JQ_COMPAT=1` is set, convert integer values > 2^53 to `Value::Double`
instead of `Value::Int`. This is a small, isolated change — a single check at
parse time. No architectural changes needed.
