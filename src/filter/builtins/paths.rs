use crate::filter::{Env, Filter};
use crate::value::Value;

use super::super::eval::{LAST_ERROR, eval};
use super::super::value_ops::{
    del_path, enum_leaf_paths, enum_paths, path_of, set_path, values_order,
};

pub(super) fn eval_paths(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "getpath" => {
            if let Some(arg) = args.first() {
                let mut path = Value::Null;
                eval(arg, input, env, &mut |v| path = v);
                if let Value::Array(path_arr) = path {
                    let mut current = input.clone();
                    for seg in path_arr.iter() {
                        current = match (&current, seg) {
                            (Value::Object(obj), Value::String(k)) => obj
                                .iter()
                                .find(|(ek, _)| ek == k)
                                .map(|(_, v)| v.clone())
                                .unwrap_or(Value::Null),
                            (Value::Array(arr), Value::Int(i)) => {
                                let idx = if *i < 0 { arr.len() as i64 + i } else { *i };
                                if idx >= 0 && (idx as usize) < arr.len() {
                                    arr[idx as usize].clone()
                                } else {
                                    Value::Null
                                }
                            }
                            _ => Value::Null,
                        };
                    }
                    output(current);
                }
            }
        }
        "setpath" => {
            if args.len() == 2 {
                let mut path = Value::Null;
                let mut val = Value::Null;
                eval(&args[0], input, env, &mut |v| path = v);
                eval(&args[1], input, env, &mut |v| val = v);
                if let Value::Array(path_arr) = path {
                    match set_path(input, &path_arr, &val) {
                        Ok(v) => output(v),
                        Err(msg) => {
                            LAST_ERROR.with(|e| *e.borrow_mut() = Some(Value::String(msg)));
                        }
                    }
                }
            }
        }
        "delpaths" => {
            if let Some(arg) = args.first() {
                let mut paths = Value::Null;
                eval(arg, input, env, &mut |v| paths = v);
                if let Value::Array(path_list) = paths {
                    let mut current = input.clone();
                    let mut sorted: Vec<_> = path_list
                        .iter()
                        .filter_map(|v| {
                            if let Value::Array(p) = v {
                                Some(p.clone())
                            } else {
                                None
                            }
                        })
                        .collect();
                    sorted.sort_by(|a, b| {
                        values_order(&Value::Array(b.clone()), &Value::Array(a.clone()))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    for path in &sorted {
                        current = del_path(&current, path);
                    }
                    output(current);
                } else {
                    LAST_ERROR.with(|e| {
                        *e.borrow_mut() = Some(Value::String(
                            "Paths must be specified as an array".to_string(),
                        ));
                    });
                }
            }
        }
        "paths" => {
            if args.is_empty() {
                enum_paths(input, &mut Vec::new(), output, None);
            } else {
                enum_paths(input, &mut Vec::new(), output, Some(&args[0]));
            }
        }
        "leaf_paths" => {
            enum_leaf_paths(input, &mut Vec::new(), output);
        }
        "path" => {
            if let Some(f) = args.first() {
                path_of(f, input, &mut Vec::new(), output);
            }
        }
        _ => {}
    }
}
