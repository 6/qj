use crate::filter::{Env, Filter};
use crate::value::Value;
use std::sync::Arc;

use super::super::eval::eval;
use super::super::value_ops::values_equal;
use super::set_error;

/// Truncate a string to at most `max` bytes, ensuring the cut falls on a UTF-8
/// char boundary. Returns the full string if it's already short enough.
fn safe_truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub(super) fn eval_strings(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "tostring" => match input {
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
        "tonumber" => match input {
            Value::Int(_) | Value::Double(..) => output(input.clone()),
            Value::String(s) => {
                if let Ok(n) = s.parse::<i64>() {
                    output(Value::Int(n));
                } else if let Ok(f) = s.parse::<f64>() {
                    output(Value::Double(f, None));
                }
            }
            _ => {}
        },
        "ascii_downcase" => {
            if let Value::String(s) = input {
                output(Value::String(
                    s.chars().map(|c| c.to_ascii_lowercase()).collect(),
                ));
            } else if !matches!(input, Value::Null) {
                set_error(format!(
                    "{} ({}) cannot be ascii_downcased",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "ascii_upcase" => {
            if let Value::String(s) = input {
                output(Value::String(
                    s.chars().map(|c| c.to_ascii_uppercase()).collect(),
                ));
            } else if !matches!(input, Value::Null) {
                set_error(format!(
                    "{} ({}) cannot be ascii_upcased",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "ltrimstr" => {
            if let Some(arg) = args.first() {
                let mut prefix = Value::Null;
                eval(arg, input, env, &mut |v| prefix = v);
                match (input, &prefix) {
                    (Value::String(s), Value::String(p)) => {
                        output(Value::String(
                            s.strip_prefix(p.as_str()).unwrap_or(s).to_string(),
                        ));
                    }
                    _ => {
                        set_error("startswith() requires string inputs".to_string());
                    }
                }
            }
        }
        "rtrimstr" => {
            if let Some(arg) = args.first() {
                let mut suffix = Value::Null;
                eval(arg, input, env, &mut |v| suffix = v);
                match (input, &suffix) {
                    (Value::String(s), Value::String(p)) => {
                        output(Value::String(
                            s.strip_suffix(p.as_str()).unwrap_or(s).to_string(),
                        ));
                    }
                    _ => {
                        set_error("endswith() requires string inputs".to_string());
                    }
                }
            }
        }
        "trimstr" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut pat = Value::Null;
                eval(arg, input, env, &mut |v| pat = v);
                if let Value::String(p) = pat {
                    let trimmed = s.strip_prefix(p.as_str()).unwrap_or(s);
                    output(Value::String(
                        trimmed
                            .strip_suffix(p.as_str())
                            .unwrap_or(trimmed)
                            .to_string(),
                    ));
                } else {
                    output(input.clone());
                }
            } else if !args.is_empty() && !matches!(input, Value::String(_)) {
                set_error(format!(
                    "{} ({}) and string cannot have their strings trimmed",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "startswith" => {
            if let Some(arg) = args.first() {
                let mut prefix = Value::Null;
                eval(arg, input, env, &mut |v| prefix = v);
                match (input, &prefix) {
                    (Value::String(s), Value::String(p)) => {
                        output(Value::Bool(s.starts_with(p.as_str())));
                    }
                    _ => {
                        set_error("startswith() requires string inputs".to_string());
                    }
                }
            }
        }
        "endswith" => {
            if let Some(arg) = args.first() {
                let mut suffix = Value::Null;
                eval(arg, input, env, &mut |v| suffix = v);
                match (input, &suffix) {
                    (Value::String(s), Value::String(p)) => {
                        output(Value::Bool(s.ends_with(p.as_str())));
                    }
                    _ => {
                        set_error("endswith() requires string inputs".to_string());
                    }
                }
            }
        }
        "split" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut sep = Value::Null;
                eval(arg, input, env, &mut |v| sep = v);
                if let Value::String(p) = sep {
                    let parts: Vec<Value> = if p.is_empty() {
                        s.chars().map(|c| Value::String(c.to_string())).collect()
                    } else {
                        s.split(p.as_str())
                            .map(|part| Value::String(part.into()))
                            .collect()
                    };
                    output(Value::Array(Arc::new(parts)));
                }
            } else if !args.is_empty() && !matches!(input, Value::String(_)) {
                set_error(format!(
                    "{} ({}) cannot be split",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "join" => {
            if let (Value::Array(arr), Some(arg)) = (input, args.first()) {
                eval(arg, input, env, &mut |sep| {
                    if let Value::String(p) = sep {
                        let mut result = String::new();
                        let mut first = true;
                        for v in arr.iter() {
                            if !first {
                                result.push_str(&p);
                            }
                            first = false;
                            match v {
                                Value::String(s) => result.push_str(s),
                                Value::Int(n) => result.push_str(itoa::Buffer::new().format(*n)),
                                Value::Double(f, _) => {
                                    result.push_str(ryu::Buffer::new().format(*f))
                                }
                                Value::Null => {}
                                Value::Bool(b) => {
                                    result.push_str(if *b { "true" } else { "false" })
                                }
                                _ => {
                                    // jq error: string ("partial,") and object ({...}) cannot be added
                                    let partial_desc = if result.len() > 10 {
                                        format!(
                                            "\"{}...",
                                            result.chars().take(10).collect::<String>()
                                        )
                                    } else {
                                        format!("\"{result}\"")
                                    };
                                    set_error(format!(
                                        "string ({}) and {} ({}) cannot be added",
                                        partial_desc,
                                        v.type_name(),
                                        v.short_desc()
                                    ));
                                    return;
                                }
                            }
                        }
                        output(Value::String(result));
                    }
                });
            }
        }
        "trim" | "ltrim" | "rtrim" => {
            if let Value::String(s) = input {
                let trimmed = match name {
                    "ltrim" => s.trim_start(),
                    "rtrim" => s.trim_end(),
                    _ => s.trim(),
                };
                output(Value::String(trimmed.to_string()));
            } else {
                set_error("trim input must be a string".to_string());
            }
        }
        "index" => {
            if let Some(arg) = args.first() {
                // Generator: produce one result per arg value
                eval(arg, input, env, &mut |needle| match (input, &needle) {
                    (Value::String(s), Value::String(n)) => {
                        if n.is_empty() {
                            output(Value::Null);
                        } else if let Some(byte_pos) = s.find(n.as_str()) {
                            output(Value::Int(s[..byte_pos].chars().count() as i64));
                        } else {
                            output(Value::Null);
                        }
                    }
                    (Value::Array(arr), _) => {
                        let pos = arr.iter().position(|v| values_equal(v, &needle));
                        match pos {
                            Some(i) => output(Value::Int(i as i64)),
                            None => output(Value::Null),
                        }
                    }
                    _ => output(Value::Null),
                });
            }
        }
        "rindex" => {
            if let Some(arg) = args.first() {
                eval(arg, input, env, &mut |needle| match (input, &needle) {
                    (Value::String(s), Value::String(n)) => {
                        if n.is_empty() {
                            output(Value::Null);
                        } else if let Some(byte_pos) = s.rfind(n.as_str()) {
                            output(Value::Int(s[..byte_pos].chars().count() as i64));
                        } else {
                            output(Value::Null);
                        }
                    }
                    (Value::Array(arr), _) => {
                        let pos = arr.iter().rposition(|v| values_equal(v, &needle));
                        match pos {
                            Some(i) => output(Value::Int(i as i64)),
                            None => output(Value::Null),
                        }
                    }
                    _ => output(Value::Null),
                });
            }
        }
        "indices" | "_indices" => {
            if let Some(arg) = args.first() {
                eval(arg, input, env, &mut |needle| match (input, &needle) {
                    (Value::String(s), Value::String(n)) => {
                        let mut positions = Vec::new();
                        if !n.is_empty() {
                            let mut start = 0;
                            while let Some(byte_pos) = s[start..].find(n.as_str()) {
                                let abs_byte = start + byte_pos;
                                positions.push(Value::Int(s[..abs_byte].chars().count() as i64));
                                // Advance by 1 character to allow overlapping matches
                                let next_char_len =
                                    s[abs_byte..].chars().next().map_or(1, |c| c.len_utf8());
                                start = abs_byte + next_char_len;
                            }
                        }
                        output(Value::Array(Arc::new(positions)));
                    }
                    (Value::Array(arr), Value::Array(needle_arr)) => {
                        // Subsequence search for array needles
                        let mut positions = Vec::new();
                        let nlen = needle_arr.len();
                        if nlen > 0 && nlen <= arr.len() {
                            for i in 0..=arr.len() - nlen {
                                let matches = arr[i..i + nlen]
                                    .iter()
                                    .zip(needle_arr.iter())
                                    .all(|(a, b)| values_equal(a, b));
                                if matches {
                                    positions.push(Value::Int(i as i64));
                                }
                            }
                        }
                        output(Value::Array(Arc::new(positions)));
                    }
                    (Value::Array(arr), _) => {
                        let positions: Vec<Value> = arr
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| values_equal(v, &needle))
                            .map(|(i, _)| Value::Int(i as i64))
                            .collect();
                        output(Value::Array(Arc::new(positions)));
                    }
                    _ => output(Value::Array(Arc::new(Vec::new()))),
                });
            }
        }
        "explode" => {
            if let Value::String(s) = input {
                let codepoints: Vec<Value> = s.chars().map(|c| Value::Int(c as i64)).collect();
                output(Value::Array(Arc::new(codepoints)));
            } else {
                set_error(format!(
                    "{} ({}) cannot be exploded",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "implode" => {
            if let Value::Array(arr) = input {
                let mut s = String::new();
                for v in arr.iter() {
                    let cp = match v {
                        Value::Int(n) => *n,
                        Value::Double(f, _) if f.is_finite() => *f as i64,
                        _ => {
                            set_error(format!(
                                "string ({}) can't be imploded, unicode codepoint needs to be numeric",
                                v.short_desc()
                            ));
                            return;
                        }
                    };
                    // Out-of-range or surrogate → replacement char U+FFFD
                    if !(0..=1114111).contains(&cp) || (55296..=57343).contains(&cp) {
                        s.push('\u{FFFD}');
                    } else if let Some(c) = char::from_u32(cp as u32) {
                        s.push(c);
                    } else {
                        s.push('\u{FFFD}');
                    }
                }
                output(Value::String(s));
            } else {
                set_error("implode input must be an array".to_string());
            }
        }
        "tojson" => {
            let mut buf = Vec::new();
            crate::output::write_compact(&mut buf, input, false).unwrap();
            output(Value::String(String::from_utf8(buf).unwrap_or_default()));
        }
        "fromjson" => {
            if let Value::String(s) = input {
                let trimmed = s.trim();
                // Handle special numeric values that JSON doesn't normally support
                match trimmed {
                    "NaN" | "nan" | "-NaN" | "-nan" => {
                        output(Value::Double(f64::NAN, None));
                        return;
                    }
                    "Infinity" | "infinity" | "inf" => {
                        output(Value::Double(f64::INFINITY, None));
                        return;
                    }
                    "-Infinity" | "-infinity" | "-inf" => {
                        output(Value::Double(f64::NEG_INFINITY, None));
                        return;
                    }
                    _ => {}
                }
                // Reject NaN/nan prefix followed by extra chars (e.g., "NaN1")
                if (trimmed.starts_with("NaN")
                    || trimmed.starts_with("nan")
                    || trimmed.starts_with("-NaN")
                    || trimmed.starts_with("-nan"))
                    && trimmed != "NaN"
                    && trimmed != "nan"
                    && trimmed != "-NaN"
                    && trimmed != "-nan"
                {
                    set_error(format!(
                        "Invalid numeric literal at EOF at line 1, column {} (while parsing '{}')",
                        trimmed.len(),
                        safe_truncate(s, 40)
                    ));
                    return;
                }
                let padded = crate::simdjson::pad_buffer(s.as_bytes());
                match crate::simdjson::dom_parse_to_value(&padded, s.len()) {
                    Ok(val) => output(val),
                    Err(_e) => {
                        // Produce jq-compatible error message
                        if trimmed.contains('\'') {
                            // Single-quote detection — find position after closing single-quote
                            // Use char_indices for safe slicing on multi-byte boundaries
                            let first = trimmed
                                .char_indices()
                                .find(|(_, c)| *c == '\'')
                                .map(|(i, _)| i)
                                .unwrap_or(0);
                            let after_first =
                                if first < trimmed.len() && trimmed.is_char_boundary(first + 1) {
                                    first + 1
                                } else {
                                    // Advance past the multi-byte char
                                    trimmed[first..]
                                        .char_indices()
                                        .nth(1)
                                        .map(|(i, _)| first + i)
                                        .unwrap_or(trimmed.len())
                                };
                            let col = trimmed[after_first..]
                                .find('\'')
                                .map(|p| after_first + p + 1 + 1) // +1 past close quote, +1 for 1-indexed
                                .unwrap_or(after_first);
                            set_error(format!(
                                "Invalid string literal; expected \", but got ' at line 1, column {} (while parsing '{}')",
                                col,
                                safe_truncate(s, 40)
                            ));
                        } else {
                            set_error(format!(
                                "Invalid numeric literal at EOF at line 1, column {} (while parsing '{}')",
                                s.len(),
                                safe_truncate(s, 40)
                            ));
                        }
                    }
                }
            } else {
                set_error(format!(
                    "{} ({}) is not a string and cannot be parsed as JSON",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "utf8bytelength" => {
            if let Value::String(s) = input {
                output(Value::Int(s.len() as i64));
            } else {
                set_error(format!(
                    "{} ({}) only strings have UTF-8 byte length",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "ascii" => {
            if let Value::String(s) = input
                && let Some(c) = s.chars().next()
            {
                output(Value::Int(c as i64));
            }
        }
        "toboolean" => match input {
            Value::Bool(_) => output(input.clone()),
            Value::String(s) => match s.as_str() {
                "true" => output(Value::Bool(true)),
                "false" => output(Value::Bool(false)),
                _ => {
                    set_error(format!(
                        "{} ({}) cannot be parsed as a boolean",
                        input.type_name(),
                        input.short_desc()
                    ));
                }
            },
            _ => {
                set_error(format!(
                    "{} ({}) cannot be parsed as a boolean",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        },
        _ => {}
    }
}
