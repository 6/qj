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
  | a regex nested deeper than the stack allows Oniguruma's parser or its walks over the parsed pattern, e.g. 60 nested groups at `ulimit -s 48` | SIGSEGV while compiling the regex | the answer | SIGSEGV at the same nesting |
  | a stack too small for jq to start (macOS: 16 KB or less, where dyld itself can't run; Linux: about 5 KB to print its help, 9 KB to compile a program) or for `--run-tests`' buffers (40 KB on macOS, 24 KB on Linux) | SIGSEGV, with nothing written | the answer, wherever its own start-up fits (see [Small stacks](#small-stacks)) | SIGSEGV |
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
| `jv_dump_term` (the printer) | the nesting of the value, **capped at `MAX_PRINT_DEPTH` (256)**, below which it writes `<skipped: too deep>` | yes — the cap keeps it inside any ordinary stack, but 258 frames of 256 bytes (304 on Linux) still need about 68 KB, and below that jq dies. Twice: from `main.c`'s output, and, from deeper in the stack, while the program runs (`tojson`, `tostring`, `@json`, `@text`, string interpolation, `debug`, `stderr`, error messages) |
| `load_library` ↔ `process_dependencies` (`linker.c`) | the length of a chain of `import`s or `include`s, one module a level | yes |

**In Oniguruma**, which jq links in for its regex builtins, two recursions follow a
pattern's nesting: the parser (`prs_alts` → `prs_branch` → `prs_exp` → `prs_bag` → …, a
level per group of any kind) and the walks over the parsed tree (`tune_tree`,
`compile_tree`, …, a level per node). jq's parse depth limit (1,024, two a group) stops
the parser at 511 nested groups, which needs about 400 KB of stack: below that, a nested
enough pattern kills jq. Compat mode follows the pattern as the parser does
(`src/compat/regex.rs`) and reproduces both.

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

jq's stack is `RLIMIT_STACK` as the kernel applies it, less what the kernel puts at its
top for argv and the environment. The kernel rounds the limit *down* to a page on Linux,
where the stack's mapping grows a page at a time while it fits, and *up* to one on
macOS, which makes what lies past the limit inaccessible a page at a time: pages are 16
KB on arm64, so `ulimit -s 17` gives a 32 KB stack there. argv and the environment take
every string with its NUL and the arrays of pointers to them. Compat mode counts both
for its own process (`compat::effective_limit`, `compat::area_of`), which are jq's when
qj runs in its place: a larger environment moves every threshold down by exactly its
size, and a smaller one up.

What is left is linear in every depth jq recurses to: `base + levels × frame`. `frame` is
what the prologue of jq's own function reserves in the release binary, and `base` is what
the stack holds before the first level, from the top: the same at every depth and every
limit, but for one thing — on macOS the allocator takes a slow path at the deepest level
at some depths, which puts up to 600 bytes more on the stack there. Both were measured by
padding the environment at a fixed limit until jq no longer answered, which is
byte-exact and free of macOS's 16 KB rounding, at 16 random depths per site (on Linux with
the kernel's stack randomization off, `setarch -R`). Each model takes the largest base any
depth showed, and qj dies once `base + levels × frame + margin` is more than jq's stack
has.

The margin is what the models can't see. On macOS it is 512 bytes, for jq's executable
path (the kernel puts that on the stack too, in the apple strings) and allocator paths no
measurement met; with the environment counted, the rest is deterministic, and the same
limit, environment and program give jq the same threshold in every run. On Linux it is
those 512 bytes plus the kernel's randomization of the initial stack pointer
(`arch_align_stack`: up to 8,191 bytes, then 16-byte alignment), 8,718 in all: jq's
thresholds move from run to run by up to 8,206 bytes (171 levels of `jv_free`), and the
model sits below the bottom of that window, so qj never survives where jq can die, and
inside the window it dies where jq only sometimes does.

The models, in bytes beyond argv and the environment. "jq's base" is the one at most
depths, with the largest one any depth showed in brackets; the last column is how many
levels short of jq's threshold qj's is, everywhere (on Linux, below the bottom of jq's
window, whose width follows):

macOS/arm64 (jq's release binary, built by Apple clang):

| site | bytes a level | jq's base | the model's base | qj short of jq |
|---|--:|--:|--:|--:|
| `jv_free` | 64 | 3,160 | 3,904 | 20 levels |
| `jv_equal` | 128 | 3,128 | 3,904 | 11 levels |
| `jv_cmp` (`sort`) | 128 | 3,448 | 3,904 | 8 levels |
| `jv_contains` | 176 | 3,336 | 3,904 | 7 levels |
| `jv_object_merge_recursive` | 112 | 3,304 (3,896) | 3,904 | 4–10 levels |
| `jv_setpath` | 144 | 3,192 (3,640) | 3,904 | 5–9 levels |
| `delpaths_sorted` | 240 | 3,192 (3,704) | 3,904 | 2–6 levels |
| `load_library` (modules) | 416 | 11,128 | 11,136 | 2 levels |
| `block_bind_subblock_inner` | 112 | 8,696 | 8,704 | 5 levels |
| `compile` / `expand_call_arglist` | 176 / 192 | 3,176 (3,432) | 3,440 | 2–5 levels |
| `jv_dump_term` from `main.c` | 256 | 2,920 (2,967) | 2,976 | 2–3 levels |
| `jv_dump_term` while the program runs | 256 | 3,288 (4,056) | 4,064 | 2–6 levels |
| Oniguruma's parser (a group) | 784 | 4,840 | 4,848 | 1 level |
| Oniguruma's tree walks (a node) | 624 | 4,392 | 4,400 | 1 level |

Linux/x86-64 (jq's release binary, built by gcc, glibc linked statically):

| site | bytes a level | jq's base | the model's base | qj short of jq |
|---|--:|--:|--:|--:|
| `jv_free` | 48 | 1,548 | 3,152 | 45 levels, + 171 of window |
| `jv_equal` | 144 | 1,548 | 3,152 | 15 levels, + 57 |
| `jv_cmp` (`sort`) | 144 | 3,148 | 3,152 | 4 levels, + 57 |
| `jv_contains` | 176 | 1,676 | 3,152 | 12 levels, + 47 |
| `jv_object_merge_recursive` | 128 | 2,028 | 3,152 | 13 levels, + 65 |
| `jv_setpath` | 160 | 1,692 | 3,152 | 13 levels, + 52 |
| `delpaths_sorted` | 240 | 1,708 | 3,152 | 9 levels, + 35 |
| `load_library` (modules) | 464 | 7,772 | 7,776 | 2 levels, + 18 |
| `block_bind_subblock_inner` | 112 | 7,324 | 7,328 | 5 levels, + 74 |
| `compile` / `expand_call_arglist` | 224 / 224 | 1,692 | 1,696 | 3 levels, + 37 |
| `jv_dump_term` from `main.c` | 304 | 1,404 (1,412) | 1,424 | 2 levels, + 27 |
| `jv_dump_term` while the program runs | 304 | 1,644 (2,172) | 2,176 | 1–4 levels, + 27 |
| Oniguruma's parser (a group) | 784 | 3,596 | 3,600 | 1 level, + 11 |
| Oniguruma's tree walks (a node) | 560 | 2,908 | 2,912 | 1 level, + 15 |

At the default 8,176 KB on macOS, in jq_diff's environment (about 350 bytes of argv and
environment), jq frees a value 130,760 levels deep, and compat-mode qj one 130,740 deep;
the model before the environment was counted stopped at 130,655, because its 6 KB margin
had to cover whatever environment it might be run in. Checked at every depth tried, from a
few hundred levels (tens of KB of stack) to 100,000 (6.4 MB): qj's need is jq's plus the
margin above, never less.

Two smaller reasons the two can't agree to the last frame:

- **One base for the six sites over values.** jq drives some of these recursions from
  *inside* another: `delpaths_sorted` compares path elements and frees values a level at a
  time, `jv_object_merge_recursive` frees the value it replaces, `load_library` parses and
  binds a module at every level, and `compile` expands a call list at every level. qj
  charges the inner recursion the stack the outer one holds, so the same value kills it
  sooner the deeper the paths go — two paths ending in a value 64,000 levels deep are
  compared fine at the top and kill jq (and qj) a thousand `delpaths` levels down. For
  that to work the margin has to be charged once, not once per recursion, so the six value
  sites all reserve the largest base any of them showed (`jv_object_merge_recursive`'s on
  macOS, `jv_cmp`'s from `sort` on Linux), and where an inner site has a base of its own
  only the larger of the two is charged. That is most of the table's last column: it costs
  `jv_free` 12 levels on macOS and 33 on Linux. A site that drives others also gives up as
  many of its own levels as the deepest thing it drives needs to start, which is one level
  everywhere except the module chain on Linux. Without that, comparing two *shallow* path
  elements at the deepest `delpaths` level the model allows would look like an overflow,
  and qj would die where jq is nowhere near its stack.
- **The printer's call sites.** `main.c` prints a result from shallower in jq's stack than
  a builtin that dumps a value while the program runs (`tojson`, `@json`, string
  interpolation, `debug`, `stderr`, error messages), by 368 to 1,136 bytes on macOS and 240
  to 768 on Linux, so the two are separate models (`Site::Print` and `Site::Dump`), the
  latter at the worst of its call sites.

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
`src/compat/regex.rs` Oniguruma's depths, `tests/jq_compat/corpus/compat_mode.toml` the
differential cases, and `tests/compat_mode.rs` tests that size their depths from the stack
limit and from the child's exact argv and environment.

The sites are independent, which is observable: at the default limit on macOS a value
nested between 65,370 and 130,740 levels deep kills jq (and qj) while it is compared and
not while it is freed, so `$a == $b` dies where `$a | length` answers.

The whole emulation is checked against the jq binary the same way it was measured: for
every shape that drives one of the recursions — 24 of them over values, from `==` to
`bsearch` to `del` to a chain of `import`s, and 18 over programs, from nested `select` to
a binop chain to nested `def`s to a syntax error after deep nesting — the deepest value or
program each tool survives is bisected at several stack limits, and qj's is never above
jq's. Below the threshold the two answer the same thing, and shapes where jq's traversal
stops early agree at every depth. Over 53 program shapes at nine depths each, at `ulimit
-s` 256 KB, 1 MB and 8 MB, there is no shape where the two disagree about whether the
program compiles. [Small stacks](#small-stacks) has the check of everything else, down to
8 KB.

### Small stacks

On a small enough stack the question is what a whole run needs, not one recursion, and
three of jq's needs are fixed rather than a depth, in bytes beyond argv and the
environment, measured as the models were:

| | macOS/arm64 | Linux/x86-64 |
|---|--:|--:|
| to start at all, whatever the program (macOS: dyld; Linux: glibc's start-up and `main.c` up to the help text, the version or a usage error) | 19,272 | 5,164–5,172 |
| to compile a program, the builtins included, or report its syntax error | less than dyld's | 9,244–9,460 |
| `--run-tests` for tests that don't nest (its loop keeps three buffers of 4 KB or more on the stack) | 40,106–40,392 | 23,796 |
| what `--run-tests` holds above everything it compiles and runs | 28,496 | 12,648 |

On macOS nothing runs below 17 KB — `ulimit -s 16` kills `/bin/echo` as well — and jq's own
start-up needs less than dyld's; with the rounding to 16 KB pages, `jq -n 1` starts at
every limit from 17 KB up in an environment of up to 13 KB. On Linux jq starts at 8 KB and
compiles from 12 KB (with the randomization off; with it on, 13 runs in 40 answer at 12
KB, 37 at 16 KB, and every one from 20 KB). Compat mode checks the three at the same points
of the run (`compat::starting`, `compat::compiling`, `compat::running_tests`), with the
margin above, and below them dies of `SIGSEGV` with nothing written, as jq does, in every
run.

**qj's own stack.** qj's frames used to be on the main thread's stack too, and at these
limits they ran out where jq's didn't: `qj -n 1` needed 17 KB on macOS and 16 KB on Linux
(19 and 18 in compat mode), a regex nested 60 deep 72 KB on Linux against jq's 52, and a
`tojson` 100 levels deep 33 KB on macOS against jq's 17. Now, below 8 MB (the default on
both), qj moves to a stack of its own before it does anything — 256 MB of address space,
committed as it is touched, still on the main thread (`src/cli/stack.rs`) — so nothing of
qj's own ever runs out, in either mode, and only compat mode's models decide whether a run
dies of the limit. What runs before qj's `main` is the platform's: on macOS dyld, which
needs what jq's needs give or take the length of the executable's path; on Linux the
dynamic loader, which needs 6,226–6,234 bytes, 1 KB more than jq's static start-up needs to
print its help and 3 KB less than anything jq compiles. So with the kernel's randomization
on, `qj -h`, `qj --version`, `qj --build-configuration` or a usage error can crash at 8–12
KB in a run where jq's would have answered; for everything else qj needs less than jq. (A
statically linked qj needs 5,179 bytes to start, jq's own figure but for its path's
length.)

The check is a corpus of 121 programs: every modelled recursion; regexes (nested groups
of every kind, backtracking, classes, `gsub`, `capture`, `scan`, `splits`); `tojson`,
`walk`; dates (`strptime`, `strftime`, `strflocaltime`, `mktime`, `localtime`); `@base64d`
and the other formats; decNumber and dtoa; `vfprintf` through `error` and `@text`; `sort`,
`group_by`, `unique_by`, `min_by` over 20,000 elements; `--stream`, `-s`, `-R`, `--seq`;
deep and wide input; parse errors; modules; `--run-tests`, `-f`, `--args`, `--slurpfile`,
`$__loc__`, `input`, `halt_error`, and every output option. For each program the stack
each tool needs beyond argv and the environment was measured byte-exact, and each tool was
run at every limit from 8 KB to 256 KB (on macOS 8, 16, 32, 48, … 256; on Linux every 4 KB
to 32 KB, then 40, 48, … 256), in jq_diff's environment:

| | macOS | Linux |
|---|---|---|
| limits × programs | 13 × 121 | 20 × 121 |
| compat mode needs less than jq (could survive where jq dies) | never | never |
| compat mode dies where jq answers in every run (a conservative window) | 2 cells: a value 3,000 deep, freed and parsed, at 192 KB | 6 cells: `--run-tests`, freeing 300 levels, printing and dumping 60 to 100, at 24 to 40 KB |
| compat mode dies where jq answers in some runs (the randomization) | — | at every such limit, by design |
| where both answer, a different stdout, stderr or status | never | never |
| qj as it is crashes where jq answers | never, its path's length aside | never with the randomization off; with it on, `-h`, `--version`, `--build-configuration` and a usage error at 8–12 KB (the dynamic loader) |
| qj as it is answers differently from jq | never | never |

Program by program, compat mode needs what jq does plus 528 bytes on macOS where dyld's
floor decides and 528 to 1,472 elsewhere, and on Linux, beyond the randomization's 8,206,
507 to 2,187 bytes: the margin and the shared base of the value sites.

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
