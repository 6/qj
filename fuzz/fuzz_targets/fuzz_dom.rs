//! simdjson to jq values (`qj::io::simd::SimdParser`) against jq's parser
//! port: whenever simdjson accepts a text, the value built from its tape must
//! be exactly what jq's parser produces (same number literals, string bytes
//! and key order; `qj::io::fuzzing::same`). Texts are parsed with and
//! without padding after them (the parser copies in the second case), and
//! one parser handles every text, so its buffers are reused.
//!
//! `cargo +nightly fuzz run fuzz_dom -s none -- -max_total_time=120`, on the
//! kernel `SIMDJSON_FORCE_IMPLEMENTATION` names (default: the best this CPU
//! runs; the target refuses to start on one it can't).

#![no_main]

use libfuzzer_sys::fuzz_target;
use qj::io::fuzzing::same;
use qj::io::simd::SimdParser;
use qj::jq::value::parse_sized;

fuzz_target!(init: qj::io::fuzzing::init_simdjson_kernel(), |data: &[u8]| {
    let mut simd = SimdParser::new();
    // The whole input, then the pieces between 0xFF bytes (never valid in
    // JSON text), so one input can hold several documents.
    let pieces = std::iter::once(data).chain(data.split(|&b| b == 0xFF));
    for text in pieces {
        // SimdParser's contract: no UTF-8 BOM at the start.
        if text.is_empty() || text.starts_with(b"\xEF\xBB\xBF") {
            continue;
        }
        // Without room for padding after the text, then with plenty.
        let mut padded = text.to_vec();
        padded.resize(text.len() + qj::simdjson::padding(), b' ');
        for buf in [text, &padded[..]] {
            let Ok(got) = simd.parse(buf, 0, text.len()) else {
                continue;
            };
            let want = parse_sized(text)
                .unwrap_or_else(|e| panic!("simdjson accepted {text:?}, jq's parser didn't: {e}"));
            assert!(
                same(&got, &want),
                "{text:?}: simdjson gave {got:?}, jq's parser {want:?}"
            );
        }
    }
});
