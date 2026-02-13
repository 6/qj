# jx — Next Steps

What to focus on next, in priority order. See [PLAN.md](PLAN.md) for
full design and history.

---

## Current state (2025-02)

217 tests passing. Passthrough paths handle the common "simple query on
big file" patterns at 12-63x jq, 3-14x jaq:

| Filter (49MB file) | jx | jq | jaq | vs jq | vs jaq |
|---------------------|----|----|-----|-------|--------|
| `-c '.'` | 18ms | 1,157ms | 253ms | 63x | 14x |
| `-c '.statuses'` | 74ms | 1,132ms | 246ms | 15x | 3.3x |
| `.statuses \| length` | 33ms | 398ms | 167ms | 12x | 5.1x |
| `.statuses \| keys` | 31ms | 393ms | 165ms | 13x | 5.3x |

Non-passthrough eval is competitive with jaq (~1x) and ~2-4x jq.

---

## Priority 1: Parallel NDJSON (Step 4)

**Why this is next:** It's the biggest remaining performance multiplier
and jx's core differentiator. ~8x speedup on 8 cores, applied to the
most common large-data workload (log processing, API dumps, data
pipelines). Combined with SIMD parsing, this is the "50-100x over jq"
headline. No other jq-compatible tool does this.

**What to build:** See [PLAN.md Phase 2](PLAN.md#phase-2-parallel-ndjson).

- NDJSON auto-detection (first 16KB heuristic, or `--jsonl` flag)
- Chunk splitter: split at newline boundaries into ~1MB chunks
- Thread pool: per-thread simdjson parser, `iterate_many` per chunk
- Ordered output merge: per-thread buffers, flush in chunk order
- File path: mmap + memchr newline scan
- Stdin path: growing buffer, dispatch complete chunks to workers

**Benchmark targets:** `jx '.field' 82mb.ndjson` — expect ~15-20ms
(down from ~120ms single-threaded). 1m.ndjson already exists in
bench/data/ for testing.

**Key decisions:**
- Use `rayon` or hand-rolled thread pool? Rayon is simpler but less
  control over chunk assignment and output ordering.
- Start with file-only parallelism (mmap), add stdin streaming after.

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

## Not prioritized (and why)

### More passthrough patterns

The big wins are captured. The remaining common patterns (`.[] | .field`,
`select()`, `map()`) involve iteration — multiple outputs, fundamentally
different from the single-result passthrough model. Implementing them as
C++ passthrough would be complex and fragile. Better to improve the
general eval path (which is already competitive with jaq) than to keep
special-casing patterns.

Possible exception: `.[] | .field` on arrays is extremely common and
could be done as a C++ loop returning newline-delimited results. But
NDJSON parallel provides more bang for the buck first.

### On-Demand fast path (Phase 1.5)

With passthrough paths already at 18-74ms on 49MB, On-Demand would only
shave a few ms off the DOM parse step. Diminishing returns unless
profiling shows otherwise after NDJSON is done.
