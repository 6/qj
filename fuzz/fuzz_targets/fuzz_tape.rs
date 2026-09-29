//! Programs evaluated on simdjson's tape (src/io/tape_eval.rs) must print
//! exactly what the VM prints on the input's value, in every layout, and
//! must decline whenever the VM raises an error. The first byte picks the
//! program, the second the layout; the rest is the input.
//!
//! `cargo +nightly fuzz run fuzz_tape -s none -- -max_total_time=120`

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    qj::io::fuzzing::check_tape_equivalence(data);
});
