//! Stable 128-bit FNV-1a hashing.
//!
//! Used for cache keys, content-addressed input file names and case
//! fingerprints. Unlike `std`'s `DefaultHasher`, the output never changes
//! between Rust releases, so on-disk caches and committed baselines stay valid.

const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

#[derive(Clone)]
pub struct Fnv128(u128);

impl Default for Fnv128 {
    fn default() -> Self {
        Fnv128(OFFSET)
    }
}

impl Fnv128 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bytes(&mut self, data: &[u8]) -> &mut Self {
        for &b in data {
            self.0 ^= u128::from(b);
            self.0 = self.0.wrapping_mul(PRIME);
        }
        self
    }

    /// Hash a length-prefixed field, so that field boundaries are unambiguous
    /// (`["ab", "c"]` and `["a", "bc"]` hash differently).
    pub fn field(&mut self, data: &[u8]) -> &mut Self {
        self.bytes(&(data.len() as u64).to_le_bytes());
        self.bytes(data)
    }

    pub fn str(&mut self, s: &str) -> &mut Self {
        self.field(s.as_bytes())
    }

    pub fn finish(&self) -> u128 {
        self.0
    }

    pub fn hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}

/// Hash of one byte string.
pub fn digest(data: &[u8]) -> u128 {
    Fnv128::new().bytes(data).finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv128_known_vectors() {
        // Reference values for FNV-1a 128 from the FNV specification.
        assert_eq!(digest(b""), OFFSET);
        assert_eq!(
            format!("{:032x}", digest(b"a")),
            "d228cb696f1a8caf78912b704e4a8964"
        );
    }

    #[test]
    fn fields_are_length_prefixed() {
        let a = Fnv128::new().str("ab").str("c").finish();
        let b = Fnv128::new().str("a").str("bc").finish();
        assert_ne!(a, b);
    }
}
