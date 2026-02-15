# Vendored simdjson

## Files

- **`simdjson.h`** and **`simdjson.cpp`** — Vendored from the [simdjson](https://github.com/simdjson/simdjson) project, release **v4.2.4** (2025-12-17). These are the single-header amalgamated files from the `singleheader/` directory of the release. **Do not edit these files.**

  Downloaded from:
  - https://raw.githubusercontent.com/simdjson/simdjson/v4.2.4/singleheader/simdjson.h
  - https://raw.githubusercontent.com/simdjson/simdjson/v4.2.4/singleheader/simdjson.cpp

- **`bridge.cpp`** — Our C-linkage FFI bridge wrapping simdjson's On-Demand API in `extern "C"` functions callable from Rust. This file is part of qj.

- **`bridge.rs`** — Safe Rust wrapper over the FFI functions. This file is part of qj.

## Updating simdjson

To update to a newer simdjson release, replace `simdjson.h` and `simdjson.cpp` with the corresponding files from the new release's `singleheader/` directory. Then verify the bridge still compiles (`cargo build`).

## License

simdjson is licensed under the Apache License 2.0. See https://github.com/simdjson/simdjson/blob/master/LICENSE for details.
