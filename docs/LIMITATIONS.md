# Known limitations

qj runs a port of jq 1.8.1 (see [JQ_PORT_PLAN.md](JQ_PORT_PLAN.md)), so its behavior is
jq's: what differs is listed in [COMPATIBILITY.md](COMPATIBILITY.md) under exemptions
(help and version text, the `qj:` name in messages, jq's own nondeterminism). This page
lists the rest.

## Platforms

qj builds for Unix (macOS and Linux). The CLI, the input layer and the port's platform
layer use Unix APIs (file descriptors, `mmap`, libc's time functions), and Windows isn't
supported.

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
programs run on the VM, the values parsed ahead. On a 3.3 GB NDJSON file with 18 threads,
peak RSS is about 90 MB for `.actor.login`, 130 MB for `select(.type == "PushEvent")`,
160–200 MB for `-c .` and 250–340 MB for `select(.actor.login | test("bot"))`; with
`--threads 1` it's 13–18 MB. (jq: 6 MB.)

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
