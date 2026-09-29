//! Replays fixtures whose expectations come from the jq 1.8.1 binary.
//!
//! The JSON files in `testdata/` are produced by `testdata/gen.py`, which runs
//! the real `jq`; these tests check the port against them without needing jq.
//! Where a fixture exercises a builtin wrapper from `builtin.c` (type checks
//! and their error messages), the wrapper is mirrored here in a few lines.

use super::*;
use serde_json::Value as J;

fn fixture(src: &str) -> Vec<J> {
    let v: J = serde_json::from_str(src).expect("fixture is JSON");
    assert_eq!(v["jq"], "jq-1.8.1", "fixture generated with another jq");
    v["cases"].as_array().expect("cases").clone()
}

fn hex_decode(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).expect("hex"))
        .collect()
}

/// A byte field stored either as text (`key`) or hex (`key_hex`).
fn field_bytes(c: &J, key: &str) -> Vec<u8> {
    if let Some(s) = c.get(key).and_then(J::as_str) {
        return s.as_bytes().to_vec();
    }
    if let Some(h) = c.get(format!("{key}_hex")).and_then(J::as_str) {
        return hex_decode(h);
    }
    panic!("case has no {key}: {c}")
}

fn s<'a>(c: &'a J, key: &str) -> &'a str {
    c[key]
        .as_str()
        .unwrap_or_else(|| panic!("no string {key} in {c}"))
}

/// Parses JSON text with the port's parser (tested on its own below).
fn jv(text: &str) -> Value {
    parse_sized(text.as_bytes()).unwrap_or_else(|e| panic!("bad test JSON {text:?}: {e}"))
}

fn arr(items: Vec<Value>) -> Value {
    Value::from(items)
}

/// `[0, value]` or `[1, error message]`, as the generator's
/// `try (... | [0, .]) catch [1, .]` prints them.
pub(super) fn tagged(r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => arr(vec![Value::from(0), v]).to_json(),
        Err(e) => arr(vec![Value::from(1), e.into_value()]).to_json(),
    }
}

fn show(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn value_size() {
    assert_eq!(std::mem::size_of::<Value>(), 24);
}

#[test]
fn numbers_fixture() {
    let mut failures = Vec::new();
    for c in fixture(include_str!("testdata/numbers.json")) {
        let lit = s(&c, "in");
        let v = jv(lit);
        let n = v.as_number().expect("number").clone();
        let times1 = Value::number(n.value() * 1.0);
        let got = if s(&c, "prog") == "full" {
            // [., . * 1, -., length, tostring]
            arr(vec![
                v.clone(),
                times1,
                Value::Number(n.negate()),
                Value::Number(n.abs()),
                Value::from(v.to_json()),
            ])
        } else {
            arr(vec![v.clone(), times1])
        };
        let got = got.to_json();
        if got != s(&c, "out") {
            failures.push(format!("{lit}: got {got}, jq {}", s(&c, "out")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn dtoa_fixture() {
    let mut failures = Vec::new();
    for c in fixture(include_str!("testdata/dtoa.json")) {
        let input = c[0].as_str().unwrap();
        let want = c[1].as_str().unwrap();
        let n = Number::from_literal(input.as_bytes()).expect("literal");
        let got = Number::from_f64(n.value() * 1.0).to_json_string();
        if got != want {
            failures.push(format!("{input}: got {got}, jq {want}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn compare_fixture() {
    for c in fixture(include_str!("testdata/compare.json")) {
        let a = jv(s(&c, "a"));
        let b = jv(s(&c, "b"));
        let bn = Value::number(b.as_f64().unwrap() * 1.0);
        let got = arr(vec![
            Value::from(a.compare(&b) == std::cmp::Ordering::Less),
            Value::from(a.equal(&b)),
            Value::from(a.compare(&b) == std::cmp::Ordering::Greater),
            Value::from(a.compare(&bn) == std::cmp::Ordering::Less),
            Value::from(a.equal(&bn)),
        ])
        .to_json();
        assert_eq!(got, s(&c, "out"), "compare {} {}", s(&c, "a"), s(&c, "b"));
    }
}

#[test]
fn fromjson_fixture() {
    let mut failures = Vec::new();
    for c in fixture(include_str!("testdata/fromjson.json")) {
        let input = s(&c, "in");
        let got = tagged(parse_sized(input.as_bytes()));
        if got != s(&c, "out") {
            failures.push(format!("{input:?}: got {got}, jq {}", s(&c, "out")));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// How jq's `jq_util_input_read_more` chunks stdin for the parser: `fgets`
/// into a 4096-byte buffer (so a chunk ends after a newline or 4095 bytes),
/// and a chunk without a newline is cut at its first NUL (`strlen`).
pub(super) fn jq_stdin_chunks(input: &[u8]) -> Vec<&[u8]> {
    let mut chunks = Vec::new();
    let mut pos = 0;
    while pos < input.len() {
        let mut end = (pos + 4095).min(input.len());
        if let Some(nl) = input[pos..end].iter().position(|&b| b == b'\n') {
            end = pos + nl + 1;
            chunks.push(&input[pos..end]);
        } else {
            let chunk = &input[pos..end];
            let valid = chunk.iter().position(|&b| b == 0).unwrap_or(chunk.len());
            chunks.push(&chunk[..valid]);
        }
        pos = end;
    }
    chunks
}

/// jq's input loop (`main.c` + `jq_util_input_next_input`) over the given
/// chunks of stdin, printing with `-c`.
pub(super) fn simulate_cli(chunks: &[&[u8]], flags: &[String]) -> (Vec<u8>, Vec<u8>) {
    let has = |f: &str| flags.iter().any(|x| x == f);
    let pf = ParseFlags {
        seq: has("--seq"),
        streaming: has("--stream") || has("--stream-errors"),
        stream_errors: has("--stream-errors"),
    };
    let mut p = Parser::new(pf);
    let mut out = Vec::new();
    let mut err = Vec::new();
    // jq feeds each chunk as partial and finally an empty last buffer.
    let mut bufs: Vec<(&[u8], bool)> = chunks.iter().map(|c| (*c, true)).collect();
    bufs.push((b"", false));
    'feed: for (buf, partial) in bufs {
        p.set_buf(buf, partial);
        loop {
            match p.next() {
                Some(Ok(v)) => {
                    if pf.seq {
                        out.push(0x1e);
                    }
                    print::dump_to_vec(&v, &DumpOptions::compact(), &mut out);
                    out.push(b'\n');
                }
                Some(Err(e)) => {
                    if !pf.seq {
                        err.extend_from_slice(format!("jq: parse error: {e}\n").as_bytes());
                        break 'feed;
                    }
                    err.extend_from_slice(format!("jq: ignoring parse error: {e}\n").as_bytes());
                }
                None => {
                    if p.remaining() == 0 {
                        break;
                    }
                }
            }
        }
    }
    (out, err)
}

#[test]
fn parse_cli_fixture() {
    let mut failures = Vec::new();
    for c in fixture(include_str!("testdata/parse_cli.json")) {
        let input = field_bytes(&c, "in");
        let want_out = field_bytes(&c, "out");
        let want_err = field_bytes(&c, "err");
        let flags: Vec<String> = c["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().to_owned())
            .collect();
        // jq's own chunking must reproduce the CLI exactly. jq's parser only
        // depends on chunk boundaries after an error (it discards the rest
        // of the buffer) or with a malformed BOM in --seq mode, so for inputs
        // that parse cleanly other chunkings must give the same result.
        let mut splits: Vec<Vec<&[u8]>> = vec![jq_stdin_chunks(&input)];
        let clean = want_err.is_empty()
            && !flags.iter().any(|f| f == "--stream-errors")
            && !input.contains(&0)
            && input.first() != Some(&0xEF);
        if clean {
            splits.push(vec![&input[..]]);
            if input.len() <= 64 {
                splits.push(input.chunks(1).collect());
            }
        }
        for (i, chunks) in splits.iter().enumerate() {
            let (out, err) = simulate_cli(chunks, &flags);
            if out != want_out || err != want_err {
                failures.push(format!(
                    "{flags:?} {:?} (split {i}):\n  got out {:?} err {:?}\n  jq  out {:?} err {:?}",
                    show(&input[..input.len().min(80)]),
                    show(&out[..out.len().min(300)]),
                    show(&err),
                    show(&want_out[..want_out.len().min(300)]),
                    show(&want_err)
                ));
                break;
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// jq's option handling for the output flags used by the print fixture.
fn dump_options(flags: &[String], env: Option<&str>) -> (DumpOptions, String) {
    let mut opts = DumpOptions::pretty();
    let mut color = false;
    let mut i = 0;
    while i < flags.len() {
        match flags[i].as_str() {
            "-c" => opts.indent = Indent::Compact,
            "--tab" => opts.indent = Indent::Tab,
            "--indent" => {
                i += 1;
                opts.indent = DumpOptions::with_indent(flags[i].parse().unwrap()).indent;
            }
            "-S" => opts.sort_keys = true,
            "-a" => opts.ascii = true,
            "-C" => color = true,
            f => panic!("unhandled flag {f}"),
        }
        i += 1;
    }
    let mut err = String::new();
    let colors = match env {
        None => Colors::default(),
        Some(spec) => Colors::parse(spec).unwrap_or_else(|| {
            err.push_str("Failed to set $JQ_COLORS\n");
            Colors::default()
        }),
    };
    if color {
        opts.colors = Some(colors);
    }
    (opts, err)
}

#[test]
fn print_fixture() {
    let mut failures = Vec::new();
    for c in fixture(include_str!("testdata/print.json")) {
        let input = field_bytes(&c, "in");
        let flags: Vec<String> = c["flags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f.as_str().unwrap().to_owned())
            .collect();
        let (opts, err) = dump_options(&flags, c.get("env").and_then(J::as_str));
        let v = parse_sized(&input).expect("print fixture input parses");
        let mut out = Vec::new();
        dump(&v, &opts, &mut out).unwrap();
        out.push(b'\n');
        let want_out = field_bytes(&c, "out");
        let want_err = field_bytes(&c, "err");
        if out != want_out || err.as_bytes() != want_err {
            failures.push(format!(
                "{flags:?} env {:?} {:?}:\n  got {:?} {err:?}\n  jq  {:?} {:?}",
                c.get("env"),
                show(&input[..input.len().min(60)]),
                show(&out[..out.len().min(400)]),
                show(&want_out[..want_out.len().min(400)]),
                show(&want_err)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// builtin.c wrappers used by the ops fixture -----------------------------

fn f_length(a: &Value) -> Result<Value, Error> {
    Ok(match a {
        Value::Array(x) => Value::from(x.len()),
        Value::Object(x) => Value::from(x.len()),
        Value::String(x) => Value::from(x.codepoint_len()),
        Value::Number(n) => Value::Number(n.abs()),
        Value::Null => Value::from(0),
        _ => return Err(Error::type_error(a, "has no length")),
    })
}

fn binop_plus(a: Value, b: Value) -> Result<Value, Error> {
    Ok(match (a, b) {
        (Value::Null, b) => b,
        (a, Value::Null) => a,
        (Value::Number(x), Value::Number(y)) => Value::number(x.value() + y.value()),
        (Value::String(mut x), Value::String(y)) => {
            x.concat(&y);
            Value::String(x)
        }
        (Value::Array(mut x), Value::Array(y)) => {
            x.extend_from_array(&y);
            Value::Array(x)
        }
        (Value::Object(mut x), Value::Object(y)) => {
            x.merge(&y);
            Value::Object(x)
        }
        (a, b) => return Err(Error::type_error2(&a, &b, "cannot be added")),
    })
}

fn binop_multiply(a: Value, b: Value) -> Result<Value, Error> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::number(x.value() * y.value())),
        (Value::String(s), Value::Number(n)) | (Value::Number(n), Value::String(s)) => {
            let d = n.value();
            let n = if d < 0.0 || d.is_nan() {
                -1
            } else if d > i32::MAX as f64 {
                i32::MAX
            } else {
                d as i32
            };
            s.repeat(n)
        }
        (Value::Object(mut x), Value::Object(y)) => {
            x.merge_recursive(&y);
            Ok(Value::Object(x))
        }
        (a, b) => Err(Error::type_error2(&a, &b, "cannot be multiplied")),
    }
}

fn by_impl(a: &Value, b: &Value, f: fn(&Array, &Array) -> Array) -> Result<Value, Error> {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => Ok(Value::Array(f(x, y))),
        _ => Err(Error::type_error2(
            a,
            b,
            "cannot be sorted, as they are not both arrays",
        )),
    }
}

pub(super) fn run_op(op: &str, a: Value, b: Value, c: Value) -> Result<Value, Error> {
    use std::cmp::Ordering::*;
    match op {
        "get" => a.get(&b),
        "set" => a.setpath(&arr(vec![b]), c),
        "has" => a.has(&b).map(Value::from),
        "getpath" => a.getpath(&b),
        "setpath" => a.setpath(&b, c),
        "delpaths" => a.delpaths(&b),
        "keys" => a.keys(),
        "keys_unsorted" => a.keys_unsorted(),
        "sort" | "unique" => match &a {
            Value::Array(x) => Ok(Value::Array(if op == "sort" {
                sort(x, x)
            } else {
                unique(x, x)
            })),
            _ => Err(Error::type_error(
                &a,
                "cannot be sorted, as it is not an array",
            )),
        },
        "sort_by_impl" => by_impl(&a, &b, sort),
        "group_by_impl" => by_impl(&a, &b, group),
        "unique_by_impl" => by_impl(&a, &b, unique),
        "contains" => {
            if a.kind() == b.kind() {
                Ok(Value::from(a.contains(&b)))
            } else {
                Err(Error::type_error2(
                    &a,
                    &b,
                    "cannot have their containment checked",
                ))
            }
        }
        "cmp" => {
            let r = a.compare(&b);
            Ok(arr(vec![
                Value::from(r == Less),
                Value::from(a.equal(&b)),
                Value::from(r == Greater),
                Value::from(r != Greater),
                Value::from(r != Less),
            ]))
        }
        "tojson" => Ok(Value::from(a.to_json())),
        "tostring" => Ok(match a {
            Value::String(_) => a,
            _ => Value::from(a.to_json()),
        }),
        "negate" => match &a {
            Value::Number(n) => Ok(Value::Number(n.negate())),
            _ => Err(Error::type_error(&a, "cannot be negated")),
        },
        "path" => {
            if a.identical(&Value::Null) {
                Ok(Value::empty_array())
            } else {
                Err(Error::msg(format!(
                    "Invalid path expression with result {}",
                    dump_string_trunc(&a, 30)
                )))
            }
        }
        "split" => match (&a, &b) {
            (Value::String(x), Value::String(y)) => Ok(Value::Array(x.split(y))),
            _ => Err(Error::msg("split input and separator must be strings")),
        },
        "explode" => match &a {
            Value::String(x) => Ok(Value::Array(x.explode())),
            _ => Err(Error::msg("explode input must be a string")),
        },
        "implode" => Str::implode(&a).map(Value::String),
        "strindices" => match (&a, &b) {
            (Value::String(x), Value::String(y)) => Ok(Value::Array(x.indexes(y))),
            _ => panic!("jq asserts here"),
        },
        "repeat" | "multiply" => binop_multiply(a, b),
        "plus" => binop_plus(a, b),
        "length" => f_length(&a),
        _ => panic!("unknown op {op}"),
    }
}

#[test]
fn ops_fixture() {
    let mut failures = Vec::new();
    for c in fixture(include_str!("testdata/ops.json")) {
        let op = s(&c, "op");
        let args: Vec<Value> = c["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| jv(a.as_str().unwrap()))
            .collect();
        let get = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
        let got = tagged(run_op(op, get(0), get(1), get(2)));
        if got != s(&c, "out") {
            failures.push(format!(
                "{op} {}: got {got}, jq {}",
                c["args"],
                s(&c, "out")
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Hand-written checks for behaviours the fixtures can't reach ----------

#[test]
fn slice_views_are_equal_like_jq() {
    // `jq -nc '[1,2,3,4] | .[0:2] == .[2:4]'` => true
    // `jq -nc '[range(4)+1] | [.[0:2], .[2:4]] | unique'` => [[1,2]]
    // `jq -nc '[range(4)+1] | [.[0:2], .[2:4]] | group_by(.)'` => [[[1,2],[3,4]]]
    let a = jv("[1,2,3,4]");
    let x = a.get(&jv("{\"start\":0,\"end\":2}")).unwrap();
    let y = a.get(&jv("{\"start\":2,\"end\":4}")).unwrap();
    assert!(x.equal(&y));
    assert_eq!(x.compare(&y), std::cmp::Ordering::Less);
    let pair = Array::from_vec(vec![x.clone(), y.clone()]);
    assert_eq!(Value::Array(unique(&pair, &pair)).to_json(), "[[1,2]]");
    let keys = Array::from_vec(vec![arr(vec![x]), arr(vec![y])]);
    assert_eq!(
        Value::Array(group(&pair, &keys)).to_json(),
        "[[[1,2],[3,4]]]"
    );
    // Views at offset >= 65536 are copies:
    // `jq -nc '[range(100000)] | .[70000:70002] == .[80000:80002]'` => false
    let big = Value::from((0..100000).map(Value::from).collect::<Vec<_>>());
    let p = big.get(&jv("{\"start\":70000,\"end\":70002}")).unwrap();
    let q = big.get(&jv("{\"start\":80000,\"end\":80002}")).unwrap();
    assert!(!p.equal(&q));
}

#[test]
fn setpath_writes_past_unique_view_like_jq() {
    // `jq -nc '[range(4)+1] | .[0:2] | .[3] = 9'` => [1,2,3,9]
    let a = jv("[1,2,3,4]");
    let view = a.get(&jv("{\"start\":0,\"end\":2}")).unwrap();
    drop(a);
    let r = view.setpath(&jv("[3]"), Value::from(9)).unwrap();
    assert_eq!(r.to_json(), "[1,2,3,9]");
}

#[test]
fn nan_delpaths_terminates() {
    // jq 1.8.1 loops forever here; the port deletes as if NaN were 0 (the
    // arm64 result of C's (int)NaN in jv_dels).
    let r = jv("[0,1,2]").delpaths(&jv("[[nan]]")).unwrap();
    assert_eq!(r.to_json(), "[1,2]");
    let r = jv("{\"a\":1}").delpaths(&jv("[[nan]]"));
    assert_eq!(
        r.unwrap_err().to_string(),
        "Cannot delete number field of object"
    );
}

#[test]
fn object_key_order_like_jq() {
    // `jq -nc '{a:1,b:2} | del(.a) | .a = 3'` => {"b":2,"a":3}
    let o = jv("{\"a\":1,\"b\":2}");
    let o = o.delpaths(&jv("[[\"a\"]]")).unwrap();
    let o = o.set(&jv("\"a\""), Value::from(3)).unwrap();
    assert_eq!(o.to_json(), "{\"b\":2,\"a\":3}");
    // `jq -c . <<< '{"b":1,"a":2,"b":3}'` => {"b":3,"a":2}
    assert_eq!(
        jv("{\"b\":1,\"a\":2,\"b\":3}").to_json(),
        "{\"b\":3,\"a\":2}"
    );
}

#[test]
fn in_place_mutation_when_unique() {
    // Appending to a uniquely owned array/string/object must not copy.
    let mut a = Array::new();
    for i in 0..1000 {
        a.push(Value::from(i));
    }
    let p = a.as_slice().as_ptr();
    a.set(3, Value::from(-1)).unwrap();
    assert_eq!(a.as_slice().as_ptr(), p);
    let v = Value::Array(a);
    let v = v.setpath(&jv("[5]"), Value::from(7)).unwrap();
    assert_eq!(v.as_array().unwrap().as_slice().as_ptr(), p);
    // Nested setpath updates in place too.
    let mut doc = jv("{\"a\":{\"b\":[1,2,3]}}");
    let inner_ptr = |d: &Value| {
        d.getpath(&jv("[\"a\",\"b\"]"))
            .unwrap()
            .as_array()
            .unwrap()
            .as_slice()
            .as_ptr()
    };
    let before = inner_ptr(&doc);
    for i in 0..10 {
        doc = doc.setpath(&jv("[\"a\",\"b\",1]"), Value::from(i)).unwrap();
    }
    assert_eq!(inner_ptr(&doc), before);
}

#[test]
fn dump_string_trunc_cases() {
    // Expectations from `jq -n '<v> | -.'` (type_error uses bufsize 15).
    let cases = [
        ("\"abc\"", "\"abc\""),
        ("\"12345678901234\"", "\"1234567890..."),
        ("\"123456789012\"", "\"123456789012\""),
        ("\"1234567890123\"", "\"1234567890..."),
        (
            "\"\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\"",
            "\"\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}...",
        ),
        ("[1,2,3,4,5,6,7,8]", "[1,2,3,4,5,..."),
        ("{\"a\":\"bbbbbbbbbbbbbbbbb\"}", "{\"a\":\"bbbbb..."),
        (
            "\"\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\"",
            "\"\u{20ac}\u{20ac}\u{20ac}...",
        ),
        (
            "\"a\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\"",
            "\"a\u{20ac}\u{20ac}\u{20ac}...",
        ),
        (
            "\"ab\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\u{20ac}\"",
            "\"ab\u{20ac}\u{20ac}...",
        ),
    ];
    for (v, want) in cases {
        assert_eq!(dump_string_trunc(&jv(v), 15), want, "{v}");
    }
}

#[test]
fn deep_json_limits() {
    // 10000 nested arrays parse, 10001 do not; object keys count too.
    let ok = "[".repeat(10000) + &"]".repeat(10000);
    assert!(parse_sized(ok.as_bytes()).is_ok());
    let bad = "[".repeat(10001) + &"]".repeat(10001);
    assert_eq!(
        parse_sized(bad.as_bytes())
            .unwrap_err()
            .to_string()
            .split(" (while")
            .next()
            .unwrap(),
        "Exceeds depth limit for parsing at line 1, column 10001"
    );
    // Printing a deep value is fine (MAX_PRINT_DEPTH elides past 256).
    let v = parse_sized(ok.as_bytes()).unwrap();
    assert!(v.to_json().contains("<skipped: too deep>"));
    drop(v);
}

#[test]
fn colors_parse() {
    let c = Colors::parse("0;31:0;32").unwrap();
    let v = jv("[null,false]");
    let opts = DumpOptions {
        colors: Some(c),
        ..DumpOptions::compact()
    };
    // `printf '[null,false]' | JQ_COLORS="0;31:0;32" jq -Cc .`
    assert_eq!(
        dump_string(&v, &opts),
        "\x1b[1;39m[\x1b[0m\x1b[0;31mnull\x1b[0m\x1b[1;39m,\x1b[0m\x1b[0;32mfalse\x1b[0m\x1b[1;39m]\x1b[0m"
    );
    assert!(Colors::parse("x").is_none());
    assert_eq!(Colors::parse("").unwrap(), Colors::default());
}

#[test]
fn updates_never_affect_other_references() {
    let a = jv("{\"x\":[1,2,{\"y\":3}],\"s\":\"ab\"}");
    let keep = a.clone();
    let b = a
        .setpath(&jv("[\"x\",2,\"y\"]"), Value::from(9))
        .unwrap()
        .delpaths(&jv("[[\"x\",0]]"))
        .unwrap()
        .set(&jv("\"s\""), Value::from("zz"))
        .unwrap();
    assert_eq!(keep.to_json(), "{\"x\":[1,2,{\"y\":3}],\"s\":\"ab\"}");
    assert_eq!(b.to_json(), "{\"x\":[2,{\"y\":9}],\"s\":\"zz\"}");
    // Slices share storage but writes go to a copy when shared.
    let arr = jv("[1,2,3,4]");
    let mut view = arr.get(&jv("{\"start\":1,\"end\":3}")).unwrap();
    if let Value::Array(v) = &mut view {
        v.as_mut_slice()[0] = Value::from(7);
        assert_eq!(v.get_mut(1).map(|x| x.to_json()), Some("3".into()));
        assert!(v.get_mut(2).is_none());
    }
    assert_eq!(view.to_json(), "[7,3]");
    assert_eq!(arr.to_json(), "[1,2,3,4]");
    // Strings and objects too.
    let mut s = Str::from("ab");
    let s2 = s.clone();
    s.push_str("c");
    assert_eq!((s.as_str(), s2.as_str()), ("abc", "ab"));
    let mut o = jv("{\"a\":1,\"b\":2,\"c\":3}").as_object().unwrap().clone();
    let o2 = o.clone();
    *o.get_mut("b").unwrap() = Value::from(20);
    assert!(o.get_mut("zz").is_none());
    o.retain(|k, _| k != "a");
    assert_eq!(Value::Object(o).to_json(), "{\"b\":20,\"c\":3}");
    assert_eq!(Value::Object(o2).to_json(), "{\"a\":1,\"b\":2,\"c\":3}");
}

#[test]
fn array_conversions() {
    let a = jv("[0,1,2,3,4,5]");
    let v = a.get(&jv("{\"start\":2,\"end\":5}")).unwrap();
    let Value::Array(view) = v else { panic!() };
    // A shared view converts by copying...
    assert_eq!(Value::from(view.clone().into_vec()).to_json(), "[2,3,4]");
    drop(a);
    // ...a unique one by moving its elements out.
    let items: Vec<Value> = view.into_iter().collect();
    assert_eq!(Value::from(items).to_json(), "[2,3,4]");
    let mut b: Array = (0..3).map(Value::from).collect();
    b.extend([Value::from("x")]);
    b.extend_from_array(&Array::from_vec(vec![Value::Null]));
    assert_eq!(Value::Array(b).to_json(), "[0,1,2,\"x\",null]");
}

#[test]
fn delpaths_many_object_keys() {
    // `jq -nc '[range(20) | {key: "k\(.)", value: .}] | from_entries
    //   | delpaths([range(0;20;2) | ["k\(.)"]] + [["zz"]]) | keys_unsorted'`
    let obj: Object = (0..20)
        .map(|i| (Str::from(format!("k{i}")), Value::from(i)))
        .collect();
    let mut paths: Vec<Value> = (0..20)
        .step_by(2)
        .map(|i| Value::from(vec![Value::from(format!("k{i}"))]))
        .collect();
    paths.push(jv("[\"zz\"]"));
    let r = Value::Object(obj).delpaths(&Value::from(paths)).unwrap();
    assert_eq!(
        r.keys_unsorted().unwrap().to_json(),
        "[\"k1\",\"k3\",\"k5\",\"k7\",\"k9\",\"k11\",\"k13\",\"k15\",\"k17\",\"k19\"]"
    );
}

#[test]
fn refcounted_dump_like_debug_trace() {
    // `jq -n --debug-trace '["ab",[1],{"x":"y"},[],{}] | .[0]'` prints
    // `["ab" (1),[1] (1),{"x":"y" (1)} (1),[],{}] (2)` for the constant held
    // by both the bytecode and the stack.
    let v = jv("[\"ab\",[1],{\"x\":\"y\"},[],{}]");
    let held = v.clone();
    let mut out = Vec::new();
    dump_refcounted(&v, &DumpOptions::compact(), &mut out).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[\"ab\" (1),[1] (1),{\"x\":\"y\" (1)} (1),[],{}] (2)"
    );
    assert_eq!(held.refcount(), 2);
    assert_eq!(Value::Null.refcount(), 1);
    assert_eq!(Value::number(1.0).refcount(), 1);
    let lit = jv("1.0");
    let lit2 = lit.clone();
    assert_eq!(lit2.refcount(), 2);
}

#[test]
fn deep_values_compare_without_overflow() {
    // Distinct maximally deep inputs (jq's parser allows 10000 stack
    // entries: 10000 nested arrays, or 5000 objects with their pending
    // keys), compared on a 2 MB thread like a worker thread.
    let handle = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            for (open, close, levels) in [("[", "]", 10000), ("{\"a\":", "}", 5000)] {
                let deep = |leaf: &str| {
                    let doc = open.repeat(levels) + leaf + &close.repeat(levels);
                    parse_sized(doc.as_bytes()).unwrap()
                };
                let (a, b, c) = (deep("1"), deep("1"), deep("2"));
                assert!(a.equal(&b));
                assert!(!a.equal(&c));
                assert_eq!(a.compare(&c), std::cmp::Ordering::Less);
                assert!(a.contains(&b));
                assert!(!a.identical(&b));
                // Paths as deep as the values.
                let step = if open == "[" {
                    Value::from(0)
                } else {
                    Value::from("a")
                };
                let path = Value::from(vec![step; levels]);
                assert_eq!(a.getpath(&path).unwrap().to_json(), "1");
                let d = a.clone().setpath(&path, Value::from(2)).unwrap();
                assert!(d.equal(&c));
                let e = a
                    .clone()
                    .delpaths(&Value::from(vec![path.clone()]))
                    .unwrap();
                assert!(!e.equal(&a));
                // Sorting deep keys, and printing (elided past depth 256).
                let arr = Array::from_vec(vec![c.clone(), a.clone()]);
                let sorted = sort(&arr, &arr);
                assert!(sorted.get(0).unwrap().equal(&a));
                assert!(a.to_json().contains("<skipped: too deep>"));
                // Object `*` merges all the way down.
                if let (Value::Object(x), Value::Object(y)) = (&a, &c) {
                    let mut m = x.clone();
                    m.merge_recursive(y);
                    assert!(Value::Object(m).equal(&c));
                }
            }
        })
        .unwrap();
    handle.join().unwrap();
}
