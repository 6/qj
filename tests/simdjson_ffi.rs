use qj::simdjson::{
    JsonType, Parser, iterate_many_count, iterate_many_extract_field, pad_buffer, padding,
    read_padded,
};
use std::io::Write;

#[test]
fn extract_string_field() {
    let json = br#"{"greeting": "hello world"}"#;
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.find_field_str("greeting").unwrap(), "hello world");
}

#[test]
fn extract_int_field() {
    let json = br#"{"count": 999999}"#;
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.find_field_int64("count").unwrap(), 999999);
}

#[test]
fn extract_negative_int() {
    let json = br#"{"temp": -42}"#;
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.find_field_int64("temp").unwrap(), -42);
}

#[test]
fn extract_double_field() {
    let json = br#"{"rate": 0.001}"#;
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    let val = doc.find_field_double("rate").unwrap();
    assert!((val - 0.001).abs() < 1e-15);
}

#[test]
fn parser_reuse_across_documents() {
    let mut parser = Parser::new().unwrap();

    for i in 0..100 {
        let json = format!(r#"{{"n": {i}}}"#);
        let buf = pad_buffer(json.as_bytes());
        let mut doc = parser.parse(&buf, json.len()).unwrap();
        assert_eq!(doc.find_field_int64("n").unwrap(), i);
    }
}

#[test]
fn invalid_json_error() {
    let json = b"{{{{";
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let result = parser.parse(&buf, json.len());
    if let Ok(mut doc) = result {
        // On-Demand is lazy — accessing content should fail on invalid JSON.
        assert!(doc.doc_type().is_err() || doc.find_field_str("x").is_err());
    }
}

#[test]
fn empty_object() {
    let json = b"{}";
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.doc_type().unwrap(), JsonType::Object);
}

#[test]
fn doc_type_array() {
    let json = b"[1, 2, 3]";
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.doc_type().unwrap(), JsonType::Array);
}

#[test]
fn ndjson_count() {
    let lines: Vec<String> = (0..50).map(|i| format!(r#"{{"id": {i}}}"#)).collect();
    let ndjson = lines.join("\n") + "\n";
    let buf = pad_buffer(ndjson.as_bytes());
    let count = iterate_many_count(&buf, ndjson.len(), 1_000_000).unwrap();
    assert_eq!(count, 50);
}

#[test]
fn ndjson_extract_field() {
    let ndjson = r#"{"name":"a"}
{"name":"bb"}
{"name":"ccc"}
"#;
    let buf = pad_buffer(ndjson.as_bytes());
    let total = iterate_many_extract_field(&buf, ndjson.len(), 1_000_000, "name").unwrap();
    // "a"(1) + "bb"(2) + "ccc"(3) = 6
    assert_eq!(total, 6);
}

#[test]
fn read_padded_with_file() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    let json = br#"{"file": true}"#;
    tmp.write_all(json).unwrap();
    tmp.flush().unwrap();

    let buf = read_padded(tmp.path()).unwrap();
    assert!(buf.len() >= json.len() + padding());
    assert_eq!(&buf[..json.len()], json);
    // Padding bytes should be zero.
    assert!(buf[json.len()..].iter().all(|&b| b == 0));

    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.doc_type().unwrap(), JsonType::Object);
}

#[test]
fn unicode_string() {
    let json = r#"{"emoji": "hello 🌍"}"#;
    let buf = pad_buffer(json.as_bytes());
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.find_field_str("emoji").unwrap(), "hello 🌍");
}

#[test]
fn escaped_string() {
    let json = r#"{"msg": "line1\nline2\ttab"}"#;
    let buf = pad_buffer(json.as_bytes());
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.find_field_str("msg").unwrap(), "line1\nline2\ttab");
}

#[test]
fn large_int() {
    let json = br#"{"big": 9223372036854775807}"#;
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert_eq!(doc.find_field_int64("big").unwrap(), i64::MAX);
}

#[test]
fn missing_field_returns_error() {
    let json = br#"{"a": 1}"#;
    let buf = pad_buffer(json);
    let mut parser = Parser::new().unwrap();
    let mut doc = parser.parse(&buf, json.len()).unwrap();
    assert!(doc.find_field_str("nonexistent").is_err());
}

/// Regression test for fuzz-found crash: malformed NDJSON `{z}:` caused a
/// SIGSEGV inside simdjson's on-demand iterate_many when find_field was called
/// on an object with matching braces but invalid interior content.
/// Fixed by switching iterate_many_extract_field to use the DOM parser which
/// fully validates JSON before field access.
#[test]
fn iterate_many_malformed_ndjson_no_crash() {
    let data: &[u8] = &[123, 122, 125, 58]; // {z}:
    let buf = pad_buffer(data);
    // Both must return without crashing — returning an error is fine.
    let _ = iterate_many_count(&buf, data.len(), 1_000_000);
    let _ = iterate_many_extract_field(&buf, data.len(), 1_000_000, "a");
}
