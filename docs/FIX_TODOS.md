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
2. Rerun: `cargo +nightly fuzz run fuzz_ndjson_diff -s none -- -max_total_time=120`

---

## Low priority: NDJSON fast path edge cases on non-standard input

The following issues were found by the differential fuzzer but are deprioritized because
they only trigger on input that doesn't occur in real-world NDJSON workloads (malformed
JSON, non-object values, leading whitespace). The fuzzer's `is_plausible_ndjson`
validator filters these out so the fuzzer focuses on semantic divergences on realistic
input instead.

### Fuzzer infrastructure notes

The differential fuzzer (`fuzz/fuzz_targets/fuzz_ndjson_diff.rs`) has a known limitation:
it produces **non-reproducible false positive crashes** on valid JSON input (e.g. `{}`)
during continuous runs, but these crashes never reproduce when the artifact is replayed.
A standalone test confirmed 1.5M calls to `process_ndjson` vs `process_ndjson_no_fast_path`
on `{}` with 15 different filters produce identical output every time.

Iterations tried to eliminate the false positives:
1. **serde_json pre-validation**: Parsing each line with `serde_json::from_str` caused heap
   allocation pressure in the long-running libfuzzer process, leading to false positives
2. **simdjson pre-validation**: Using `dom_parse_to_value` for validation had the same issue
3. **No pre-validation**: Skipping validation entirely and only comparing when both paths
   succeed — still produced non-reproducible crashes on `{}`
4. **Final approach**: Lightweight allocation-free validator (`is_plausible_ndjson`) that
   does byte-level checks (control char rejection, brace-balance, starts-with-`{`). Still
   produces occasional non-reproducible false positives, but catches real malformed-input
   divergences (e.g. `,[` where the fast path does raw passthrough while the normal path
   produces different output).

The root cause is likely a libfuzzer infrastructure issue with C++ FFI objects on
macOS ARM64 (`-s none` disables sanitizers to work around the Apple Clang / rustc
ASan incompatibility, which may reduce fuzzer stability).

The `process_ndjson_no_fast_path` function was added to avoid env var mutation
(`QJ_NO_FAST_PATH`) within the fuzzer process, which was another source of
non-determinism. The env var still works for CLI benchmarking.

### Malformed JSON error handling disagreement

**Status**: Partially mitigated with C++ bridge fixes, remainder accepted as architectural limitation

The fast path (on-demand parser) and normal path (DOM parser) use different simdjson
APIs with different strictness. The on-demand parser is lazy and can extract fields from
partially valid JSON; the DOM parser does full validation upfront. This is fundamental
to simdjson's architecture.

**Mitigations applied** in `src/simdjson/bridge.cpp`:
1. Parse error propagation in `jx_dom_find_fields_raw_reuse`: `nav == 2` now returns -1
   instead of silently writing "null"
2. First-byte validation in `navigate_fields_raw`: rejects garbage values the lenient
   parser might extract from structurally invalid JSON

**Why deprioritized**: Real NDJSON data from well-formed sources (APIs, log pipelines,
databases) is always valid JSON. The fast path handles valid JSON correctly. Adding a
pre-validation DOM parse to close the gap would add overhead that penalizes the normal
case. The fuzzer validates input with serde_json to focus on finding real bugs.

### FieldChain returns null instead of error on non-objects

**Reproducer**: `printf '8\n2\n' | qj -c '.a.b.c'` — fast path outputs `null`, normal
path errors with "Cannot index number with string"

**Why deprioritized**: Real NDJSON is always object-per-line. Non-object JSON values
(bare numbers, strings, arrays) never appear in NDJSON workloads. The fuzzer restricts
to JSON objects via `is_valid_ndjson_objects`.

### Leading/trailing whitespace handling in NDJSON lines — Partially fixed

`process_line` in `src/parallel/ndjson.rs` now trims both leading (space, tab) and
trailing (space, tab, CR) whitespace before passing to fast-path handlers. Previously
only trailing whitespace was trimmed, so raw passthrough of select-matching lines
preserved leading whitespace.

However, `process_line` does not strip `\r` from the leading edge, or other JSON-legal
whitespace like `\r` embedded in the line. serde_json and simdjson may handle these
differently. The differential fuzzer requires trimmed lines to start with `{` to avoid
these edge cases, since real NDJSON never has embedded control characters outside JSON
string values.

### Select fast path raw passthrough preserves internal whitespace

**Reproducer**: `printf '{ \t}\n' | qj -c 'select(.value == null)'` — fast path outputs
`{ \t}` (raw passthrough), normal path outputs `{}` (re-serialized compact)

When a select-type fast path matches, it outputs the raw NDJSON line bytes via
`output_buf.extend_from_slice(trimmed)`. In compact mode, the normal path
re-serializes without internal whitespace. This diverges on non-compact input.

**Why deprioritized**: Machine-generated NDJSON (APIs, log pipelines, databases) is
virtually always compact. The differential fuzzer restricts input to compact JSON
objects (round-trips through serde_json identically) to avoid this. A full fix would
require the select fast path to re-serialize matching lines in compact mode, which
partially defeats the purpose of raw passthrough.
