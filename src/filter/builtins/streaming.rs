use std::sync::Arc;

use crate::filter::{Env, Filter};
use crate::value::Value;

use super::super::eval::eval;
use super::super::value_ops::set_path;

pub(super) fn eval_streaming(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "tostream" => {
            to_stream(input, &mut Vec::new(), output);
        }
        "fromstream" => {
            if let Some(expr) = args.first() {
                from_stream(expr, input, env, output);
            }
        }
        "truncate_stream" => {
            if let Some(expr) = args.first() {
                truncate_stream(expr, input, env, output);
            }
        }
        _ => {}
    }
}

/// Recursively walk a value, emitting `[path, value]` for leaves and `[path]`
/// terminators when containers close. Empty containers and scalars are leaves.
fn to_stream(value: &Value, path: &mut Vec<Value>, output: &mut dyn FnMut(Value)) {
    match value {
        Value::Object(obj) if !obj.is_empty() => {
            let pairs = obj.as_ref();
            for (key, val) in pairs {
                path.push(Value::String(key.clone()));
                to_stream(val, path, output);
                path.pop();
            }
            // Terminator: path ending at the last key
            let last_key = &pairs[pairs.len() - 1].0;
            path.push(Value::String(last_key.clone()));
            output(Value::Array(Arc::new(vec![Value::Array(Arc::new(
                path.clone(),
            ))])));
            path.pop();
        }
        Value::Array(arr) if !arr.is_empty() => {
            let items = arr.as_ref();
            for (i, val) in items.iter().enumerate() {
                path.push(Value::Int(i as i64));
                to_stream(val, path, output);
                path.pop();
            }
            // Terminator: path ending at the last index
            path.push(Value::Int((items.len() - 1) as i64));
            output(Value::Array(Arc::new(vec![Value::Array(Arc::new(
                path.clone(),
            ))])));
            path.pop();
        }
        _ => {
            // Scalar, null, empty array, empty object → leaf
            output(Value::Array(Arc::new(vec![
                Value::Array(Arc::new(path.clone())),
                value.clone(),
            ])));
        }
    }
}

/// Reconstruct JSON values from a stream of `[path, value]` pairs and `[path]`
/// terminators. The filter argument `expr` is evaluated against `input` to
/// produce stream items.
fn from_stream(expr: &Filter, input: &Value, env: &Env, output: &mut dyn FnMut(Value)) {
    let mut acc: Option<Value> = None;

    eval(expr, input, env, &mut |item| {
        if let Value::Array(arr) = &item {
            let len = arr.len();
            if len == 2 {
                // [path, value] — set value at path
                if let Value::Array(path_arr) = &arr[0] {
                    if path_arr.is_empty() {
                        // Empty path → emit scalar directly
                        output(arr[1].clone());
                        acc = None;
                    } else {
                        let base = acc.take().unwrap_or(Value::Null);
                        match set_path(&base, path_arr, &arr[1]) {
                            Ok(v) => acc = Some(v),
                            Err(_) => acc = Some(base),
                        }
                    }
                }
            } else if len == 1 {
                // [path] — terminator
                if let Value::Array(path_arr) = &arr[0]
                    && path_arr.len() <= 1
                {
                    // Root container closed — emit accumulated value
                    if let Some(v) = acc.take() {
                        output(v);
                    }
                }
            }
        }
    });
}

/// Drop the first path element from each stream entry produced by `expr`.
/// `truncate_stream(f)` applies `f` to the input, then for each stream entry:
/// - `[path, value]` → `[path[1:], value]`
/// - `[path]` (terminator) → `[path[1:]]` or `[[]]` if path has 1 element
fn truncate_stream(expr: &Filter, input: &Value, env: &Env, output: &mut dyn FnMut(Value)) {
    eval(expr, input, env, &mut |item| {
        if let Value::Array(arr) = &item {
            let len = arr.len();
            if len == 2 {
                // [path, value]
                if let Value::Array(path_arr) = &arr[0] {
                    let new_path = if path_arr.len() > 1 {
                        Value::Array(Arc::new(path_arr[1..].to_vec()))
                    } else {
                        Value::Array(Arc::new(vec![]))
                    };
                    output(Value::Array(Arc::new(vec![new_path, arr[1].clone()])));
                }
            } else if len == 1 {
                // [path] — terminator
                if let Value::Array(path_arr) = &arr[0] {
                    if path_arr.len() > 1 {
                        output(Value::Array(Arc::new(vec![Value::Array(Arc::new(
                            path_arr[1..].to_vec(),
                        ))])));
                    } else {
                        // Path has 0 or 1 element → root terminator [[]]
                        output(Value::Array(Arc::new(vec![Value::Array(Arc::new(vec![]))])));
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_stream(value: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        to_stream(value, &mut Vec::new(), &mut |v| out.push(v));
        out
    }

    fn collect_from_stream(items: Vec<Value>) -> Vec<Value> {
        let stream = Value::Array(Arc::new(items));
        let expr = Filter::Iterate;
        let env = Env::empty();
        let mut out = Vec::new();
        from_stream(&expr, &stream, &env, &mut |v| out.push(v));
        out
    }

    fn path(keys: &[Value]) -> Value {
        Value::Array(Arc::new(keys.to_vec()))
    }

    fn leaf(keys: &[Value], val: Value) -> Value {
        Value::Array(Arc::new(vec![path(keys), val]))
    }

    fn terminator(keys: &[Value]) -> Value {
        Value::Array(Arc::new(vec![path(keys)]))
    }

    #[test]
    fn tostream_scalar() {
        // Scalars emit a single leaf with empty path
        let entries = collect_stream(&Value::Int(42));
        assert_eq!(entries, vec![leaf(&[], Value::Int(42))]);
    }

    #[test]
    fn tostream_empty_array() {
        let entries = collect_stream(&Value::Array(Arc::new(vec![])));
        assert_eq!(entries, vec![leaf(&[], Value::Array(Arc::new(vec![])))]);
    }

    #[test]
    fn tostream_empty_object() {
        let entries = collect_stream(&Value::Object(Arc::new(vec![])));
        assert_eq!(entries, vec![leaf(&[], Value::Object(Arc::new(vec![])))]);
    }

    #[test]
    fn tostream_flat_object() {
        let obj = Value::Object(Arc::new(vec![
            ("a".to_string(), Value::Int(1)),
            ("b".to_string(), Value::Int(2)),
        ]));
        let entries = collect_stream(&obj);
        assert_eq!(
            entries,
            vec![
                leaf(&[Value::String("a".into())], Value::Int(1)),
                leaf(&[Value::String("b".into())], Value::Int(2)),
                terminator(&[Value::String("b".into())]),
            ]
        );
    }

    #[test]
    fn tostream_flat_array() {
        let arr = Value::Array(Arc::new(vec![Value::Int(10), Value::Int(20)]));
        let entries = collect_stream(&arr);
        assert_eq!(
            entries,
            vec![
                leaf(&[Value::Int(0)], Value::Int(10)),
                leaf(&[Value::Int(1)], Value::Int(20)),
                terminator(&[Value::Int(1)]),
            ]
        );
    }

    #[test]
    fn tostream_nested() {
        // {"a":{"x":1}}
        let inner = Value::Object(Arc::new(vec![("x".to_string(), Value::Int(1))]));
        let obj = Value::Object(Arc::new(vec![("a".to_string(), inner)]));
        let entries = collect_stream(&obj);
        assert_eq!(
            entries,
            vec![
                leaf(
                    &[Value::String("a".into()), Value::String("x".into())],
                    Value::Int(1)
                ),
                terminator(&[Value::String("a".into()), Value::String("x".into())]),
                terminator(&[Value::String("a".into())]),
            ]
        );
    }

    #[test]
    fn fromstream_roundtrip_object() {
        let obj = Value::Object(Arc::new(vec![
            ("a".to_string(), Value::Int(1)),
            ("b".to_string(), Value::Int(2)),
        ]));
        let entries = collect_stream(&obj);
        let rebuilt = collect_from_stream(entries);
        assert_eq!(rebuilt, vec![obj]);
    }

    #[test]
    fn fromstream_roundtrip_array() {
        let arr = Value::Array(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        let entries = collect_stream(&arr);
        let rebuilt = collect_from_stream(entries);
        assert_eq!(rebuilt, vec![arr]);
    }

    #[test]
    fn fromstream_scalar_emits_directly() {
        // [[], 42] → 42 (empty path = scalar passthrough)
        let items = vec![leaf(&[], Value::Int(42))];
        let rebuilt = collect_from_stream(items);
        assert_eq!(rebuilt, vec![Value::Int(42)]);
    }
}
