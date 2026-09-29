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
