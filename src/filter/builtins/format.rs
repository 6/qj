use crate::filter::{Env, Filter};
use crate::value::Value;

pub(super) fn eval_format(
    name: &str,
    _args: &[Filter],
    input: &Value,
    _env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "@json" => {
            let mut buf = Vec::new();
            crate::output::write_compact(&mut buf, input, false).unwrap();
            output(Value::String(String::from_utf8(buf).unwrap_or_default()));
        }
        "@text" => match input {
            Value::String(_) => output(input.clone()),
            Value::Int(n) => output(Value::String(itoa::Buffer::new().format(*n).into())),
            Value::Double(f, _) => output(Value::String(ryu::Buffer::new().format(*f).into())),
            Value::Bool(b) => output(Value::String(if *b { "true" } else { "false" }.into())),
            Value::Null => output(Value::String("null".into())),
            Value::Array(_) | Value::Object(_) => {
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, input, false).unwrap();
                output(Value::String(String::from_utf8(buf).unwrap_or_default()));
            }
        },
        "@html" => {
            if let Value::String(s) = input {
                let mut out = String::with_capacity(s.len());
                for c in s.chars() {
                    match c {
                        '&' => out.push_str("&amp;"),
                        '<' => out.push_str("&lt;"),
                        '>' => out.push_str("&gt;"),
                        '\'' => out.push_str("&apos;"),
                        '"' => out.push_str("&quot;"),
                        _ => out.push(c),
                    }
                }
                output(Value::String(out));
            }
        }
        "@uri" => {
            if let Value::String(s) = input {
                let mut out = String::with_capacity(s.len());
                for byte in s.bytes() {
                    match byte {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            out.push(byte as char)
                        }
                        _ => {
                            out.push('%');
                            out.push(
                                char::from_digit((byte >> 4) as u32, 16)
                                    .unwrap()
                                    .to_ascii_uppercase(),
                            );
                            out.push(
                                char::from_digit((byte & 0xf) as u32, 16)
                                    .unwrap()
                                    .to_ascii_uppercase(),
                            );
                        }
                    }
                }
                output(Value::String(out));
            }
        }
        "@urid" => {
            if let Value::String(s) = input {
                let bytes = s.as_bytes();
                let mut decoded_bytes = Vec::with_capacity(s.len());
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'%' && i + 2 < bytes.len() {
                        let hi = (bytes[i + 1] as char).to_digit(16);
                        let lo = (bytes[i + 2] as char).to_digit(16);
                        if let (Some(h), Some(l)) = (hi, lo) {
                            decoded_bytes.push((h * 16 + l) as u8);
                            i += 3;
                            continue;
                        }
                    }
                    if bytes[i] == b'+' {
                        decoded_bytes.push(b' ');
                    } else {
                        decoded_bytes.push(bytes[i]);
                    }
                    i += 1;
                }
                output(Value::String(
                    String::from_utf8(decoded_bytes).unwrap_or_default(),
                ));
            }
        }
        "@csv" => {
            if let Value::Array(arr) = input {
                let parts: Vec<String> = arr
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => {
                            let escaped = s.replace('"', "\"\"");
                            format!("\"{escaped}\"")
                        }
                        Value::Int(n) => itoa::Buffer::new().format(*n).to_string(),
                        Value::Double(f, _) => ryu::Buffer::new().format(*f).to_string(),
                        Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
                        Value::Null => "".to_string(),
                        _ => String::new(),
                    })
                    .collect();
                output(Value::String(parts.join(",")));
            }
        }
        "@tsv" => {
            if let Value::Array(arr) = input {
                let parts: Vec<String> = arr
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => s
                            .replace('\\', "\\\\")
                            .replace('\t', "\\t")
                            .replace('\n', "\\n")
                            .replace('\r', "\\r"),
                        Value::Int(n) => itoa::Buffer::new().format(*n).to_string(),
                        Value::Double(f, _) => ryu::Buffer::new().format(*f).to_string(),
                        Value::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
                        Value::Null => "".to_string(),
                        _ => String::new(),
                    })
                    .collect();
                output(Value::String(parts.join("\t")));
            }
        }
        "@sh" => {
            if let Value::String(s) = input {
                let escaped = s.replace('\'', "'\\''");
                output(Value::String(format!("'{escaped}'")));
            }
        }
        "@base64" => {
            if let Value::String(s) = input {
                use base64::Engine;
                output(Value::String(
                    base64::engine::general_purpose::STANDARD.encode(s.as_bytes()),
                ));
            }
        }
        "@base64d" => {
            if let Value::String(s) = input {
                use base64::Engine;
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(s.as_bytes())
                    && let Ok(text) = String::from_utf8(decoded)
                {
                    output(Value::String(text));
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Env;
    use std::rc::Rc;

    fn run_builtin(name: &str, input: &Value) -> Vec<Value> {
        let env = Env::empty();
        let mut results = Vec::new();
        eval_format(name, &[], input, &env, &mut |v| results.push(v));
        results
    }

    #[test]
    fn format_html_escapes() {
        let input = Value::String("<b>a & b</b>".into());
        let out = run_builtin("@html", &input);
        assert_eq!(
            out,
            vec![Value::String("&lt;b&gt;a &amp; b&lt;/b&gt;".into())]
        );
    }

    #[test]
    fn format_uri_encodes() {
        let input = Value::String("hello world".into());
        let out = run_builtin("@uri", &input);
        assert_eq!(out, vec![Value::String("hello%20world".into())]);
    }

    #[test]
    fn format_csv_array() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(1),
            Value::String("two".into()),
            Value::Int(3),
        ]));
        let out = run_builtin("@csv", &input);
        assert_eq!(out, vec![Value::String("1,\"two\",3".into())]);
    }

    #[test]
    fn format_sh_quotes() {
        let input = Value::String("it's a test".into());
        let out = run_builtin("@sh", &input);
        assert_eq!(out, vec![Value::String("'it'\\''s a test'".into())]);
    }

    #[test]
    fn format_base64_roundtrip() {
        let input = Value::String("hello".into());
        let encoded = run_builtin("@base64", &input);
        assert_eq!(encoded, vec![Value::String("aGVsbG8=".into())]);
        let decoded = run_builtin("@base64d", &encoded[0]);
        assert_eq!(decoded, vec![Value::String("hello".into())]);
    }

    #[test]
    fn format_json() {
        let input = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]));
        let out = run_builtin("@json", &input);
        assert_eq!(out, vec![Value::String("[1,2]".into())]);
    }

    #[test]
    fn format_text_string_passthrough() {
        let input = Value::String("abc".into());
        let out = run_builtin("@text", &input);
        assert_eq!(out, vec![Value::String("abc".into())]);
    }
}
