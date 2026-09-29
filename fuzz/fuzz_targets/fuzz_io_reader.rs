//! The input layer (src/io) must give the same values, errors,
//! input_filename and input_line_number with and without its simdjson fast
//! path, whole or streamed, and through the parallel engine.
//!
//! `cargo +nightly fuzz run fuzz_io_reader -s none -- -max_total_time=120`

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    qj::io::fuzzing::check_reader_equivalence(data);
});
