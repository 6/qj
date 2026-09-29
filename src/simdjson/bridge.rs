//! Safe wrapper over the bridge: simdjson's DOM parser, whose tape `src/io`
//! turns into jq values (`crate::io::simd`), and simdjson's padding rule.

use super::ffi::*;

/// simdjson couldn't allocate a parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocError;

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed to create simdjson tape parser")
    }
}

impl std::error::Error for AllocError {}

/// The number of bytes simdjson may read past the end of a text
/// (`SIMDJSON_PADDING`). Their contents don't matter.
pub fn padding() -> usize {
    // SAFETY: returns a constant.
    unsafe { jx_simdjson_padding() }
}

/// A copy of `data` followed by [`padding()`] zero bytes.
pub fn pad_buffer(data: &[u8]) -> Vec<u8> {
    let pad = padding();
    let mut buf = Vec::with_capacity(data.len() + pad);
    buf.extend_from_slice(data);
    buf.resize(data.len() + pad, 0);
    buf
}

/// simdjson error codes that [`TapeParser::parse`] callers distinguish
/// (`simdjson::error_code`).
pub mod tape_error {
    /// Document larger than the parser supports (4 GiB).
    pub const CAPACITY: i32 = 1;
    /// Structural error: mismatched brackets, missing separators, or trailing
    /// content after the document.
    pub const TAPE_ERROR: i32 = 3;
    /// Nesting deeper than the parser's maximum depth (1024).
    pub const DEPTH_ERROR: i32 = 4;
    /// Unterminated string.
    pub const UNCLOSED_STRING: i32 = 15;
    /// The document ends inside an array or object.
    pub const INCOMPLETE_ARRAY_OR_OBJECT: i32 = 28;
}

/// A reusable simdjson DOM parser that exposes its tape after each parse.
///
/// Not `Sync`: each thread uses its own parser.
pub struct TapeParser {
    ptr: *mut JxTapeParser,
}

// SAFETY: the parser is an independent heap object with no thread affinity;
// `&mut self` on every call keeps it single-threaded.
unsafe impl Send for TapeParser {}

/// A parsed document, borrowed from its [`TapeParser`]: simdjson's tape
/// (`dom::document::tape`), its string buffer, and the stage-1 structural
/// indexes (offsets of every structural character and of the first byte of
/// every scalar, relative to the start of the parsed text).
pub struct Tape<'a> {
    /// The tape, from the opening root word through the closing root word.
    pub words: &'a [u64],
    /// Structural indexes, in document order.
    pub structurals: &'a [u32],
    strings: *const u8,
}

impl<'a> Tape<'a> {
    /// The bytes of the string whose tape word has payload `offset`.
    ///
    /// # Safety
    /// `offset` must be the payload of a string word of this tape.
    #[inline]
    pub unsafe fn string(&self, offset: usize) -> &'a [u8] {
        // SAFETY: simdjson stores each string as a native-endian u32 length
        // followed by the bytes, at the offset recorded in its tape word.
        unsafe {
            let p = self.strings.add(offset);
            let len = u32::from_ne_bytes(std::ptr::read_unaligned(p.cast::<[u8; 4]>())) as usize;
            std::slice::from_raw_parts(p.add(4), len)
        }
    }
}

impl TapeParser {
    pub fn new() -> Result<Self, AllocError> {
        // SAFETY: plain constructor; null means allocation failure.
        let ptr = unsafe { jx_tape_parser_new() };
        if ptr.is_null() {
            return Err(AllocError);
        }
        Ok(Self { ptr })
    }

    /// Parses `padded[..len]` as exactly one JSON document.
    ///
    /// `padded` must extend at least [`padding()`] bytes beyond `len` (their
    /// contents don't matter), and the text must not start with a UTF-8 BOM
    /// (simdjson skips one; jq only skips it at the start of its input).
    /// Returns the simdjson error code on failure (see [`tape_error`]).
    pub fn parse(&mut self, padded: &[u8], len: usize) -> std::result::Result<Tape<'_>, i32> {
        assert!(
            padded.len() >= len + padding(),
            "buffer must include SIMDJSON_PADDING extra bytes"
        );
        debug_assert!(!padded[..len].starts_with(b"\xEF\xBB\xBF"));
        let mut tape: *const u64 = std::ptr::null();
        let mut strings: *const u8 = std::ptr::null();
        let mut structurals: *const u32 = std::ptr::null();
        let mut n_structurals: usize = 0;
        // SAFETY: `padded` holds len + SIMDJSON_PADDING readable bytes
        // (asserted above); the out-parameters are valid for writes.
        let code = unsafe {
            jx_tape_parse(
                self.ptr,
                padded.as_ptr(),
                len,
                &mut tape,
                &mut strings,
                &mut structurals,
                &mut n_structurals,
            )
        };
        if code != 0 {
            return Err(code);
        }
        // SAFETY: on success the tape starts with a root word whose payload
        // is the tape's length (the index just past the closing root word:
        // simdjson's `visit_document_end`), and the structural array has
        // `n_structurals` entries. Both live in the parser, which the
        // returned value borrows.
        unsafe {
            let root = *tape;
            let words = (root & 0x00FF_FFFF_FFFF_FFFF) as usize;
            Ok(Tape {
                words: std::slice::from_raw_parts(tape, words),
                structurals: std::slice::from_raw_parts(structurals, n_structurals),
                strings,
            })
        }
    }

    /// The document size the parser's buffers are currently allocated for.
    pub fn capacity(&self) -> usize {
        // SAFETY: self.ptr is a live parser.
        unsafe { jx_tape_parser_capacity(self.ptr) }
    }
}

impl Drop for TapeParser {
    fn drop(&mut self) {
        // SAFETY: self.ptr came from jx_tape_parser_new and is freed once.
        unsafe { jx_tape_parser_free(self.ptr) };
    }
}
