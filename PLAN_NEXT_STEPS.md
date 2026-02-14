# jx — Next Steps

What to focus on next, in priority order. See [PLAN.md](PLAN.md) for
full design and history.

---

## Current state (2026-02)

716 tests passing (387 unit + 291 e2e + 21 ndjson + 15 ffi + 2 conformance).
95 builtins implemented. jq conformance at **52.1%** (259/497 on jq's
official test suite). Feature compatibility at **76.5%** (126/166 features
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
| Builtin functions | 34/42 (81%) | 42/42 | 38/42 | 39/42 |
| Multiple outputs/iteration | 33/49 (67%) | 49/49 | 44/49 | 48/49 |
| String operations | 57/88 (65%) | 88/88 | 72/88 | 79/88 |
| Paths | 9/22 (41%) | 22/22 | 1/22 | 16/22 |
| User-defined functions | 25/59 (42%) | 59/59 | 44/59 | 59/59 |
| toliteral number | 12/43 (28%) | 43/43 | 25/43 | 26/43 |
| Assignment | 1/19 (5%) | 19/19 | 11/19 | 14/19 |
| Module system | 3/26 (12%) | 26/26 | 6/26 | 15/26 |
| **Total** | **259/497 (52%)** | **497/497** | **344/497** | **425/497** |

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
into Priority 3 below.

---

## Priority 2: Assignment operators

**Why:** 18/19 conformance tests failing, 0/8 feature tests passing.
Assignment is extremely common in real jq scripts — `.foo |= . + 1`,
`.[] |= select(. > 0)`, `.config.debug = true`. Without these, most
non-trivial jq scripts can't run.

**Operators:** `|=`, `+=`, `-=`, `*=`, `/=`, `%=`, `//=`, `=`

**Core complexity:** Path evaluation. jq's `|=` operates on paths, not
values. `.foo |= . + 1` means "resolve `.foo` as a path, get value,
apply filter, set result back." This needs:
- New AST variants: `Update`, `Assign`, `UpdateOp`
- Lexer tokens: `|=`, `+=`, `-=`, `*=`, `/=`, `%=`, `//=`
- Path evaluation function: `filter → Vec<PathComponent>`
- Set-at-path function (can reuse existing `setpath` logic)
- Special handling for `.[]` in path context (update all elements)

**Files:** `src/filter/lexer.rs`, `src/filter/parser.rs`,
`src/filter/eval.rs`, `src/filter/mod.rs`

---

## Priority 3: CLI flags

**Why:** 0/7 feature tests passing for these flags. Blocks scripting
use — anyone piping jx in shell scripts hits this immediately.

| Flag | Impact |
|------|--------|
| `--slurp` / `-s` | Very common. Read all inputs, wrap in array |
| `--arg name val` | Any script using `$TOKEN` etc. Seed `Env` with string binding |
| `--argjson name val` | Same but parse val as JSON |
| `--raw-input` / `-R` | Read lines as strings, not JSON |
| `-S` / `--sort-keys` | Sort object keys in output |
| `--join-output` / `-j` | No newline after each output |
| `-M` / `--monochrome-output` | Disable color (for piping) |

**Files:** `src/main.rs` — add CLI args to `Cli` struct, modify input
processing logic. `--arg`/`--argjson` need to seed `Env` before eval.

---

## Priority 4: Regex

**Why:** 0/9 feature tests. Regex is heavily used in real jq scripts
for log parsing, field extraction, data cleaning. Contributes to
31/88 string operations conformance tests failing.

**Functions:** `test(re)`, `test(re; flags)`, `match(re)`,
`match(re; flags)`, `capture(re)`, `scan(re)`, `sub(re; repl)`,
`gsub(re; repl)`, `splits(re)`

**Effort:** Add `regex` crate. Each function is independent. The `match`
output format must exactly match jq's `{offset, length, string, captures}`
structure.

**Files:** `Cargo.toml`, `src/filter/builtins.rs`

---

## Priority 5: Format strings

**Why:** 0/10 feature tests. Used for data export (`@csv`, `@tsv`),
encoding (`@base64`, `@uri`), and serialization (`@json`).

**Formats:** `@base64`, `@base64d`, `@uri`, `@csv`, `@tsv`, `@json`,
`@text`, `@html`

**Needs:** Lexer/parser change to recognize `@name` as a format token.
Each format is a small, independent implementation.

**Files:** `src/filter/lexer.rs`, `src/filter/parser.rs`,
`src/filter/builtins.rs`

---

## Priority 6: Smaller gaps

Individually small but collectively improve conformance and real-world
compatibility.

| Feature | Status | Impact |
|---------|--------|--------|
| String interpolation `"\(.x)"` | 0/2 feature tests | Common, likely parser fix |
| `in` builtin | 0/2 feature tests | Inverse of `has` |
| `combinations` | 0/2 feature tests | Cartesian product |
| `pick` | 0/1 feature tests | Extract paths from object |
| `INDEX` | 0/1 feature tests | Index array by key |
| `values` type selector | 0/1 feature tests | Filter non-null values |
| Negative array indices | 0/5 conformance tests | Edge cases in indexing |
| `strptime`/`gmtime`/`mktime` | 0/3 feature tests | Date/time gaps |
| `del` edge cases | 1/3 feature tests | Path-based deletion |
| `with_entries` edge cases | 1/2 feature tests | Object transformation |
| `explode`/`implode` edge cases | 0/3 conformance tests | Unicode handling |

---

## Conformance trajectory

| After | Est. Tests | Est. % | Key unlock |
|-------|-----------|--------|------------|
| Current | 259/497 | 52% | — |
| + assignment | ~277/497 | ~56% | update operators |
| + CLI flags | ~282/497 | ~57% | scripting use |
| + regex | ~295/497 | ~59% | string operations |
| + format strings | ~305/497 | ~61% | @base64, @csv, @tsv |
| + smaller gaps | ~320/497 | ~64% | cross-cutting fixes |

---

## Later

None of these block the next push. Revisit after Priorities 2-6.

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

31/43 toliteral number conformance tests failing. The `Value::Double(f64,
Option<Box<str>>)` raw text tracking exists but has edge cases around
arithmetic operations, conversions, and output formatting.

### Other deferred items

- `label`/`break` — advanced control flow
- `input`/`inputs` — multi-input reading
- `tostream`/`fromstream` — streaming operations
- Module system (`import`/`include`) — out of scope
- On-Demand fast path, arena allocation, string interning — perf optimizations
