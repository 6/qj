# qj — a fast, jq-compatible JSON processor

A faithful Rust port of jq 1.8.1, made fast by its input layer: SIMD parsing (C++ simdjson via FFI), memory-mapped and streaming input, and ordered parallel processing of NDJSON.

## After writing Rust code
```
cargo fmt
cargo clippy --release -- -D warnings
cargo test
```

## After writing shell scripts
```
shellcheck <file>.sh
```

## Testing
`cargo test` runs the fast suite: unit tests, e2e, ndjson, the simdjson FFI tests, and the
port's compiler and parser suites (~20s).
Compat suites are `#[ignore]` — run them with `--release`. **jq_diff is the gate**: every
change to behavior must keep it free of regressions against its baseline. The other compat
runners predate the port; they're kept as extra checks (all pass), but they compare
leniently or cover less, and jq_diff subsumes them.

```
cargo test                                                              # fast suite
cargo test --release jq_diff -- --ignored                               # THE GATE: strict differential vs jq 1.8.1 (scoreboard on stderr)
JQ_DIFF_FILTER=onig.test JQ_DIFF_VERBOSE=1 cargo test --release jq_diff -- --ignored  # one suite, with details
cargo test --release -- --ignored --nocapture                           # everything, older runners included
cargo test --release --lib io:: -- --ignored                            # src/io's long differential tests (util.c port, jq binary, engine)
cargo test --release --test native_diff -- --ignored --nocapture        # natives vs their builtin.jq definitions, out of process, capped
cargo test --release --test vm_opt_diff -- --ignored --nocapture        # the VM's regions vs jq's instructions as compiled, out of process, capped
# Older runners:
cargo test --release jq_compat -- --ignored --nocapture                 # cross-tool comparison
cargo test --release feature_compat -- --ignored --nocapture            # feature matrix
cargo test --release jq_differential -- --ignored --nocapture          # proptest differential vs jq
cargo test --release differential_filter -- --ignored --nocapture      # differential: random filters
cargo test --release differential_arithmetic -- --ignored --nocapture  # differential: arithmetic focus
cargo test --release differential_builtins -- --ignored --nocapture    # differential: builtins focus
cargo test --release differential_formats -- --ignored --nocapture     # differential: format strings
```

**Note:** libtest captures `eprintln!` (even from spawned threads), so the older runners'
summaries only show with `--nocapture`; jq_diff writes its scoreboard straight to the stderr
handle, so it shows without it.
Never pipe `--nocapture` output through `tail` — the verbose test produces 500+ lines which
can OOM `tail` on macOS. Use `grep` to filter if needed, or run the non-verbose test.

### jq_diff: the conformance gate

`tests/jq_diff.rs` (`#[ignore]`) measures the definition of done in `docs/JQ_PORT_PLAN.md`,
and is the gate for every change to qj's behavior. The default binary passes every case
strictly except 7: qj's own help/version text (`corpus/cli_meta.toml`), which the plan
exempts.
It runs jq 1.8.1 and qj with identical argv, stdin, environment and cwd, and compares stdout
bytes, exit code, and stderr with only the program name rewritten: a line-initial `qj:` to
`jq:`, and the exact line `Use qj --help for help with command-line options,` (the usage hint
after option errors) to `Use jq --help ...`. Nothing else is normalized, and jq's output is the
only expectation. Levels: `pass` (all three match),
`stdout` (stdout + exit code match), `fail`. A case counts as `skip` only when jq never
finishes it (timeout, or the output or memory cap) **and** qj doesn't either; a qj that
answers where jq hangs is a `fail`. The scoreboard shows both per suite and mode.
- **Cases:** `tests/jq_compat/*.test` (jq 1.8.1's own suites, `upstream/...`);
  `tests/jq_compat/corpus/*.test` (qj's corpus: program line, input line, blank line; no
  expected output); `tests/jq_compat/corpus/*.toml` (CLI cases with any argv, files, env,
  binary stdin, or standard descriptors closed with `close_fds = [0, 1, 2]`;
  format in `tests/jq_diff/cli.rs`). A CLI case with `merge = "file"` or
  `"pipe"` sends stderr where stdout goes (`>out 2>&1`, `2>&1 |`), which shows stdio's
  buffering order (`corpus/merged_output.toml`). Ids look like `upstream/man.test:280:compact`.
- **Modes** for `.test` cases: `compact` (`-c`, stdin), `pretty` (stdin), `file` (`-c`, input
  as a file argument, which qj memory-maps), `ndjson` (`-c`, input line twice in a
  file; only single objects/arrays, no `input`/`$__loc__`/`halt`). `%%FAIL` blocks run once as
  `fail` (`-c -n`); TOML cases as `cli`. `# jq_diff: modes=compact` limits a `.test` file.
- **Adding cases:** found a divergence? Add it to the corpus file for its category (or a
  `[[case]]` in `corpus/cli.toml`), run jq_diff, and fix it. `corpus/builtins_matrix.test` is
  generated: `python3 tests/jq_compat/corpus/gen_builtin_matrix.py`.
- **Knobs:** `JQ_DIFF_FILTER=a,b` (id substrings), `JQ_DIFF_MODES=compact,pretty,file,ndjson,fail,cli`,
  `JQ_DIFF_VERBOSE=1` (print every non-passing case), `JQ_DIFF_QJ_ENV="QJ_NO_SIMD_INPUT=1"` (extra env,
  given to jq too so `$ENV` stays comparable), `JQ_DIFF_BASELINE=path`, `JQ_DIFF_UPDATE_BASELINE=1`,
  `JQ_DIFF_JQ`/`JQ_DIFF_QJ` (binaries), `JQ_DIFF_TIMEOUT` (s, default 10), `JQ_DIFF_MEM_MB`
  (per-process RSS cap, default 2048), `JQ_DIFF_JOBS`.
- **Outputs:** `target/tmp/jq_diff/report.txt` (every non-passing case, jq vs qj stdout, stderr,
  exit code), `results.tsv` (one line per case), `baseline_candidate.txt`.
- **Ratchet:** `tests/jq_compat/diff_baseline.txt` (macOS; `diff_baseline_linux.txt` on Linux)
  lists each case at `pass` or `stdout`. The test fails when a listed case drops a level, and
  prints cases that improved. After a fix, record the gains with
  `JQ_DIFF_UPDATE_BASELINE=1 cargo test --release jq_diff -- --ignored` and commit the baseline
  with the fix. Never regenerate the baseline to make a regression pass. Runs filtered with
  `JQ_DIFF_FILTER`/`JQ_DIFF_MODES` update only their cases. Entries match by fingerprint, so
  moving a case within its file is fine (but changing a case's files, argv or stdin makes it a
  new case). To score a variant without the ratchet, point `JQ_DIFF_BASELINE` at a file that
  doesn't exist: `JQ_DIFF_QJ_ENV=QJ_NO_SIMD_INPUT=1 JQ_DIFF_BASELINE=target/tmp/no_baseline.txt`.
- CI runs it on Linux, ratcheting against `diff_baseline_linux.txt`. To record gains made on
  Linux, commit the `baseline_candidate.txt` from the run's `jq-diff-linux` artifact.
- jq results are cached in `tests/jq_compat/.cache/jq_diff.json` (invalidated automatically).
  A full run takes ~10s cold and ~7s cached on 18 cores.

- **Unit tests:** `#[cfg(test)]` modules alongside code.
- **Integration tests:** `tests/e2e.rs` — runs the `qj` binary against known JSON inputs.
  - Includes **jq conformance tests** (`assert_jq_compat`) that run both qj and jq and
    compare output. These run automatically when jq is installed, and are skipped otherwise.
  - **Zero divergence policy:** Every e2e test that exercises jq-compatible behavior MUST
    use `assert_jq_compat` to verify output matches jq exactly. Never write tests that
    accept output differing from jq — if a fast path (such as the reader's simdjson path)
    would produce different results, it must hand over to jq's parser or VM.
  - Includes **number literal tests** — jq 1.8.1's decNumber semantics: literals keep their
    exact value and print canonically (trailing zeros kept, `1e2` → `1E+2`), arithmetic is f64.
  - `assert_jq_compat_strict` also compares stderr and the exact exit code, as jq_diff does.
- **NDJSON tests:** `tests/ndjson.rs` — NDJSON integration tests (parallel engine, ordering,
  malformed lines). Most compare with jq, on stdin and as a file, including every shape the old
  core had an NDJSON fast path for.
- **FFI tests:** `tests/simdjson_ffi.rs` — the simdjson bridge's boundary: `TapeParser`'s tape
  layout, strings, structural indexes, error codes, buffer reuse and padding.
- **Input layer tests:** `src/io/tests/` — the reader against a line-by-line port of `util.c`
  (with its 4096-byte `fgets` chunks) on generated adversarial inputs, against the jq binary,
  streamed vs whole, and the parallel engine against the sequential reader. Whole inputs there
  are anonymous mappings the reader releases a page at a time (a stale read of released input
  faults); `release.rs` checks the release itself, including jobs the engine cancelled while
  their worker still reads them. The long runs are `#[ignore]`d
  (`cargo test --release --lib io:: -- --ignored`).
- **Cross-tool compat comparison** (`#[ignore]`, legacy): `tests/jq_compat_runner.rs` — runs
  jq.test against qj, jq, jaq, and gojq with the same arguments. Writes
  `tests/jq_compat/results.md`.
- **Feature compatibility suite** (`#[ignore]`, legacy): `tests/jq_compat/features.toml` —
  TOML-defined tests, per-feature Y/~/N matrix. Writes `docs/COMPATIBILITY.md` (below the
  marker; the text above it is hand-written and cites jq_diff's numbers).
- **Differential testing** (`#[ignore]`): `tests/jq_differential.rs` — property-based tests using
  `proptest` that generate random (filter, input) pairs and compare qj vs jq output. Four focused
  tests: general filters, arithmetic, builtins, and format strings. 2000 cases each, with a new
  random seed per run. Catches behavioral divergences that hand-written tests miss. Add each
  divergence it finds to the jq_diff corpus, then fix it.
- **Updating the vendored test suites:** `tests/jq_compat/update_test_suite.sh` — downloads
  all of jq's `.test` suites, `shtest` (reference only, in `tests/jq_compat/shtest/`) and the
  test modules from a jq release tag, and updates `mise.toml`. Then regenerate the builtin
  matrix and the jq_diff baselines.
  ```
  bash tests/jq_compat/update_test_suite.sh          # uses version from mise.toml
  bash tests/jq_compat/update_test_suite.sh 1.9.0    # upgrade to new version
  ```
- **When changing qj's behavior** (a divergence fixed, a fast path added), always:
  1. Add the case to the jq_diff corpus (`tests/jq_compat/corpus/`), and e2e tests with
     `assert_jq_compat` checks where they help
  2. Run `cargo test --release jq_diff -- --ignored`: no regressions, and record improvements
     with `JQ_DIFF_UPDATE_BASELINE=1`
  3. If the scoreboard's totals change, update the numbers in `README.md` and
     `docs/COMPATIBILITY.md`
  The builtin set is jq 1.8.1's `builtins`: don't add qj-only builtins.
- **When adding a fast path** (anything that produces values or output without going through
  jq's parser or VM, like the reader's simdjson path), prove it equivalent before enabling it:
  1. Add its shapes to the jq_diff corpus: the `fastpath` sweep in
     `tests/jq_compat/corpus/ndjson.toml` (run over adversarial NDJSON), and input edge cases
     in `corpus/gen_cli_io.py` (regenerate `cli_io.toml`)
  2. Check it against the slow path in tests and in a fuzz target (as `src/io/fuzzing.rs` and
     `fuzz_io_reader` do for the reader), and run the fuzzer
- **Cache:** External tool results (jq, jaq, gojq) are cached in `tests/jq_compat/.cache/`.
  Cache auto-invalidates when test definitions or tool versions (`mise.toml`) change.
  Delete to force full re-run: `rm -rf tests/jq_compat/.cache/`
- **Conformance:** compare output against jq on real data.
```
diff <(./target/release/qj '.field' test.json) <(jq '.field' test.json)
```

## Fuzzing

Three fuzz targets in `fuzz/`, for the simdjson FFI boundary and the input layer. Requires
nightly and `cargo-fuzz`. The port's own differential coverage is jq_diff and
`jq_differential`.

Fuzz binaries use libfuzzer which runs indefinitely without `-max_total_time`.
All `[[bin]]` entries have `test = false` to prevent `cargo test` from picking them up.
Always run fuzz targets individually via `cargo +nightly fuzz run <target> -- -max_total_time=N`.
Without cargo-fuzz, `cargo +nightly build --manifest-path fuzz/Cargo.toml` at least checks
that they build.

**ASan link error on macOS:** The C++ FFI objects (simdjson/bridge) are compiled with Apple
Clang, whose ASan runtime is incompatible with rustc nightly's. Use `-s none` to disable
sanitizers: `cargo +nightly fuzz run <target> -s none -- -max_total_time=N`.

**FFI boundary** (run after changing `src/simdjson/` or updating simdjson):
```
cargo +nightly fuzz run fuzz_parse     -s none -- -max_total_time=120   # TapeParser on arbitrary bytes, every tape word walked
cargo +nightly fuzz run fuzz_dom       -s none -- -max_total_time=120   # simdjson -> jq values vs jq's parser port
```

**Input layer** (run after changing `src/io/`):
```
cargo +nightly fuzz run fuzz_io_reader -s none -- -max_total_time=120   # fast path, streaming, engine vs jq's input loop
```

## Benchmarking

All benchmark scripts, data generators, and results live in `benches/`.

### Regression detection (iai-callgrind, requires valgrind)
```
cargo bench --bench eval_regression
```
Counts CPU instructions (deterministic, no wall-clock noise). Runs on CI for every PR (Ubuntu only).
Covers: compiling programs, running them on the VM, simdjson's tape to jq values, jq's parser
port, the NDJSON reader, and the printer.

### Parse throughput (input layer stages vs serde_json)
```
bash benches/download_data.sh --json --gharchive  # twitter.json + gharchive.ndjson
cargo build --release                             # to include qj end to end
cargo bench --bench parse_throughput
```

### C++ baseline (simdjson's DOM parse without FFI, for overhead comparison)
```
bash benches/build_cpp_bench.sh
./benches/bench_cpp
```

### End-to-end tool comparison (qj vs jq vs jaq vs gojq)
```
bash benches/setup_bench_data.sh    # all test data (includes ~1GB GH Archive download)
cargo run --release --features bench --bin bench_tools -- --type json                    # JSON (large_twitter.json)
cargo run --release --features bench --bin bench_tools -- --type ndjson                  # NDJSON (gharchive_medium.ndjson, 3.4GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson --size small     # NDJSON (gharchive.ndjson, 1.1GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson --size large     # NDJSON (gharchive_large.ndjson, 6.2GB)
cargo run --release --features bench --bin bench_tools -- --type ndjson-extended --size xsmall  # extended: streaming + stdin + complex + slurp
cargo run --release --features bench --bin bench_tools -- --type json --runs 3 --cooldown 2  # quick JSON run
```

### Memory usage comparison (qj vs jq vs jaq vs gojq)
```
cargo run --release --features bench --bin bench_mem -- --type json     # JSON (large_twitter.json)
cargo run --release --features bench --bin bench_mem -- --type ndjson    # NDJSON (gharchive.ndjson)
```
Measures peak RSS via `wait4()` rusage. No external tools needed (no hyperfine).
Results written to `benches/results_mem_json.md` / `benches/results_mem_ndjson.md`.

### GH Archive data (for NDJSON benchmarks)
```
bash benches/download_data.sh --gharchive           # gharchive.ndjson (~1.1GB) + .ndjson.gz
bash benches/download_data.sh --xsmall              # gharchive_xsmall.ndjson (~500MB)
bash benches/download_data.sh --medium              # gharchive_medium.ndjson (~3.4GB, ~1.2M records)
bash benches/download_data.sh --large               # gharchive_large.ndjson (~4.7GB)
```
Use `QJ_GHARCHIVE_HOURS=2` for quick testing with fewer hours of data.

### Profiling a single run
```
QJ_ENGINE_STATS=1 ./target/release/qj -c '.' benches/data/gharchive.ndjson > /dev/null   # parallel engine counters
cargo test --release --test io_throughput -- --ignored --nocapture                     # input layer stage timings
```
The `io_throughput` timings are sanity checks, not benchmarks; use `hyperfine` for accurate
numbers. (`--debug-timing` was the old core's; qj accepts it and ignores it.)

### CPU profiling with `sample` (macOS)
Use `cargo build --profile profiling` for optimized builds with debug symbols.
The `profiling` profile inherits from release but keeps symbols (`strip = false, debug = 1`).
```
# Warm cache first, then run and sample in one shot:
./target/profiling/qj --threads 1 -c 'select(.type == "PushEvent")' benches/data/gharchive_medium.ndjson > /dev/null
./target/profiling/qj --threads 1 -c 'select(.type == "PushEvent")' benches/data/gharchive_medium.ndjson > /dev/null &
sample $! 3 1 -file /tmp/qj_profile.txt
```
- Use `--threads 1` to isolate work on a single worker thread (cleaner call stacks).
- Use `gharchive_medium.ndjson` (3.4GB, ~1s) — xsmall finishes too fast to sample.
- The profile is a call tree with sample counts; look for the deepest frames to find hotspots.
- Symbols show Rust function names + source locations (e.g., `simd.rs:212`).

### Ad-hoc comparison
Always warm cache with `--warmup 1` (sufficient for file I/O cache; higher values add time without improving accuracy).
```
hyperfine --warmup 1 './target/release/qj ".field" test.json' 'jq ".field" test.json' 'jaq ".field" test.json'
```

### Environment variables
Read by the input layer (`src/io`, `src/cli/{input,run}.rs`):
- `QJ_NO_MMAP=1` — stream regular files (and stdin redirected from a file) instead of
  memory-mapping them.
- `QJ_WINDOW_SIZE=N` — at most N MB of input in flight in the parallel record engine (default:
  threads × 8 MB, clamped to 16–128 MB); `NK` is N KB (for tests). Jobs are at most a
  quarter of the window.
- `QJ_NO_RELEASE=1` — keep memory-mapped input resident (A/B checks). By default the reader
  gives back the pages of a mapped input that nothing can read anymore, in steps of 8 MB, so
  residency stays near the window rather than the file size (see "Memory" in
  `docs/LIMITATIONS.md`).
- `QJ_RELEASE_STEP=N` — release mapped input in steps of N bytes instead; `1` releases every
  page as soon as nothing can read it (tests: released pages are `PROT_NONE`, so a stale read
  faults). Test mode: `QJ_RELEASE_STEP=1 QJ_WINDOW_SIZE=16K`, e.g. as
  `JQ_DIFF_QJ_ENV="QJ_RELEASE_STEP=1 QJ_WINDOW_SIZE=16K" JQ_DIFF_BASELINE=target/tmp/none cargo test --release jq_diff -- --ignored`
  (`corpus/release.toml` runs large-ish NDJSON that way in every jq_diff run).
- `QJ_NO_SIMD_INPUT=1` — parse all input with the jq parser port instead of the simdjson fast
  path (A/B checks).
- `QJ_INPUT=util` — read input with the CLI's plain `util.c` port instead of `src/io`'s reader
  (A/B checks).
- `QJ_NO_TAPE=1` — run every program on built values: turns off the evaluation of simple
  programs (paths, `.[]`, `.a?`, `.[]?`, `length`, `keys`, `type`, `has("k")`, `map`, `add`
  of numbers, `{...}`, `def f: ...;`, `select(... == c and ...)`, `select(... > c)`,
  `select(.a | type == "t")`, `not` or `.a == c` as values, the type filters such as
  `numbers` and `values`, `isnan`, `isnormal`) on simdjson's tape (`src/io/tape_eval.rs`;
  A/B checks).
- `QJ_ENGINE_STATS=1` — after a parallel run, print the engine's counters to stderr (and how
  many ranges of mapped input were released).
- `QJ_NO_NATIVE=1` — run jq's bytecode definitions of builtin.jq functions instead of the exact
  native fast paths in `src/jq/builtins/native/` (A/B checks). Natives are also off under
  `--debug-trace`, while tracking paths, and with `QJ_JQ_COMPAT=1`.
- `QJ_NO_VM_OPT=1` — run jq's instructions as compiled, without the VM's regions (straight-line
  runs compiled to register code) and frameless calls of region bodies
  (`src/jq/lang/execute/region.rs`; A/B checks). Also off under `--debug-trace` and with
  `QJ_JQ_COMPAT=1`.
- `--threads N` — worker threads for the engine (default: all non-efficiency cores); 0 or 1
  runs sequentially. Records always run sequentially for -n, -s, -R, --seq, --stream,
  --debug-trace, and programs using input/inputs, now, halt/halt_error, debug/stderr,
  localtime/strflocaltime/mktime/strptime, _strindices, modulemeta, labels, $__loc__, or
  modules (see `SEQUENTIAL_BUILTINS` and `parallel_plan` in `src/cli/run.rs`).

Read by everything (`src/compat.rs`):
- `QJ_JQ_COMPAT=1` — **be exactly jq 1.8.1, bugs included.** Reproduces jq's crashes and
  hangs and turns off qj's own additions (glob expansion, `.gz`/`.zst` decompression,
  `--threads`/`--jsonl`/`--debug-timing`, which become jq's "Unknown option"). The crashes
  are a module import cycle (SIGSEGV), `delpaths` with a `nan` path element (hangs,
  growing), and jq's C stack running out in any of its seven recursions, each at its own
  depth: freeing (`jv_free`), comparing (`jv_equal`/`jv_cmp`, so `==`, `<`, `sort`,
  `group_by`, `unique`, `min`, `max`, `bsearch`, `-`, `index`), `contains`/`inside`,
  object `*`, `setpath`/`=`/`|=`, `delpaths`/`del`, and a chain of `import`s
  (`load_library`, about 20,000 modules at 8 MB). Only the depth jq's own traversal
  reaches counts (`src/compat/depth.rs`). Natives, the VM's
  regions and tape evaluation are off here, so everything goes through the value layer.
  Parallel processing stays on, and qj's help/version text and `qj:` name stay qj's. Read
  once at start-up; set means anything but empty or `0`. `docs/COMPATIBILITY.md` has the
  models per OS, the recursions that cannot overflow, and the one that isn't reproduced;
  `tests/jq_compat/corpus/compat_mode.toml` and `tests/compat_mode.rs` are the tests.
  (It used to make the old evaluator imitate jq's number precision, which is simply how qj
  behaves now, in every mode.)

Gone with the old core, and ignored now: `QJ_CORE` (`old` selected the old evaluator) and
`QJ_NO_FAST_PATH` (disabled its NDJSON fast paths).

### Important
Never run benchmarks concurrently with tests or other CPU-intensive processes.
Benchmarks require exclusive CPU access for reliable results.

## Architecture
qj is a port of jq 1.8.1 (see `docs/JQ_PORT_PLAN.md`). `src/main.rs` restores the default
SIGPIPE handling and runs `qj::cli::run::main`.

- `src/cli/` — the command line, a port of jq's `main.c` and `util.c`: option handling
  (`args.rs`: options, their errors and exit codes, colors, `-f`), qj's own help/version text
  (`usage.rs`), the rest of `main.c` on the port (`run.rs`: compile, `process()`, output, exit
  codes, and whether records can run in parallel), `util.c`'s plain input reader (`input.rs`,
  behind the `Reader` trait; `QJ_INPUT=util`) and `jq_test.c` (`run_tests.rs`, `--run-tests`)
- `src/jq/` — the port of jq's core: `value/` (values and numbers, the printer, the JSON
  parser), `lang/` (lexer, parser on bison's tables, compiler, bytecode VM, linker),
  `builtins/` (the C builtins and jq's own `builtin.jq`), `platform/` (Oniguruma regex, libc
  time, libm)
- `src/io/` — the input layer: jq's input loop with a simdjson fast path (`reader.rs`),
  opening inputs (`source.rs`: mmap, streams, decompression), simdjson's tape to jq values
  (`simd.rs`), printing and navigating the tape without values (`tape.rs`), simple programs
  evaluated on the tape (`tape_eval.rs`; the CLI's `tape_program` decides when), the ordered
  parallel record engine (`parallel.rs`), and the checks the fuzzer runs (`fuzzing.rs`)
- `src/simdjson/` — the C-linkage bridge to the vendored simdjson (`simdjson/`): its DOM
  parser (`TapeParser`), whose tape `src/io` reads
- `src/compat.rs` — `QJ_JQ_COMPAT=1`, "be exactly jq": reproducing jq's crashes and hangs
  (including the model of jq's C stack) and switching qj's own additions off
- `src/decompress.rs` — which inputs are compressed (`.gz`/`.gzip`, `.zst`/`.zstd`); the readers
  decompress them as streams
- `benches/` — all benchmark scripts, data generators, C++ baseline, and Cargo benchmarks
- `fuzz/` — cargo-fuzz targets for the simdjson FFI boundary and the input layer (requires nightly)

The old core (a tree-walking evaluator, flat-buffer evaluation, NDJSON fast paths and C++
passthroughs, selectable with `QJ_CORE=old` after the switch) was deleted; see
`docs/OPTIMIZATION_IDEAS.md` for what it taught.

## Compressed file support
Transparent decompression for `.gz` (gzip) and `.zst`/`.zstd` (zstd) files, detected by extension.
Glob patterns in file arguments are expanded (quote to bypass shell: `'data/*.json.gz'`), but
only when the argument isn't an existing path and matches something; otherwise it stays a file
name and fails to open exactly as in jq.
```
qj '.actor.login' data/*.json.gz                      # shell-expanded
qj 'select(.type == "PushEvent")' 'data/*.ndjson.gz'  # qj-expanded glob
qj -s 'add' file1.json.zst file2.json                 # mixed compressed + plain
```
Compressed files are decompressed as they're read (gzip files may hold several members), then
processed like any other input.
For benchmarking, `benches/download_data.sh --gharchive` produces a `.ndjson.gz` alongside the uncompressed files.
