# Security Plan

jx processes **untrusted input** (arbitrary JSON from files and stdin) through a
**C++ FFI boundary** (simdjson) at multi-GB/s throughput. Rust's memory safety
guarantees stop at that boundary. This document covers real risks, what's already
mitigated, and what needs hardening.

---

## 1. Threat Model

- **Input is attacker-controlled.** JSON from files, stdin, or pipes can be
  arbitrarily large, deeply nested, or malformed. Any crash or memory corruption
  reachable from malformed input is a security bug.
- **Runs with user permissions.** No sandboxing, no privilege separation.
  jx can read any file the user can read. A vulnerability means arbitrary
  code execution as the user.
- **Performance amplifies risk.** Processing at 7+ GB/s means even a brief
  window of undefined behavior processes a large volume of attacker input
  before detection.

---

## 2. FFI Boundary (Current — Phase 0)

### What's already done well

Some commonly raised concerns are already addressed. The current bridge is solid:

**Padding enforcement** (`bridge.rs:124-127`, `bridge.rs:229`):
```rust
assert!(
    buf.len() >= json_len + padding(),
    "buffer must include SIMDJSON_PADDING extra bytes"
);
```
This is `assert!()`, **not** `debug_assert!()` — it is present in release builds.
The C++ side uses `padded_string_view(buf, len, len + SIMDJSON_PADDING)` which
tells simdjson exactly how much padding is available. Both layers enforce the
invariant independently.

**Lifetime tracking** (`bridge.rs:123`, `bridge.rs:143-146`):
```rust
pub fn parse<'a>(&'a mut self, buf: &'a [u8], json_len: usize) -> Result<Document<'a>>
```
`Document<'a>` borrows `&'a mut Parser` via the `parser` field + `PhantomData`.
This prevents use-after-free at compile time: you cannot parse a new document
while an old `Document` exists, and the document cannot outlive its parser.
The `&'a [u8]` buffer lifetime is also tied in, preventing the buffer from being
freed while the document references it.

**Exception safety** (`bridge.cpp`): Every C++ function is wrapped in
`try { ... } catch (...) { return -1; }`. No C++ exceptions cross the FFI
boundary. The catch-all `(...)` is correct here — we don't need exception type
information on the Rust side, just success/failure.

**Error propagation** (`bridge.rs:92-98`): C++ error codes (simdjson error enum
cast to `int`) are checked by `check()` and converted to `anyhow::Result`.
Negative values (-1) indicate caught C++ exceptions.

**FFI overhead**: Measured at <5% (often <1%), confirming the bridge is thin.

### What needs hardening

**`build.rs` lacks compiler warnings** (`build.rs:1-15`):
The C++ compilation uses `-O3` but no warning flags. Adding `-Wall -Wextra` to
the `cc::Build` would catch potential issues in `bridge.cpp` at build time.
This is easy to fix and has no runtime cost.

**No `// SAFETY:` comments on unsafe blocks** (`bridge.rs`):
There are 11 `unsafe` blocks in `bridge.rs` with no `// SAFETY:` documentation.
Each one has clear invariants (non-null pointer, valid padding, lifetime
guarantees) that should be documented for maintainability. This doesn't affect
runtime safety but makes review and auditing harder.

**No fuzz testing at the FFI boundary**: The bridge processes arbitrary input
bytes through C++ code. This is the single highest-value fuzz target in the
project. Any simdjson bug triggered by malformed input would be reachable.

**`jx_parser_free` doesn't null-check** (`bridge.cpp:37-39`):
```cpp
void jx_parser_free(JxParser* p) {
    delete p;
}
```
`delete nullptr` is safe per the C++ standard (it's a no-op). However, this
should be documented in a comment for anyone unfamiliar with this guarantee.
The Rust `Drop` impl always passes a non-null pointer anyway (checked at
allocation in `Parser::new()`), so this is defense-in-depth only.

**`read_padded()` reads entire file with no size limit** (`bridge.rs:74-81`):
```rust
pub fn read_padded(path: &Path) -> Result<Vec<u8>> {
    let data = fs::read(path)?;
    // ...
}
```
`fs::read()` loads the entire file into memory. A multi-TB file causes OOM
with no graceful error. Need either a configurable `--max-input-size` limit
or at minimum a check against available memory / a reasonable default cap.

**`len + SIMDJSON_PADDING` overflow in padding helpers** (`bridge.rs:77`):
```rust
let mut buf = Vec::with_capacity(data.len() + pad);
```
If `data.len()` is close to `usize::MAX`, `data.len() + pad` could overflow.
Rust's `Vec::with_capacity` would then allocate a tiny buffer. In practice this
requires a file close to `usize::MAX` bytes (~18 EB on 64-bit), which can't
happen on current hardware, but a `checked_add` is still good hygiene.

---

## 3. Input Validation & DoS (Phases 1-5)

These are requirements for future phases — none of this code exists yet.

### Recursion depth

**Filter parser** (Phase 1): PLAN calls for a recursive descent parser. Must
include a depth limit or a deeply nested filter like `(((((((...))))))` causes
a stack overflow. 256 is reasonable — jq allows ~1000 but nobody writes filters
that deep.

**Filter evaluator** (Phase 5): Recursive `def` functions can recurse
indefinitely. Must include an eval recursion depth limit (1024). This also
covers pathological `reduce`/`foreach` nesting.

**JSON nesting**: simdjson has an internal depth limit of 1024 levels. Deeply
nested `[[[[...]]]]` is rejected by simdjson with an error code, which
`check()` converts to a Rust error. This is already handled in the Phase 0
bridge.

### Resource exhaustion

| Vector | Phase | Mitigation |
|--------|-------|------------|
| Large file via `read_padded()` | **0 (now)** | Add `--max-input-size` or check file size before reading |
| Infinite stdin stream | **2** | Stdin streaming should process in chunks; without chunking, a single huge object causes OOM |
| `--slurp` accumulates all input | **1** | By design — document that `--slurp` requires memory proportional to input. Same behavior as jq |
| NDJSON per-chunk buffers | **2** | Plan calls for bounded thread pool with ~1MB fixed chunks. Dynamically sized output buffers will need bounds checking |
| Regex in `test()`/`match()` | **5** | Add regex size limits and/or timeout to prevent ReDoS |

### Non-issues

**"Billion Laughs" attacks don't apply to JSON.** The Billion Laughs attack
exploits XML entity expansion — a feature JSON doesn't have. JSON has no
entities, no references, no macros. Each byte of input produces at most one
byte of parsed output. The actual DoS vectors are: (1) large input size,
(2) deeply nested structures (handled by simdjson's depth limit), and
(3) pathological user-defined filters (recursive `def`, complex regex).

---

## 4. On-Demand API Constraints

### On-Demand does NOT skip validation

A common misconception is that simdjson's On-Demand API "skips validation." This is incorrect.
simdjson's **stage 1** (structural indexing) runs over the entire document
in both DOM and On-Demand modes. Stage 1 uses SIMD to identify all structural
characters (`{`, `}`, `[`, `]`, `:`, `,`, `"`) and validates structural
correctness (balanced braces/brackets, proper quoting). This happens before
any field access.

On-Demand is **forward-only** but still **structurally validated**. The
difference from DOM is that On-Demand doesn't materialize values until
accessed — it navigates the structural index lazily. This is a performance
optimization, not a validation bypass.

### What IS a real concern

**Forward-only access means out-of-order filters need fallback.** If a filter
accesses `.b` then `.a` (fields out of document order), the On-Demand cursor
has already passed `.a`. When the On-Demand fast path is added (Phase 1.5),
it must detect this at filter compile time and fall back to DOM parsing. This
is a correctness issue, not a security issue — the fallback would produce
correct results, just at DOM speed (~2-3 GB/s instead of ~7 GB/s).

**String content validation vs structural validation.** simdjson validates
structural JSON in stage 1, and validates string escaping (e.g., rejects
unescaped control characters in strings, validates `\uXXXX` escapes). However,
simdjson does **not** reject lone surrogates (`\uD800` without a trailing
`\uDC00`) — it passes them through. This matches jq's behavior. Document
this if downstream consumers expect fully-validated UTF-8.

---

## 5. Output Serialization Security (Phase 1)

None of this code exists yet. PLAN.md Phase 1d describes a multi-tier output
approach. These are security requirements for when it gets built.

### Passthrough (zero-copy)

The plan calls for copying raw bytes from the input buffer directly to stdout.

**Risk:** If a filter selects a sub-object and the raw bytes contain content
that simdjson validated structurally but didn't re-encode (e.g., lone surrogate
pairs in `\uXXXX` escapes), they pass through to downstream consumers. A
downstream JSON parser may reject what jx emitted.

**Requirement:** Document that passthrough output is byte-identical to input —
not re-validated or re-encoded. This is the same behavior as jq's compact
output for identity filters.

### SIMD string escaping

If a vectorized escaping implementation has an off-by-one or misses a control
character (`0x00`-`0x1F`), the output is invalid JSON that downstream tools may
interpret differently.

**Requirement:** Comprehensive test suite comparing jx output byte-for-byte
against jq for the same inputs. Fuzz the escaping code path with arbitrary
UTF-8, control characters, and edge cases (empty strings, strings at buffer
boundaries, maximum-length strings).

### Buffer size arithmetic

Integer overflow in buffer size calculations: Rust panics on overflow in debug
but **wraps in release**. All dynamically computed buffer sizes must use
`checked_add`/`checked_mul`.

On the C++ side: `len + SIMDJSON_PADDING` could theoretically overflow
`size_t` near `SIZE_MAX`. The Rust-side `assert!()` catches this in practice
(since `buf.len()` can't exceed `isize::MAX` in safe Rust, and
`SIMDJSON_PADDING` is small), but document the reasoning.

If Phase 2 uses pre-allocated fixed-size per-thread buffers (128KB-1MB as
planned), those are not at risk — their sizes would be compile-time constants.

### Raw pointer lifetime in passthrough

simdjson's `raw_json_token()` and string accessors return pointers into the
parser's internal buffer. These pointers are valid only while the
`Document`/`Parser` is alive and no new document is parsed. The existing Rust
wrapper enforces this with the `Document<'a>` borrow — the `'a` lifetime ties
the document to both the parser and the input buffer. Any future passthrough
output code must go through the `Document` API to inherit these lifetime
guarantees.

---

## 6. Environment Variable Exposure (Phase 5)

PLAN.md includes `env` / `$ENV` support as a Tier 3 filter. When implemented,
jq compatibility means exposing all environment variables by default —
`$ENV.HOME`, `$ENV.AWS_SECRET_ACCESS_KEY`, etc. would all be accessible.

**Policy:** Match jq's behavior for compatibility. `$ENV` should read any
environment variable accessible to the process.

**Documentation:** When implemented, the man page should note that `$ENV`
exposes all environment variables. Users running jx in contexts where env vars
contain secrets (CI pipelines, containers) should be aware that a filter like
`$ENV` (no field) dumps all variables.

---

## 7. Build & Supply Chain

**simdjson is vendored** — `simdjson.h` and `simdjson.cpp` are checked into the
repo, not pulled from a crate registry or package manager. This eliminates
supply chain attacks via crate squatting but means updates require manual action.

| Item | Status | Action |
|------|--------|--------|
| Pin simdjson version | **Not done** | Add version comment in vendored files and `build.rs` |
| Update procedure | **Not documented** | Document: download from simdjson releases, replace files, run tests |
| `-Wall -Wextra` in build.rs | **Not done** | Add to `cc::Build` |
| AddressSanitizer in CI | **Not done** | Add debug build with ASan/UBSan for CI |
| ThreadSanitizer in CI | **Not done** | Add for Phase 2 parallel code |

---

## 8. Recommended Hardening Checklist

### Phase 1 (filter parser + evaluator + output)

- [ ] Add recursion depth limit to filter parser (256)
- [ ] Add eval recursion depth limit (1024)
- [ ] Add `// SAFETY:` comments to all 11 unsafe blocks in `bridge.rs`
- [ ] Add `-Wall -Wextra` to `build.rs` C++ compilation
- [ ] Set up `cargo-fuzz` harness for simdjson FFI boundary (arbitrary JSON bytes)
- [ ] Set up `cargo-fuzz` harness for filter lexer + parser (arbitrary filter strings)
- [ ] Add file size check in `read_padded()` with configurable `--max-input-size`
- [ ] Use `checked_add` for `data.len() + pad` in `read_padded()` and `pad_buffer()`
- [ ] Passthrough output: document that raw bytes are not re-validated
- [ ] SIMD string escaper: byte-for-byte conformance tests vs jq output
- [ ] Fuzz the string escaping path with arbitrary bytes + control characters
- [ ] Pin simdjson version in a comment (`simdjson.h` header or `build.rs`)

### Phase 2 (parallel NDJSON)

- [ ] Bound per-chunk output buffer sizes (cap dynamic growth)
- [ ] Ensure thread panic isolation (no poisoned state across threads)
- [ ] Add ThreadSanitizer CI job for parallel code paths
- [ ] Test with AddressSanitizer on NDJSON edge cases (truncated lines, empty chunks)

### Phase 5 (Tier 3 filters)

- [ ] Document `$ENV` behavior in man page (exposes all env vars)
- [ ] Add regex size limit for `test()`/`match()` pattern compilation
- [ ] Add regex match timeout or step limit (ReDoS prevention)
- [ ] Fuzz `def` recursion and `reduce`/`foreach` with pathological inputs

---

## 9. Security Profile Summary

| Component | Risk | Primary Concern | Status |
|-----------|------|-----------------|--------|
| **FFI bridge** (`bridge.rs` / `bridge.cpp`) | Medium | Memory safety at C++ boundary | Padding enforced, lifetimes tracked, exceptions caught. Needs fuzz testing and `SAFETY:` docs |
| **SIMD parsing** (simdjson) | Low | Bugs in vendored C++ library | simdjson is heavily fuzzed upstream. Pin version, update periodically |
| **Filter parser** (Phase 1) | Medium | Stack overflow from deep nesting | Not yet built — must include depth limit |
| **Filter evaluator** (Phase 1-5) | Medium | Infinite recursion via `def`, resource exhaustion | Not yet built — must include eval depth limit |
| **Output formatting** (Phase 1) | Medium | Invalid JSON output from escaping bugs, buffer overflow from size arithmetic | Not yet built — needs conformance tests and `checked_add` |
| **Parallel processing** (Phase 2) | Medium | Data races, thread-safety of shared state | Not yet built — each thread gets own parser (by design). Needs TSan CI |
| **File I/O** (`read_padded`) | Low-Medium | OOM on huge files | No size limit. Add `--max-input-size` or pre-check |
| **mmap** (Phase 2) | Low | SIGBUS on truncated file | Use `madvise` + handle SIGBUS, or fall back to `read()` |
| **`$ENV`** (Phase 5) | Low | Information disclosure | By design (matches jq). Document it |

---

## 10. Fuzzing Strategy

Recommend `cargo-fuzz` with libFuzzer. Five harnesses, ordered by priority:

| Harness | Target | Input | Goal |
|---------|--------|-------|------|
| `fuzz_parse` | `simdjson::Parser::parse()` | Arbitrary bytes (with padding) | Crash/UB in simdjson via FFI |
| `fuzz_filter_parser` | Filter lexer + parser | Arbitrary UTF-8 strings | Stack overflow, panic in parser |
| `fuzz_ndjson` | `iterate_many_count` / `iterate_many_extract_field` | Arbitrary bytes as NDJSON | Crash in iterate_many |
| `fuzz_output` | String escaping + output formatting | Arbitrary `Value` trees | Invalid JSON output, buffer overflows |
| `fuzz_e2e` | Full pipeline (parse filter, parse JSON, evaluate, format) | `(filter_string, json_bytes)` pair | Any crash or divergence from jq |

The `fuzz_parse` harness is highest priority — it exercises the most
safety-critical code (C++ via FFI processing untrusted input) with the simplest
setup (just feed bytes to the parser).

Run fuzz harnesses in CI with a time budget (e.g., 5 minutes per harness per PR)
and longer runs (hours) on a nightly schedule.
