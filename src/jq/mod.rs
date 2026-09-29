//! Faithful Rust port of jq 1.8.1's semantic core.
//!
//! See `docs/JQ_PORT_PLAN.md` for the architecture, module ownership, and
//! conformance targets. Ported code is derived from jq (MIT, see `LICENSE-jq`).

pub mod lang;
pub mod platform;
pub mod value;
