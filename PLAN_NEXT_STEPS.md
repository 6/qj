# jx — Next Steps

What to focus on next, in priority order. See [PLAN.md](PLAN.md) for
full design and history.

---

## Current state (2026-02)

837 tests passing (437 unit + 364 e2e + 21 ndjson + 15 ffi).
~121 builtins implemented. `def` (user-defined functions) implemented with
filter parameters, `$param` sugar, recursion, arity overloading, and closures.
jq conformance at **60.0%** (298/497 on jq's official test suite). Feature
compatibility at **93.7%** (155/166 features fully passing). Passthrough
paths handle "simple query on big file" at
12-63x jq, 3-14x jaq. Parallel NDJSON processing at ~10x jq, ~5.6x jaq.

| Filter (49MB file) | jx | jq | jaq | vs jq | vs jaq |
|---------------------|----|----|-----|-------|--------|
| `-c '.'` | 18ms | 1,157ms | 253ms | 63x | 14x |
| `-c '.statuses'` | 74ms | 1,132ms | 246ms | 15x | 3.3x |
| `.statuses \| length` | 33ms | 398ms | 167ms | 12x | 5.1x |
| `.statuses \| keys` | 31ms | 393ms | 165ms | 13x | 5.3x |

| Filter (1M NDJSON) | jx | jq | jaq | vs jq | vs jaq |
|---------------------|----|----|-----|-------|--------|
| `.name` | 120ms | 1,230ms | 670ms | 10x | 5.6x |

Non-passthrough eval is competitive with jaq (~1x) and ~2-4x jq.

**jq conformance by category:**

| Category | jx | jq | jaq | gojq |
|---|---|---|---|---|
| Conditionals | 37/44 (84%) | 44/44 | 40/44 | 41/44 |
| Builtin functions | 34/42 (81%) | 42/42 | 37/42 | 39/42 |
| String operations | 60/88 (68%) | 88/88 | 72/88 | 79/88 |
| Multiple outputs/iteration | 33/49 (67%) | 49/49 | 44/49 | 48/49 |
| Assignment | 10/19 (53%) | 19/19 | 11/19 | 14/19 |
| Paths | 10/22 (45%) | 22/22 | 1/22 | 16/22 |
| User-defined functions | 35/59 (59%) | 59/59 | 44/59 | 59/59 |
| toliteral number | 21/43 (49%) | 43/43 | 25/43 | 26/43 |
| walk | 8/27 (30%) | 27/27 | 7/27 | 17/27 |
| Module system | 3/26 (12%) | 26/26 | 6/26 | 15/26 |
| **Total** | **298/497 (60%)** | **497/497** | **343/497** | **425/497** |

---

## ~~Priority 1: Parallel NDJSON (Step 4)~~ COMPLETE

Implemented with rayon work-stealing thread pool. Auto-detection via
heuristic + `--jsonl` flag. ~1MB chunks processed in parallel, output
merged in order. See [PLAN.md Phase 2 results](PLAN.md#phase-2-results-apple-silicon-m-series-2025-02).

---

## ~~Priority 1.5: Core language features~~ COMPLETE

From the original Priority 2 list, these are done:

- **Array slicing** `.[2:5]`, `.[start:]`, `.[:end]`, `.[-2:]` — full implementation with string support
- **Variable binding** `. as $x | ...` — scoped `Env` with `bind_var()`
- **`reduce`** `reduce .[] as $x (init; update)` — generator-based evaluation
- **`foreach`** — bonus, wasn't in original plan

Remaining from original Priority 2 (`--slurp`, `--arg`/`--argjson`) folded
into CLI flags below.

---

## ~~Priority 2: Assignment operators~~ COMPLETE

All 8 operators implemented: `|=`, `+=`, `-=`, `*=`, `/=`, `%=`, `//=`, `=`.
`Filter::Assign` AST node with `AssignOp` enum, right-recursive parsing,
fast O(N) `update_recursive()` path for common patterns plus `eval_assign_via_paths()`
fallback. Supports auto-structure creation, element deletion via `|= empty`,
cross-reference semantics for `=` vs `|=`. Feature compat: 0/8 → 8/8.
Conformance: 1/19 → 10/19 (remaining 9 depend on `def` and other missing features).

---

## ~~Priority 2.5: CLI flags~~ COMPLETE

All 7 flags implemented: `--slurp`/`-s`, `--arg`, `--argjson`,
`--raw-input`/`-R`, `--sort-keys`/`-S`, `--join-output`/`-j`,
`--monochrome-output`/`-M`. Feature compat CLI flags: 0/7 → 7/7.
Env threading via `eval_filter_with_env`, sort_keys through write
functions, sequential fallback for NDJSON when env is non-empty.

---

## ~~Priority 3: Regex~~ COMPLETE

All 7 regex builtins implemented using the `regex` crate: `test(re; flags)`,
`match(re; flags)`, `capture(re; flags)`, `scan(re; flags)`,
`sub(re; repl; flags)`, `gsub(re; repl; flags)`, `splits(re; flags)`.
Supports jq flags: `i` (case-insensitive), `m` (multiline), `s` (single-line),
`g` (global), `x` (extended/verbose). sub/gsub evaluate replacement as a
filter against the match object, matching jq semantics. Feature compat
regex: 0/9 → 9/9. Overall feature compat: 83.4% → 87.0%.

---

## ~~Priority 4: String interpolation, format strings, small builtins~~ COMPLETE

**String interpolation** `"\(.x)"`: Lexer produces `InterpStr(Vec<StringSegment>)`
tokens, parser builds `Filter::StringInterp(Vec<StringPart>)`. AST and evaluator
already existed; only lexer/parser were missing. Handles nested parens and
nested strings inside interpolation expressions.

**Format strings** (10 builtins): `@base64`, `@base64d`, `@uri`, `@csv`,
`@tsv`, `@json`, `@text`, `@html`, `@sh`. Lexer produces `Format(String)`
token, parser maps to `Filter::Builtin(name, vec![])`. Uses `base64` crate
for base64 encode/decode; all others implemented with std.

**Small builtins**: `in(expr)` (inverse of `has`), `combinations`/`combinations(n)`
(cartesian product).

Feature compat: 87.0% → 93.7% (155/166). Conformance: 280/497 → 288/497 (58%).
String interpolation had cross-cutting conformance impact across walk,
conditionals, and string operations categories.

---

## ~~Priority 5: `def` (user-defined functions)~~ COMPLETE

Implemented `def` with:
- AST: `Filter::Def { name, params, body, rest }` node
- Parser: `def name(params): body; rest` syntax, `$param` sugar
- Evaluator: function table in `Env`, lexical scoping, recursion via
  self-registration in body_env (only for `is_def` functions, not param wrappers)
- Filter-argument parameters: params bound as zero-arg functions with caller's
  closure env, enabling generator semantics and backtracking
- Arity overloading: `(name, arity)` keying allows same-name functions with
  different param counts
- Closures: lexical scoping of `$var` bindings captured at def time

Impact: +10 conformance tests (288 → 298, 58% → 60%). User-defined functions
category: 25/59 → 35/59. Remaining failures are mostly: `def` inside expressions
(parser limitation), destructuring bind patterns, `label`-`break`, and complex
`try`-`catch` interactions.

---

## Priority 6: Quick wins (remaining feature gaps)

10 features still at N in feature_results.md. Some are easy:

| Feature | Status | Effort | Impact |
|---------|--------|--------|--------|
| `values` type selector | 0/1 | Low | Filter non-null values |
| `del` edge cases | 1/3 | Low | Complex path deletion |
| `pick` | 0/1 | Low | Extract paths from object |
| `INDEX` | 0/1 | Low | Index array by key |
| `gmtime`/`mktime` | 0/2 | Medium | Unix epoch ↔ broken-down time |
| `strptime` | 0/1 | Medium | Date string parsing |
| `label`-`break` | 0/1 | Medium | Advanced control flow |
| `input`/`inputs` | 0/1 | Medium | Multi-input reading |

---

## Conformance trajectory

| After | Est. Tests | Est. % | Key unlock |
|-------|-----------|--------|------------|
| Current | 298/497 | 60% | — |
| + quick wins (P6) | ~310/497 | ~62% | feature gaps closed |
| + def-in-expressions | ~330/497 | ~66% | cascading unlock across categories |
| + number literal fixes | ~350/497 | ~70% | toliteral precision |

---

## Later

None of these block the next push. Revisit after Priorities 5-6.

### SmallString for Value type

Low effort, broad impact on performance. Most JSON object keys are <24
bytes. `compact_str` crate inlines short strings on the stack. Mechanical
refactor of `Value::String(String)` → `Value::String(CompactString)`.

### Number literal preservation

22/43 toliteral number conformance tests failing. The `Value::Double(f64,
Option<Box<str>>)` raw text tracking exists but has edge cases around
arithmetic operations, conversions, and output formatting.

### Module system

23/26 failures but low ROI — even jaq only passes 6/26, gojq 15/26.
Requires module loading infrastructure (`import`/`include`/`modulemeta`).

### Other deferred items

- `tostream`/`fromstream` — streaming operations
- On-Demand fast path, arena allocation, string interning — perf optimizations
