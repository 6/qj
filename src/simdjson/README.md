# simdjson FFI bridge

## Vendored simdjson

The vendored simdjson single-header amalgamated files live in the top-level `simdjson/` directory:

- **`simdjson/simdjson.h`** and **`simdjson/simdjson.cpp`** — Vendored from the [simdjson](https://github.com/simdjson/simdjson) project, release **v4.2.4** (2025-12-17). These are the single-header amalgamated files from the `singleheader/` directory of the release. **Do not edit these files.**

  Downloaded from:
  - https://raw.githubusercontent.com/simdjson/simdjson/v4.2.4/singleheader/simdjson.h
  - https://raw.githubusercontent.com/simdjson/simdjson/v4.2.4/singleheader/simdjson.cpp

## Bridge files (this directory)

qj uses one part of simdjson: its DOM parser. `src/io/simd.rs` builds jq values straight from the parser's tape, and falls back to jq's parser port for anything simdjson rejects.

- **`bridge.cpp`** — `extern "C"` functions: `jx_simdjson_padding` and a reusable DOM parser (`jx_tape_parser_new`/`_free`/`_capacity`, and `jx_tape_parse`, which hands back the tape, the string buffer and the structural indexes). This file is part of qj.
- **`ffi.rs`** — the matching Rust declarations.
- **`bridge.rs`** — the safe Rust wrapper: `TapeParser`, `Tape`, `tape_error`, `padding`, `pad_buffer`.

`tests/simdjson_ffi.rs` tests the boundary, and `fuzz/fuzz_targets/fuzz_parse.rs` fuzzes it.

## Updating simdjson

To update to a newer simdjson release, replace `simdjson/simdjson.h` and `simdjson/simdjson.cpp` (in the project root) with the corresponding files from the new release's `singleheader/` directory. Then verify the bridge still compiles (`cargo build`) and run `cargo test --test simdjson_ffi` (the tape layout and error codes are simdjson internals).

## License

simdjson is licensed under the Apache License 2.0. See https://github.com/simdjson/simdjson/blob/master/LICENSE for details.
