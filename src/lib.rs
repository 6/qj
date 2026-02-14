pub mod filter;
pub mod output;
pub mod parallel;
pub mod simdjson;
pub mod value;

/// Strip UTF-8 BOM (U+FEFF, bytes EF BB BF) from the beginning of a buffer.
pub fn strip_bom(buf: &mut Vec<u8>) {
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        buf.drain(..3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_bom_present() {
        let mut buf = vec![0xEF, 0xBB, 0xBF, b'"', b'h', b'i', b'"'];
        strip_bom(&mut buf);
        assert_eq!(buf, b"\"hi\"");
    }

    #[test]
    fn strip_bom_absent() {
        let mut buf = b"\"hi\"".to_vec();
        strip_bom(&mut buf);
        assert_eq!(buf, b"\"hi\"");
    }

    #[test]
    fn strip_bom_empty() {
        let mut buf = Vec::new();
        strip_bom(&mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn strip_bom_only_bom() {
        let mut buf = vec![0xEF, 0xBB, 0xBF];
        strip_bom(&mut buf);
        assert!(buf.is_empty());
    }
}
