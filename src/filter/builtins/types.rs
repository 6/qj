use crate::filter::{Env, Filter};
use crate::value::Value;
use std::sync::Arc;

use super::super::eval::eval;
use super::super::value_ops::value_contains;

pub(super) fn eval_types(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "length" => match input {
            Value::String(s) => output(Value::Int(s.chars().count() as i64)),
            Value::Array(a) => output(Value::Int(a.len() as i64)),
            Value::Object(o) => output(Value::Int(o.len() as i64)),
            Value::Null => output(Value::Int(0)),
            Value::Int(n) => output(
                n.checked_abs()
                    .map_or_else(|| Value::Double((*n as f64).abs(), None), Value::Int),
            ),
            Value::Double(f, raw) => {
                let abs_raw = if f.is_infinite() || *f == 0.0 {
                    raw.as_ref().map(|s| {
                        s.strip_prefix('-')
                            .map_or_else(|| s.clone(), |rest| rest.into())
                    })
                } else {
                    None
                };
                output(Value::Double(f.abs(), abs_raw));
            }
            Value::Bool(_) => output(Value::Null),
        },
        "type" => {
            output(Value::String(input.type_name().to_string()));
        }
        "empty" => {}
        "not" => {
            output(Value::Bool(!input.is_truthy()));
        }
        "null" => output(Value::Null),
        "true" => output(Value::Bool(true)),
        "false" => output(Value::Bool(false)),
        "numbers" => {
            if matches!(input, Value::Int(_) | Value::Double(..)) {
                output(input.clone());
            }
        }
        "strings" => {
            if matches!(input, Value::String(_)) {
                output(input.clone());
            }
        }
        "booleans" => {
            if matches!(input, Value::Bool(_)) {
                output(input.clone());
            }
        }
        "nulls" => {
            if matches!(input, Value::Null) {
                output(input.clone());
            }
        }
        "arrays" => {
            if matches!(input, Value::Array(_)) {
                output(input.clone());
            }
        }
        "objects" => {
            if matches!(input, Value::Object(_)) {
                output(input.clone());
            }
        }
        "iterables" => {
            if matches!(input, Value::Array(_) | Value::Object(_)) {
                output(input.clone());
            }
        }
        "scalars" => {
            if !matches!(input, Value::Array(_) | Value::Object(_)) {
                output(input.clone());
            }
        }
        "has" => {
            if let Some(key_filter) = args.first() {
                let mut key_val = Value::Null;
                eval(key_filter, input, env, &mut |v| key_val = v);
                match (input, &key_val) {
                    (Value::Object(obj), Value::String(key)) => {
                        let found = obj.iter().any(|(k, _)| k == key);
                        output(Value::Bool(found));
                    }
                    (Value::Array(arr), Value::Int(idx)) => {
                        let found = *idx >= 0 && (*idx as usize) < arr.len();
                        output(Value::Bool(found));
                    }
                    _ => output(Value::Bool(false)),
                }
            }
        }
        "contains" => {
            if let (Some(arg), _) = (args.first(), input) {
                let mut pattern = Value::Null;
                eval(arg, input, env, &mut |v| pattern = v);
                output(Value::Bool(value_contains(input, &pattern)));
            }
        }
        "inside" => {
            if let Some(arg) = args.first() {
                let mut container = Value::Null;
                eval(arg, input, env, &mut |v| container = v);
                output(Value::Bool(value_contains(&container, input)));
            }
        }
        "in" => {
            if let Some(arg) = args.first() {
                let mut container = Value::Null;
                eval(arg, input, env, &mut |v| container = v);
                match (&container, input) {
                    (Value::Object(obj), Value::String(key)) => {
                        output(Value::Bool(obj.iter().any(|(k, _)| k == key)));
                    }
                    (Value::Array(arr), Value::Int(idx)) => {
                        output(Value::Bool(*idx >= 0 && (*idx as usize) < arr.len()));
                    }
                    _ => output(Value::Bool(false)),
                }
            }
        }
        "to_entries" => {
            if let Value::Object(obj) = input {
                let entries: Vec<Value> = obj
                    .iter()
                    .map(|(k, v)| {
                        Value::Object(Arc::new(vec![
                            ("key".into(), Value::String(k.clone())),
                            ("value".into(), v.clone()),
                        ]))
                    })
                    .collect();
                output(Value::Array(Arc::new(entries)));
            }
        }
        "from_entries" => {
            if let Value::Array(arr) = input {
                let mut obj = Vec::new();
                for entry in arr.iter() {
                    if let Value::Object(fields) = entry {
                        let key = fields
                            .iter()
                            .find(|(k, _)| k == "key" || k == "Key" || k == "name" || k == "Name")
                            .map(|(_, v)| match v {
                                Value::String(s) => s.clone(),
                                Value::Int(n) => n.to_string(),
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        let val = fields
                            .iter()
                            .find(|(k, _)| k == "value" || k == "Value")
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Null);
                        obj.push((key, val));
                    }
                }
                output(Value::Object(Arc::new(obj)));
            }
        }
        _ => {}
    }
}
