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

### Adjusted "effective compatibility" estimate

Weighting by real-world usage frequency (giving higher weight to field access,
builtins, conditionals, and string ops; lower weight to toliteral, walk, and
module system):

| Tool | Raw score | Weighted estimate |
|------|-----------|-------------------|
| jx | 26% | ~35-40% |
| jaq | 69% | ~75-80% |
| gojq | 85% | ~85-90% |

jx's weighted score is higher because it does well on high-frequency categories
(field access 57%, builtins 60%, conditionals 57%) and poorly on low-frequency
ones (toliteral 7%, walk 0%, module system 12%). But even at ~35-40% weighted,
there are major gaps: zero support for variables, slices, negative indices,
assignment, and paths.

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
2. Automatic parallel NDJSON processing
3. Passthrough fast paths that bypass the Value pipeline entirely

jaq would need to replace its parser with simdjson bindings AND add threading to
match jx on large-data workloads. That's a fundamental architecture change, not an
incremental improvement.

---

## Next steps / ideas

### Quick wins to move the needle on compatibility

These are relatively simple to implement and would unlock common real-world usage:

| Feature | Est. effort | Compat impact | Why it matters |
|---------|:-----------:|:-------------:|:---------------|
| Negative indices (`[-1]`, `[-2:]`) | Small | +5 tests | Very common pattern |
| Array slicing (`[2:5]`, `[:-1]`) | Small | +5 tests | Common, simple extension of existing index code |
| Variables (`. as $x \| ...`) | Medium | +11 tests | Required for many jq idioms |
| `--slurp` / `-s` | Medium | 0 (untested) | Extremely common CLI flag |
| `--arg` / `--argjson` | Medium | 0 (untested) | Scripts can't use jx without this |
| Math builtins (`floor`, `ceil`, `sqrt`, etc.) | Small | +18 tests | Basic numbers category goes from 0 to ~18 |
| Assignment operators (`\|=`, `+=`, etc.) | Medium | +19 tests | Common in data transformation |
| `reduce` | Medium | indirect | Aggregation patterns |
| `path`/`getpath`/`setpath` | Medium | +22 tests | Path operations category |

Implementing negative indices, slicing, math builtins, and variables alone would
add ~39 tests, bringing the raw score from 26% to ~34%. Adding assignment and
paths would push to ~42%.

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
| 35% (~170 tests) | Negative indices, slicing, math builtins, variables | 1-2 days |
| 45% (~225 tests) | + assignment, paths, `walk`, number formatting | 3-5 days |
| 55% (~275 tests) | + regex, format strings, `reduce`, `foreach` | 1-2 weeks |
| 65% (~325 tests) | + full `def` support, complex string ops, edge cases | 2-4 weeks |
| 80% (~400 tests) | + generator semantics edge cases, error handling | 1-2 months |

The first 35% is low-hanging fruit. 35-55% is solid incremental work. Beyond 65%,
you hit the long tail of jq's quirky semantics where each percentage point costs
significantly more effort.
