# qj — Positioning and Strategy

Internal notes on competitive positioning, launch readiness, and future
optimization ideas. Public-facing content lives in README.md.

---

## Why not just use jaq?

jaq is good. It wins 23/29 benchmarks vs jq on filter evaluation speed
and has near-zero startup time. But jaq doesn't address the three biggest
performance problems:

1. **No SIMD parsing.** jaq uses `hifijson` (its own parser) or serde_json.
   Both process byte-by-byte at ~400-600 MB/s. C++ simdjson's On-Demand
   API reaches 7 GB/s — a 10-15x gap. For simple filters (`.field`,
   `.[] | .name`), parsing dominates runtime. jaq is faster at
   *evaluating filters* than jq, but still bottlenecked on *parsing*.

2. **No On-Demand parsing.** jaq (and the Rust simd-json port) always
   builds a full DOM tree regardless of what the filter accesses. For
   `jq '.name' huge.json`, the entire document is materialized just to
   extract one field. simdjson's On-Demand API navigates the SIMD
   structural index directly — only the accessed fields are materialized.
   This is the difference between 1.4 GB/s (full DOM) and 7 GB/s
   (On-Demand) for simple path queries.

3. **No parallelism.** jaq processes input sequentially. For NDJSON (one
   JSON object per line), each line is independent — trivially
   parallelizable. People currently work around this with
   `GNU parallel | jq`, spawning dozens of processes with startup/IPC
   overhead. qj's built-in parallel NDJSON processing gives 6-11x
   speedup on multi-core machines with zero user configuration.

qj beats everything on *parsing throughput* and *parallel processing*,
while matching jaq on *filter evaluation speed*. The performance story
is SIMD parsing + parallelism, not a faster evaluator.

---

## Competitive landscape

| Tool | Parsing (measured) | E2E (measured) | Parallel | SIMD | On-Demand | jq compat | Platform |
|------|-------------------|----------------|----------|------|-----------|-----------|----------|
| jq 1.7 | 23-62 MB/s e2e | baseline | No | No | No | 100% | All |
| jaq 2.3 | 93-187 MB/s e2e | 1.3-2x jq | No | No | No | ~90% | All |
| gojq 0.12 | 47-122 MB/s e2e | 0.8-2.5x jq | No | No | No | ~85% | All |
| **qj** | **7-9 GB/s parse** | **3-63x jq** | **Yes** | **Yes (NEON/AVX2)** | **Passthrough** | **~98%** | **macOS/Linux** |

Note on jq's `--stream` mode: jq parses incrementally, emitting
`[[path], value]` pairs without loading the full tree. It works for
constant-memory processing of large files, but requires a completely
different programming model — you work with path arrays instead of
normal jq filters. It's also significantly slower than normal mode
because every value gets wrapped in a path tuple.

---

## Honest performance comparison

| Scenario | vs jq | vs jaq | Why |
|----------|-------|--------|-----|
| Simple filter, large file (1 thread) | 10-20x | 3-8x | SIMD DOM parsing, fast output |
| Simple filter, large NDJSON (multi-core) | 11-14x | 5-6x | SIMD + parallelism |
| Complex filter, large file | 5-10x | 2-4x | SIMD parsing, similar eval |
| Complex filter, small file | 2-3x | ~1x | Eval-dominated, similar speed |
| Small file, simple filter | 2-5x | ~1x | Startup-dominated |

The win over jaq is almost entirely in the parser and threading, not
the evaluator. On eval-dominated workloads (complex filters, small files),
we're at parity. That's fine — nobody runs `reduce` on a 100-byte JSON
file and complains about speed. The people who need "faster jq" have
large inputs, and that's where the 10-50x advantage lives.

---

## Positioning

**"jq, but fast on large data."** Two concrete claims backed by hardware:

1. **10x faster parsing** via SIMD (simdjson On-Demand, NEON/AVX2)
2. **10x faster NDJSON** via built-in parallel processing

**What we're NOT claiming:** Faster filter evaluation than jaq. On
eval-dominated workloads (complex filters, small inputs), qj and jaq
are roughly equivalent. The performance story is parsing + parallelism.

**Primary audience:** Developers processing large JSON. Log pipelines,
NDJSON datasets, large API dumps. Specifically: anyone who has added
`parallel | jq` to a pipeline, hit OOM on a large JSON file, or waited
more than a second for jq to finish. LLM agents (Claude Code, Cursor,
aider) that parse JSON tool outputs thousands of times per session.

**Competitive positioning vs jaq:** jaq is "jq but correct and clean."
qj is "jq but fast on large data." Different niches. However, qj now
has ~98% jq compatibility (exceeding jaq's ~90%), so the "why not just
use qj" argument is strong for anyone with large-data workloads — and
qj is no worse than jaq for small-data use.

---

## Scope control — what we're NOT building

- No module system (`import`/`include`). Niche usage, complex to implement.
- No arbitrary precision arithmetic. i64/f64 internally, large numbers
  preserved on passthrough.
- No interactive mode / REPL (jnv, jless exist for this).
- No YAML/TOML/XML input (jaq does this; differentiate on speed, not
  format support).
- No SQL-like query language (different paradigm entirely).
- No GUI / TUI. Pipe-friendly CLI only.

---

## Launch readiness

### Distribution

Homebrew formula and prebuilt binaries from day one. If users can't
install in under 30 seconds, adoption stalls. Needs:
- Homebrew tap with formula (compile from source via `cc` crate)
- GitHub Releases with prebuilt binaries for macOS ARM, macOS x86,
  Linux x86, Linux ARM
- `cargo install qj` support (already works, but slow — C++ compile)

### First impressions — compatibility threshold

At ~98% jq compatibility, most common filters work. Remaining gaps:
- Module system (`import`/`include`) — out of scope
- Arbitrary precision — out of scope
- `tostream`/`fromstream`/`--stream` — not yet implemented
- Some edge cases in `def` scoping and path expressions

**Clear error messages for unsupported features** matter more than
implementing every niche feature.

### The "just use jaq" question

This will be the first question from anyone who knows the space. Clear,
honest answer: qj wins on large data (SIMD parsing + automatic
parallelism) AND now has higher jq compatibility (~98% vs ~90%). jaq
has no C++ build dependency. They're complementary, not competing.

### Real-world demo

Microbenchmarks alone aren't convincing. Need at least one concrete
real-world story alongside the numbers:
- "1GB CloudTrail log: jq takes 2 minutes, qj takes 2 seconds"
- "NDJSON pipeline: replaced `parallel -j8 | jq` with `qj`, same
  result, zero configuration"
- Show a real log processing or data pipeline task, not synthetic data

### Maintenance burden

jq's language surface area is large. The risk is launching then facing
a steady stream of "qj doesn't support X" issues that slowly consume
all development time.

**Strategy:** Be explicit about scope. Core filters are maintained;
niche features (module system, `$__loc__`) are documented as out of
scope. Focus ongoing effort on performance (parallelism, streaming)
rather than chasing 100% compat.

---

## Future optimization ideas — small-file performance

qj already beats jaq ~2x on small files (2ms vs 5ms on 631KB
twitter.json). These optimizations target widening that to 3-4x.

### Where time goes (631KB twitter.json, ~3ms total)

| Component | Time | % | Opportunity |
|-----------|------|---|-------------|
| File I/O | 0.2ms | 7% | Optimal |
| simdjson DOM parse | 0.8ms | 27% | Optimal (C++ baseline) |
| Flat buffer serialize (C++) | 0.3ms | 10% | Avoidable for some filters |
| Flat buffer deserialize (Rust) | 0.6ms | 20% | String alloc heavy |
| Eval + output | 0.5ms | 17% | Cloning overhead |
| Process startup + clap | 0.6ms | 19% | Fixed cost |

### Optimization ideas (priority order)

**1. Compact string representation (SmallString).** Object keys and short
strings dominate allocation count. Inline ≤24 bytes on the stack,
heap-allocate only longer strings. `compact_str` or `smol_str` crates.
Expected impact: ~0.2-0.3ms saved.

**2. On-Demand evaluator for `.[] | .field` patterns.** The most common
non-passthrough filter. Use simdjson On-Demand to iterate array elements
and navigate directly to the target field, materializing only the
accessed value. Expected impact: 2-3x faster for iterate+field patterns.

**3. Direct DOM→Value without flat buffer.** The two-pass approach
(C++ → flat tokens → Rust Values) takes ~0.9ms on small files (30% of
total). A tighter FFI that walks the simdjson DOM directly could
eliminate the intermediate copy. Expected impact: ~0.3-0.5ms saved.

**4. Arena allocation for Values.** Bump allocator (`bumpalo`) for the
entire Value tree of a single document. Eliminates per-allocation
overhead, improves cache locality. Expected impact: ~0.1-0.2ms saved.

**5. String interning for object keys.** JSON documents reuse the same
key names across array elements. Intern during DOM→Value conversion.
Expected impact: ~0.1-0.2ms saved on key-heavy JSON.

Items 1-2 have the best effort-to-impact ratio. Items 3-5 are
diminishing returns — pursue only if profiling justifies them.

**None of these block launch.** qj is already faster than jaq on small
files. These are post-launch optimizations.
