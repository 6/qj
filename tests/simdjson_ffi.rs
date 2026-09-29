//! The simdjson bridge's FFI boundary: `padding`, `pad_buffer` and
//! `TapeParser` (simdjson's DOM parser, whose tape src/io turns into jq
//! values; `src/io/tests/simd.rs` checks those values against jq's parser).

use qj::simdjson::{Tape, TapeParser, pad_buffer, padding, tape_error};

const PAYLOAD: u64 = 0x00FF_FFFF_FFFF_FFFF;

fn tag(word: u64) -> u8 {
    (word >> 56) as u8
}

/// The type tag of every tape word (a number's second word holds its bits,
/// so its "tag" is arbitrary).
fn tags(tape: &Tape<'_>) -> Vec<u8> {
    tape.words.iter().map(|&w| tag(w)).collect()
}

/// simdjson's error code for `json`, which must be rejected.
fn parse_error(json: &[u8]) -> i32 {
    let buf = pad_buffer(json);
    match TapeParser::new().unwrap().parse(&buf, json.len()) {
        Ok(_) => panic!("simdjson accepted {:?}", String::from_utf8_lossy(json)),
        Err(code) => code,
    }
}

#[test]
fn pad_buffer_appends_zeroed_padding() {
    assert!(padding() > 0);
    let buf = pad_buffer(b"[1]");
    assert_eq!(buf.len(), 3 + padding());
    assert_eq!(&buf[..3], b"[1]");
    assert!(buf[3..].iter().all(|&b| b == 0));
}

/// The tape runs from the opening root word through the closing one:
/// simdjson's root payload is the tape's length, not the closing word's index
/// (`TapeParser::parse` used to include one word past the end).
#[test]
fn tape_ends_with_the_closing_root_word() {
    let json = br#"{"a":[1,2.5,"x"],"b":null}"#;
    let buf = pad_buffer(json);
    let mut parser = TapeParser::new().unwrap();
    let tape = parser.parse(&buf, json.len()).unwrap();
    let tags = tags(&tape);
    // r { "a" [ l <1> d <2.5> "x" ] "b" n } r (the words after l and d
    // hold their numbers)
    assert_eq!(tags.len(), 14);
    assert_eq!(tags[..5], *b"r{\"[l");
    assert_eq!(tags[6], b'd');
    assert_eq!(tags[8..], *b"\"]\"n}r");
    assert_eq!(tape.words[13] & PAYLOAD, 0, "points back at the root");
}

#[test]
fn scalars_and_numbers() {
    let mut parser = TapeParser::new().unwrap();
    for (json, want) in [
        ("null", b'n'),
        ("true", b't'),
        ("false", b'f'),
        (" \"s\"", b'"'),
    ] {
        let buf = pad_buffer(json.as_bytes());
        let tape = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(tags(&tape), [b'r', want, b'r'], "{json}");
    }
    // Numbers take two words: the tag, then the value's bits.
    for (json, want, bits) in [
        ("-5", b'l', (-5i64) as u64),
        ("9223372036854775807", b'l', i64::MAX as u64),
        ("18446744073709551615", b'u', u64::MAX),
        ("2.5", b'd', 2.5f64.to_bits()),
        ("1e2", b'd', 100f64.to_bits()),
        // An integer zero: jq's -0 comes from the literal text.
        ("-0", b'l', 0),
    ] {
        let buf = pad_buffer(json.as_bytes());
        let tape = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(tape.words.len(), 4, "{json}");
        assert_eq!(tag(tape.words[1]), want, "{json}");
        assert_eq!(tape.words[2], bits, "{json}");
        // src/io reads the literal from the text at its structural index.
        assert_eq!(tape.structurals, [0], "{json}");
    }
}

#[test]
fn strings_are_unescaped_in_the_string_buffer() {
    let json = br#"{"k\u00e9y":"a\"b\\c\n\u00e9\ud83d\ude00","":""}"#;
    let buf = pad_buffer(json);
    let mut parser = TapeParser::new().unwrap();
    let tape = parser.parse(&buf, json.len()).unwrap();
    let strings: Vec<&[u8]> = tape
        .words
        .iter()
        .filter(|&&w| tag(w) == b'"')
        // SAFETY: the payload of a string word is its string's offset.
        .map(|&w| unsafe { tape.string((w & PAYLOAD) as usize) })
        .collect();
    let want: [&[u8]; 4] = ["kéy".as_bytes(), "a\"b\\c\né😀".as_bytes(), b"", b""];
    assert_eq!(strings, want);
}

#[test]
fn structurals_index_the_text() {
    let json = br#" {"a" : [1, true]} "#;
    let buf = pad_buffer(json);
    let mut parser = TapeParser::new().unwrap();
    let tape = parser.parse(&buf, json.len()).unwrap();
    let chars: Vec<u8> = tape.structurals.iter().map(|&p| json[p as usize]).collect();
    assert_eq!(chars, b"{\":[1,t]}");
}

#[test]
fn errors_are_simdjson_codes() {
    // Not exactly one text: the reader then looks for the text's end.
    assert_eq!(parse_error(b"[1] [2]"), tape_error::TAPE_ERROR);
    assert_eq!(parse_error(b"{\"a\":1}x"), tape_error::TAPE_ERROR);
    assert_eq!(parse_error(b"[1,2"), tape_error::TAPE_ERROR);
    assert_eq!(parse_error(b"{\"a\":"), tape_error::TAPE_ERROR);
    assert_eq!(parse_error(b"\"abc"), tape_error::UNCLOSED_STRING);
    assert_eq!(parse_error(b"[\"abc"), tape_error::UNCLOSED_STRING);
    let deep = format!("{}{}", "[".repeat(1100), "]".repeat(1100));
    assert_eq!(parse_error(deep.as_bytes()), tape_error::DEPTH_ERROR);
    let ok = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
    let buf = pad_buffer(ok.as_bytes());
    assert!(TapeParser::new().unwrap().parse(&buf, ok.len()).is_ok());
    // Other rejections (jq's parser takes over for all of them).
    for json in [
        &b""[..],
        b"   ",
        b"[\"\xff\"]",
        b"\"\\ud800\"",
        b"[1,]",
        b"01",
        b"nan",
        b"[1e400]",
        b"18446744073709551616",
    ] {
        parse_error(json);
    }
}

#[test]
fn one_parser_reuses_its_buffers_across_sizes() {
    let mut parser = TapeParser::new().unwrap();
    let small = br#"{"x":1}"#.to_vec();
    let big = format!("[{}0]", "1,".repeat(100_000)).into_bytes();
    for json in [&small, &big, &small, &big, &small] {
        let buf = pad_buffer(json);
        let tape = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(tag(tape.words[0]), b'r');
        assert_eq!((tape.words[0] & PAYLOAD) as usize, tape.words.len());
        assert_eq!(tape.words[tape.words.len() - 1], u64::from(b'r') << 56);
    }
    assert!(parser.capacity() >= big.len());
}

#[test]
fn padding_contents_do_not_matter() {
    // The text can be followed by anything, such as the rest of the input.
    let mut buf = b"[1,2]".to_vec();
    buf.extend(std::iter::repeat_n(b'x', padding()));
    let mut parser = TapeParser::new().unwrap();
    let tape = parser.parse(&buf, 5).unwrap();
    assert_eq!(tags(&tape)[..3], *b"r[l");
    assert_eq!(tape.words.len(), 8);
}

#[test]
#[should_panic(expected = "SIMDJSON_PADDING")]
fn parse_requires_padding() {
    let _ = TapeParser::new().unwrap().parse(b"[1]", 3);
}
