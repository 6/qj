# Known limitations

qj runs a port of jq 1.8.1 (see [JQ_PORT_PLAN.md](JQ_PORT_PLAN.md)), so its behavior is
jq's: what differs is listed in [COMPATIBILITY.md](COMPATIBILITY.md) under exemptions
(help and version text, the `qj:` name in messages, jq's own nondeterminism). This page
lists the rest.

## Platforms

qj builds for macOS and Linux (glibc), where every check runs, and for Linux with musl (as
on Alpine), FreeBSD, NetBSD and Windows, which CI builds and tests. The Platform
conformance workflow (run by the Release check) runs jq_diff on musl, FreeBSD and NetBSD
against jq 1.8.1 built there, and the Windows job runs its `.test` cases against jq's
Windows release binary.

On musl, FreeBSD and NetBSD, everything matches jq except two things. First, where stdout
and stderr go to the same file, the order of large outputs between them (when qj's stdout
buffer goes out) differs: on musl it depends on the size of each of jq's writes, which qj
doesn't model, and on FreeBSD on a few buffer-boundary cases. Second, compat mode's
stack-overflow depths, whose models were measured on macOS and glibc Linux only (see
[COMPATIBILITY.md](COMPATIBILITY.md)).

### Windows

On Windows, qj is built for MSVC, whose C runtime (UCRT) is the one jq's Windows release
binary uses, and does what that binary does where it is cheap to: standard input, output
and error and the files jq reads are in text mode (`\r\n` out; `\r\n` in, and Ctrl-Z
ends the input), `-b` puts the standard streams in binary mode, output to a console goes
out as UTF-16 with colors only where the console takes them, the time builtins are the C
runtime's (with jq's own `strptime`), messages carry the C runtime's `errno` text, and
`~` is `%USERPROFILE%` when `HOME` isn't set (`src/os.rs`). CI compares qj with
`jq-windows-amd64.exe` byte for byte on cases that lean on those
(`.github/windows_smoke.sh`), but the conformance suites don't run there, so other
differences can exist. Known ones:

- `QJ_JQ_COMPAT=1` isn't supported: its models are of jq's Unix builds, so qj refuses
  to run.
- jq.exe reads `$ENV`, `env` and the variables it uses (`HOME`, `TZ`) in the ANSI code
  page, which mangles non-ASCII values; qj reads them as Unicode.
- jq.exe dies on recursion deep enough to exhaust its 2 MB stack; qj reserves 256 MB.
- Input files aren't memory-mapped (they are read in text mode), so large files are read
  as streams.

## Where the fast paths don't apply

qj's speed comes from its input layer (`src/io`): simdjson parses each text, memory-mapped
files avoid copies, and independent records run on worker threads with ordered output.
Programs themselves run on the ported VM, at jq-like speed. The old core's NDJSON fast
paths, which answered common filters from raw bytes, were removed with it; any fast path
that comes back must first be proven equivalent to jq (see JQ_PORT_PLAN.md).

- **Sequential programs.** Records are processed in parallel only when they are
  independent. `-n`, `-s`, `-R`, `--seq`, `--stream` and `--debug-trace` run sequentially,
  as do programs that use `input`/`inputs`, `halt`, `debug`/`stderr`, `now`, local time,
  `$__loc__`, labels or modules (the full list is `SEQUENTIAL_BUILTINS` and
  `parallel_plan` in `src/cli/run.rs`). Their output is the same; only the speedup is
  smaller.
- **jq's parser.** simdjson accepts strict JSON only. Anything else (`nan`, invalid UTF-8,
  lone surrogates, numbers beyond 64-bit integers or double range, nesting deeper than
  1024, malformed text) goes through the port of jq's parser, which produces jq's values
  and error messages, more slowly. So do `--seq` and `--stream`.
- **Documents over 4 GiB.** simdjson can't parse a single text larger than 4 GiB, so such
  a document is parsed by jq's parser port: correct, but slower. NDJSON is unaffected,
  since each line is its own text.

## Memory

Like jq, qj builds the whole value of each input text in memory. Regular files are
memory-mapped, and the reader gives back the pages of a file that nothing can read anymore
as it goes (`src/io/source.rs`). So for input of many texts (NDJSON, or texts one after
another), the input resident at a time is about the parallel engine's window
(`QJ_WINDOW_SIZE`, threads × 8 MB, at most 128 MB) plus an 8 MB release step, whatever the
file's size. The window's jobs also hold their output until it's written in order, and, for
programs run on the VM, the values parsed ahead. On the 3.4 GB GH Archive NDJSON file with
18 threads (final M5 Max runs), peak RSS is about 80–90 MB for `.actor.login` and
`select(.type == "PushEvent")`, and up to about 210 MB for `-c .`, pretty output and
`select(.actor.login | test("bot"))`, with output to a file or `/dev/null`. Output into a
pipe that's read slower than qj writes raises those peaks to about 360–410 MB, because
finished jobs wait in memory. With `--threads 1` it's 13–18 MB. (jq: 6 MB.)

It isn't bounded that way for:

- **A single large text** (one big document, or a huge line): it's resident whole while it's
  read, since its value needs all of it.
- **Large texts one after another, the first more than 1/1024 of the file:** the reader tries
  the rest of the file as one document first (a large input often is one), which reads all
  of it.
- **`QJ_NO_RELEASE=1`**, which keeps mapped input resident (for A/B checks).

Streams (pipes, `.gz`/`.zst` files, and files with `QJ_NO_MMAP=1`) are read into a buffer that
holds the unfinished text and, in the parallel engine, the window: bounded, as before.

## qj's additions

`--threads`, transparent `.gz`/`.zst` decompression and glob expansion are qj's own. None
of them changes output relative to jq. Two options of the old core are still accepted and
do nothing: `--jsonl` (jq's input loop reads NDJSON as it is) and `--debug-timing`.
