# Compatibility

qj aims to be indistinguishable from jq 1.8.1. For every program, input and command
line, it should write the same bytes to stdout, exit with the same code, and print the
same messages on stderr, apart from its own name. To get there, qj runs a port of jq
1.8.1's own implementation: the value and number model, parser, compiler, bytecode
interpreter, builtins (the C ones and `builtin.jq`), and `main.c`. See
[JQ_PORT_PLAN.md](JQ_PORT_PLAN.md).

## How it's measured

The gate is the jq_diff harness (`tests/jq_diff.rs`), run with
`cargo test --release jq_diff -- --ignored`. It runs jq 1.8.1 and qj with the same
arguments, stdin, environment and working directory. It then compares stdout byte for
byte, the exit code, and stderr. jq's own output is the only expectation; the
expected-output lines in the test files are never used. The only rewriting is qj's
name in messages (see [Exemptions](#exemptions)).

The cases come from three places:

- **jq 1.8.1's own test suites.** These are `jq.test`, `man.test`, `manonig.test`,
  `onig.test`, `base64.test`, `uri.test` and `optional.test`, vendored in
  `tests/jq_compat/`. Each case runs four ways:
  - compact output (`-c`), input on stdin
  - pretty output, input on stdin
  - input as a file argument
  - input as multi-line NDJSON (for object and array inputs)

  `%%FAIL` cases, programs that must not compile, run once.
- **qj's corpus** (`tests/jq_compat/corpus/`), which has three parts:
  - probes by category: paths and assignment, `reduce`/`foreach`, sorting and
    comparison, `try` and errors, strings and formats, streams, dates, number
    formatting, literals, input robustness, and value layout (array allocation,
    duplicate keys, identity of parsed values in path expressions)
  - a generated matrix of builtins × inputs
  - command-line cases: options and their errors, exit codes, input and output modes,
    adversarial NDJSON, and the programs qj evaluates on simdjson's tape without building
    values (`corpus/tape.toml`, under every output option)
- **A ratchet**, `tests/jq_compat/diff_baseline.txt` (`diff_baseline_linux.txt` on
  Linux), which fails the run when any case gets worse.

Results against jq 1.8.1, the same on macOS (arm64, against jq's macOS release binary)
and on Linux (x86-64 with glibc 2.39, against its Linux release binary, which links glibc
statically):

| Cases | Count | Byte-exact (stdout, exit code, stderr) |
|---|--:|--:|
| jq's own suites | 2,903 | **2,903 (100%)** |
| qj's corpus | 36,300 | 36,289 |
| **Total** | **39,203** | **39,192** |

The 7 cases that differ are all qj's own help, version and usage text; see
[Exemptions](#exemptions). Four more are neither matched nor missed: jq never finishes
them (`QJ_JQ_COMPAT=1` with a `nan` path element in `delpaths`), and all that can be
required is that qj not finish either. Across modes, the counts are 15,483 compact, 6,529
pretty, 6,529 file, 4,542 NDJSON, 19 `%%FAIL`, and 6,101 command-line cases. The
command-line cases include some that merge stdout and stderr into one file or pipe,
checking that output and error messages interleave exactly as jq's stdio buffering
interleaves them, and some that start the tool with standard descriptors closed
(`corpus/cli_closed_fds.toml`).

The older runners also pass with the default binary. They're either lenient (they compare
outputs as JSON, with numbers as f64) or narrower, and jq_diff covers what they check:

| Runner | Result |
|---|---|
| `jq_differential` | no divergence over 4 × 2,000 random programs and inputs |
| `jq_compat` (jq.test, all tools) | qj 497/497, jq 497/497, gojq 425/497, jaq 343/497 |
| `feature_compat` | the matrix below, 181/181 features |

Three more runners were removed with the old evaluator: `jq_conformance` (jq.test, which
jq_diff runs in every mode), `conformance_gaps` (jq.test's number-model cases) and
`cli_conformance` (its command lines are now jq_diff cases, `corpus/cli_basics.toml`).

CI runs jq_diff on Linux, where it ratchets against `tests/jq_compat/diff_baseline_linux.txt`.
What jq does there that it doesn't on macOS, qj does too: glibc's libm (for example the last
bit of `cbrt`), glibc's stdio buffering (the order of output and errors in one file or
pipe), glibc's `assert()` text, and the x86-64 conversions of out-of-range doubles to
integers (`halt_error(1e10)`).

The reference on Linux is jq's official 1.8.1 release binary (`jq-linux-amd64`, which
mise installs). It links glibc statically, and after its `setlocale(LC_ALL, "")` it
translates glibc's messages for `LC_MESSAGES` (errno texts, such as "Datei oder Verzeichnis
nicht gefunden", and `assert()` lines) and classifies bytes for `LC_CTYPE`, but formats and
parses dates in the C locale whatever the environment says. qj does the same on Linux. A
jq built by a distribution, linked dynamically, can differ: its dates follow the locale,
as they do on macOS. `corpus/locale.toml` checks this, and CI installs German and
French messages and a Latin-1 locale for it.

## Exemptions

- **Help and version text.** qj has its own output for `-h`/`--help`, `-V`/`--version`,
  `--build-configuration` (and `$JQ_BUILD_CONFIGURATION`), and for the usage summary
  printed after usage errors.
- **qj's name.** qj's messages say `qj:` where jq's say `jq:`
  (`qj: error (at <stdin>:0): ...`). The hint after an option error says
  `Use qj --help ...`. jq_diff maps both back before comparing stderr, and rewrites
  nothing else.
- **jq's own nondeterminism.** When jq's output doesn't depend only on its input, there's
  nothing to match. For example, jq's `lgamma_r` returns an uninitialized sign for 0, -0,
  NaN and ±infinity, so the corpus leaves out those inputs. `now` reads the clock.
- **jq's crashes and hangs.** Where jq 1.8.1 crashes or never finishes, the result
  depends on the stack limit or the heap layout rather than on jq's semantics, so by
  default qj returns a sane answer instead. `QJ_JQ_COMPAT=1` reproduces jq's behaviour;
  see [Being exactly jq](#being-exactly-jq).

  | Program | jq 1.8.1 | qj, default | qj, `QJ_JQ_COMPAT=1` |
  |---|---|---|---|
  | `[1] \| delpaths([[nan]])` | hangs, growing ~1.2 GB/s | `[]` | hangs, growing |
  | a value nested deeper than the C stack allows, e.g. `reduce range(200000) as $i (null; [.]) \| length` | SIGSEGV | `1` | SIGSEGV at the same depth |
  | a value or path that drives one of jq's other recursions past its stack: comparing (`==`, `<`, `sort`, `group_by`, `unique`, `min`, `max`, `bsearch`, `-`, `index`), `contains`/`inside`, object `*`, `setpath`/`=`/`\|=`, `delpaths`/`del` | SIGSEGV, at a depth of its own (each recursion has a different frame) | the answer | SIGSEGV at the same depth |
  | a module that imports itself, directly or through others | SIGSEGV | `qj: error: ... imports itself (import cycle)`, exit 3 | SIGSEGV |
  | a deeply nested *program*, below `ulimit -s` 2 MB (e.g. 4,990 nested `select(...)` at 1 MB) | SIGSEGV while compiling | the answer | the answer — not reproduced, see below |
  | a chain of more than about 20,000 imported modules (8 MB stack; 18,000 on Linux) | SIGSEGV | the answer, however long the chain | SIGSEGV at the same depth |
  | `--run-tests --skip` with no count | SIGSEGV (`atoi(NULL)`) | SIGSEGV | SIGSEGV |
  | `[1,2] \| try delpaths([[{}]]) catch .` | prints the error, then SIGSEGV | prints the error, exit 0 | prints the error, exit 0 |
  | `delpaths([[{"start":1}],[0]])` over two inputs, e.g. `[1] {}` | the first input's error, then `Assertion failed: (JVP_HAS_KIND(a, JV_KIND_STRING))` on the second, exit 134 | both inputs' errors, exit 5 | both inputs' errors, exit 5 |
  | `--debug-trace=all` of an instruction that runs with an empty data stack, e.g. the BACKTRACK after `[.[] \| . * 2]`'s APPEND | reads 4 bytes of its stack memory it never wrote as the next stack entry: on macOS they are 0, and nothing more is printed; with glibc they are leftover heap data, and jq prints garbage or dies of SIGSEGV | the trace jq prints on macOS | the same |

  The last three are not reproduced in either mode, because there is nothing to reproduce.
  In the trace, what jq reads is whatever its allocator left in that memory, so jq_diff's
  `--debug-trace=all` cases stay off it. The two `delpaths` rows are the same bug: jq frees
  the key twice in `jv_dels`' slice-delete error path. In the second of them, the key is a
  program constant, so later inputs run on freed memory.
  Whether that kills jq, and how, is decided by the heap, not by the program: `[1,2]` with an empty `{}` dies, while
  `[1]`, `[1,2,3]`, `[1,2,3,4]`, `[range(2)]`, `{"start":"x"}` as the key, and even
  wrapping the same expression in an array (`[[1,2] | try delpaths([[{}]]) catch .]`) all
  exit 0 — 20 runs each, no variation. Crashing at the site would invent failures where
  jq succeeds.

  jq's deliberate aborts, which come from `assert()` and are deterministic, are
  reproduced in **both** modes, including macOS stdio flushing the output produced before
  the abort.

  A reproduced crash is jq's in everything jq_diff compares: the signal, the exit status
  a shell reports (139 for SIGSEGV, 134 for SIGABRT) and the output lost with it. One thing
  differs, on Linux systems that collect core dumps: qj tells the kernel not to dump its
  core first, so a shell reports jq's crash as `Segmentation fault (core dumped)` and qj's
  as `Segmentation fault`. A core of a deliberate crash shows nothing wrong, and qj's is
  big: its allocator reserves about 1 GB of address space, which a core handler such as
  systemd-coredump reads in full (1.5 s a crash on GitHub's runners, 50 ms for jq's).

## Being exactly jq

`QJ_JQ_COMPAT=1` makes qj a drop-in jq 1.8.1, bugs included. It

- reproduces the crashes and hangs in the table above, and
- turns off qj's own additions: glob expansion (jq opens the pattern as a file name and
  fails), `.gz`/`.zst` decompression (jq reads the bytes and fails to parse them), and
  `--threads`, `--jsonl` and `--debug-timing` (jq's `Unknown option`, exit 2).

Parallel processing stays on: it isn't observable. qj's help and version text and the
`qj:` name in messages stay qj's own, as the exemptions above say. Everything else is
unchanged — compat mode is not a different evaluator, and the default is already
jq-exact for every program that doesn't reach one of jq's own bugs.

The variable is read once at start-up, and counts as set unless it is empty or `0`.

`tests/jq_compat/corpus/compat_mode.toml` checks every one of these against the jq
binary, and `tests/compat_mode.rs` checks that the default keeps the extensions.

Compat mode also turns off the three things qj does that jq's own code doesn't: the
native versions of `builtin.jq` functions, the VM's regions, and evaluation on
simdjson's tape. All three are proven equivalent to jq's own path, but the value layer
is where jq's C stack is modelled, so compat mode goes through it.

### jq's recursions, and which ones can overflow

jq walks values, paths and programs by recursing in C. Every recursion in jq 1.8.1's
`src/*.c` — direct or mutual, found from the call graph of the compiled sources — is
below, with what drives its depth and whether that depth can outgrow the C stack.

**Over values and paths.** These six are driven by data, so nothing bounds them but the
stack. qj does all six with a loop, and compat mode reproduces each one's crash:

| jq | driven by | emulated |
|---|---|---|
| `jv_free` → `jvp_array_free` / `jvp_object_free` | the nesting of the value | yes |
| `jv_equal` → `jvp_array_equal` / `jvp_object_equal` | the nesting the comparison reaches | yes |
| `jv_cmp` (`<`, `sort`, `group_by`, `unique`, `min`, `max`, `bsearch`) | the same, plus a level for an object's sorted key array | yes |
| `jv_contains` → `jvp_array_contains` / `jvp_object_contains` | the deepest attempt of its search | yes |
| `jv_object_merge_recursive` (object `*`) | the nesting shared by both operands | yes |
| `jv_setpath` (`setpath`, `=`, `\|=`, `+=`, …) | the length of the path | yes |
| `delpaths_sorted` (`delpaths`, `del`) | the length of the paths, a level at a time | yes |
| `jv_getpath` (`getpath`, `path`) | the length of the path | **not needed**: it is a tail call, and both release compilers turn it into a loop. `[range(1000000)\|0] as $p \| null \| getpath($p)` answers `null` at `ulimit -s 256` on macOS and on Linux |
| `jv_dump_term` (the printer) | the nesting of the value, **capped at `MAX_PRINT_DEPTH` (256)**, below which it writes `<skipped: too deep>` | **cannot overflow** |
| `load_library` ↔ `process_dependencies` (`linker.c`) | the length of a chain of `import`s or `include`s, one module a level | yes |

The JSON parser is not in the list: `jv_parse` keeps its own stack on the heap and stops
at `MAX_PARSING_DEPTH` (10,000), so input alone cannot drive any of these past 10,000
levels. That is below every threshold at `ulimit -s` 1024 KB or more; at smaller limits
input *can* reach them, and it does, through the same checks (`qj -c length` on 9,000
nested arrays dies in both tools at `ulimit -s 256`).

**Over the program.** `compile.c`'s `block_bind_subblock_inner`, `block_free`/`inst_free`,
`block_get_unbound_vars`, `block_mark_referenced`, `compile`, `count_cfunctions` and
`expand_call_arglist`; `bytecode.c`'s `bytecode_free` and `dump_disassembly`;
and `execute.c`'s `optimize` and `ret_follows`. All of them are bounded by how deeply a
*program* nests, which
bison's parser bounds in turn: at `YYMAXDEPTH` (10,000 parser stack entries) jq stops
with `jq: error: memory exhausted`, which is 4,990 nested `select(...)`, 3,330 nested
`def`s, 1,995 nested `def f(g):` or 9,990 nested `[`.

The deepest of those costs jq about 226 bytes a level on macOS and 234 on Linux
(nested `select`, measured at 256, 512 and 1024 KB), so **no program jq will parse can
overflow a stack of 2 MB or more**: jq survives 4,990 nested `select(...)` at
`ulimit -s 2048` and dies at 1024. qj does not reproduce that crash, and this is the one
exemption in compat mode's stack emulation: the bytes a level depend on which kind of
nesting it is — 226 for `select(...)`, 177 for nested `def`s, and parenthesis and pipe
nesting is cheap enough that jq survives 9,990 of them at 256 KB, where 9,990 nested `[`
kills it — so there is no single model, and approximating it would invent crashes where jq
succeeds. Below 2 MB of stack, and only there, a deeply nested program crashes jq and not
qj: over 19 nesting shapes × 3 depths × `ulimit -s` 256, 512, 1024 and 8176 KB, there is
no case where qj crashes and jq answers, and 14 where jq crashes and qj answers.

`jq_format_error` recurses through an invalid value inside an invalid value, which jq
never nests more than a few deep.

The module chain used to be the other exemption, for the opposite reason: qj mirrored jq's
recursion and parsed each module inside it, so its frames were more than twice jq's and it
died at 7,678 modules where jq answers — a `SIGSEGV` where jq succeeds, the worst kind of
difference. `process_dependencies` is now the loop those two functions make, with their
locals on the heap, so qj answers for every chain jq does and for chains twice as long; in
compat mode it dies at jq's depth. `import` cycles, which make jq recurse forever, are
reproduced there too (the table above).

**One thing about long chains is jq's own, and qj reproduces it.** Past **4,096** modules,
jq resolves a namespaced call 4,096 levels too shallow: a chain of `n` modules each
defining `def f: 1 + m::f;` answers `n - 1` up to 4,095 and `n - 4097` from 4,096 on
(`4,500` gives `404`). qj gives the same answer at every length, so it isn't a divergence
— but it means a chain that long doesn't mean what it looks like in either tool.

### How exact the stack-overflow emulation is

Each site has its own frame, so each has its own threshold, and each is modelled as
`(stack_bytes - reserved) / frame` levels. `frame` is exact: the deepest value jq
survives is linear in `ulimit -s` with that slope at every limit measured. `reserved` is
what is left over at the *worst* of the call sites measured, plus a margin that keeps qj
from ever surviving where jq dies.

The six recursions over values share one `reserved` (see below). The module chain has its
own, because the level where its stack runs out is *parsing a module*, with bison's three
`YYINITDEPTH` (200) arrays on the stack — 12,000 bytes against the 4,096 of the deepest
value site on macOS, 8,304 against 3,856 on Linux — and folding that into the shared figure
would cost `jv_free` 128 levels for nothing.

On macOS/arm64, bisected against jq's release binary at `ulimit -s` 1024, 2048, 4096,
8176 and 16384 KB. The last two columns are the default 8176 KB:

| site | bytes a level | deepest jq survives | qj, `QJ_JQ_COMPAT=1` |
|---|--:|--:|--:|
| `jv_free` | 64 | 130,760 | 130,655 |
| `jv_equal` / `jv_cmp` | 128 | 65,375 | 65,327 |
| `jv_contains` | 176 | 47,547 | 47,510 |
| `jv_object_merge_recursive` | 112 | 74,719 | 74,659 |
| `jv_setpath` | 144 | 58,114 | 58,067 |
| `delpaths_sorted` | 240 | 34,868 | 34,839 |
| `load_library` (modules) | 416 | 20,096 | 20,079 |

On Linux/x86-64 (jq's release binary, built by gcc), bisected at 1024, 4096, 8192 and
16384 KB (the module chain also at 2048) with the kernel's stack randomization off
(`setarch -R`), which is what makes the numbers repeatable. The last two columns are the 16 MB of GitHub's runners:

| site | bytes a level | deepest jq survives | qj, `QJ_JQ_COMPAT=1` |
|---|--:|--:|--:|
| `jv_free` | 48 | 349,486 | 349,269 |
| `jv_equal` / `jv_cmp` | 144 | 116,481 | 116,422 |
| `jv_contains` | 176 | 95,312 | 95,254 |
| `jv_object_merge_recursive` | 128 | 131,053 | 130,975 |
| `jv_setpath` | 160 | 104,843 | 104,779 |
| `delpaths_sorted` | 240 | 69,895 | 69,852 |
| `load_library` (modules) | 464 | 36,139 | 36,119 |

The margin is 6 KB of stack on macOS and 8¼ KB on Linux, which is most of why qj's
threshold is 29 to 218 levels below jq's. What it covers is different on each:

- **macOS: the environment.** argv and the environment sit on top of jq's stack, so its
  threshold drops by about a level per 64 bytes of them — `jv_free` reaches 130,760 in
  the harness's five variables and 130,664 under an interactive shell's, 6 KB more. The
  bisections use the harness's environment, so the margin is what a larger one can take.
- **Linux: the kernel.** `arch_align_stack` starts the main thread's stack up to 8 KB
  below its top at random, so jq's threshold moves from run to run over about 170 levels
  for `jv_free` and 57 for `jv_equal`. Sampling 10 runs per depth at 8 MB, jq survived
  every run up to about 160 levels below the deterministic threshold and no run above it.
  The model takes the whole 8 KB, so it lands at or below the bottom of that window:
  inside it, qj dies where jq only sometimes does.

Three smaller reasons the two can't agree to the last frame:

- **One reserve for every site.** jq drives some of these recursions from *inside*
  another: `delpaths_sorted` compares path elements and frees values a level at a time,
  and `jv_object_merge_recursive` frees the value it replaces. qj charges the inner
  recursion the stack the outer one holds, so the same value kills it sooner the deeper
  the paths go — two paths ending in a value 64,000 levels deep are compared fine at the
  top and kill jq (and qj) a thousand `delpaths` levels down. For that to work the margin
  has to be charged once, not once per recursion, so every site reserves what the *worst*
  site's base cost measured (`jv_cmp` from `sort`) rather than its own. That costs the
  other sites up to 576 bytes on macOS and 2,000 on Linux — 9 and 42 levels of `jv_free`.
  A site that drives others also gives up its last level, so that a frame of what it
  drives still fits where it stops: without that, comparing two *shallow* path elements at
  the deepest `delpaths` level the model allows would look like an overflow, and qj would
  die where jq is nowhere near its stack.
- which call site reaches the recursion shifts it by a frame or two — `jv_free` reaches
  130,760 through a builtin such as `length`, 130,763 from `main.c`'s output path and
  130,757 through `tojson`; `jv_cmp` 65,379 through `==` and 65,375 through `sort`. Each
  model follows the worst.
- the bytes a level don't always divide the stack evenly, which costs one more level at
  some limits.

Only the depth jq *reaches* counts, so qj follows jq's traversal rather than the nesting
of the values:

- `jv_equal` answers from the pointer when both sides are one allocation, so `$v == $v`
  never recurses however deep `$v` is — while `jv_cmp` has no such shortcut, and
  `[$v, $v] | sort` on the same value dies.
- a comparison stops at the first difference: two values that differ in their first
  element are compared in two frames.
- `jvp_array_equal` answers "not equal" from the lengths, without looking at an element;
  `jvp_object_equal` walks the first object's slots in order, so a key the other object
  hasn't got stops it there — but it compares the *counts* only at the end, so
  `{a: deep} == {a: deep, b: 1}` still compares the deep values in full before saying
  false.
- `jv_cmp` compares two objects' sorted key arrays before their values, which is why an
  object goes one level deeper than an array; a NaN is compared as `null` one frame down.
- `jv_contains` is a search: each element of `b` is tried against the elements of `a`
  until one contains it, and the attempts that fail count as much as the one that works.
- `jv_object_merge_recursive` descends only where the key holds an object on both sides.
- `delpaths_sorted` groups the sorted paths and descends only as far as the value goes.
- a module already loaded is bound from `lib_state` without being read again, so only the
  chain jq actually descends counts: a diamond of four modules is three levels deep, not
  four.

`src/compat.rs` has the models and the unit tests, `src/compat/depth.rs` the traversals,
`tests/jq_compat/corpus/compat_mode.toml` the differential cases, and
`tests/compat_mode.rs` tests that size their depths from `ulimit -s`.

The sites are independent, which is observable: at the default limit on macOS a value
nested between 65,328 and 130,655 levels deep kills jq (and qj) while it is compared and
not while it is freed, so `$a == $b` dies where `$a | length` answers.

The whole emulation is checked against the jq binary the same way it was measured: for
every shape that drives one of the recursions — 24 of them, from `==` to `bsearch` to
`del` to a chain of `import`s — the deepest value each tool survives is bisected at several
stack limits, and
qj's is never above jq's, and never more than the margin's worth of levels below it (29 to
98 at 256 KB and 1 MB on macOS, 43 to 218 at 1, 8 and 16 MB on Linux). Below the
threshold the two answer the same thing, and shapes where jq's traversal stops early agree
at every depth.

## Numbers

jq 1.8.1 is built with decNumber, and qj follows its number model exactly:

- A number literal, in the input or in the program, keeps its exact decimal value.
  It prints in canonical form: `100000000000000000001` and `1.10` print as written,
  `1e2` prints as `1E+2`, `2.5E-3` as `0.0025`, and `9E999999999` (beyond f64's range)
  as `9E+999999999`.
- Literals compare exactly: `13911860366432393 == 13911860366432392` is `false`.
- Arithmetic is IEEE 754 double precision, as in jq: `13911860366432393 - 10` is
  `13911860366432382`. Results print in jq's shortest round-trip form: `0.1 + 0.2` is
  `0.30000000000000004`, and `1 * 1e20` is `1e+20`.
- `have_decnum` and `have_literal_numbers` are `true`.

**`QJ_JQ_COMPAT` no longer has anything to do with numbers.** qj used to compute with
i64 and f64 and needed the variable to imitate jq's precision; jq's number model is now
the only one qj has, in every mode. The variable was then a no-op for a while, and now
means "be exactly jq" — see [Being exactly jq](#being-exactly-jq). `QJ_CORE=old`, which
selected the old evaluator, is gone and ignored.

## qj's additions

qj adds a few things jq doesn't have, so they aren't part of the comparison. Each is off
under `QJ_JQ_COMPAT=1`, where the whole command line behaves as jq's does:

- `--threads N` and `--jsonl`
- transparent decompression of `.gz` and `.zst` inputs, chosen by file extension
- glob expansion of input file names that don't exist, such as `'logs/*.json.gz'`

A file name that exists, or a pattern that matches nothing, behaves exactly as in jq.

<!-- AUTO-GENERATED BELOW — do not edit below this line -->

## Feature compatibility matrix

Status: **Y** = all tests pass, **~** = partial, **N** = none pass

### Advanced def

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| Destructuring bind | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Inner def scoping | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| def-based assignment | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Filter-param vs $-param equivalence | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Array functions

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| length | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| reverse | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| sort | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| sort_by | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| group_by | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| unique | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| unique_by | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| min/max | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| min_by/max_by | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| add | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| flatten | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| transpose | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| contains | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| inside | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| index/rindex | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| indices | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| first/last | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| first(f)/last(f) | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| nth | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| range | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| any/all | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| map | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| map_values | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| select | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| limit | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| until | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| while | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| repeat | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| recurse | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| isempty | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| walk | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| combinations | 2 | **2/2 Y** | 2/2 Y | 0/2 N | 2/2 Y |
| bsearch | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| pick | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |

### Assignment operators

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| Update assignment | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Arithmetic assignment | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Alternative assignment | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Plain assignment | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |

### Bignum

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| Large integer precision | 2 | **2/2 Y** | 2/2 Y | 1/2 ~ | 1/2 ~ |
| Large integer arithmetic | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 0/1 N |

### CLI flags

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| Compact output | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Raw output (-r/--raw-output0) | 3 | **3/3 Y** | 3/3 Y | 1/3 ~ | 3/3 Y |
| Null input | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Exit status | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Indentation (--tab/--indent) | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Slurp | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Sort keys | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 0/1 N |
| Raw input | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Join output | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| --arg/--argjson | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Color control (-C/-M) | 3 | **3/3 Y** | 3/3 Y | 1/3 ~ | 1/3 ~ |
| --stream | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| --stream-errors | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 0/1 N |
| --seq | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 0/1 N |
| ASCII output | 2 | **2/2 Y** | 2/2 Y | 0/2 N | 0/2 N |
| --slurpfile | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| --rawfile | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| --args/--jsonargs | 4 | **4/4 Y** | 4/4 Y | 2/4 ~ | 4/4 Y |
| --from-file | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |

### Control flow

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| if-then-else-end | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| elif | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| try | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| try-catch | 2 | **2/2 Y** | 2/2 Y | 1/2 ~ | 2/2 Y |
| Variable binding | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| reduce | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| foreach | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| def | 4 | **4/4 Y** | 4/4 Y | 4/4 Y | 4/4 Y |
| def with args | 4 | **4/4 Y** | 4/4 Y | 4/4 Y | 4/4 Y |
| label-break | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| empty | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| error | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |

### Date/time

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| todate | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| fromdate | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| strftime | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| strptime | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| gmtime/mktime | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| now | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Format strings

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| @base64 | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| @uri | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| @csv | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| @tsv | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| @html | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| @sh | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| @json | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| @text | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### I/O and environment

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| env | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| debug | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| builtins | 2 | **2/2 Y** | 2/2 Y | 0/2 N | 2/2 Y |
| halt_error | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| input/inputs | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Math functions

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| floor/ceil/round | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| sqrt | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| fabs | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| exp/exp2 | 2 | **2/2 Y** | 2/2 Y | 1/2 ~ | 2/2 Y |
| log/log2/log10 | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| pow | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| sin/cos/tan | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| asin/acos/atan | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| atan2 | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| sinh/cosh/tanh | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| nan/infinite | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| isnan/isinfinite/isfinite/isnormal | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| logb | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| significand | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| nearbyint/rint | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| remainder/hypot | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |

### Modules

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| import | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| include | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Object functions

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| keys | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| keys_unsorted | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 0/1 N |
| values (iterate) | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| values (type selector) | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| has | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| in | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| to_entries | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| from_entries | 2 | **2/2 Y** | 2/2 Y | 1/2 ~ | 2/2 Y |
| with_entries | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| del | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| paths | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| path | 2 | **2/2 Y** | 2/2 Y | 0/2 N | 2/2 Y |
| leaf_paths | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| getpath | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| setpath | 2 | **2/2 Y** | 2/2 Y | 0/2 N | 2/2 Y |
| delpaths | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |

### Operators

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| Addition | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| Subtraction | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Multiplication | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Division | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Modulo | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Equality | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Comparison | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| and/or | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| not | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Alternative operator | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Pipe | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Comma | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| Unary negation | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### SQL-style operators

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| IN | 3 | **3/3 Y** | 3/3 Y | 0/3 N | 3/3 Y |
| INDEX | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| GROUP_BY | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Streaming

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| tostream | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| fromstream | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |

### String functions

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| tostring | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| tonumber | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| split | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| join | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| ltrimstr/rtrimstr | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| startswith/endswith | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| ascii_upcase/ascii_downcase | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| explode/implode | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| tojson/fromjson | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| utf8bytelength | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| String interpolation | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| test | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| match | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| capture | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| sub/gsub | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| scan | 1 | **1/1 Y** | 1/1 Y | 0/1 N | 1/1 Y |
| splits | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Type selectors

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| arrays | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| objects | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| numbers | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| strings | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| booleans | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| nulls | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| values (selector) | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| scalars | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |
| iterables | 1 | **1/1 Y** | 1/1 Y | 1/1 Y | 1/1 Y |

### Types and basic filters

| Feature | Tests | **qj** | jq | jaq | gojq |
|---------|------:|-----:|-----:|-----:|-----:|
| Identity | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Field access | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Optional field | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Array index | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Array/string slice | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |
| Iterator | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Recursive descent | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| Type literals | 2 | **2/2 Y** | 2/2 Y | 2/2 Y | 2/2 Y |
| type | 3 | **3/3 Y** | 3/3 Y | 3/3 Y | 3/3 Y |

## Summary

| Tool | Y | ~ | N | Score |
|------|--:|--:|--:|------:|
| **qj** | **181** | **0** | **0** | **100.0%** |
| jq | 181 | 0 | 0 | 100.0% |
| jaq | 155 | 7 | 19 | 87.6% |
| gojq | 173 | 2 | 6 | 96.1% |

Score = (Y + 0.5 × ~) / total × 100
