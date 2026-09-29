//! String builtins defined in `builtin.jq`.
//!
//! ```jq
//! def ascii_downcase:
//!   explode | map( if 65 <= . and . <= 90 then . + 32  else . end) | implode;
//! def ascii_upcase:
//!   explode | map( if 97 <= . and . <= 122 then . - 32  else . end) | implode;
//! ```

use crate::jq::lang::execute::native::Stop;
use crate::jq::value::{Error, Str, Value};

/// `ascii_downcase` (`lo..=hi` is `A..=Z`) or `ascii_upcase` (`a..=z`): `explode`'s
/// error for non-strings, else a new string (`implode` always makes one) with those
/// ASCII letters' case flipped. UTF-8 continuation and lead bytes are never ASCII, so
/// mapping bytes is mapping codepoints.
pub(super) fn ascii_case(input: Value, lo: u8, hi: u8) -> Result<Value, Stop> {
    let Value::String(s) = &input else {
        return Err(Error::msg("explode input must be a string").into());
    };
    let mut bytes = s.as_bytes().to_vec();
    for b in &mut bytes {
        if (lo..=hi).contains(b) {
            *b ^= 0x20;
        }
    }
    let text = String::from_utf8(bytes).expect("ASCII case mapping keeps UTF-8 valid");
    Ok(Value::String(Str::from(text)))
}
