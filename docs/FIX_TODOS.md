# Fix TODOs

## Negative zero (`-0`) not preserved from JSON input

**Found by**: `fuzz_ndjson_diff` differential fuzzer (2026-02-17)
**Severity**: Low (edge case, unlikely in real data)
**Reproducer**: `echo '{"count":-0}' | qj -c '.count'` outputs `0`, jq outputs `-0`

### Root cause

simdjson's on-demand parser classifies `-0` as a signed integer with value `0`.
The C++ bridge (`emit_number` in `bridge.cpp:329`) emits it as `TAG_INT(0)`,
losing the sign bit entirely. On the Rust side it becomes `Value::Int(0)` which
serializes as `0`.

jq preserves `-0` from JSON input (outputting `-0`) but normalizes computed `-0`
(e.g. `null | -0` outputs `0`). So input-preservation is the correct behavior.

### Fix approach

In `emit_number()` in `bridge.cpp`, detect the `-0` case: when the number type
is `signed_integer`, value is `0`, and the raw JSON token starts with `-`, emit
as `TAG_DOUBLE` with `f64 = -0.0` and raw text `"-0"` instead of `TAG_INT(0)`.

Also remove the negative-zero normalization on output.rs:510 (`let f = if f == 0.0 { 0.0 } else { f }`)
since jq preserves `-0` from input. The output path should only normalize `-0`
for *computed* doubles (no raw text), but currently it normalizes all of them.

### Affected paths

- `src/simdjson/bridge.cpp` — `emit_number()` (line ~329)
- `src/output.rs` — negative zero normalization (line ~510)
- NDJSON fast path is **not** affected (it does raw byte passthrough, which correctly preserves `-0`)

### After fixing

1. Remove the `-0` normalization workaround in `fuzz/fuzz_targets/fuzz_ndjson_diff.rs`
   (the `replace(":-0}", ":0}")` block in the comparison)
2. Delete fuzz artifact: `rm fuzz/artifacts/fuzz_ndjson_diff/crash-9e199b53679b7f9f998af7a5cc9c54b165825417`
3. Rerun: `cargo +nightly fuzz run fuzz_ndjson_diff -s none -- -max_total_time=120`

### Fuzz artifact

`fuzz/artifacts/fuzz_ndjson_diff/crash-9e199b53679b7f9f998af7a5cc9c54b165825417`

## NDJSON fast path and normal path disagree on malformed JSON errors

**Found by**: `fuzz_ndjson_diff` differential fuzzer (2026-02-17)
**Severity**: Medium (data correctness — silently produces wrong output instead of error)
**Status**: Partially mitigated

Two directions of the same bug:

1. **Fast path succeeds, normal path errors**: Input `{"actor":{bob","count":2}`
   (invalid JSON) with filter `{type: .type, login: .actor.login}`. Fast path outputs
   `{"type":null,"login":null}`, normal path correctly returns a parse error.

2. **Fast path errors, normal path succeeds**: Input with embedded null bytes
   (`{"b":\x00...2,"a":1}`) with filter `.meta | keys`. Fast path returns
   `simdjson error code -1`, normal path succeeds with empty output.

### Root cause

The fast path and normal path use different simdjson entry points with different
error handling behavior on malformed input:

- Fast path uses on-demand parser (`navigate_fields_raw`) which is lazy — it can
  extract fields from partially valid JSON without fully validating the document
- Normal path uses DOM parser (`dom_parse_to_flat_buf`) which does full validation

This is a fundamental architectural difference in simdjson's two APIs.

### Mitigations applied

Two targeted C++ fixes in `src/simdjson/bridge.cpp`:

1. **Parse error propagation** in `jx_dom_find_fields_raw_reuse` (~line 1049): When
   `navigate_fields_raw` returns 2 (parse error), propagate as -1 instead of falling
   through to the else branch which silently writes "null" for missing fields.

2. **First-byte validation** in `navigate_fields_raw` (~line 766): After the on-demand
   parser extracts a raw JSON value, validate the first byte is a legal JSON value start
   (`"`, `{`, `[`, `t`, `f`, `n`, `-`, `0`-`9`). Rejects garbage values the lenient
   parser might extract from structurally invalid JSON.

These fixes catch many cases but **do not fully resolve the issue** — the on-demand parser
is fundamentally lazy and can still produce results on certain classes of malformed input.

### Current fuzzer approach

The differential fuzzer (`fuzz/fuzz_targets/fuzz_ndjson_diff.rs`) uses `serde_json` to
validate input before testing. Only well-formed JSON lines are tested, since the on-demand
vs DOM strictness difference is a known architectural limitation. The fuzzer has full
error-mismatch assertions enabled — if both paths disagree on valid JSON, it panics.

### Remaining fix (aspirational)

To fully resolve: add a pre-validation step in the NDJSON fast path that runs a full
DOM parse (or equivalent validation) before using the on-demand parser for field extraction.
This would eliminate the strictness gap but may add overhead. Benchmark before implementing.

### Affected paths

- `src/simdjson/bridge.cpp` — `navigate_fields_raw`, `jx_dom_find_fields_raw_reuse`,
  and other `_reuse` functions
- `src/parallel/ndjson.rs` — error handling in each fast-path `process_line_*` function

## NDJSON fast path FieldChain returns null instead of error on non-objects

**Found by**: `fuzz_ndjson_diff` differential fuzzer (2026-02-17)
**Severity**: Low (non-object NDJSON lines are rare in practice)
**Reproducer**: `printf '8\n2\n' | qj -c '.a.b.c'` — fast path outputs `null\nnull\n`,
normal path errors with "Cannot index number with string"

### Root cause

The FieldChain fast path calls `dom_find_field_raw_reuse` which returns "not found"
(treated as null) when the input is a non-object JSON value (number, string, array,
bool, null). The normal evaluator correctly returns an error matching jq's behavior
(`Cannot index number with string "a"`).

### Fix approach

In the C++ bridge field-lookup functions (`navigate_fields_raw`,
`jx_dom_find_field_raw_reuse`), check the document's root type. If it's not an
object, return an error code instead of "field not found" / null.

### Fuzzer note

The differential fuzzer restricts input to JSON objects (`is_valid_ndjson_objects`)
to avoid this known divergence. After fixing, change back to `is_valid_ndjson` to
accept all valid JSON values.
