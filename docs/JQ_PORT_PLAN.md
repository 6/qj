# jq 1.8.1 port plan: 100% conformance

**Goal:** qj is indistinguishable from jq 1.8.1 (same stdout bytes, exit codes, and error
messages) for every program, input, and flag, while keeping qj's speed advantage
(SIMD parsing, mmap, parallel NDJSON).

**Approach:** stop maintaining a second, approximate implementation of jq's semantics.
Port jq's semantic core faithfully to Rust (`src/jq/`), run jq's own `builtin.jq`
verbatim, and put qj's fast I/O layer on top. Fast paths return only after they are proven
equivalent by differential tests.

## Why (audit, 2026-09-29)

The previous "100%" was measured leniently: `jq.test` only, `%%FAIL` cases skipped, and
outputs compared semantically (numbers as f64, key order ignored, `-c` only,
`QJ_JQ_COMPAT=1`). Measured byte-exact against jq 1.8.1:

| measure | match |
|---|---|
| jq's official test files (838 cases) | 762 (91%) |
| edge-case probe (paths, errors, generators, sort, formats, dates, streams) | 256/338 (76%) |
| builtins behaving identically on 9 simple inputs | 124/226 |

The root causes are structural, and they are the reason for the port:
- Errors are a thread-local flag and output callbacks can't stop producers. Errors get
  swallowed, `first`/`limit` don't short-circuit, `try` over generators is wrong, and
  unknown builtins or variables silently produce nothing.
- Path tracking is a partial separate implementation (`(.. | numbers) |= .+1` fails).
- The value model has objects as `Vec` pairs. Equality depends on key order, duplicate keys
  are first-wins, and lookups are O(n). No in-place mutation, so `reduce`/`INDEX`/`add`
  go quadratic.
- There are four separate implementations of jq semantics (tree-walker, `flat_eval`, 18
  NDJSON fast paths, 8 C++ passthroughs) that diverge at the seams.
- Regex uses the `regex` crate (no lookaround or backreferences, byte offsets). jq uses
  Oniguruma.

## Definition of done

1. **Upstream suites:** every case in jq 1.8.1's `jq.test`, `man.test`, `manonig.test`,
   `onig.test`, `base64.test`, `uri.test`, `optional.test` passes **byte-exact** (stdout,
   exit code, normalized stderr). It must pass in compact and pretty mode, with input on
   stdin, as a file argument, and as multi-line NDJSON.
2. **Extended corpus:** 100% on qj's own corpus: category probes, the builtin × input
   matrix, CLI flags, input robustness, and adversarial NDJSON for every fast path.
3. **Differential fuzzing:** random program + input generation against jq finds no
   divergence over a sustained run.
4. **Performance:** no regression versus qj v0.1.4 on the `benches/` suite, except where
   documented. Common idioms (`reduce` into objects, `INDEX`, `add` of arrays, `first`,
   `limit`) must not go quadratic.

**Help and version text** (`-h`, `--help`, `-V`, `--version`, `--build-configuration`) are
qj's own and exempt from comparison. Everything else on the command line (option parsing,
errors, exit codes) must match.

**stderr policy:** compared after replacing the program-name prefix (`qj:` for `jq:`).
Everything else must match, including runtime error messages (they are observable via
`catch`) and `(at <file>:<line>)` locations. Syntax-error wording is tracked as a separate
metric (bison's "expecting ..." lists are a long tail). Syntax-error *detection* and exit
code 3 are required.

## Decisions (defaults; revisit only with the user)

- **Numbers are jq-exact by default.** The port follows jq 1.8.1 built with decNumber:
  literals are preserved and printed canonically (`1e2` → `1E+2`), literals compare
  precisely against each other, arithmetic is f64, and `have_decnum` is true.
  qj's old i64-exact arithmetic goes away. `QJ_JQ_COMPAT` becomes a no-op once the port is
  the default.
- **The builtin set equals jq 1.8.1's `builtins`.** qj-only names jq removed (`leaf_paths`,
  `date`, `dateadd`, `datesub`, `ascii`) are dropped.
- **qj extensions kept:** `--threads N`, `--jsonl`, transparent `.gz`/`.zst`, and glob
  expansion when a literal path doesn't exist. None may change output relative to jq.
- **Values are `Rc`, not `Arc`.** Each worker thread compiles its own program instance;
  nothing jq-semantic is shared across threads.
- **License:** ported code is derived from jq (MIT). Keep `LICENSE-jq` (jq's `COPYING`, which
  includes the dtoa/decNumber notices), and mention it in the README before release.

## Reference

- jq 1.8.1 source: `bash tests/jq_compat/fetch_jq_source.sh` → `target/jq-src/jq-1.8.1/`
  (per worktree, gitignored). The files that matter:

  | jq file | lines | ported to |
  |---|---:|---|
  | `jv.c`, `jv_aux.c`, `jv_unicode.c` | 2970 | `src/jq/value/` |
  | `jv_print.c` | 425 | `src/jq/value/` |
  | `jv_parse.c` | 913 | `src/jq/value/` |
  | `jv_dtoa.c` (`jvp_dtoa_fmt` only) | — | `src/jq/value/` (Rust shortest-float digits + jq's formatting rules) |
  | `lexer.l`, `parser.y` | 1174 | `src/jq/lang/` (AST) |
  | `compile.c`, `bytecode.c`, `opcode_list.h`, `parser.y` actions | 1590 | `src/jq/lang/` |
  | `execute.c`, `exec_stack.h`, `locfile.c` | 1430 | `src/jq/lang/` |
  | `linker.c` | 452 | `src/jq/lang/` |
  | `builtin.c` | 2071 | `src/jq/builtins/` |
  | `builtin.jq` | 243 | vendored verbatim |
  | `main.c`, `util.c` | 1977 | CLI / input layer |

- The `jq` 1.8.1 binary (via mise) is the oracle. When the source and the binary disagree,
  the binary wins.

## Porting rules

1. **Faithful over clever.** Mirror jq's structure so each Rust function is recognizable
   (`// port of jv_aux.c: jv_setpath`). Keep error message strings verbatim. Keep the
   quirks: they are part of the spec (e.g. `[label $f | try break $f catch .]` →
   `[{"__jq":0}]`).
2. **Idiomatic where it doesn't change behavior:** `Result` instead of `jv_invalid`,
   `Rc::make_mut` for jq's refcount-1 in-place mutation, `IndexMap`-style objects, and
   Rust's shortest-float digits instead of porting dtoa.
3. **Test against the binary.** Every ported behavior gets tests whose expectations came
   from running jq 1.8.1, not from reading the C.
4. **No O(n²) where jq is O(n).** Mutation must happen in place when the value is unique.

## Module ownership (parallel tracks edit disjoint files)

| path | owner | contents |
|---|---|---|
| `src/jq/value/` | Track V | values, numbers, strings, arrays, objects, cmp/sort, aux ops, printer, JSON parser |
| `src/jq/lang/lexer.rs`, `parser.rs`, `ast.rs`, `locfile.rs` | Track P | lexer + parser → AST, syntax errors |
| `src/jq/lang/` (everything else) | Track C | lowering (parser.y actions + compile.c), bytecode, VM, linker |
| `src/jq/platform/` | Track X | value-free platform primitives: Oniguruma regex, libc time, libm set |
| `src/jq/builtins/` | Tracks C + B | builtin registry, C builtins, `builtin.jq` |
| `tests/jq_diff*`, `tests/jq_compat/` | Track H | differential harness, vendored suites, corpus |
| `src/main.rs`, `src/input.rs`, `src/parallel/`, `src/simdjson/` | Tracks CLI + IO (wave 3) | CLI, fast I/O |

Shared registration files (`src/lib.rs`, `src/jq/mod.rs`, `Cargo.toml`): keep edits minimal
and additive. The orchestrator resolves merge conflicts.

## Waves

Each wave's agents work in isolated git worktrees branched from `main`. The orchestrator
merges them back into `main` (never pushed), keeps `main` green, and writes the next wave's
interface scaffold before spawning it.

### Wave 1: foundations (parallel)

- **H: strict differential harness.** Vendor all upstream `.test` files. Byte-exact runner
  against the jq binary (cached), covering compact, pretty, file, and NDJSON modes, plus a
  scoreboard and a ratchet baseline. Port the audit probes into a corpus. Measures the
  current qj; does not change `src/`.
- **V: value layer.** Port `jv*.c` to `src/jq/value/`: types, the number model (decNumber
  literal semantics), cmp/equal/sort, `jv_aux` operations with verbatim errors, the printer
  (all flags, colors, `JQ_COLORS`), and the JSON parser (exact errors, depth 10000, U+FFFD
  replacement, `nan`, streaming mode if time allows).
- **P: language front-end.** Port `lexer.l` and `parser.y` to an AST that mirrors
  `parser.y`'s productions, with locations. Accept/reject must match jq exactly. Port
  locfile-style error formatting.
- **X: platform primitives.** These don't use `Value`, so they can start now.
  - `_match_impl`'s engine on Oniguruma (the `onig` crate) with jq's flag mapping, codepoint
    offsets, and named captures.
  - `strptime`/`strftime`/`strflocaltime`/`mktime`/`gmtime`/`localtime`/`now` via libc,
    exactly as `builtin.c` uses them, with verbatim error messages.
  - jq's libm function table (`libm.h`), matching which functions exist on macOS and Linux.

  Wave 2's B2 then just adapts these to `Value`.

### Wave 2: evaluator (parallel, after wave 1 is merged)

- **C: compiler + VM.** Port the `parser.y` actions (constant folding, `gen_update`, …),
  `compile.c` (binding, closures, subfunctions), `execute.c` (fork/backtrack, path
  tracking, try, label/break, TCO), bytecoded builtins, `builtin.jq` loading (lazy: bind only
  referenced definitions, for fast startup), and `linker.c` (modules, `-L`, search
  list). Exposes a `jq_compile`/`jq_start`/`jq_next`-style API.
- **B1: C builtins, core.** Arithmetic and comparison ops, tojson/fromjson/tostring/
  tonumber, keys/has/contains/getpath/setpath/delpaths, string functions,
  `format` (`@text` … `@base32d`), sort/group/unique/min/max impls, error/env/halt/
  input/debug/stderr/input_filename/input_line_number/get_*/modulemeta hooks.
- **B2: C builtins, platform.** Wrap Track X's primitives as C builtins (`_match_impl`,
  dates, libm), plus any leftover `builtin.c` functions not covered by B1.

### Wave 3: integration (parallel)

- **CLI:** port `main.c` + `util.c` onto the new core: options, `--args` handling, exit
  codes, error formats, `-e`, `--seq`, `--stream`/`--stream-errors`, `-R`/`-s`/`-n`,
  `input`/`inputs` as lazy pulls, `input_filename`/`input_line_number`, colors. Selectable
  with `QJ_CORE=port` until it beats the old core on the harness, then the default.
- **IO:** simdjson → `Value` conversion (literal-preserving; falls back to the ported parser
  on any error or unusual input), mmap and windowed parallel NDJSON on the new core
  (per-thread program, ordered output, sequential fallback for
  `input`/`inputs`/`halt`/`input_line_number`/`$__loc__`), and **streaming stdin**.

### Wave 4: switch-over, performance, long tail

- Delete the old evaluator (`src/filter/`, `flat_eval`, `flat_value`, `value.rs`,
  `output.rs`, most NDJSON fast paths) and update the README, COMPATIBILITY and CLAUDE.md.
- Benchmarks with exclusive machine access; profile and tune the VM.
- Reintroduce fast paths only with adversarial differential proofs (nested and duplicate
  keys, escapes, number canonicalization, whitespace, invalid lines).
- Differential fuzzing against jq (program generator + adversarial inputs) until clean. Port
  jq's `shtest` CLI cases.

## Agent conventions

- Work only in your worktree. Commit early and often: small coherent commits with
  imperative subjects, each ending with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. **Never push. Never amend,
  rebase, or force.**
- Before each commit, run `cargo fmt`, `cargo clippy --release -- -D warnings`, and
  `cargo test` (fast suite). Keep `main`'s existing tests green; the old evaluator stays
  until wave 4.
- **Don't run benchmarks.** The machine is shared with other agents, so timings are noise.
- Stay inside your track's paths. If you must touch a shared file, keep the change minimal
  and list it in your report.
- End with a report: branch name, commits, what's done and what isn't, test and harness
  numbers, and surprising jq behaviors you found (with the reproducing command).

## Status

- 2026-09-29: plan written. Thread-count fix landed (`0f9f2cc`). Wave 1 started.
- 2026-09-29: **H merged** (`0ef13b5`): `cargo test --release jq_diff -- --ignored` has 18,085
  cases. Old qj scores 9,286 strict (51.3%) and 11,038 stdout+rc (61.0%); upstream suites
  alone are 2,639/2,903 strict (90.9%). There's no Linux baseline yet, so CI only reports.
  Track A (jq `main.c` argument handling) started.
- 2026-09-29: **X merged** (`dc9ff95`): Oniguruma `_match_impl` primitive (650/650 calls
  byte-exact), libc time functions, libm table (4415/4415 bit-exact on macOS).
- 2026-09-29: **P merged** (`f2a7029`): lexer, plus bison's own LALR tables driven by a
  port of the yacc skeleton. Accept/reject and parse-error text match jq by construction
  (100% on 56k programs).
- 2026-09-29: **V merged in progress** (`2c69b8d`); refinements still coming. Builtin
  interface scaffold landed (`3eeba5e`). Wave 2 started: C1 (compiler, checked against
  `--debug-dump-disasm`), B1 (core builtins), B2 (platform builtins), IO (simdjson → Value,
  streams, parallel engine).
- 2026-09-29: **A merged** (`0dc1608`): `src/cli/args.rs` ports main.c's option loop and
  replaces clap. jq_diff: 9,397/18,185 strict. Next: C2 (VM) as soon as C1 commits
  `bytecode.rs`.
- Pending policy: the usage hint and other mid-line program names should say `qj`, with
  the harness normalizing them the same way as the prefix. This lands with the wave 3 CLI.
- 2026-09-29: **B1 binops merged** early (`cbb5a49`) for constant folding. **B2 merged**
  (`245b459`). C2 (VM) started, merging C1's `bytecode.rs` as it lands.
- 2026-09-29: **V final merged** (`04c6ba2`) and **B1 merged** (`ce4c7f5`): every C builtin
  is ported, and B1's fixture suite (15,163 cases) matches jq byte for byte.
- 2026-09-29: **C2 merged, including C1's compiler** (`21eeaa7`). The whole new core
  (compiler, VM, builtins) passes jq's upstream suites **819/819** against the binary, and
  the corpus 11,200/11,205 (the rest is jq nondeterminism). `--debug-trace` matches jq on
  11,202 programs. Wave 3 started: CLI (main.c/util.c processing on the new core, behind
  `QJ_CORE=port`). C1 (disassembly parity, startup) and IO are still running.
- 2026-09-29: **C1 final merged** (`7ea3243`): disassembly parity on 3,519/3,519 corpus
  programs and 23,000/23,000 random programs.
- 2026-09-29: **CLI merged** (`c890e5c`). With `QJ_CORE=port`, the qj binary scores
  **19,577/19,584 strict** on jq_diff: 100% in compact, pretty, file, ndjson and fail modes,
  upstream 2,903/2,903. The 7 misses are qj's exempt help/version/usage text. No per-case
  regressions against the old core. Started: SW (make the port the default, update old tests,
  docs), IO (wire the simdjson reader and parallel engine into the port), FZ (sustained
  differential fuzzing against jq).
- 2026-09-29: **SW merged** (`8331ec0`): **the port is qj's default core.** jq_diff scores
  19,577/19,584 strict (100% in every mode but `cli`, whose 7 misses are exempt help/version
  text). The audit's probes are all clean: upstream byte-exact 838/838 in compact and pretty
  mode, and the category probe 338/338. `QJ_CORE=old` runs the legacy core until it's deleted.
- **Performance status after the switch** (noisy, other agents running). Correct everywhere,
  with no quadratic cliffs: `reduce` into a 50k-key object takes 0.6s (was >20s), and
  `INDEX(.id)` 0.6s (was 17.9s). But the port currently runs at 1–2x jq's speed, where the
  old fast paths gave 10–37x on NDJSON. Two reasons:
  1. The port reads input with V's plain parser: no simdjson, no parallelism. IO is wiring
     those in.
  2. jq-defined builtins (`with_entries`, `to_entries`, `walk`, `paths`,
     `ascii_downcase/upcase`, `join`) run on the faithful VM, at jq-like speed.
  Next, after IO and FZ, with exclusive machine access:
  - (a) VM hot paths (C2's list);
  - (b) native versions of hot jq-defined builtins, proven equivalent by jq_diff and fuzzing,
    including errors and path semantics;
  - (c) raw NDJSON fast paths reintroduced only where canonical output and validity are
    proven;
  - (d) delete the old core, and rewrite the iai-callgrind regression bench for the new core.
- 2026-09-29: **IO merged** (`e76d7b5`): simdjson tape → values with exact fallback, a
  byte-exact util.c reader, mmap/streaming, and an ordered parallel engine on the default core.
  NDJSON throughput is back to old-core levels on 18 threads (noisy sanity runs). Single-doc
  JSON is still ~4x slower than the old passthroughs, and that's the target of the performance
  phase. Programs using `now` now run sequentially (`4a098fb`). Started: CL (delete the old
  core). FZ is still running.
- 2026-09-29: **FZ merged** (`0aef9d1`): about 430k differential fuzz cases, every
  divergence fixed at its source. The final 100k-case campaign was clean (the last 91,417
  consecutive cases). jq_diff: 19,745/19,752 strict; the 7 misses are exempt help/version text.
- 2026-09-29: **User decisions.**
  1. `QJ_JQ_COMPAT=1` becomes "be exactly jq": it reproduces jq's crashes and hangs and turns
     off globbing, decompression and qj-only flags. The default keeps sane behavior plus the
     extras.
  2. Verify Linux by pushing a non-main branch for CI, but only after the macOS performance
     regressions against the old core are fixed.
  Tracks: PF (data path: value layer, simd → Value, printer, memory, NDJSON extraction),
  PF-B (VM hot paths plus faster, provably identical jq-defined builtins: walk 36x,
  tostream 4.8x, paths 2.4–3.9x, ascii_downcase 3.7x, from_entries 3x, with_entries 1.9x
  slower than the old core), CM (compat mode, plus a closed-stdout bug: `qj -n 1 >&-` exits 0
  where jq reports "writing output failed" and exits 2).
- 2026-09-29: **CM merged.** `qj -n 1 >&-` now reports "writing output failed" and exits 2,
  and `qj . <&-` "Bad file descriptor": `src/main.rs` is `#![no_main]` with a C `main`, so
  Rust's start-up no longer reopens the closed descriptors on /dev/null. `QJ_JQ_COMPAT=1` is
  "be exactly jq" (`src/compat.rs`): it reproduces jq's stack-overflow SIGSEGV at jq's own
  depth (modelled from `ulimit -s` at 64 bytes a frame, exact at the default limit),
  the module-import-cycle SIGSEGV, and the `delpaths([[nan]])` hang including its memory
  growth, and turns off globbing, decompression and the qj-only options. jq's double free in
  `jv_dels` is not reproduced: whether it kills jq depends on the heap layout, not the
  program (see `docs/COMPATIBILITY.md`). jq_diff: 19,944/19,955 strict, plus 4 cases where
  neither tool finishes; the harness now runs qj even when jq hangs and requires that qj hang
  too.
- 2026-09-30: **Performance and Linux tracks merged.** PF (`17636c2`), PF-B (`8563801`) and
  PF-C (`7efc4ff`) made the port fast without giving up exactness. They added tape evaluation
  of simple programs, native versions of jq-defined builtins checked against their
  `builtin.jq` definitions (`native_diff`), and VM regions checked against jq's instructions
  as compiled (`vm_opt_diff`). MW (`2dd5923`) bounds how much of a memory-mapped input stays
  resident. FB (`2562e72`) re-measured the README's numbers on the M5 Max. TT (`c54b4e4`)
  moved `type`, `has`, `not` and the type filters onto the tape. LX (`851e73a`) verified
  Linux on GitHub's Ubuntu runner, fixing the Linux-only differences: glibc's libm and stdio,
  jq's Linux stack depth, and its locale handling. LX also made the Linux jq_diff step a
  ratchet (`diff_baseline_linux.txt`). jq_diff: **39,114/39,125 strict on both macOS and
  Linux**. The rest are the 7 exempt help/version cases and 4 where jq and qj both hang
  (`delpaths` with `nan` under `QJ_JQ_COMPAT=1`). The known deviations are all in
  `docs/COMPATIBILITY.md`. Crashes decided by jq's heap layout aren't reproduced
  (`jv_dels`' double free, `--debug-trace=all` of an empty stack on glibc). Nor is
  `jv_equal`'s stack overflow in compat mode. qj's deliberate crashes don't dump core on
  Linux.
- Wave 3 CLI requirements from B2:
  1. When a builtin aborts like jq (SIGABRT), jq's already-buffered stdout survives on macOS
     (Apple's `abort()` flushes stdio) but is lost on glibc. Flush qj's stdout before
     aborting on macOS only, via an additive `Host` hook.
  2. `%Z` for pre-1900 local times depends on the order of earlier libc time calls in the
     process, so run programs that use localtime/strflocaltime/mktime sequentially.
- Harness hygiene: jq's `lgamma_r` sign is random at 0, -0, NaN and ±inf (uninitialized in
  jq), so exclude those inputs from the corpus.
