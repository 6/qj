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

### Fuzz artifact

`fuzz/artifacts/fuzz_ndjson_diff/crash-9e199b53679b7f9f998af7a5cc9c54b165825417`
