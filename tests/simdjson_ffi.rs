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

// --- Batch field extraction (dom_find_fields_raw) ---

#[test]
fn batch_extract_basic() {
    use qj::simdjson::dom_find_fields_raw;
    let json = br#"{"type":"PushEvent","id":1,"actor":{"login":"alice"}}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[&["type"], &["id"], &["actor", "login"]];
    let results = dom_find_fields_raw(&buf, json.len(), chains).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0], b"\"PushEvent\"");
    assert_eq!(results[1], b"1");
    assert_eq!(results[2], b"\"alice\"");
}

#[test]
fn batch_extract_missing_field() {
    use qj::simdjson::dom_find_fields_raw;
    let json = br#"{"type":"PushEvent"}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[&["type"], &["missing"]];
    let results = dom_find_fields_raw(&buf, json.len(), chains).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], b"\"PushEvent\"");
    assert_eq!(results[1], b"null");
}

#[test]
fn batch_extract_empty_chains() {
    use qj::simdjson::dom_find_fields_raw;
    let json = br#"{"a":1}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[];
    let results = dom_find_fields_raw(&buf, json.len(), chains).unwrap();
    assert!(results.is_empty());
}

#[test]
fn batch_extract_complex_values() {
    use qj::simdjson::dom_find_fields_raw;
    let json = br#"{"arr":[1,2,3],"obj":{"nested":true},"str":"hello","num":42.5,"bool":false,"nil":null}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[&["arr"], &["obj"], &["str"], &["num"], &["bool"], &["nil"]];
    let results = dom_find_fields_raw(&buf, json.len(), chains).unwrap();
    assert_eq!(results.len(), 6);
    assert_eq!(results[0], b"[1,2,3]");
    assert_eq!(results[1], b"{\"nested\":true}");
    assert_eq!(results[2], b"\"hello\"");
    assert_eq!(results[3], b"42.5");
    assert_eq!(results[4], b"false");
    assert_eq!(results[5], b"null");
}

#[test]
fn batch_extract_deep_nesting() {
    use qj::simdjson::dom_find_fields_raw;
    let json = br#"{"a":{"b":{"c":{"d":"deep"}}}}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[&["a", "b", "c", "d"], &["a", "b"]];
    let results = dom_find_fields_raw(&buf, json.len(), chains).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0], b"\"deep\"");
    assert_eq!(results[1], b"{\"c\":{\"d\":\"deep\"}}");
}

#[test]
fn batch_extract_single_chain() {
    use qj::simdjson::dom_find_fields_raw;
    let json = br#"{"name":"alice"}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[&["name"]];
    let results = dom_find_fields_raw(&buf, json.len(), chains).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0], b"\"alice\"");
}

// --- Reusable DOM parser (DomParser) ---

#[test]
fn dom_parser_reuse_find_field_raw() {
    use qj::simdjson::DomParser;
    let mut dp = DomParser::new().unwrap();
    for i in 0..100 {
        let json = format!(r#"{{"n": {i}}}"#);
        let buf = pad_buffer(json.as_bytes());
        let out = dp.find_field_raw(&buf, json.len(), &["n"]).unwrap();
        assert_eq!(std::str::from_utf8(&out).unwrap(), i.to_string());
    }
}

#[test]
fn dom_parser_reuse_find_fields_raw() {
    use qj::simdjson::DomParser;
    let mut dp = DomParser::new().unwrap();
    let json = br#"{"type":"PushEvent","id":1,"actor":{"login":"alice"}}"#;
    let buf = pad_buffer(json);
    let chains: &[&[&str]] = &[&["type"], &["id"], &["actor", "login"]];

    // Call twice to confirm reuse works
    for _ in 0..2 {
        let results = dp.find_fields_raw(&buf, json.len(), chains).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0], b"\"PushEvent\"");
        assert_eq!(results[1], b"1");
        assert_eq!(results[2], b"\"alice\"");
    }
}

#[test]
fn dom_parser_reuse_field_length() {
    use qj::simdjson::DomParser;
    let mut dp = DomParser::new().unwrap();
    let json = br#"{"items":[1,2,3]}"#;
    let buf = pad_buffer(json);
    let out = dp
        .field_length(&buf, json.len(), &["items"])
        .unwrap()
        .unwrap();
    assert_eq!(std::str::from_utf8(&out).unwrap(), "3");

    // Reuse with different input
    let json2 = br#"{"data":{"a":1,"b":2,"c":3,"d":4}}"#;
    let buf2 = pad_buffer(json2);
    let out2 = dp
        .field_length(&buf2, json2.len(), &["data"])
        .unwrap()
        .unwrap();
    assert_eq!(std::str::from_utf8(&out2).unwrap(), "4");
}

#[test]
fn dom_parser_reuse_field_keys() {
    use qj::simdjson::DomParser;
    let mut dp = DomParser::new().unwrap();
    let json = br#"{"data":{"b":2,"a":1}}"#;
    let buf = pad_buffer(json);
    let out = dp.field_keys(&buf, json.len(), &["data"]).unwrap().unwrap();
    assert_eq!(std::str::from_utf8(&out).unwrap(), r#"["a","b"]"#);

    // Reuse with different input
    let json2 = br#"{"items":["x","y"]}"#;
    let buf2 = pad_buffer(json2);
    let out2 = dp
        .field_keys(&buf2, json2.len(), &["items"])
        .unwrap()
        .unwrap();
    assert_eq!(std::str::from_utf8(&out2).unwrap(), "[0,1]");
}

#[test]
fn dom_parser_reuse_across_varied_sizes() {
    use qj::simdjson::DomParser;
    let mut dp = DomParser::new().unwrap();

    // Small document
    let small = br#"{"x":1}"#;
    let buf_small = pad_buffer(small);
    let out = dp.find_field_raw(&buf_small, small.len(), &["x"]).unwrap();
    assert_eq!(&out, b"1");

    // Large document (many fields)
    let mut fields = Vec::new();
    for i in 0..100 {
        fields.push(format!(r#""f{i}":{i}"#));
    }
    let large = format!("{{{}}}", fields.join(","));
    let buf_large = pad_buffer(large.as_bytes());
    let out = dp
        .find_field_raw(&buf_large, large.len(), &["f50"])
        .unwrap();
    assert_eq!(&out, b"50");

    // Back to small document
    let out = dp.find_field_raw(&buf_small, small.len(), &["x"]).unwrap();
    assert_eq!(&out, b"1");
}

#[test]
fn dom_parser_reuse_missing_field() {
    use qj::simdjson::DomParser;
    let mut dp = DomParser::new().unwrap();
    let json = br#"{"name":"alice"}"#;
    let buf = pad_buffer(json);
    let out = dp.find_field_raw(&buf, json.len(), &["missing"]).unwrap();
    assert_eq!(&out, b"null");
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
