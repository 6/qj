use crate::filter::{Env, Filter};
use crate::value::Value;
use std::rc::Rc;

use super::super::eval::eval;

pub(super) fn eval_regex(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "test" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_pattern_flags(args, input, env);
                if let Some(re) = build_regex(&pattern, &flags) {
                    output(Value::Bool(re.is_match(s)));
                }
            }
        }
        "match" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_pattern_flags(args, input, env);
                let global = flags.contains('g');
                if let Some(re) = build_regex(&pattern, &flags) {
                    if global {
                        for caps in re.captures_iter(s) {
                            output(regex_match_object(&re, &caps, s));
                        }
                    } else if let Some(caps) = re.captures(s) {
                        output(regex_match_object(&re, &caps, s));
                    }
                }
            }
        }
        "capture" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_pattern_flags(args, input, env);
                if let Some(re) = build_regex(&pattern, &flags)
                    && let Some(caps) = re.captures(s)
                {
                    let mut obj = Vec::new();
                    for (i, name) in re.capture_names().enumerate() {
                        if let Some(name) = name {
                            let val = caps
                                .get(i)
                                .map(|m| Value::String(m.as_str().to_string()))
                                .unwrap_or(Value::Null);
                            obj.push((name.to_string(), val));
                        }
                    }
                    output(Value::Object(Rc::new(obj)));
                }
            }
        }
        "scan" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_pattern_flags(args, input, env);
                if let Some(re) = build_regex(&pattern, &flags) {
                    for caps in re.captures_iter(s) {
                        if re.captures_len() > 1 {
                            let arr: Vec<Value> = (1..caps.len())
                                .map(|i| {
                                    caps.get(i)
                                        .map(|m| Value::String(m.as_str().to_string()))
                                        .unwrap_or(Value::Null)
                                })
                                .collect();
                            output(Value::Array(Rc::new(arr)));
                        } else {
                            output(Value::String(caps[0].to_string()));
                        }
                    }
                }
            }
        }
        "sub" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_sub_pattern_flags(args, 2, input, env);
                if let Some(re) = build_regex(&pattern, &flags) {
                    let mut repl_str = String::new();
                    if let Some(repl_f) = args.get(1)
                        && let Some(caps) = re.captures(s)
                    {
                        let match_obj = regex_match_object(&re, &caps, s);
                        eval(repl_f, &match_obj, env, &mut |v| {
                            if let Value::String(rs) = v {
                                repl_str = rs;
                            }
                        });
                    }
                    if let Some(caps) = re.captures(s) {
                        let m = caps.get(0).unwrap();
                        let mut result = String::with_capacity(s.len());
                        result.push_str(&s[..m.start()]);
                        result.push_str(&repl_str);
                        result.push_str(&s[m.end()..]);
                        output(Value::String(result));
                    } else {
                        output(Value::String(s.clone()));
                    }
                }
            }
        }
        "gsub" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_sub_pattern_flags(args, 2, input, env);
                if let Some(re) = build_regex(&pattern, &flags) {
                    let mut result = String::with_capacity(s.len());
                    let mut last_end = 0;
                    for caps in re.captures_iter(s) {
                        let m = caps.get(0).unwrap();
                        result.push_str(&s[last_end..m.start()]);
                        let mut repl_str = String::new();
                        if let Some(repl_f) = args.get(1) {
                            let match_obj = regex_match_object(&re, &caps, s);
                            eval(repl_f, &match_obj, env, &mut |v| {
                                if let Value::String(rs) = v {
                                    repl_str = rs;
                                }
                            });
                        }
                        result.push_str(&repl_str);
                        last_end = m.end();
                    }
                    result.push_str(&s[last_end..]);
                    output(Value::String(result));
                }
            }
        }
        "splits" => {
            if let Value::String(s) = input {
                let (pattern, flags) = eval_pattern_flags(args, input, env);
                if let Some(re) = build_regex(&pattern, &flags) {
                    let mut last_end = 0;
                    for m in re.find_iter(s) {
                        output(Value::String(s[last_end..m.start()].to_string()));
                        last_end = m.end();
                    }
                    output(Value::String(s[last_end..].to_string()));
                }
            }
        }
        _ => {}
    }
}

/// Compile a regex from a pattern string and jq-style flags string.
fn build_regex(pattern: &str, flags: &str) -> Option<regex::Regex> {
    let mut p = String::new();
    let case_insensitive = flags.contains('i');
    let multiline = flags.contains('m');
    let single_line = flags.contains('s');
    if case_insensitive || multiline || single_line {
        p.push_str("(?");
        if case_insensitive {
            p.push('i');
        }
        if multiline {
            p.push('m');
        }
        if single_line {
            p.push('s');
        }
        p.push(')');
    }
    if flags.contains('x') {
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                p.push(c);
                if let Some(next) = chars.next() {
                    p.push(next);
                }
            } else if c == '#' {
                for nc in chars.by_ref() {
                    if nc == '\n' {
                        break;
                    }
                }
            } else if c.is_ascii_whitespace() {
                // Skip
            } else {
                p.push(c);
            }
        }
    } else {
        p.push_str(pattern);
    }
    regex::Regex::new(&p).ok()
}

/// Evaluate pattern and flags from the first two args.
fn eval_pattern_flags(args: &[Filter], input: &Value, env: &Env) -> (String, String) {
    let mut pattern = String::new();
    let mut flags = String::new();
    if let Some(pat_f) = args.first() {
        eval(pat_f, input, env, &mut |v| {
            if let Value::String(s) = v {
                pattern = s;
            }
        });
    }
    if let Some(flags_f) = args.get(1) {
        eval(flags_f, input, env, &mut |v| {
            if let Value::String(s) = v {
                flags = s;
            }
        });
    }
    (pattern, flags)
}

/// Evaluate pattern and flags for sub/gsub where flags is at `flags_idx`.
fn eval_sub_pattern_flags(
    args: &[Filter],
    flags_idx: usize,
    input: &Value,
    env: &Env,
) -> (String, String) {
    let mut pattern = String::new();
    let mut flags = String::new();
    if let Some(pat_f) = args.first() {
        eval(pat_f, input, env, &mut |v| {
            if let Value::String(s) = v {
                pattern = s;
            }
        });
    }
    if let Some(flags_f) = args.get(flags_idx) {
        eval(flags_f, input, env, &mut |v| {
            if let Value::String(s) = v {
                flags = s;
            }
        });
    }
    (pattern, flags)
}

/// Build a jq-compatible match result object from a regex::Captures.
fn regex_match_object(re: &regex::Regex, caps: &regex::Captures, _input: &str) -> Value {
    let m = caps.get(0).unwrap();
    let mut captures = Vec::new();
    for (i, name) in re.capture_names().enumerate() {
        if i == 0 {
            continue;
        }
        let cap_val = if let Some(cm) = caps.get(i) {
            Value::Object(Rc::new(vec![
                ("offset".to_string(), Value::Int(cm.start() as i64)),
                ("length".to_string(), Value::Int(cm.len() as i64)),
                ("string".to_string(), Value::String(cm.as_str().to_string())),
                (
                    "name".to_string(),
                    name.map(|n| Value::String(n.to_string()))
                        .unwrap_or(Value::Null),
                ),
            ]))
        } else {
            Value::Object(Rc::new(vec![
                ("offset".to_string(), Value::Int(-1)),
                ("length".to_string(), Value::Int(0)),
                ("string".to_string(), Value::Null),
                (
                    "name".to_string(),
                    name.map(|n| Value::String(n.to_string()))
                        .unwrap_or(Value::Null),
                ),
            ]))
        };
        captures.push(cap_val);
    }
    Value::Object(Rc::new(vec![
        ("offset".to_string(), Value::Int(m.start() as i64)),
        ("length".to_string(), Value::Int(m.len() as i64)),
        ("string".to_string(), Value::String(m.as_str().to_string())),
        ("captures".to_string(), Value::Array(Rc::new(captures))),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_regex_basic() {
        let re = build_regex("^foo", "").unwrap();
        assert!(re.is_match("foobar"));
        assert!(!re.is_match("barfoo"));
    }

    #[test]
    fn build_regex_case_insensitive() {
        let re = build_regex("FOO", "i").unwrap();
        assert!(re.is_match("foobar"));
    }

    #[test]
    fn build_regex_extended_mode() {
        let re = build_regex("foo  # match foo\n  bar", "x").unwrap();
        assert!(re.is_match("foobar"));
        assert!(!re.is_match("foo bar"));
    }

    #[test]
    fn build_regex_combined_flags() {
        let re = build_regex("^foo$", "im").unwrap();
        assert!(re.is_match("bar\nfoo\nbaz"));
    }

    #[test]
    fn build_regex_invalid_pattern() {
        assert!(build_regex("[invalid", "").is_none());
    }

    #[test]
    fn match_object_structure() {
        let re = regex::Regex::new("(o+)").unwrap();
        let caps = re.captures("foobar").unwrap();
        let obj = regex_match_object(&re, &caps, "foobar");
        if let Value::Object(fields) = &obj {
            let offset = fields
                .iter()
                .find(|(k, _)| k == "offset")
                .unwrap()
                .1
                .clone();
            let length = fields
                .iter()
                .find(|(k, _)| k == "length")
                .unwrap()
                .1
                .clone();
            let string = fields
                .iter()
                .find(|(k, _)| k == "string")
                .unwrap()
                .1
                .clone();
            assert_eq!(offset, Value::Int(1));
            assert_eq!(length, Value::Int(2));
            assert_eq!(string, Value::String("oo".to_string()));
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn match_object_named_capture() {
        let re = regex::Regex::new("(?P<year>\\d{4})-(?P<month>\\d{2})").unwrap();
        let caps = re.captures("2024-01-15").unwrap();
        let obj = regex_match_object(&re, &caps, "2024-01-15");
        if let Value::Object(fields) = &obj {
            let captures_val = fields
                .iter()
                .find(|(k, _)| k == "captures")
                .unwrap()
                .1
                .clone();
            if let Value::Array(caps_arr) = captures_val {
                assert_eq!(caps_arr.len(), 2);
                if let Value::Object(c0) = &caps_arr[0] {
                    let name = c0.iter().find(|(k, _)| k == "name").unwrap().1.clone();
                    assert_eq!(name, Value::String("year".to_string()));
                }
                if let Value::Object(c1) = &caps_arr[1] {
                    let name = c1.iter().find(|(k, _)| k == "name").unwrap().1.clone();
                    assert_eq!(name, Value::String("month".to_string()));
                }
            } else {
                panic!("expected array for captures");
            }
        } else {
            panic!("expected object");
        }
    }
}
