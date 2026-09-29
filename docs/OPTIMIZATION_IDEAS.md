# Optimization ideas

qj runs a faithful port of jq 1.8.1, so every optimization must leave its output byte for
byte identical to jq's. This page describes where qj's speed comes from now, what's next,
and what the old core (removed) taught. The performance plan itself is in
[JQ_PORT_PLAN.md](JQ_PORT_PLAN.md) (wave 4).

## Where the speed comes from

- **simdjson parsing** (`src/io/simd.rs`). simdjson's DOM parser builds a tape, and jq
  values are built straight from it: no intermediate format, number literals read from the
  text at their structural index, recurring object keys shared through a small cache. Any
  text simdjson rejects goes to the port of jq's parser, which is the authority.
- **Input without copies** (`src/io/source.rs`). Regular files (and stdin redirected from
  one) are memory-mapped with `MADV_SEQUENTIAL`; pipes are streamed; `.gz`/`.zst` inputs
  are decompressed in memory.
- **Ordered parallel records** (`src/io/parallel.rs`, `src/cli/run.rs`). Independent
  records are cut into jobs for worker threads, each with its own compiled program (values
  are `Rc`, so nothing jq-semantic is shared), and their output is written in input order.
  The window of input in flight is bounded (`QJ_WINDOW_SIZE`). On Apple Silicon only the
  non-efficiency cores are used.
- **mimalloc** as the global allocator.

## Status

The port's VM runs jq-defined builtins (`with_entries`, `to_entries`, `walk`, `paths`,
`join`, ...) the way jq does, at jq-like speed, and the old core's NDJSON fast paths are
gone. The numbers in the README and `benches/` predate the port and are being re-measured.

## Next

In the order of JQ_PORT_PLAN.md's performance plan, with exclusive machine access:

1. **VM hot paths**, profiled on the `benches/` workloads (`eval_regression` counts
   instructions for compiling, running and printing).
2. **Native versions of hot jq-defined builtins**, proven equivalent to `builtin.jq`'s by
   jq_diff and differential fuzzing, errors and path expressions (`paths`, `|=`) included.
3. **Raw NDJSON fast paths**, reintroduced only where canonical output and validity are
   proven: answering from bytes must still print literals canonically (`1e2` as `1E+2`),
   keep jq's duplicate-key rule (last value, first position), unescape strings, and fall
   back on anything unusual (invalid lines, BOMs, NULs). Each one gets adversarial cases in
   the jq_diff corpus (`corpus/ndjson.toml`) before it's enabled.

## Lessons from the old core

The old core (a tree-walking evaluator, a lazy evaluator over a flat token buffer, 18
NDJSON fast paths and 8 C++ passthroughs) was fast on common filters, 10–37x jq on NDJSON,
but each path was a separate approximation of jq's semantics, and they disagreed at the
seams: number formatting, duplicate keys, escapes, errors. That's why qj became a port. What
it measured still guides the work:

- Kept in the new input layer: mmap (about 23% faster on 1.1 GB of NDJSON), one reusable
  simdjson parser per thread (about 40% on per-line parsing), building values from the
  DOM tape rather than through On-Demand (about 1.7x), and skipping efficiency cores on
  Apple Silicon (1.2–1.9x on M5 Pro/Max compared with using only the first core tier).
- Batch work per FFI call: extracting N fields with N calls per line was 35% slower than
  one call.
- Not worth it: `Rc<str>` strings (measured neutral), arena allocation (too invasive for
  the gain), more threads than cores (slower on macOS). simdjson's `parse_many` wasn't
  pursued: memchr already splits lines fast.
