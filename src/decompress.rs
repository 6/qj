//! Transparent decompression for gzip and zstd compressed files.
//!
//! Detects compression by file extension (.gz → gzip, .zst/.zstd → zstd).
//! Decompresses entire file to an in-memory buffer for further processing.

use anyhow::{Context, Result};
use std::io::Read;

/// Returns true if the file path has a recognized compressed extension.
pub fn is_compressed(path: &str) -> bool {
    path.ends_with(".gz")
        || path.ends_with(".gzip")
        || path.ends_with(".zst")
        || path.ends_with(".zstd")
}

/// Decompress a file to bytes based on its extension.
///
/// Panics if called on a file that isn't compressed (use `is_compressed` first).
pub fn decompress_file(path: &str) -> Result<Vec<u8>> {
    if path.ends_with(".gz") || path.ends_with(".gzip") {
        let file =
            std::fs::File::open(path).with_context(|| format!("failed to open file: {path}"))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .with_context(|| format!("failed to decompress gzip file: {path}"))?;
        Ok(buf)
    } else if path.ends_with(".zst") || path.ends_with(".zstd") {
        let file =
            std::fs::File::open(path).with_context(|| format!("failed to open file: {path}"))?;
        let mut decoder = zstd::Decoder::new(file)
            .with_context(|| format!("failed to initialize zstd decoder for: {path}"))?;
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .with_context(|| format!("failed to decompress zstd file: {path}"))?;
        Ok(buf)
    } else {
        unreachable!("decompress_file called on non-compressed file: {path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_gz() {
        assert!(is_compressed("data.json.gz"));
        assert!(is_compressed("data.ndjson.gz"));
        assert!(is_compressed("/path/to/file.gz"));
    }

    #[test]
    fn detect_gzip() {
        assert!(is_compressed("data.json.gzip"));
    }

    #[test]
    fn detect_zst() {
        assert!(is_compressed("data.json.zst"));
        assert!(is_compressed("data.ndjson.zstd"));
    }

    #[test]
    fn detect_uncompressed() {
        assert!(!is_compressed("data.json"));
        assert!(!is_compressed("data.ndjson"));
        assert!(!is_compressed("file.txt"));
    }
}
