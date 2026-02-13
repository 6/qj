# jx project analysis

Honest assessment of where jx stands after a half-day build. Written as a strategic
reference for deciding where to invest effort next.

---

## What jx is today

jx is a **fast JSON extractor**, not a jq replacement. It handles the most common
jq operations (field access, iteration, select, object construction, 30+ builtins)
and does them significantly faster than jq on large inputs thanks to SIMD parsing
and parallel NDJSON processing. But it covers only 26% of jq's official test suite
and is missing several features that real-world scripts depend on (variables,
slicing, `--slurp`, `--arg`).

### Genuine strengths

- **SIMD speed is real.** simdjson On-Demand delivers 7-9 GB/s parse throughput on
  Apple Silicon. The passthrough fast path (identity compact) hits 63x faster than
  jq and 14x faster than jaq on a 49MB file. This isn't a benchmarking trick — it's
  `simdjson::minify()` running at SIMD speeds.
- **Architecture is sound.** The layered design (SIMD parse → flat token buffer →
  Rc-wrapped Value tree → generator evaluator) gives clean separation between the
  C++ FFI boundary and the Rust evaluator. Passthrough fast paths bypass the Value
  pipeline entirely for eligible filters.
- **Parallel NDJSON works.** rayon-based chunked processing gives ~10x over jq and
  ~5.6x over jaq on 1M-line NDJSON, with correct output ordering.
- **Benchmarks are honest.** End-to-end measurements with hyperfine, not
  parse-only microbenchmarks. The README reports real numbers and the narrower wins
  on complex filters, not just the best case.

### Real weaknesses

- **26% compatibility is low.** Anyone trying `--slurp`, `$var`, `.[2:5]`,
  `reduce`, or `def` in their first session will hit a wall. jaq is at 69%, gojq
  at 85%.
- **Speed advantage narrows on complex filters.** On iterate+field patterns over
  large files, jx is only ~1.08x faster than jaq (was 0.67x before Rc optimization).
  On small files with complex filters, it's roughly parity.
- **Competitive landscape is real.** jaq already exists, is well-maintained, has
  69% compat, and is fast enough for most users. gojq has 85% compat. "Faster on
  large data" is a real niche but a narrow one.

---

## Conformance test suite analysis

### What the 497-test suite actually tests

The jq.test suite (vendored from jqlang/jq 1.8.1) tests **filter expressions only**.
The test runner (`run_compat.sh`) always invokes tools the same way:

```
printf '%s' "$input" | tool -c -- "$filter"
```

This means:

1. **No CLI flags are tested.** `--slurp`, `--arg`, `--argjson`, `--raw-output`,
   `--raw-input`, `--null-input`, `--sort-keys` — none of these are exercised.
   A tool could pass 100% of jq.test while having zero CLI flag support.

2. **Error cases are skipped.** 19 `%%FAIL` test blocks (expected-error cases)
   are skipped entirely by the runner. Error handling and error message
   compatibility are not measured.

3. **Output is always compact.** Every test uses `-c`, so pretty-print correctness,
   color output, and `--tab`/`--indent` behavior are untested.

4. **Input is always piped.** File argument handling, multiple file inputs, and
   stdin-vs-file behavior differences are not covered.

### Per-category breakdown with real-world weighting

| Category | Tests | jx | jaq | gojq | Real-world frequency |
|----------|------:|---:|----:|-----:|---------------------|
| Field access, piping | 14 | 8 | 11 | 13 | Very high |
| Builtin functions | 42 | 25 | 38 | 39 | Very high |
| Conditionals | 44 | 25 | 40 | 41 | High |
| Multiple outputs, iteration | 49 | 15 | 44 | 48 | High |
| Simple value tests | 19 | 10 | 18 | 18 | High |
| string operations | 88 | 27 | 72 | 79 | High |
| User-defined functions | 59 | 13 | 44 | 59 | Medium |
| Variables | 11 | 0 | 11 | 11 | Medium |
| Slices | 5 | 0 | 4 | 5 | Medium |
| Negative array indices | 5 | 0 | 2 | 2 | Medium |
| Assignment | 19 | 0 | 11 | 14 | Medium |
| Paths | 22 | 0 | 1 | 16 | Low |
| Basic numbers tests | 18 | 0 | 7 | 18 | Low |
| Dictionary construction | 3 | 2 | 3 | 3 | Medium |
| toliteral number | 43 | 3 | 25 | 26 | Low |
| walk | 27 | 0 | 7 | 17 | Low |
| module system | 26 | 3 | 6 | 15 | Very low |
| explode/implode | 3 | 0 | 0 | 1 | Very low |

### Suite composition bias

The test suite overweights some rarely-used features and underweights common ones:

**Overrepresented** (19.3% of suite for rarely-used features):
- `toliteral number`: 43 tests (8.7%) — number formatting edge cases
- `walk`: 27 tests (5.4%) — recursive transforms, rarely used in one-liners
- `module system`: 26 tests (5.2%) — `import`/`include`, almost never used interactively

**Underrepresented** (6.0% of suite for very common features):
- Field access: 14 tests (2.8%) — the single most common jq operation
- Variables: 11 tests (2.2%) — common in scripts
- Slices: 5 tests (1.0%) — used frequently for array manipulation

**Completely absent** (not tested at all):
- All CLI flags (`--slurp`, `--arg`, `--raw-output`, etc.)
- Error handling and error messages
- Multiple input files
- NDJSON/streaming behavior
- Pretty-print formatting

---

## Failure analysis: ERRORs vs wrong output

Of jx's 366 failures against jq.test, the breakdown is:

- **282 ERRORs** — jx produces no output at all (unknown builtins, parse failures
  on unsupported syntax like `as $var`, `def`, `try-catch`, `.[start:end]`, etc.)
- **83 FAILs** — jx produces output but it's wrong (bugs in existing implementations)
- **1 test** — counted in both (edge case)

### Wrong-output bugs (83 FAILs) — easy wins

These are bugs in *already-implemented* features. Many are one-line fixes:

**Operator precedence (parser bug, ~3 tests)**
`1 + 2 * 2 + 10 / 2` evaluates left-to-right instead of respecting `*`/`/`
binding tighter than `+`/`-`. The `parse_arith` function in `parser.rs` treats
all five operators at the same precedence level. Fix: split into `parse_add`
(+/-) and `parse_mul` (*/ /%) with mul binding tighter.

**Cross-type sort ordering (~2 tests)**
jq defines a total ordering across types: `null < false < true < numbers <
strings < arrays < objects`. jx's `sort`/`unique` don't implement this,
producing wrong results when arrays contain mixed types.

**`from_entries` key variants (~1 test)**
jq accepts `key`, `Key`, `name`, `Name` for the key field and `value`, `Value`
for the value field. jx only accepts `key`/`value`.

**`values` on non-objects (~1 test)**
`[.[]|values]` on `[1,2,"foo",[],[3,[]],{},true,false,null]` — `values` should
be a type selector that filters to non-null values, not just object values.

**Object construction with multiple outputs (~1 test)**
`{x: (1,2)},{x:3} | .x` — comma expressions in object value positions should
produce multiple objects. Currently doesn't work.

**Negative array indexing edge cases (~2 tests)**
`[.[-4,-3,-2,-1,0,1,2,3]]` on `[1,2,3]` — out-of-range negative indices
should return `null`, not error.

**`ascii_upcase`/`ascii_downcase` on non-ASCII (~1 test)**
`ascii_upcase` on `"useful but not for é"` — non-ASCII characters should pass
through unchanged. Currently fails on multi-byte UTF-8.

**`if` with multiple condition outputs (~2 tests)**
`[if 1,null,2 then 3 else 4 end]` should produce `[3,4,3]` — the condition
can produce multiple values, each generating a then/else branch.

**`length` on null (~1 test)**
`length` on `null` should return `0`, not error or produce no output.

**String `split("")` (~1 test)**
`split("")` should split into individual characters. Currently fails.

**`range` with arguments (~6 tests)**
`range(n)`, `range(from;to)`, `range(from;to;step)` all produce wrong output.
The evaluator has `range` but only handles it as a zero-arg identity-like
builtin. Needs proper multi-arg evaluation producing multiple outputs.

**`floor`/`sqrt`/`cos`/`sin` (~4 tests)**
These are matched as builtins but produce wrong output — likely dispatching
to the wrong implementation or not handling the Value type correctly.

**`abs`/`fabs` (~3 tests)**
Partially implemented but incorrect for edge cases like `-0`, large integers,
and `null`.

### Missing language features (282 ERRORs)

These errors come from syntax jx can't parse or builtins it doesn't recognize:

**Language constructs not in parser:**
- `try expr catch expr` — only the `?` suffix form works, not `try-catch` blocks
- `elif` chains — parser only handles `if/then/else/end`, not `elif`
- `.[start:end]` array/string slicing — no `Slice` AST node
- `expr as $var | body` — no variable binding
- `reduce expr as $var (init; update)` — no reduce
- `foreach expr as $var (init; update; extract)` — no foreach
- `def name(args): body;` — token exists in lexer but parser ignores it
- `label $name | ... break $name` — no label/break control flow
- `@format` strings (`@base64`, `@csv`, `@uri`, etc.) — no format string support
- `\(expr)` string interpolation — parser comment notes "For now, just literal"
- `?//` alternative destructuring operator

**Unimplemented builtins (partial list of most impactful):**
- Math: `floor`, `ceil`, `round`, `pow`, `log`, `exp`, `nan`, `infinite`,
  `isnan`, `isinfinite`, `sin`, `cos`, etc.
- String: `trim`, `ltrim`, `rtrim`, `indices`, `index`, `rindex`, `explode`,
  `implode`, `tojson`, `fromjson`, `utf8bytelength`, `inside`, `gsub`, `test`,
  `match`, `capture`, `scan`
- Path: `path`, `paths`, `leaf_paths`, `getpath`, `setpath`, `delpaths`
- Collection: `transpose`, `walk`, `pick`, `with_entries`, `input`, `inputs`,
  `env`, `$ENV`, `builtins`, `debug`, `stderr`, `halt`
- Assignment: `|=`, `+=`, `-=`, `*=`, `/=`, `%=`, `//=`
- Date/time: `now`, `todate`, `fromdate`, `strftime`, `strptime`, `mktime`,
  `gmtime`

### The `def` multiplier effect

User-defined functions (`def`) account for 59 tests (12% of the suite) directly.
But their impact is much larger: once `def` works, many "builtins" can be
implemented as jq definitions rather than Rust code:

```jq
def with_entries(f): to_entries | map(f) | from_entries;
def walk(f): if type == "array" then map(walk(f)) | f
             elif type == "object" then to_entries | map(.value |= walk(f)) | from_entries | f
             else f end;
def paths: path(recurse(if (type|. == "array" or . == "object") then .[] else empty end))|select(length > 0);
def leaf_paths: . as $dot | paths | select(. as $p | $dot | getpath($p) | type | . != "array" and . != "object");
def isempty(f): first((f|false), true);
def limit(n; f): foreach f as $x (n; .-1; $x, if . <= 0 then error else empty end);
def until(cond; update): def _until: if cond then . else (update | _until) end; _until;
def while(cond; update): def _while: if cond then ., (update | _while) else . end; first(_while);
def repeat(f): def _repeat: f | _repeat; _repeat;
```

This means implementing `def` is a force multiplier — it unlocks not just the
59 direct tests but potentially 20-30 more tests across `walk` (27 tests),
multiple outputs/iteration, and other categories.

---

## Performance reality check

### Where speed wins are real

Parse-dominated workloads on large files — this is jx's sweet spot:

| Filter | File | jx | jq | jaq | jx vs jq | jx vs jaq |
|--------|------|---:|---:|----:|---------:|----------:|
| `-c '.'` | 49MB | 18ms | 1,157ms | 253ms | **63x** | **14x** |
| `-c '.statuses'` | 49MB | 74ms | 1,132ms | 246ms | **15x** | **3.3x** |
| `.statuses \| length` | 49MB | 33ms | 398ms | 167ms | **12x** | **5.1x** |
| `.statuses \| keys` | 49MB | 31ms | 393ms | 165ms | **13x** | **5.3x** |
| `.name` (1M NDJSON) | 82MB | 120ms | 1,230ms | 670ms | **10x** | **5.6x** |

The identity compact number (63x over jq) is the SIMD passthrough path —
`simdjson::minify()` at full speed. Field extraction (15x) goes through DOM parse.
NDJSON (10x) adds rayon parallelism.

### Where speed narrows

Complex filters where evaluator cost dominates:

| Filter | File | jx | jq | jaq | jx vs jq | jx vs jaq |
|--------|------|---:|---:|----:|---------:|----------:|
| `.statuses[]\|.user.name` | 49MB | 157ms | 398ms | 169ms | **2.5x** | **~1x** |
| `.statuses[]\|.user.name` | 631KB | 5.0ms | 10.1ms | 5.5ms | **2x** | **~1x** |
| select+construct | 631KB | ~5ms | ~10ms | ~5ms | **2x** | **~1x** |

On iterate+field over large files, jx and jaq are essentially tied. jx's SIMD
parsing advantage is offset by DOM-to-Value construction overhead. On small files,
startup cost (~2-3ms) dominates everything.

### Profiling breakdown (49MB, `-c '.'`)

| Phase | Time | % |
|-------|-----:|--:|
| File read | 5ms | 31% |
| simdjson::minify() | 10ms | 63% |
| Write output | 1ms | 6% |
| **Total** | **16ms** | |

For non-passthrough filters (49MB, `.statuses[]|.user.name`):

| Phase | Time | % |
|-------|-----:|--:|
| File read | 12ms | 8% |
| DOM parse + flat→Value | 112ms | 71% |
| Eval | 5ms | 3% |
| Output | ~28ms | 18% |
| **Total** | **~157ms** | |

DOM-to-Value construction (not parsing itself) is the bottleneck on the non-passthrough
path. The Rc optimization reduced eval from 33ms to 5ms, but the 112ms
parse+construct step dominates.

---

## Competitive landscape

| Tool | jq compat | Speed vs jq | Architecture | Maturity |
|------|----------:|:------------|:-------------|:---------|
| **jq** | 100% | baseline | C, custom parser | Decades, the standard |
| **gojq** | 85% | 0.8-2.5x | Go, clean rewrite | Years, well-maintained |
| **jaq** | 69% | 1.3-2x | Rust, hifijson parser | Years, well-maintained |
| **jx** | 26% | 2-63x | Rust + C++ simdjson FFI | Half a day |

### Why jaq is at 69% (not higher)

jaq has 153 failures: 99 ERRORs (no output) and 54 wrong-output FAILs:

| Gap area | Failures | Notes |
|----------|---------|-------|
| `path()` expressions | ~15 errors | Not supported in many contexts |
| `?//` destructuring | ~12 errors | Alternative destructuring not implemented |
| `foreach` edge cases | 2 errors | Parser limitation with division |
| Newer builtins (`toboolean`, `pick`) | 3+ errors | Not implemented |
| Path ops (`getpath`/`setpath`/`delpaths`) | ~8 errors | Partial support |
| Module system | 20/26 failures | Partial module support |
| Error message differences | ~15 fails | Different wording causes comparison failures |

jaq's compat is genuinely strong for core features (variables, `def`, `reduce`,
`try-catch`, slicing all work) but has real gaps in path operations, modules,
and newer jq additions. 69% is a fair score.

### Where each tool wins

- **jq**: Maximum compatibility, the standard. Use when correctness matters more
  than speed.
- **gojq**: Best balance of compatibility (85%) and speed. Good default choice
  for scripts.
- **jaq**: Best pure-Rust option. 69% compat, fast evaluator, no C++ dependency.
  Active development pushing toward higher compat.
- **jx**: Fastest on large data. SIMD parsing + NDJSON parallelism are unique.
  But 26% compat limits real-world usability.

### jx's unique position

No other tool combines:
1. SIMD parsing (simdjson On-Demand, 7-9 GB/s)
2. Automatic parallel NDJL processing
3. Passthrough fast paths that bypass the Value pipeline entirely

jaq would need to replace its parser with simdjson bindings AND add threading to
match jx on large-data workloads. That's a fundamental architecture change, not an
incremental improvement.

---

## Roadmap to 50% compatibility

### Prioritized implementation plan

| Priority | Feature | Tests gained | Effort | Key files |
|:--------:|---------|:-----------:|:------:|-----------|
| 1 | Fix operator precedence (`*`/`/` before `+`/`-`) | ~3 | Trivial | `parser.rs` |
| 2 | Math builtins (`floor`..`atan2`, `nan`, `infinite`, `isnan`...) | ~15 | Small | `eval.rs` |
| 3 | String builtins (`trim`, `indices`, `explode`/`implode`, `tojson`/`fromjson`) | ~12 | Small | `eval.rs` |
| 4 | Fix wrong-output bugs (sort ordering, `from_entries`, `length` null, etc.) | ~10 | Small | `eval.rs` |
| 5 | `range`/`limit`/`until`/`while` with proper multi-arg support | ~10 | Small | `eval.rs` |
| 6 | Array/string slicing `.[start:end]` | ~5 | Medium | `mod.rs`, `parser.rs`, `eval.rs` |
| 7 | `try expr catch expr` + `elif` chains | ~10 | Medium | `lexer.rs`, `parser.rs`, `mod.rs`, `eval.rs` |
| 8 | Variable binding (`expr as $var \| body`) | ~11 | Medium | `mod.rs`, `parser.rs`, `eval.rs` |
| 9 | `reduce expr as $var (init; update)` | ~5 | Medium | `mod.rs`, `parser.rs`, `eval.rs` |
| 10 | `def name(args): body;` (user-defined functions) | ~30 | Large | `mod.rs`, `parser.rs`, `eval.rs` |
| 11 | `walk(f)` (recursive transform) | ~15 | Small | `eval.rs` (or as jq `def` once #10 lands) |
| 12 | Path builtins (`getpath`/`setpath`/`delpaths`/`paths`) | ~10 | Medium | `eval.rs` |

**Milestones:**
- Items 1-5: ~181/497 (**36%**) — no parser changes needed
- Items 6-9: ~236/497 (**47%**) — language features
- Items 10-12: ~256/497 (**51%+**) — `def` is the big unlock

### Phase 1: Fix existing bugs + add simple builtins (~50 new tests)

No parser changes — just new match arms in `eval_builtin()` and bug fixes.

**Math builtins to add** (`eval.rs`):
`floor`, `ceil`, `round`, `fabs`, `abs` (fix existing), `sqrt` (fix existing),
`pow(base;exp)`, `log`, `log2`, `log10`, `exp`, `exp2`, `nan`, `infinite`,
`isnan`, `isinfinite`, `isfinite`, `isnormal`, `significand`, `exponent`,
`logb`, `nearbyint`, `rint`, `trunc`, `sin`, `cos`, `asin`, `acos`, `atan`,
`atan2(y;x)`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`, `cbrt`,
`fma(x;y)`, `remainder(x;y)`, `hypot(x;y)`, `j0`, `j1`

**String builtins to add** (`eval.rs`):
`trim`, `ltrim`, `rtrim`, `indices(s)`, `index(s)`, `rindex(s)`, `explode`,
`implode`, `tojson`, `fromjson`, `utf8bytelength`, `inside`

**String arithmetic to add** (`eval.rs`, Arith handler):
- `string * number` — repeat string
- `string / string` — split into array

**Collection builtins to add** (`eval.rs`):
`transpose`, `with_entries(f)`, `getpath(p)`, `setpath(p;v)`, `delpaths(ps)`,
`paths`, `leaf_paths`, `builtins`, `range(n)`, `range(from;to)`,
`range(from;to;step)`, `limit(n;f)`, `until(cond;update)`, `while(cond;update)`

**Bug fixes** (`eval.rs` and `parser.rs`):
- Operator precedence: split `parse_arith` into additive and multiplicative
- `sort`/`unique`: implement jq's cross-type ordering
- `from_entries`: accept `Key`/`Value`/`Name`/`name` variants
- `values`: make it a type selector (non-null), not just object values
- `length` on null: return 0
- `ascii_upcase`/`ascii_downcase`: pass through non-ASCII unchanged
- `if` with multiple condition outputs: iterate over condition values
- Negative index edge cases: out-of-range returns null
- `split("")`: split into individual characters
- `abs`/`fabs`: handle `-0`, large integers, null

### Phase 2: Language features (~55 new tests)

Parser + AST + evaluator changes.

**Array/string slicing** (`mod.rs`, `parser.rs`, `eval.rs`):
- New AST: `Slice(Option<Box<Filter>>, Option<Box<Filter>>)` — either bound optional
- Parse `.[expr:expr]`, `.[expr:]`, `.[:expr]` in postfix position
- Evaluate for both arrays and strings; support negative indices

**`try expr catch expr`** (`lexer.rs`, `mod.rs`, `parser.rs`, `eval.rs`):
- New token: `Catch` keyword
- New AST: `TryCatch(Box<Filter>, Box<Filter>)`
- Parse: after `try`, parse expression, then optional `catch` expression

**`elif` chains** (`parser.rs`):
- When parsing `if-then`, after `then` branch, check for `elif` token
- Desugar `elif` to nested `if-then-else-end`

**Variable binding** (`mod.rs`, `parser.rs`, `eval.rs`):
- New AST: `Bind(Box<Filter>, String, Box<Filter>)` — `expr as $name | body`
- Add `env: HashMap<String, Value>` parameter to `eval()`
- Parse `as` in pipe position (lower precedence than most operators)

**`reduce`** (`mod.rs`, `parser.rs`, `eval.rs`):
- New AST: `Reduce(Box<Filter>, String, Box<Filter>, Box<Filter>)` —
  `reduce expr as $var (init; update)`
- Requires variable binding context

**`def`** (`mod.rs`, `parser.rs`, `eval.rs`):
- New AST: `FuncDef { name, params, body, rest }` — `def name(args): body; rest`
- Add function environment to eval context
- Parse `def` as a prefix to any expression (binds until end of scope)
- Support recursive definitions and closures over variables

### Phase 3: Stretch to 50%+ (~20 more tests)

**`walk(f)`** — implement as builtin or as jq `def` once #10 lands

**`label $name | ... break $name`** — exception-like control flow mechanism

**Format strings** (`@base64`, `@base64d`, `@uri`, `@csv`, `@tsv`, `@html`,
`@sh`, `@json`) — new `Format(String)` AST node, `@ident` lexer token

**String interpolation `\(expr)`** — detect `\(` during string lexing, switch
to expression parsing mode, produce `StringInterp` AST (already exists in
`mod.rs` but parser doesn't use it)

### Not in jq.test but critical for real-world use

These don't move the test score but are essential for adoption:
- `--slurp` / `-s` — extremely common
- `--arg name value` / `--argjson name value` — scripts can't use jx without these
- `--null-input` / `-n` — needed for generators like `range`, `null | ...`
- `--raw-output` / `-r` — check if already implemented; essential for shell pipelines
- `--sort-keys` / `-S` — common for diffable output

---

## Next steps / ideas

### Build a real-world test suite

The jq.test suite is necessary but not sufficient. A real-world test suite should
cover:

- **Common one-liners**: The top 50 jq patterns from Stack Overflow and tutorials
- **CLI flag matrix**: Every flag combination that people actually use
- **Real JSON shapes**: GitHub API responses, npm package.json, CloudTrail logs,
  Kubernetes manifests, NDJSON log streams
- **Error handling**: What happens with malformed JSON, missing fields, type
  mismatches
- **Edge cases**: Empty arrays, null values, deeply nested objects, very large
  numbers, unicode

### Strategic direction options

**Option A: Niche speed tool (current trajectory)**
- Focus on parse-dominated workloads: large files, NDJSON pipelines
- Target 40-50% jq compat — enough for common extraction patterns
- Marketing: "10-60x faster than jq for large data processing"
- Risk: Narrow audience, "just use jaq" for anything complex

**Option B: Chase compatibility (60-80%)**
- Implement variables, reduce, def, assignment, paths, regex, format strings
- Target feature parity with jaq (69%) then push toward gojq (85%)
- Marketing: "Faster AND compatible"
- Risk: Multi-month effort, jaq has years of head start on edge cases

**Option C: Complementary tool**
- Position as a preprocessor/accelerator for pipelines: `jx -c '.' huge.json | jq 'complex_filter'`
- Focus purely on parse speed + NDJSON parallelism + streaming
- Minimal filter language — just enough for extraction and selection
- Risk: Hard to justify a separate install for `jq -c '.'`

**Recommended: Option A with selective Option B features.** Focus on speed as the
primary differentiator, but implement the highest-impact compat features
(variables, slicing, `--slurp`, `--arg`, assignment) to make jx usable for the
80% of real-world one-liners. Don't chase full compat — that's a multi-month
effort with diminishing returns against jaq and gojq.

### Estimated effort per compat tier

| Target | Features needed | Est. effort |
|--------|:----------------|:-----------:|
| 36% (~181 tests) | Bug fixes, math/string/collection builtins | 2-3 days |
| 47% (~236 tests) | + slicing, try-catch, elif, variables, reduce, def | 1-2 weeks |
| 51% (~256 tests) | + walk, format strings, string interpolation | 2-3 weeks |
| 65% (~325 tests) | + assignment, regex, full path ops, edge cases | 1-2 months |
| 80% (~400 tests) | + generator semantics edge cases, error handling | 2-4 months |

The first 36% is largely bug fixes and adding builtin functions — no parser changes.
36-51% requires real language features (the parser/AST work). Beyond 65%, you hit
the long tail of jq's quirky semantics where each percentage point costs
significantly more effort.
