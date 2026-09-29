//! Fast input layer for the jq port (work in progress).

pub mod reader;
pub mod simd;
pub mod source;

#[cfg(test)]
mod tests;

pub use reader::{InputReader, ReaderOptions, input_names};
pub use source::{FsOpener, InputMessage, Opened, Opener};
