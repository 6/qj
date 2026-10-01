# simdjson FFI bridge

## Vendored simdjson

The vendored simdjson single-header amalgamated files live in the top-level `simdjson/` directory:

- **`simdjson/simdjson.h`** and **`simdjson/simdjson.cpp`** — Vendored from the [simdjson](https://github.com/simdjson/simdjson) project, release **v4.2.4** (2025-12-17). These are the single-header amalgamated files from the `singleheader/` directory of the release. **Do not edit these files.**

  Downloaded from:
  - https://raw.githubusercontent.com/simdjson/simdjson/v4.2.4/singleheader/simdjson.h
  - https://raw.githubusercontent.com/simdjson/simdjson/v4.2.4/singleheader/simdjson.cpp

## Bridge files (this directory)

qj uses one part of simdjson: its DOM parser. `src/io/simd.rs` builds jq values straight from the parser's tape, and falls back to jq's parser port for anything simdjson rejects.

- **`bridge.cpp`** — `extern "C"` functions: `jx_simdjson_padding`, a reusable DOM parser (`jx_tape_parser_new`/`_free`/`_capacity`, and `jx_tape_parse`, which hands back the tape, the string buffer and the structural indexes), and simdjson's kernels (`jx_simdjson_implementation_count`/`_implementation`/`_active_implementation`, and `jx_tape_parser_new_implementation`, a parser on a named kernel). This file is part of qj.
- **`ffi.rs`** — the matching Rust declarations.
- **`bridge.rs`** — the safe Rust wrapper: `TapeParser` (also `TapeParser::with_implementation`), `Tape`, `tape_error`, `padding`, `pad_buffer`, and `implementations`, `supported_implementations`, `active_implementation`, `checked_active_implementation`.

## Kernels

simdjson picks a kernel ("implementation") at run time: on x86-64 `icelake`, `haswell`, `westmere` or `fallback`, on ARM `arm64`. `SIMDJSON_FORCE_IMPLEMENTATION=<name>` makes it use that one (unchecked: a name not compiled in makes every parse fail, and a kernel the CPU can't run dies of an illegal instruction; `checked_active_implementation` reports both). On arm64 simdjson compiles only `arm64` in, and ignores the variable; qj's `simdjson-fallback` feature (`build.rs`) adds `fallback` there, for tests and fuzzers.

`tests/simdjson_ffi.rs` tests the boundary, `tests/simdjson_kernels.rs` checks that every kernel the CPU runs parses alike, and `fuzz/fuzz_targets/fuzz_parse.rs` fuzzes both.

## Updating simdjson

To update to a newer simdjson release, replace `simdjson/simdjson.h` and `simdjson/simdjson.cpp` (in the project root) with the corresponding files from the new release's `singleheader/` directory. Then verify the bridge still compiles (`cargo build`) and run `cargo test --test simdjson_ffi` (the tape layout and error codes are simdjson internals).

## License

simdjson is licensed under the Apache License 2.0. See https://github.com/simdjson/simdjson/blob/master/LICENSE for details.
