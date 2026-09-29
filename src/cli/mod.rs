//! qj's command line.
//!
//! - [`args`]: a port of jq 1.8.1's `main.c` argument handling: the option
//!   loop, its diagnostics and exit codes, and the option-dependent setup that
//!   follows it (output flags, colors, the default program, `-f` files).
//! - [`usage`]: qj's own help, version and build-configuration text, which
//!   `docs/JQ_PORT_PLAN.md` exempts from comparison with jq.
//! - [`run`]: the rest of `main.c` on the ported core (`QJ_CORE=port`):
//!   compilation, the `process()` loop, output and exit codes.
//! - [`input`]: `util.c`'s input reader (fgets chunks into jq's parser).

pub mod args;
pub mod input;
pub mod run;
pub mod run_tests;
pub mod usage;
