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
byte, the exit code — or the signal, and whether the kernel dumped a core — and stderr.
jq's own output is the only expectation; the expected-output lines in the test files are
never used.

It keeps two scoreboards over the same cases:

- **qj as it is.** The only rewriting is qj's name in messages (see
  [Exemptions](#exemptions)).
- **`QJ_JQ_COMPAT=1`** ([Being exactly jq](#being-exactly-jq)), given to both tools, both
  started as `argv[0]` = `jq`, and **nothing rewritten at all**: qj's name, help and
  version text are compared like everything else.

Where jq never finishes a case — it runs past the 10 s timeout, the 16 MB output cap or
the 2 GB memory cap — its output up to that point is the expectation: qj matches only if
it is stopped by the same cap, with the same bytes on stdout and stderr. Anything else,
an answer included, is a failure.

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
- **Ratchets**, `tests/jq_compat/diff_baseline.txt` and `diff_baseline_compat.txt`
  (`diff_baseline_linux.txt` and `diff_baseline_compat_linux.txt` on Linux), which fail
  the run when any case gets worse on either scoreboard.

Results against jq 1.8.1 on macOS (arm64, against jq's macOS release binary) and on Linux
(x86-64 with glibc 2.39, against its Linux release binary, which links glibc statically).
Byte-exact means stdout, exit code (or signal and core dump) and stderr all match:

| | Cases | qj as it is | `QJ_JQ_COMPAT=1` |
|---|--:|--:|--:|
| **macOS** total | 39,258 | **39,251** | **39,258 (100%)** |
| &nbsp;&nbsp;jq's own suites | 2,903 | 2,903 | 2,903 |
| &nbsp;&nbsp;qj's corpus | 36,355 | 36,348 | 36,355 |
| **Linux** total | 39,282 | **39,275** | **39,282 (100%)** |
| &nbsp;&nbsp;jq's own suites | 2,903 | 2,903 | 2,903 |
| &nbsp;&nbsp;qj's corpus | 36,379 | 36,372 | 36,379 |

The 7 cases qj's default mode doesn't match are all its own help, version and usage text
(see [Exemptions](#exemptions)); with `QJ_JQ_COMPAT=1` they match too. Each count includes
four cases jq never finishes (`QJ_JQ_COMPAT=1` with a `nan` path element in `delpaths`: jq
grows until the memory cap stops it), which qj matches by growing until the same cap stops
it. The two platforms differ by the cases that run on one of them only, where jq does
something on one platform that it can't do the same way on the other
([jq's undefined behaviour](#jqs-undefined-behaviour)): 67 on Linux
(`corpus/lgamma_glibc.test`) and 43 on macOS (`corpus/debug_trace_macos.toml`). Across
modes, the counts on macOS are 15,483 compact, 6,529 pretty, 6,529 file, 4,542 NDJSON, 19
`%%FAIL`, and 6,156 command-line cases. The command-line cases include some that merge
stdout and stderr into one file or pipe, checking that output and error messages interleave
exactly as jq's stdio buffering interleaves them, and some that start the tool with standard
descriptors closed (`corpus/cli_closed_fds.toml`).

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

The first two are qj's own identity, and only by default: with `QJ_JQ_COMPAT=1` qj says
and prints exactly what jq does, and the compat scoreboard compares it unrewritten.

- **Help and version text.** By default qj has its own output for `-h`/`--help`,
  `-V`/`--version`, `--build-configuration` (and `$JQ_BUILD_CONFIGURATION`), and for the
  usage summary printed after usage errors.
- **qj's name.** By default qj's messages say `qj:` where jq's say `jq:`
  (`qj: error (at <stdin>:0): ...`), and the hint after an option error says
  `Use qj --help ...`. The default scoreboard maps both back before comparing stderr, and
  rewrites nothing else.
- **jq's own nondeterminism.** When jq's output doesn't depend only on its input, there's
  nothing to match: `now` reads the clock, and three of jq's bugs read memory it never
  wrote or had freed, with outcomes that change from run to run or with allocator settings
  that change nothing a program means. [jq's undefined behaviour](#jqs-undefined-behaviour)
  has the evidence, platform by platform, and what qj does.
- **jq's crashes and hangs.** Where jq 1.8.1 crashes or never finishes, the result
  depends on the stack limit rather than on jq's semantics, so by default qj returns a
  sane answer instead. `QJ_JQ_COMPAT=1` reproduces jq's behaviour; see
  [Being exactly jq](#being-exactly-jq).

  | Program | jq 1.8.1 | qj, default | qj, `QJ_JQ_COMPAT=1` |
  |---|---|---|---|
  | `[1] \| delpaths([[nan]])` | hangs, growing ~1.2 GB/s | `[]` | hangs, growing |
  | a value nested deeper than the C stack allows, e.g. `reduce range(200000) as $i (null; [.]) \| length` | SIGSEGV | `1` | SIGSEGV at the same depth |
  | a value or path that drives one of jq's other recursions past its stack: comparing (`==`, `<`, `sort`, `group_by`, `unique`, `min`, `max`, `bsearch`, `-`, `index`), `contains`/`inside`, object `*`, `setpath`/`=`/`\|=`, `delpaths`/`del` | SIGSEGV, at a depth of its own (each recursion has a different frame) | the answer | SIGSEGV at the same depth |
  | a module that imports itself, directly or through others | SIGSEGV | `qj: error: ... imports itself (import cycle)`, exit 3 | SIGSEGV |
  | a deeply nested *program*: 4,990 nested `select(...)` below `ulimit -s` 2 MB, or a long enough left-associative chain (`. + . + …`) at any limit — 37,335 terms at 8 MB | SIGSEGV while compiling | the answer | SIGSEGV at the same nesting |
  | a value printed on a stack of about 80 KB or less, e.g. one nested 242 levels deep at `ulimit -s 64` | SIGSEGV in the printer, before the `MAX_PRINT_DEPTH` cap can stop it | the answer | SIGSEGV at the same depth |
  | a chain of more than about 20,000 imported modules (8 MB stack; 18,000 on Linux) | SIGSEGV | the answer, however long the chain | SIGSEGV at the same depth |
  | `--run-tests --skip` with no count | SIGSEGV (`atoi(NULL)`) | SIGSEGV | SIGSEGV |

  jq's deliberate aborts, which come from `assert()` and are deterministic, are
  reproduced in **both** modes, including macOS stdio flushing the output produced before
  the abort. jq's undefined behaviour is not, because there is nothing deterministic to
  reproduce — see [jq's undefined behaviour](#jqs-undefined-behaviour). One face of it is
  worth naming here: `jv_dels`' double free can make jq **spin forever** (not only crash or
  answer). On macOS, `delpaths([[{"start":1}]])` over the inputs `[1] [1]` prints the first
  input's error and then loops on the second at 98% CPU with its resident size flat — so,
  unlike `delpaths([[nan]])`, a memory cap doesn't stop it, only a timeout. qj reports both
  errors and exits 5. (It never spins on Linux.)

  A reproduced crash is jq's in everything jq_diff compares: the signal, the exit status
  a shell reports (139 for SIGSEGV, 134 for SIGABRT), the output lost with it, and the
  core dump. The kernel decides whether to dump qj's core exactly as it does for jq's (the
  signal, `ulimit -c`, `core_pattern`), so a shell reports `Segmentation fault (core
  dumped)` for both or for neither, and the wait status carries the same flag. What goes
  into the core is qj's own business, because a core of a deliberate crash shows nothing
  wrong: before dying, qj writes `0` to `/proc/self/coredump_filter`, so the core has the
  registers and the list of mappings but no memory. That keeps out the 1 GB of address
  space qj's allocator reserves, which a pipe handler such as systemd-coredump used to
  read in full. On GitHub's Ubuntu runner, which hands every core to systemd-coredump
  whatever `ulimit -c` says, every crash of both tools is `(core dumped)`; qj's take 70
  to 180 ms and are stored as 5 KB (45 KB as files), where jq's take 90 to 900 ms and are
  stored as 1 to 3 MB (0.4 to 74 MB as files).

## Being exactly jq

`QJ_JQ_COMPAT=1` makes qj a drop-in jq 1.8.1, bugs included. It

- reproduces the crashes and hangs in the table above,
- turns off qj's own additions: glob expansion (jq opens the pattern as a file name and
  fails), `.gz`/`.zst` decompression (jq reads the bytes and fails to parse them), and
  `--threads`, `--jsonl` and `--debug-timing` (jq's `Unknown option`, exit 2), and
- is jq by name and text: every message starts with `jq:`, the hint after an option
  error says `Use jq --help`, `-h` and the usage after errors are jq's `usage()`,
  `--version` prints `jq-1.8.1`, and `--build-configuration` (and
  `$JQ_BUILD_CONFIGURATION`) prints the configure line of jq's release binary for the
  platform, e.g. `--host=arm64-apple-darwin23.6.0 --disable-docs ...` on macOS arm64 and
  `--host=x86_64-linux-gnu --disable-docs ...` on Linux x86-64. jq 1.8.1 never prints its
  `argv[0]` — `main.c` spells out `jq` in every message, and uses `argv[0]` only for
  `$ORIGIN` — so neither does qj. The one line of jq's output that carries it comes from
  glibc: `assert()` prints the last component of `argv[0]`, and in compat mode qj's does
  too, from its own.

Parallel processing stays on: it isn't observable. Everything else is unchanged — compat
mode is not a different evaluator, and the default is already jq-exact for every program
that doesn't reach one of jq's own bugs.

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
| `jv_dump_term` (the printer) | the nesting of the value, **capped at `MAX_PRINT_DEPTH` (256)**, below which it writes `<skipped: too deep>` | yes — the cap keeps it inside any ordinary stack, but 257 frames of 256 bytes (304 on Linux) still need about 68 KB, and below that jq dies |
| `load_library` ↔ `process_dependencies` (`linker.c`) | the length of a chain of `import`s or `include`s, one module a level | yes |

The JSON parser is not in the list: `jv_parse` keeps its own stack on the heap and stops
at `MAX_PARSING_DEPTH` (10,000), so input alone cannot drive any of these past 10,000
levels. That is below every threshold at `ulimit -s` 1024 KB or more; at smaller limits
input *can* reach them, and it does, through the same checks (`qj -c length` on 9,000
nested arrays dies in both tools at `ulimit -s 256`).

**Over the program.** Twelve more recursions compile it. Each walks a different part of
the same structure, so each has a depth of its own, and the frames below are what the
prologues of jq's own release binaries reserve (`otool -tV` on macOS, the `.eh_frame` CFA
offsets on Linux — both binaries are stripped, so each function is named by the `assert()`
text or the strings only it uses):

| jq | frame (macOS / Linux) | one level is | first to run out for |
|---|--:|---|---|
| `block_bind_subblock_inner` (`compile.c`) | 112 / 112 | an instruction's closure body or argument list | **everything but nested `def`s** |
| `compile` (`compile.c`) | 176 / 224 | a nested closure, one subfunction | **nested `def`s** |
| `expand_call_arglist` (`compile.c`) | 192 / 224 | a nested argument of a C function, inside `compile` | (it is what `compile`'s deepest level calls) |
| `block_free` / `inst_free` (`compile.c`) | 48 / 48 | an instruction's subblocks | no: half of binding's cost over the same tree |
| `block_mark_referenced` (`compile.c`) | 64 / 48 | the same tree again | no |
| `block_get_unbound_vars` (`compile.c`) | 64 / 64 | a closure body, for `?//` patterns | no |
| `count_cfunctions` (`compile.c`) | 48 / 160 | a closure body, before `compile` | no: `compile` walks the same closures and costs more |
| `bytecode_free` (`bytecode.c`) | 48 / 48 | a subfunction, at teardown | no |
| `dump_disassembly` (`bytecode.c`) | 144 / 96 | a subfunction, for `--debug-dump-disasm` | no |
| `optimize` (`execute.c`) | 64 / 48 | a subfunction, after `compile` | no |
| `ret_follows` (`execute.c`) | — | a `JUMP` in a chain | **not needed**: a tail call both release compilers turn into a loop |
| `jq_format_error` (`execute.c`) | — | an invalid value inside an invalid value | never more than a few deep |

Compat mode models the first three and lets the rest follow: every one of them walks a
tree that binding walks too, at no more than half its cost a level and from a base no
larger, so binding (or, for the closure tree, `compile`) always runs out first. `compile`
and `expand_call_arglist` are one threshold: `compile` calls it at every level, so the
level where jq's stack runs out is inside it.

How deep a program can nest is bounded in turn by bison's parser: at `YYMAXDEPTH` (10,000
parser stack entries) jq stops with `jq: error: memory exhausted`, which is 4,990 nested
`select(...)`, 3,330 nested `def`s, 1,995 nested `def f(g):` or 9,990 nested `[`. The
deepest nesting costs jq 224 bytes a level (two binding frames), so **no program bison
parses this way can overflow a stack of about 2.2 MB or more**.

**A left-associative chain is not bounded that way**, which is the part that matters at an
ordinary stack limit: bison reduces `. + . + …` as it goes, so its parser stack stays
shallow while the program's closures nest one per term (each `+` is
`_plus(lambda; lambda)`). 37,335 terms — a 150 KB program — is the deepest jq compiles at
the default 8 MB stack on this machine, and one more crashes it:

```
python3 -c 'print(" + ".join(["."] * 40000))' > prog.jq && jq -nc -f prog.jq; echo $?
```

Constants fold during the parse, so `[[[…1…]]]` never reaches the compiler at all: what
9,990 nested `[` kills jq in is `jv_free` over the folded constant at the end of the run,
which is the value site above.

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
`(stack_bytes - base - margin) / frame` levels. `frame` is exact — it is the prologue's
own figure, and the deepest value or program jq survives is linear in `ulimit -s` with
that slope at every limit measured. `base` is what is left over at the *worst* of the call
sites measured, and the margin keeps qj from ever surviving where jq dies.

The six recursions over values share one `base` (see below). The other four have their
own: the module chain's level where the stack runs out is *parsing a module*, with bison's
three `YYINITDEPTH` (200) arrays on it, and binding's deepest walks are parser.y's actions,
which run inside `yyparse` — 5,616 bytes of arrays on macOS, 5,696 on Linux — while
`compile` runs after the parse, with less on the stack than any value site.

| site | base (macOS / Linux) | what it holds |
|---|--:|---|
| the six over values | 4,096 / 3,856 | `jv_cmp` from `sort`: the VM and the builtin below it |
| `load_library` (modules) | 12,000 / 8,304 | `find_lib`, the file, and `yyparse`'s arrays for the module |
| `block_bind_subblock_inner` | 8,976 / 7,536 | `load_program` → `jq_parse` → `yyparse` → the action |
| `compile` / `expand_call_arglist` | 3,552 / 1,856 | `jq_compile_args` → `block_compile` |
| `jv_dump_term` | 3,584 / 1,696 | `main`'s output path |

On macOS/arm64, bisected against jq's release binary at `ulimit -s` 1024, 2048, 4096,
8176 and 16384 KB (the compiler's sites at 128, 256, 384, 512, 576, 1024 and 2048 KB, and
the printer at 32, 48 and 64 KB — the frames are large enough that its thresholds need a
tiny stack). The last two columns are the default 8176 KB, except for the sites whose
depth is bounded before that, where they are the limit in brackets:

| site | bytes a level | deepest jq survives | qj, `QJ_JQ_COMPAT=1` |
|---|--:|--:|--:|
| `jv_free` | 64 | 130,760 | 130,655 |
| `jv_equal` / `jv_cmp` | 128 | 65,375 | 65,327 |
| `jv_contains` | 176 | 47,547 | 47,510 |
| `jv_object_merge_recursive` | 112 | 74,719 | 74,659 |
| `jv_setpath` | 144 | 58,114 | 58,067 |
| `delpaths_sorted` | 240 | 34,868 | 34,839 |
| `load_library` (modules) | 416 | 20,096 | 20,079 |
| `block_bind_subblock_inner` (nested `select`, at 256 KB) | 112 | 1,129 | 1,102 |
| `compile` (nested `def`, at 256 KB) | 176 | 1,467 | 1,429 |
| `jv_dump_term` (at 64 KB) | 256 | 241 | 217 |

On Linux/x86-64 (jq's release binary, built by gcc), bisected at 1024, 4096, 8192 and
16384 KB (the module chain also at 2048; the compiler's sites at 256, 512, 1024 and 2048
KB) with the kernel's stack randomization off (`setarch -R`), which is what makes the
numbers repeatable. The last two columns are the 16 MB of GitHub's runners, or the limit
in brackets:

| site | bytes a level | deepest jq survives | qj, `QJ_JQ_COMPAT=1` |
|---|--:|--:|--:|
| `jv_free` | 48 | 349,486 | 349,269 |
| `jv_equal` / `jv_cmp` | 144 | 116,481 | 116,422 |
| `jv_contains` | 176 | 95,312 | 95,254 |
| `jv_object_merge_recursive` | 128 | 131,053 | 130,975 |
| `jv_setpath` | 160 | 104,843 | 104,779 |
| `delpaths_sorted` | 240 | 69,895 | 69,852 |
| `load_library` (modules) | 464 | 36,139 | 36,117 |
| `block_bind_subblock_inner` (nested `select`, at 256 KB) | 112 | 1,136 | 1,099 |
| `compile` (nested `def`, at 256 KB) | 224 | 1,160 | 1,113 |
| `jv_dump_term` (at 64 KB) | 304 | 209 | 181 |

The margin is 6 KB of stack on macOS and 8¼ KB on Linux, which is most of why qj's
threshold is 13 to 223 levels below jq's. What it covers is different on each:

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
  `jv_object_merge_recursive` frees the value it replaces, `load_library` parses and binds
  a module at every level, and `compile` expands a call list at every level. qj charges the
  inner recursion the stack the outer one holds, so the same value kills it sooner the
  deeper the paths go — two paths ending in a value 64,000 levels deep are compared fine at
  the top and kill jq (and qj) a thousand `delpaths` levels down. For that to work the
  margin has to be charged once, not once per recursion, so the six value sites all reserve
  what the *worst* of them measured (`jv_cmp` from `sort`) rather than their own, and where
  an inner site has a base of its own only the larger of the two is charged. That costs the
  other value sites up to 576 bytes on macOS and 2,000 on Linux — 9 and 42 levels of
  `jv_free`. A site that drives others also gives up as many of its own levels as the
  deepest thing it drives needs to start, which is one level everywhere except the module
  chain on Linux, where binding's base is 976 bytes above the chain's and it costs three.
  Without that, comparing two *shallow* path elements at the deepest `delpaths` level the
  model allows would look like an overflow, and qj would die where jq is nowhere near its
  stack.
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

The compiler's sites follow jq's walks in the same way, which is not always the source's
nesting:

- binding recurses into an instruction's closure body *and* its argument list, so a level
  of `select(select(…))` or of a binop chain costs two frames, not one.
- an instruction whose `any_unbound` is 0 is skipped, and so is everything under it — which
  is why nested `def`s, each bound to itself as it is parsed, never drive binding at all
  and are `compile`'s threshold instead of binding's.
- a lambda is bound to itself too, which binds nothing but is where jq's deepest binding
  walk happens while parsing. qj skips that walk by default (it is what keeps a long chain
  of lambdas linear rather than quadratic, as jq's is) and does it in compat mode.
- jq's actions run as bison reduces, so a program that fails to parse has *already* been
  bound as far as the parser got: jq crashes there instead of reporting the syntax error.
  qj parses first and lowers afterwards, so in compat mode it replays the lowering of
  everything the parser reduced (`ParseHooks::replay_reduced`) to reach the same depth.
- the printer stops at `MAX_PRINT_DEPTH`, so past 257 levels the depth stops growing and a
  value of any depth prints on a stack that holds the cap.

`src/compat.rs` has the models and the unit tests, `src/compat/depth.rs` the traversals,
`tests/jq_compat/corpus/compat_mode.toml` the differential cases, and
`tests/compat_mode.rs` tests that size their depths from `ulimit -s`.

The sites are independent, which is observable: at the default limit on macOS a value
nested between 65,328 and 130,655 levels deep kills jq (and qj) while it is compared and
not while it is freed, so `$a == $b` dies where `$a | length` answers.

The whole emulation is checked against the jq binary the same way it was measured: for
every shape that drives one of the recursions — 24 of them over values, from `==` to
`bsearch` to `del` to a chain of `import`s, and 18 over programs, from nested `select` to
a binop chain to nested `def`s to a syntax error after deep nesting — the deepest value or
program each tool survives is bisected at several stack limits, and qj's is never above
jq's, and never more than the margin's worth of levels below it (13 to 106 at 256 KB and
1 MB on macOS, 43 to 223 at 256 KB and at 1, 8 and 16 MB on Linux). Below the
threshold the two answer the same thing, and shapes where jq's traversal stops early agree
at every depth. Over 53 program shapes at nine depths each, at `ulimit -s` 256 KB, 1 MB
and 8 MB, there is no shape where the two disagree about whether the program compiles.

### Small stacks

jq itself needs very little: `jq -n 1` runs at `ulimit -s` **17 KB** on macOS/arm64 and
**12 KB** on Linux/x86-64 (in jq_diff's five-variable environment; argv and the environment
sit on the stack too). Below that it dies of `SIGSEGV` with nothing on stdout, the same way
in every run — 8 runs at each of 8, 12, 14 and 16 KB, no variation.

qj needs a little more of its own: 17 KB on macOS and 16 KB on Linux by default, 19 and 18
KB in compat mode. Between jq's floor and qj's, qj crashes where jq answers. The same is
true of a few programs further up, where qj's *own* recursion (not a model of jq's) is what
runs out: on Linux jq compiles 60 nested regex groups at 52 KB and qj needs 72, and
`reduce range(100) as $i (0;[.]) | tojson` needs 17 KB in jq and 33 in qj on macOS. These
are qj's own frames in Rust and in its Oniguruma; there is nothing of jq's to model, and at
these limits nothing else runs either.

Otherwise, at every limit from 20 KB up, a corpus of runtime code with big frames but
bounded recursion behaves the same in both: decNumber and dtoa (`9E999999999`, `1e308 *
10`), `vfprintf` through `error` and `@text`, Oniguruma's compiler and matcher (nested
groups, backtracking, character classes, `gsub`), glibc's `qsort`/`msort` and its `alloca`
(`sort`, `group_by`, `unique` over 20,000 elements), `strptime`/`strftime`/`strflocaltime`,
`@base64d`, `tojson`/`fromjson`, `@csv`/`@uri`/`@sh`, `--stream`, `-s` and `-R`. The
printer is the one that does overflow, and compat mode models it (above); the JSON parser
keeps its stack on the heap, so deep *input* only reaches `jv_free`, which is modelled too.

## jq's undefined behaviour

Three bugs in jq 1.8.1 read memory that jq never wrote, or had already freed:

- **`jv_dels`' double free.** Deleting a slice of an array whose bounds aren't numbers
  (`delpaths([[{}]])`, `{"start":1}` without an `"end"`, ...) fails with `Array/string
  slice indices must be integers`, but on the way out `jv_dels` frees the key a second
  time: `parse_slice` had consumed it already. When the key is a program constant, the
  constant then points at freed memory, which jq frees again at exit, or reads when a
  later input reaches it.
- **`--debug-trace=all` with an empty data stack.** For an instruction that runs with
  nothing on the data stack, such as the `BACKTRACK` after `[.[] | . * 2]`'s `APPEND`, the
  trace reads `*stack_block_next(&jq->stk, 0)`: the 4 bytes at the top of the data stack's
  allocation, which nothing ever writes. 0 ends the line; anything else is followed as an
  offset into the stack, and what is there is printed as a value.
- **`lgamma_r`'s sign.** `LIBM_DA(lgamma_r)` hands libm the address of an `int` it never
  initializes, and prints what libm leaves there as the sign.

Whether jq's outcome is a function of the program and its input at all was settled per
platform with a corpus of 821 programs that reach one of the three (369, 264 and 188 of
them). They vary the containers and their sizes, constants against values computed at run
time, bound variables and `--argjson`, the other paths deleted alongside, what runs before
and after, several inputs, the output options, and for the trace the size of the first
frame. Each ran against jq's release binary under settings that change nothing a program
means: 5 times as it is; with the environment 1, 4 and 16 KB larger; with ASLR off
(`setarch -R` on Linux, `posix_spawn` with `_POSIX_SPAWN_DISABLE_ASLR` on macOS); and
under allocator settings: `MallocNanoZone=0`, `MallocScribble=1` and
`MallocGuardEdges=1` on macOS, `MALLOC_PERTURB_=85` and `170` and
`GLIBC_TUNABLES=glibc.malloc.tcache_count=0` on glibc. That is 22 runs a program on macOS
and 21 on Linux. It was run on two macOS machines (macOS 27 on an M5 Max, and GitHub's
macos-26 runner) and the Linux runner (GitHub's Ubuntu 24.04, x86-64), with the same
conclusions on each; the numbers below are from the M5 Max and the Ubuntu runner:

| | macOS | Linux |
|---|---|---|
| the double free (369) | **changes from run to run** for 7 programs on the M5 Max, 31 on the macos-26 runner; each of the other settings changes a handful. On a second input, the freed key decides the outcome: exit 0, SIGABRT, SIGSEGV, or a flat spin — deterministic per program | the same in every run, environment and ASLR setting; **`tcache_count=0` changes 128** |
| the empty-stack trace (264, of which 166 read the word) | the same in every run, environment, ASLR, `MallocNanoZone=0` and `MallocGuardEdges=1` setting: the word is 0 in all 166; **`MallocScribble=1` kills all 166** | the same in every run, environment and ASLR setting: the word is 0 in 89, and 77 die (74 of SIGSEGV, 3 of an `assert()`); **`MALLOC_PERTURB_` kills the 89 too, `tcache_count=0` changes 41** |
| `lgamma_r`'s sign (188) | **changes from run to run** in all 183 that reach it | defined after all: glibc writes the sign for every input, the same under every setting |

So there is jq behaviour to conform to in two places, and qj conforms to both:

- **`lgamma_r` on Linux.** qj calls glibc's `lgamma_r` as jq does, and matches jq in all
  188 programs. They're jq_diff cases now, on Linux (`corpus/lgamma_glibc.test`).
- **The trace on macOS, as macOS runs it.** The memory jq reads comes to it zeroed, so its
  trace is the one qj prints, which reads 0: qj matches jq in all 264 programs, and 43 that
  read the word are jq_diff cases now, on macOS (`corpus/debug_trace_macos.toml`). But
  this is jq's allocator speaking, not jq: `MallocScribble=1`, which fills new
  allocations with `0xAA`, kills every program that reads the word. (That is also how the
  166 were told from the 98 that never read it.)

Everywhere else jq doesn't conform to itself, and qj gives the answer jq gives when the
memory happens to be harmless: the double free reports its error and carries on, the trace
reads 0, and `lgamma_r`'s sign is what libm writes, or 0 where Apple's libm writes nothing.
Reproducing more would mean reproducing jq's heap — glibc's tcache and bins, or
libmalloc's zones, fed exactly jq's allocations — to know what a freed or never-written
block holds, and even that would be wrong for anyone who runs jq with one of the settings
above. In detail:

**macOS.** The sign is the low half of a heap pointer, which moves with every run, with
ASLR off too:

```
$ for i in 1 2 3; do jq -nc '[0, -0, nan, infinite, -infinite] | map(lgamma_r)'; done
[[1.7976931348623157e+308,52549124],[1.7976931348623157e+308,52549124],[null,52549124],...
[[1.7976931348623157e+308,58480132],[1.7976931348623157e+308,58480132],[null,58480132],...
[[1.7976931348623157e+308,87954948],[1.7976931348623157e+308,87954948],[null,87954948],...
```

`jq -nc '[1] | try delpaths([[{}]]) catch .'`, run 100 times, prints the error every time,
then exits 0 in 51 runs and dies of SIGSEGV in 49. With `[1,2]` all 100 runs die, with
`[1,2,3]` none; `[1,2,3,4] | try delpaths([[{}]]) catch .` exits 0 in 200 runs out of 200,
which is why it stays a jq_diff case (`corpus/compat_mode.toml`). With two inputs, the
second runs into the freed constant, and what it does then is decided by what is in that
memory — deterministic per program, and a third outcome on top of "exits 0" and "SIGSEGV":
`delpaths([[{"start":1}],[0]])` over `[1] {}` prints the first input's error and then
aborts on the second in all 100 runs (`Assertion failed: (JVP_HAS_KIND(a, JV_KIND_STRING)),
function jvp_string_ptr`), while `delpaths([[{"start":1}]])` over `[1] [1]` prints the
first error and then **spins forever** on the second (98% CPU, resident size flat — not the
growing hang of `delpaths([[nan]])`, so only a timeout stops it, not a memory cap), in every
one of 22 runs and on file input as well as stdin. qj reports both inputs' errors and exits 5.

`jq -c --debug-trace=all '[.[] | . * 2]' <<< '[1,2]'` prints the trace and exits 0; with
`MallocScribble=1` it dies of SIGSEGV, having printed nothing.

**Linux** (`jq` is `jq-linux-amd64`). Here the double free is the same from run to run:

```
$ jq -nc '[] | try delpaths([[{}]]) catch .'; echo "exit $?"
"Array/string slice indices must be integers"
exit 0
$ GLIBC_TUNABLES=glibc.malloc.tcache_count=0 jq -nc '[] | try delpaths([[{}]]) catch .'; echo "exit $?"
"Array/string slice indices must be integers"
corrupted double-linked list
exit 134
```

With glibc's per-thread cache on, the freed key goes into it, and jq's second release only
decrements what is now the cache's bookkeeping; with it off, glibc's consistency checks find
the damage. The setting turns 90 of the 369 programs from exit 0 into `corrupted
double-linked list`, 8 into `malloc(): unsorted double linked list corrupted`, and others
into assertion failures. Under the default settings, every program with one input exits
the way qj does, and so does `delpaths([[{"start":1}],[0]])` over `[1] {}` — both errors,
exit 5 — which aborts on macOS; 12 programs whose second input reaches the freed key abort
(`jvp_object_get_slot: Assertion ... failed`, e.g. `delpaths([[{"start":1}]])` over
`[1] [1]`), where qj reports both errors.

`jq -c --debug-trace=all '[.[]]' <<< '[1,2]'` dies of SIGSEGV in every run, with ASLR on
or off; with `tcache_count=0` it prints the trace and exits 0, while
`[.[] | select(. > 1)]` does the opposite. With `MALLOC_PERTURB_=85`,
`jq -nc --debug-trace=all '[range(10)] | [.[]]'` goes from exit 0 to SIGSEGV.

`ub_corpus.py`, `ub_run.py` and `ub_analyze.py` on the `ci/ex-identity-debug` branch
(`.github/ci-debug/ex/`) generate the corpus, run it under every setting and summarize it.

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
