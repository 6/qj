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
    and adversarial NDJSON
- **A ratchet**, `tests/jq_compat/diff_baseline.txt`, which fails the run when any case
  gets worse.

Results on macOS (arm64) against jq 1.8.1:

| Cases | Count | Byte-exact (stdout, exit code, stderr) |
|---|--:|--:|
| jq's own suites | 2,903 | **2,903 (100%)** |
| qj's corpus | 17,013 | 17,006 |
| **Total** | **19,916** | **19,909** |

The 7 cases that differ are all qj's own help, version and usage text; see
[Exemptions](#exemptions). Across modes, the counts are 11,244 compact, 2,290 pretty,
2,290 file, 1,455 NDJSON, 19 `%%FAIL`, and 2,618 command-line cases. The command-line cases
include some that merge stdout and stderr into one file or pipe, checking that output and
error messages interleave exactly as jq's stdio buffering interleaves them.

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

CI runs jq_diff on Linux too, but only reports there until a Linux baseline is committed.

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

**`QJ_JQ_COMPAT` is obsolete.** qj used to compute with i64 and f64, and needed
`QJ_JQ_COMPAT=1` to imitate jq's precision. jq's behavior is now the only one: the old
evaluator that read the variable (and `QJ_CORE=old`, which selected it) has been removed,
and qj ignores both.

## qj's additions

qj adds a few things jq doesn't have, so they aren't part of the comparison:

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
