# jx — Next Steps

What to focus on next, in priority order. See [PLAN.md](PLAN.md) for
full design and history.

---

## Current state (2026-02)

789 tests passing (417 unit + 334 e2e + 21 ndjson + 15 ffi + 2 conformance).
~112 builtins implemented. jq conformance at **56.3%** (280/497 on jq's
official test suite). Feature compatibility at **87.0%** (144/166 features
fully passing). Passthrough paths handle "simple query on big file" at
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
| User-defined functions | 25/59 (42%) | 59/59 | 44/59 | 59/59 |
| toliteral number | 17/43 (40%) | 43/43 | 25/43 | 26/43 |
| walk | 8/27 (30%) | 27/27 | 7/27 | 17/27 |
| Module system | 3/26 (12%) | 26/26 | 6/26 | 15/26 |
| **Total** | **280/497 (56%)** | **497/497** | **343/497** | **425/497** |

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

## Priority 4: Format strings

**Why:** 0/10 feature tests. Used for data export (`@csv`, `@tsv`),
encoding (`@base64`, `@uri`), and serialization (`@json`).

**Formats:** `@base64`, `@base64d`, `@uri`, `@csv`, `@tsv`, `@json`,
`@text`, `@html`

**Needs:** Lexer/parser change to recognize `@name` as a format token.
Each format is a small, independent implementation.

**Files:** `src/filter/lexer.rs`, `src/filter/parser.rs`,
`src/filter/builtins.rs`

---

## Priority 5: Smaller gaps

Individually small but collectively improve conformance and real-world
compatibility.

| Feature | Status | Impact |
|---------|--------|--------|
| String interpolation `"\(.x)"` | 0/2 feature tests | Common; AST+eval exist, lexer/parser missing |
| `in` builtin | 0/2 feature tests | Inverse of `has` |
| `combinations` | 0/2 feature tests | Cartesian product |
| `pick` | 0/1 feature tests | Extract paths from object |
| `INDEX` | 0/1 feature tests | Index array by key |
| `values` type selector | 0/1 feature tests | Filter non-null values |
| `strptime`/`gmtime`/`mktime` | 0/3 feature tests | Date/time gaps |
| `del` edge cases | 1/3 feature tests | Path-based deletion |

---

## Conformance trajectory

| After | Est. Tests | Est. % | Key unlock |
|-------|-----------|--------|------------|
| Current (with regex) | 280/497 | 56% | — |
| + format strings | ~294/497 | ~59% | @base64, @csv, @tsv |
| + smaller gaps | ~309/497 | ~62% | cross-cutting fixes |

---

## Later

None of these block the next push. Revisit after Priorities 3-5.

### `def` (user-defined functions)

High impact (34/59 conformance tests failing, cascading effect on walk
and conditionals categories) but high complexity. Needs: recursive
definitions, filter-argument parameters (`def map(f): [.[] | f];`),
lexical scoping. Deferred until the lower-risk items above are done.

### SmallString for Value type

Low effort, broad impact on performance. Most JSON object keys are <24
bytes. `compact_str` crate inlines short strings on the stack. Mechanical
refactor of `Value::String(String)` → `Value::String(CompactString)`.

### Number literal preservation

26/43 toliteral number conformance tests failing. The `Value::Double(f64,
Option<Box<str>>)` raw text tracking exists but has edge cases around
arithmetic operations, conversions, and output formatting.

### Other deferred items

- `label`/`break` — advanced control flow
- `input`/`inputs` — multi-input reading
- `tostream`/`fromstream` — streaming operations
- Module system (`import`/`include`) — out of scope
- On-Demand fast path, arena allocation, string interning — perf optimizations
