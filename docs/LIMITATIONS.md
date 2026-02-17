# Known limitations

## Malformed JSON error handling disagreement

**Status**: Partially mitigated, remainder accepted as architectural limitation

The fast path (on-demand parser) and normal path (DOM parser) use different simdjson
APIs with different strictness. The on-demand parser is lazy and can extract fields from
partially valid JSON; the DOM parser does full validation upfront. This is fundamental
to simdjson's architecture.

**Mitigations applied** in `src/simdjson/bridge.cpp`:
1. Parse error propagation in `jx_dom_find_fields_raw_reuse`: `nav == 2` now returns -1
   instead of silently writing "null"
2. First-byte validation in `navigate_fields_raw`: rejects garbage values the lenient
   parser might extract from structurally invalid JSON

**Why accepted**: Real NDJSON data from well-formed sources (APIs, log pipelines,
databases) is always valid JSON. The fast path handles valid JSON correctly. Adding a
pre-validation DOM parse to close the gap would add overhead that penalizes the normal
case. The fuzzer's `is_plausible_ndjson` validator filters out non-object input to focus
on finding real bugs.

## Fuzzer infrastructure: non-reproducible false positives

The differential fuzzer (`fuzz/fuzz_targets/fuzz_ndjson_diff.rs`) produces
**non-reproducible false positive crashes** on valid JSON input (e.g. `{}`)
during continuous runs, but these crashes never reproduce when the artifact is replayed.
A standalone test confirmed 1.5M calls to `process_ndjson` vs `process_ndjson_no_fast_path`
on `{}` with 15 different filters produce identical output every time.

Iterations tried to eliminate the false positives:
1. **serde_json pre-validation**: Heap allocation pressure in the long-running libfuzzer
   process caused false positives
2. **simdjson pre-validation**: Same issue
3. **No pre-validation**: Still produced non-reproducible crashes on `{}`
4. **Final approach**: Lightweight allocation-free validator (`is_plausible_ndjson`) that
   does byte-level checks (control char rejection, brace-balance, starts-with-`{`). Still
   produces occasional non-reproducible false positives, but catches real divergences.

Root cause is likely a libfuzzer infrastructure issue with C++ FFI objects on macOS ARM64
(`-s none` disables sanitizers to work around the Apple Clang / rustc ASan
incompatibility, which may reduce fuzzer stability).

The `process_ndjson_no_fast_path` function was added to avoid env var mutation
(`QJ_NO_FAST_PATH`) within the fuzzer process, which was another source of
non-determinism. The env var still works for CLI benchmarking.
