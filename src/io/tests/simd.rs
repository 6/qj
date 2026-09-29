use super::same;
use crate::io::simd::SimdParser;
use crate::jq::value::{Value, parse_sized};

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
