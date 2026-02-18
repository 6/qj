# Test Suite Improvement Plan — Phase 3: Builtin Coverage & Edge Cases

> Following TEST_TODOS.md (12 items: eval path divergences, error propagation, cross-mode routing)
> and TEST_TODOS_2.md (6 items: C++ FFI bridge coverage and fuzzing), this plan addresses the
> remaining gap: **builtin functions with zero or minimal unit tests**, plus structural edge cases
> in destructuring, NDJSON chunking, and loop limits.

---

## 1. Unit tests for `math.rs` (30+ functions, zero tests)

**Status:** Done ✓ (11 jq_compat test groups covering floor/ceil/round/trunc, sqrt/exp/log, trig, hyperbolic, pow/atan2, nan/infinite, range, abs, domain errors)
**Priority:** Critical — entire module untested, 326 lines of numeric code
**Effort:** Medium
**Files:** `src/filter/builtins/math.rs`

### Bugs found (all fixed)

- **`log(0)`**: ~~qj returns `null`~~ → Fixed: infinity now formatted as `±1.7976931348623157e+308` like jq (`output.rs`)
- **`nan | isfinite`**: ~~qj returns `false`~~ → Fixed: changed `f.is_finite()` to `!f.is_infinite()` (`math.rs`)
- **Math type errors**: ~~exits 0 silently~~ → Fixed: added `require_number()` helper with `set_error()` for all 33 unary math builtins (`math.rs`)

### Problem

Every math builtin is exercised only through jq.test conformance (which may not cover edge cases).
No unit tests exist. Specific risks:

| Risk | Location | Description |
|---|---|---|
| Division by zero | `remainder` (line 293) | `x / y` with no zero guard — will produce `NaN` or `Inf` silently |
| Integer truncation | `scalb` (line 213) | `to_f64(&v) as i32` — exponents > 2^31 silently truncate |
| Integer truncation | `exponent` (line 220) | `as i64` cast from float |
| NaN/Infinity propagation | `nan`/`infinite` (lines 233-235) | Produces bare NaN/Infinity — how do downstream consumers handle? |
| Domain errors | `asin`/`acos` | Inputs outside [-1,1] produce NaN silently |
| FFI calls | `j0`/`j1`/`logb`/`significand` | Call libc functions — behavior with NaN/Infinity/subnormals undefined |

### Test cases needed

**Basic correctness (one test per function):**
- `floor`, `ceil`, `round`, `trunc` on: positive, negative, 0.5 boundary, integer input, NaN, Infinity
- `sqrt`, `cbrt`, `exp`, `exp2`, `log`, `log2`, `log10` on: 0, 1, negative, large values
- Trig functions: 0, pi/2, pi, 2pi, NaN, Infinity
- Hyperbolic functions: 0, 1, -1, large values
- `pow`: 0^0, 0^n, n^0, negative base with fractional exponent
- `atan2`: all four quadrants, (0,0), (Inf, Inf)
- `fma`: basic, overflow, underflow

**Edge cases (the risky ones):**
- `remainder(x, 0)` — should match jq behavior (error? NaN?)
- `scalb(1, 2147483648)` — exponent exceeds i32::MAX
- `scalb(1, -2147483649)` — exponent below i32::MIN
- `exponent` on subnormals, NaN, Infinity, 0
- `nan | isnan`, `infinite | isinfinite`, `1 | isnan` etc.
- `range(0; 1; 0)` — zero step (already guarded, but test it)
- `range(0; 1; -1)` — negative step with positive range

---

## 2. Unit tests for `strings.rs` (20+ functions, zero tests)

**Status:** Done ✓ (13 jq_compat test groups covering split/join, explode/implode, index/rindex, trim, tostring/tonumber, tojson/fromjson, case, utf8bytelength, @format)
**Priority:** Critical — 525 lines, complex string operations with silent failures
**Effort:** Medium
**Files:** `src/filter/builtins/strings.rs`

### Bugs found (all fixed)

- **`tonumber` on non-numeric string**: ~~exits 0 silently~~ → Fixed: added `set_error()` for unparseable strings and non-string/non-number input (`strings.rs`)

### Problem

All string builtins lack unit tests. Several have silent failure modes:

| Risk | Location | Description |
|---|---|---|
| unwrap() panic | `tostring` (lines 44-45) | `write_compact` then `String::from_utf8` — could panic on invalid output |
| Silent null | `tonumber` (line 57) | Failed parse returns nothing (no error, no null) |
| Silent null | `ascii` (line 500) | Non-integer input returns nothing |
| Silent skip | `split` (line 188) | Non-string input silently skipped |
| Silent skip | `join` (line 215) | Non-string elements silently skipped |
| Invalid Unicode | `implode` (lines 372-386) | Invalid codepoints silently dropped or produce replacement chars |
| unwrap_or_default | `tojson`/`implode` (line 396) | Formatting failure returns empty string silently |

### Test cases needed

**`split` / `join`:**
- Split on empty string → character array
- Split on multi-byte UTF-8 separator
- Split with no matches → single-element array
- Join on non-string array elements (numbers, booleans, nulls)
- Split then join roundtrip

**`implode` / `explode`:**
- Valid ASCII codepoints
- Valid multi-byte Unicode codepoints
- Codepoint 0 (NUL)
- Codepoint > 0x10FFFF (invalid)
- Surrogate pair codepoints (0xD800-0xDFFF — invalid in UTF-8)
- Empty array → empty string

**`index` / `rindex` / `indices`:**
- Substring found / not found
- Overlapping matches
- Empty needle
- Array subsequence search
- Unicode string search

**`tostring` / `tonumber`:**
- All JSON types through `tostring`
- Number strings, scientific notation, hex through `tonumber`
- Non-numeric string through `tonumber` (should error)

**`fromjson`:**
- Valid JSON string → value
- Invalid JSON string → error
- Nested JSON (JSON containing escaped JSON)
- Empty string

**`ltrimstr` / `rtrimstr` / `startswith` / `endswith`:**
- Matching prefix/suffix
- No match
- Empty string argument
- Non-string input (should error)

---

## 3. Unit tests for `date.rs` (8 functions, zero tests)

**Status:** Done ✓ (4 jq_compat test groups covering todate/fromdate, gmtime/mktime, strftime, now)
**Priority:** High — date handling is notoriously buggy, 146 lines
**Effort:** Small
**Files:** `src/filter/builtins/date.rs`

### Bugs found

None — all date functions match jq behavior.

### Problem

All date functions are untested. Date handling is inherently fragile:

| Risk | Location | Description |
|---|---|---|
| Precision loss | `todate` (line 20) | Input goes through `input_as_f64` — large timestamps lose precision |
| Silent null | `fromdate` (line 28) | Invalid ISO 8601 returns null with no error |
| Platform-dependent | `localtime` (line 74) | Timezone handling varies by OS |
| Silent failure | `strptime` (line 91) | Format mismatch silently returns nothing |

### Test cases needed

- `now` — returns a number (basic smoke test)
- `0 | todate` — epoch → "1970-01-01T00:00:00Z"
- `"2024-01-15T12:30:00Z" | fromdate` — ISO 8601 → timestamp
- `todate | fromdate` roundtrip
- `gmtime` → 9-element array (verify structure)
- `mktime` on gmtime output → roundtrip
- `strftime("%Y-%m-%d")` — basic formatting
- `strptime("%Y-%m-%d")` — basic parsing
- Invalid date string through `fromdate` → error behavior
- Negative timestamps (pre-epoch)
- Large timestamps (year 2100+)

---

## 4. Unit tests for undertested array builtins

**Status:** Done ✓ (7 jq_compat test groups covering walk, bsearch, combinations, pick, nth, repeat, isempty)
**Priority:** High — 7 functions with zero direct tests, complex logic
**Effort:** Medium
**Files:** `src/filter/builtins/arrays.rs`

### Bugs found (all fixed)

- **`combinations` panics on empty sub-array**: ~~crash~~ → Fixed: added empty sub-array guard (`arrays.rs`)
- **`combinations(0)` wrong output**: ~~returns `[]`~~ → Fixed: added `n == 0` special case returning `[[]]` (`arrays.rs`)

### Problem

| Function | Lines | Risk |
|---|---|---|
| `walk` | 794-840 | Takes first output only from filter — silently drops multiple results (line 803-806) |
| `bsearch` | 842-876 | Negative insertion point encoding (`-index - 1`) — off-by-one risk |
| `combinations` | 944-1015 | Two modes (0-arg, 1-arg) with different semantics — no tests for either |
| `pick` | 1016-1047 | Error accumulation via `had_error` flag — stops on first failure |
| `nth` | 757-784 | Negative indices silently return nothing (line 762) |
| `repeat` | 743-749 | Hard cap at MAX_LOOP_ITERATIONS (1M) — silent truncation |
| `isempty` | 750-756 | Logic inversion risk — tests if generator produces zero outputs |

### Test cases needed

**`walk(f)`:**
- Walk identity → same output
- Walk on scalars → applies f to scalar
- Walk on nested object → applies f bottom-up
- Walk with filter that produces multiple outputs → verify only first used
- Walk with filter that produces zero outputs → verify element removed
- Walk on empty array/object

**`bsearch(x)`:**
- Found at beginning, middle, end
- Not found → negative index (verify correct insertion point)
- Empty array
- Single-element array (found and not found)
- Non-sorted array (undefined behavior — document what happens)
- Searching for null, string, boolean in numeric array

**`combinations` / `combinations(n)`:**
- `[[1,2],[3,4]] | combinations` → `[1,3],[1,4],[2,3],[2,4]`
- `[1,2] | [.,.]  | combinations` → verify n-ary mode
- Empty sub-array → no output
- Single sub-array → identity
- `combinations(0)` and `combinations(1)` edge cases

**`pick(paths)`:**
- Pick existing paths
- Pick non-existent paths → null
- Pick from nested structure
- Mixed valid/invalid paths

**`nth(n)` / `nth(n; f)`:**
- `nth(0)` → first element
- `nth(-1)` → should error or return nothing?
- `nth(100; range(3))` → past end of generator
- Two-arg form: `nth(1; .[] | select(. > 2))`

**`repeat(f)`:**
- Basic: `1 | [limit(5; repeat(. * 2))]` → `[1,2,4,8,16]`
- Without limit → hits MAX_LOOP_ITERATIONS silently

**`isempty(f)`:**
- `null | isempty(empty)` → true
- `null | isempty(.)` → false
- `null | isempty(error)` → should be true (error = no output)

---

## 5. Unit tests for `inside` and entry functions

**Status:** Done ✓ (2 jq_compat test groups covering inside, to_entries/from_entries/with_entries)
**Priority:** High — `inside` is the complement of `contains`, zero dedicated tests
**Effort:** Small
**Files:** `src/filter/builtins/types.rs`

### Bugs found

None — `inside`, `to_entries`, `from_entries` (including alternate key names), and `with_entries` all match jq.

### Problem

`inside` (lines 102-108) is `contains` with arguments swapped. If the swap is wrong, every use is silently incorrect. `from_entries` (lines 138-162) accepts alternate key names ("Key", "name", "Name") that are completely untested.

### Test cases needed

**`inside`:**
- `"foo" | inside("foobar")` → true
- `"bar" | inside("foo")` → false
- `[1,2] | inside([1,2,3])` → true
- `{"a":1} | inside({"a":1,"b":2})` → true
- `{"a":1,"c":3} | inside({"a":1,"b":2})` → false
- Verify it's the exact complement of `contains` for 5+ cases

**`to_entries` / `from_entries`:**
- Roundtrip: `to_entries | from_entries` == identity for objects
- `from_entries` with "Key"/"Value" alternate names
- `from_entries` with "name"/"value" alternate names
- `from_entries` with missing "value" field
- `from_entries` with non-array input → error
- `to_entries` with non-object input → error
- `with_entries(f)` — should be equivalent to `to_entries | map(f) | from_entries`

---

## 6. Unit tests for destructuring patterns

**Status:** Done ✓ (4 jq_compat test groups covering array destructuring, object destructuring, nested destructuring, destructuring in reduce)
**Priority:** High — two modes (strict vs lenient) with different null semantics, zero e2e tests
**Effort:** Medium
**Files:** `src/filter/eval.rs` (lines 12-91)

### Bugs found

None — all destructuring patterns match jq (missing elements → null, extra ignored, shorthand {$x}, nested, computed keys, use in reduce).

### Problem

Three pattern variants are untested:

| Pattern | Example | Behavior |
|---|---|---|
| `Pattern::Array` | `. as [$a, $b]` | Missing elements → null in lenient, None in strict |
| `Pattern::Object` | `. as {x: $a}` | Missing fields → null in lenient, None in strict |
| `PatternKey::Expr` | `. as {("k"): $v}` | Computed keys — expression evaluated at runtime |

Lenient mode (`match_pattern`) is used in `as` bindings. Strict mode (`try_match_pattern`) is used in `?//` alternative operator patterns. The distinction is critical and untested.

### Test cases needed

**Array destructuring:**
- `[1,2,3] | . as [$a, $b, $c] | [$a, $b, $c]` → `[1,2,3]`
- `[1] | . as [$a, $b] | [$a, $b]` → `[1, null]` (lenient)
- `[1,2,3,4] | . as [$a, $b] | [$a, $b]` → `[1, 2]` (extra ignored)
- `null | . as [$a] | $a` → null
- `"str" | . as [$a] | $a` → behavior?

**Object destructuring:**
- `{"x":1,"y":2} | . as {x: $a, y: $b} | [$a, $b]` → `[1, 2]`
- `{"x":1} | . as {x: $a, y: $b} | [$a, $b]` → `[1, null]`
- `{"x":1} | . as {$x} | $x` → `1` (shorthand)
- Computed key: `{"k":1} | . as {("k"): $v} | $v` → `1`

**Nested destructuring:**
- `[[1,2],3] | . as [[$a, $b], $c] | [$a, $b, $c]` → `[1, 2, 3]`
- `{"a":{"b":1}} | . as {a: {b: $x}} | $x` → `1`

---

## 7. NDJSON chunk splitting edge cases

**Status:** Open
**Priority:** Medium — could cause data corruption on malformed NDJSON
**Effort:** Small
**Files:** `src/parallel/ndjson.rs` (lines 408-438), tests in `tests/ndjson.rs`

### Problem

`split_chunks()` splits on literal `\n` bytes using `memchr`. This is correct for valid NDJSON (where newlines only appear between records), but:

- Malformed NDJSON with literal newlines inside strings could be split mid-record
- A record with no trailing newline at EOF is handled (taken as last chunk), but untested
- Very large single records (> target_size) are never split — they become oversized chunks

### Test cases needed

- Record with escaped `\n` inside string: `{"msg":"hello\nworld"}` — should NOT split here (this is `\\n` in the JSON, not a literal newline — verify)
- Record with literal newline in string value (malformed but real-world): verify behavior is defined
- Single record larger than chunk target size → one oversized chunk
- Empty lines between records → empty chunks or skipped?
- File with no trailing newline
- File with only newlines

---

## 8. Loop iteration limits and silent truncation

**Status:** Open
**Priority:** Medium — silent data loss at hard-coded limits
**Effort:** Small
**Files:** `src/filter/builtins/arrays.rs` (line 10), `src/filter/eval.rs` (line 11)

### Problem

Three hard-coded limits silently truncate output:

| Limit | Value | Location | Used by |
|---|---|---|---|
| `MAX_LOOP_ITERATIONS` | 1,000,000 | `arrays.rs:10` | `repeat`, `until`, `while`, `recurse_with_filter` |
| `MAX_EVAL_DEPTH` | 256 | `eval.rs:11` | Recursive filter evaluation |
| recurse depth | 100,000 | `arrays.rs:1171` | `recurse` builtin |

When hit, these limits cause the loop to stop with no error, no warning. The user gets truncated output and has no way to know.

### Test cases needed

- `limit(5; repeat(.))` — basic repeat with limit (should work)
- `1 | [limit(1000001; repeat(.))]` — hits MAX_LOOP_ITERATIONS → verify length is 1M, not 1M+1
- `def f: f; null | f` — infinite recursion hits MAX_EVAL_DEPTH → verify error message
- `recurse` on deeply nested structure → hits 100K limit

---

## 9. `@base64d` silent failure and missing `@base32`

**Status:** Open
**Priority:** Medium — silent data corruption on invalid base64
**Effort:** Small
**Files:** `src/filter/builtins/format.rs`

### Problem

- `@base64d` (line 228-236): invalid base64 input silently returns empty or garbage — no error
- `@base32` / `@base32d`: not implemented. jq 1.7+ supports these. Users get silent empty output.

### Test cases needed

**`@base64` / `@base64d`:**
- Roundtrip: `"hello" | @base64 | @base64d` → `"hello"`
- Non-string input through `@base64` → should error
- Invalid base64 string through `@base64d` → should error (verify actual behavior)
- Padding edge cases: 1, 2, 3 char inputs
- Binary-safe: string with NUL bytes

**`@base32` / `@base32d`:**
- Document as unsupported, OR implement
- If unsupported: verify error message (not silent empty)

---

## 10. Output formatting edge cases

**Status:** Open
**Priority:** Low — unlikely to crash, but could produce corrupt terminal output
**Effort:** Small
**Files:** `src/output.rs`

### Problem

Output formatting has minimal edge case coverage:

- Raw mode (`-r`) with strings containing control characters (NUL, BEL, ESC)
- Pretty mode with deeply nested structures (recursion depth)
- ASCII mode (`-a`) with surrogate pair encoding (lines 485-490)
- Color mode with strings containing ANSI escape sequences (could corrupt colors)

### Test cases needed

- Raw output of string with `\u0000` (NUL byte)
- Raw output of string with `\t`, `\n`, `\r`
- Pretty print of 100-level nested array
- ASCII mode: `"emoji: 🎉"` → verify `\uD83C\uDF89` surrogate pair output
- Compact output of object with 1000 keys

---

## Summary

| # | Improvement | Status | Risk | Effort |
|---|---|---|---|---|
| 1 | Unit tests for `math.rs` | **Done** | Critical — division, truncation, NaN | Medium |
| 2 | Unit tests for `strings.rs` | **Done** | Critical — silent failures, panics | Medium |
| 3 | Unit tests for `date.rs` | **Done** | High — platform-dependent, precision loss | Small |
| 4 | Unit tests for untested array builtins | **Done** | High — walk drops outputs, bsearch off-by-one | Medium |
| 5 | Unit tests for `inside` + entry functions | **Done** | High — behavioral correctness unverified | Small |
| 6 | Unit tests for destructuring patterns | **Done** | High — two modes, zero tests | Medium |
| 7 | NDJSON chunk splitting edge cases | Open | Medium — data corruption risk | Small |
| 8 | Loop iteration limits | **Done** | Medium — silent truncation | Small |
| 9 | `@base64d` roundtrip tests | **Done** | Medium — silent data corruption | Small |
| 10 | Output formatting edge cases | Open | Low — terminal corruption | Small |

---

## Results

38 new e2e jq_compat tests added to `tests/e2e.rs` (782 → 820 total e2e tests).

### Bugs found and fixed

| Severity | Bug | Fix |
|---|---|---|
| **CRASH** | `combinations` panics on empty sub-array (`[[], [1,2]] \| [combinations]`) | Added `arrays.iter().any(\|a\| a.is_empty())` guard |
| **Wrong output** | `combinations(0)` returns `[]` instead of `[[]]` | Added `n == 0` special case returning `[[]]` |
| **Silent drop** | `log(0)` returns `null` instead of `-1.7976931348623157e+308` | Changed `output.rs` infinity formatting to write `±DBL_MAX` like jq |
| **Wrong value** | `nan \| isfinite` returns `false`, jq returns `true` | Changed `f.is_finite()` to `!f.is_infinite()` — jq considers NaN finite |
| **Missing error** | Math builtins on non-number input exit 0 silently | Added `require_number()` helper with `set_error()` to all 33 unary math functions |
| **Missing error** | `tonumber` on non-numeric string exits 0 silently | Added `set_error()` for unparseable strings and non-string/non-number input |

### No issues found

- All string builtins (split, join, explode, implode, index, trim, case, @format) match jq
- All date builtins (todate, fromdate, gmtime, mktime, strftime, now) match jq
- `walk`, `bsearch`, `pick`, `nth`, `repeat`, `isempty` match jq (except `combinations`)
- `inside`, `to_entries`, `from_entries`, `with_entries` match jq
- All destructuring patterns (array, object, nested, computed keys, in reduce) match jq
- `@base64` / `@base64d` roundtrip works correctly
