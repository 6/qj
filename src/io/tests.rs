use super::simd::SimdParser;
use crate::jq::value::{Value, parse_sized};

/// Strict structural identity: same kinds, same number literal text and
/// double bits, same string bytes, same key order.
pub(crate) fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => {
            x.literal() == y.literal()
                && x.is_literal() == y.is_literal()
                && x.value().to_bits() == y.value().to_bits()
        }
        (Value::String(x), Value::String(y)) => x.as_bytes() == y.as_bytes(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same(v1, v2))
        }
        _ => false,
    }
}

fn simd(text: &str) -> Option<Value> {
    SimdParser::new().parse(text.as_bytes(), 0, text.len()).ok()
}

#[test]
fn simd_matches_jq_parser_on_valid_json() {
    let docs = [
        r#"{"a":1,"b":[true,false,null],"c":"x"}"#,
        r#"[1.50, 1e2, -0, 0.0, 18446744073709551615, 1E-7, 12.34e5, 1e308]"#,
        r#"{"a":1,"a":2,"b":3,"a":4}"#,
        r#"{"k":{"k":{"k":[[],{},[{}]]}}}"#,
        r#""just a string""#,
        r#""esc \" \\ \/ \b \f \n \r \t \u00e9 \ud83d\ude00 \u0000 end""#,
        r#"[" leading", "trailing ", "", "\u2028"]"#,
        r#"-1.0e-400"#,
        r#"-9223372036854775808"#,
        r#"true"#,
        r#"null"#,
        "[\n  1,\n  2\n]",
        r#"{"":""}"#,
        r#"{"a":[1,{"b":[2,{"c":3}]}],"d":{"e":{"f":null}}}"#,
    ];
    for d in docs {
        let got = simd(d).unwrap_or_else(|| panic!("simdjson rejected {d}"));
        let want = parse_sized(d.as_bytes()).unwrap();
        assert!(same(&got, &want), "{d}: {got:?} vs {want:?}");
    }
}

#[test]
fn simd_rejects_what_jq_treats_specially() {
    for d in [
        "nan",
        "[NaN]",
        "[Infinity]",
        "01",
        "[1,2,]",
        "{\"a\":1,}",
        "\"\\ud800\"",
        "\"\\udc00x\"",
        "\"a\tb\"",
        "\"\u{0}\"",
        "[1] [2]",
        "{\"a\":1}{\"b\":2}",
        "[1e400]",
        // Beyond 64-bit integers: simdjson's DOM rejects them (jq keeps the
        // literal), so these take jq's parser.
        "100000000000000000001",
        "18446744073709551616",
        "-9223372036854775809",
        "[+1]",
        "[.5]",
        "tru",
        "'a'",
    ] {
        assert!(simd(d).is_none(), "simdjson accepted {d:?}");
    }
    // Invalid UTF-8.
    let bad = b"[\"\xff\"]";
    assert!(SimdParser::new().parse(bad, 0, bad.len()).is_err());
    // Nesting beyond simdjson's 1024 levels.
    let deep = format!("{}{}", "[".repeat(1100), "]".repeat(1100));
    assert!(simd(&deep).is_none());
    let ok = format!("{}{}", "[".repeat(1000), "]".repeat(1000));
    let got = simd(&ok).unwrap();
    assert!(same(&got, &parse_sized(ok.as_bytes()).unwrap()));
}

/// Synthetic GH-Archive-like NDJSON (deterministic).
pub(crate) fn synthetic_ndjson(records: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut x: u64 = 0x2545F4914F6CDD1D;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for i in 0..records {
        let r = rnd();
        let line = format!(
            r#"{{"id":"{}","type":"PushEvent","actor":{{"id":{},"login":"user{}","display_login":"user{}","gravatar_id":"","url":"https://api.github.com/users/user{}","avatar_url":"https://avatars.githubusercontent.com/u/{}?"}},"repo":{{"id":{},"name":"org{}/repo{}","url":"https://api.github.com/repos/org/repo"}},"payload":{{"push_id":{},"size":{},"distinct_size":1,"ref":"refs/heads/main","head":"{:016x}{:016x}","commits":[{{"sha":"{:016x}","author":{{"email":"a{}@example.com","name":"Author {}"}},"message":"Fix the thing\nwith a newline and \"quotes\" and unicode é","distinct":true,"url":"https://api.github.com/repos/x/y/commits/abc"}}]}},"public":{},"created_at":"2024-01-01T00:00:{:02}Z","score":{}.{:02}}}"#,
            20000000000u64 + i as u64,
            r % 100000000,
            r % 1000,
            r % 1000,
            r % 1000,
            r % 100000,
            r % 1000000,
            r % 50,
            r % 70,
            r % 10000000000,
            r % 20,
            r,
            r.rotate_left(17),
            r.rotate_left(29),
            r % 997,
            r % 991,
            r % 2 == 0,
            i % 60,
            r % 100,
            r % 100,
        );
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }
    out
}

#[test]
#[ignore]
fn throughput_sanity() {
    use crate::jq::value::{ParseFlags, Parser};
    use std::time::Instant;
    let data = synthetic_ndjson(100_000);
    let mb = data.len() as f64 / 1e6;
    // jq's parser port over the whole buffer.
    let t = Instant::now();
    let mut p = Parser::new(ParseFlags::default());
    p.set_buf(&data, false);
    let mut n = 0;
    while let Some(Ok(_)) = p.next() {
        n += 1;
    }
    let dt = t.elapsed().as_secs_f64();
    eprintln!("jq parser: {n} values, {:.0} MB/s", mb / dt);
    // simdjson per line.
    let t = Instant::now();
    let mut s = SimdParser::new();
    let mut n = 0;
    let mut start = 0;
    for nl in memchr::memchr_iter(b'\n', &data) {
        let _v = s.parse(&data, start, nl).unwrap();
        n += 1;
        start = nl + 1;
    }
    let dt = t.elapsed().as_secs_f64();
    eprintln!("simdjson: {n} values, {:.0} MB/s", mb / dt);
    // simdjson parse only (no value building).
    let t = Instant::now();
    let mut tp = crate::simdjson::TapeParser::new().unwrap();
    let mut words = 0;
    let mut start = 0;
    let mut padded = data.clone();
    padded.resize(data.len() + 64, 0);
    for nl in memchr::memchr_iter(b'\n', &data) {
        words += tp.parse(&padded[start..], nl - start).unwrap().words.len();
        start = nl + 1;
    }
    let dt = t.elapsed().as_secs_f64();
    eprintln!("simdjson parse only: {words} words, {:.0} MB/s", mb / dt);
    // Allocation-only baseline: the strings and numbers of a record.
    let t = Instant::now();
    let mut start = 0;
    let mut n = 0;
    for nl in memchr::memchr_iter(b'\n', &data) {
        let v = parse_sized(&data[start..nl]).unwrap();
        n += v.to_json().len();
        start = nl + 1;
    }
    let dt = t.elapsed().as_secs_f64();
    eprintln!("jq parser per line + dump: {n} bytes, {:.0} MB/s", mb / dt);
}

#[test]
fn simd_parses_slices_with_and_without_padding() {
    let buf = br#"xx{"a":[1,2.50]}yy"#;
    let mut p = SimdParser::new();
    // Too little padding after the slice: copied internally.
    let v = p.parse(buf, 2, buf.len() - 2).unwrap();
    assert_eq!(v.to_json(), r#"{"a":[1,2.50]}"#);
    // Plenty of padding.
    let mut big = buf.to_vec();
    big.extend_from_slice(&[b'9'; 100]);
    let v = p.parse(&big, 2, buf.len() - 2).unwrap();
    assert_eq!(v.to_json(), r#"{"a":[1,2.50]}"#);
}
