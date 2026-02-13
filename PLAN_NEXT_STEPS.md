# jx — Next Steps

What to focus on next, in priority order. See [PLAN.md](PLAN.md) for
full design and history.

---

## Current state (2025-02)

248 tests passing (159 unit + 56 e2e + 18 ndjson + 15 ffi). Passthrough
paths handle "simple query on big file" at 12-63x jq, 3-14x jaq.
Parallel NDJSON processing at ~10x jq, ~5.6x jaq.

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

---

## ~~Priority 1: Parallel NDJSON (Step 4)~~ COMPLETE

Implemented with rayon work-stealing thread pool. Auto-detection via
heuristic + `--jsonl` flag. ~1MB chunks processed in parallel, output
merged in order. See [PLAN.md Phase 2 results](PLAN.md#phase-2-results-apple-silicon-m-series-2025-02).

---

## Priority 2: Missing core filters (Step 5)

**Why:** These block real-world jq replacement. Nobody can switch from
jq to jx until these exist.

| Feature | Impact | Effort |
|---------|--------|--------|
| `--slurp` / `-s` | Very common flag. Blocks `[inputs]` patterns | Small — read all, wrap in array |
| `--arg name val` / `--argjson` | Any script using `$TOKEN` etc. | Medium — needs variable scoping |
| `.[2:5]` array slicing | Common, simple | Small — extend Index handling |
| `. as $x \| ...` variable binding | Required for advanced jq | Medium — scoped env in evaluator |
| `reduce` | Aggregation — needs variables | Medium — depends on var binding |

Recommended order: `--slurp` → array slicing → `--arg` → variable
binding → `reduce`. Each one unblocks a class of real-world scripts.

See [PLAN.md Phase 3 remaining](PLAN.md#remaining-to-implement).

---

## Priority 3: SmallString for Value type

**Why:** Low effort, broad impact. Most JSON object keys are <24 bytes
("name", "id", "status", "user"). Switching from `String` to a
small-string type (e.g. `compact_str` crate) inlines short strings on
the stack, eliminating the majority of heap allocations during
DOM→Value construction. Improves all code paths — small files, large
files, NDJSON — and reduces allocation noise when profiling parallel
NDJSON.

**Effort:** Crate dependency + change `Value::String(String)` to
`Value::String(CompactString)` + update `value.rs`, `bridge.rs`,
`eval.rs`, `output.rs`. Mechanical refactor, no logic changes.

See [PLAN.md small-file optimization](PLAN.md#small-file-performance-optimization).

---

## Later

None of these block launch. Revisit after Priorities 1-3.

### More passthrough patterns

The big wins are captured. The remaining common patterns (`.[] | .field`,
`select()`, `map()`) involve iteration — multiple outputs, fundamentally
different from the single-result passthrough model. Better to improve
the general eval path than to keep special-casing patterns.

### On-Demand fast path (Phase 1.5)

With passthrough paths already at 18-74ms on 49MB, On-Demand would only
shave a few ms off the DOM parse step. Diminishing returns unless
profiling shows otherwise after NDJSON is done.

### Small-file performance

Optimizations to widen the jx vs jaq gap on small inputs (currently ~2x,
target 3-4x). See [PLAN.md](PLAN.md#small-file-performance-optimization)
for profiling breakdown.

| Optimization | Expected impact | Effort |
|--------------|-----------------|--------|
| On-Demand evaluator for `.[] \| .field` | 2-3x for iterate+field | High — new eval path |
| Direct DOM→Value (skip flat buffer) | ~0.3-0.5ms saved | Medium — FFI redesign |
| Arena allocation for Value trees | ~0.1-0.2ms, better cache | Medium — `bumpalo` crate |
| String interning for repeated keys | ~0.1-0.2ms on key-heavy JSON | Small — HashMap during decode |
