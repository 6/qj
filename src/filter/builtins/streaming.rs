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
