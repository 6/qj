//! Transparent decompression for gzip and zstd compressed files (a qj
//! extension): which inputs are compressed, by file extension (.gz/.gzip →
//! gzip, .zst/.zstd → zstd). The readers decompress them as streams
//! (`crate::io::source`, and `crate::cli::input` for `QJ_INPUT=util`).

/// Returns true if the file path has a recognized compressed extension.
pub fn is_compressed(path: &str) -> bool {
    path.ends_with(".gz")
        || path.ends_with(".gzip")
        || path.ends_with(".zst")
        || path.ends_with(".zstd")
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
