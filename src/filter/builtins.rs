/// jq builtin functions — the `eval_builtin()` dispatch table.
use crate::filter::{ArithOp, Env, Filter};
use crate::value::Value;
use std::rc::Rc;

use super::eval::{arith_values, eval, recurse, values_equal, values_order};
use super::value_ops::{
    del_path, enum_leaf_paths, enum_paths, f64_to_value, format_strftime_jiff, fromdate,
    input_as_f64, libc_frexp, libc_j0, libc_j1, libc_ldexp, libc_logb, now_timestamp, path_of,
    set_path, to_f64, todate,
};

pub(super) fn eval_builtin(
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
            Value::Double(f, _) => output(Value::Double(f.abs(), None)),
            Value::Bool(_) => output(Value::Null),
        },
        "keys" | "keys_unsorted" => match input {
            Value::Object(obj) => {
                let mut keys: Vec<Value> =
                    obj.iter().map(|(k, _)| Value::String(k.clone())).collect();
                if name == "keys" {
                    keys.sort_by(|a, b| {
                        if let (Value::String(a), Value::String(b)) = (a, b) {
                            a.cmp(b)
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    });
                }
                output(Value::Array(Rc::new(keys)));
            }
            Value::Array(arr) => {
                let keys: Vec<Value> = (0..arr.len() as i64).map(Value::Int).collect();
                output(Value::Array(Rc::new(keys)));
            }
            _ => {}
        },
        "values" => match input {
            Value::Object(obj) => {
                for (_, v) in obj.iter() {
                    output(v.clone());
                }
            }
            Value::Array(arr) => {
                for v in arr.iter() {
                    output(v.clone());
                }
            }
            _ => {}
        },
        "type" => {
            output(Value::String(input.type_name().to_string()));
        }
        "empty" => {
            // Produces no output
        }
        "not" => {
            output(Value::Bool(!input.is_truthy()));
        }
        "null" => output(Value::Null),
        "true" => output(Value::Bool(true)),
        "false" => output(Value::Bool(false)),
        // Type-selector builtins — act like select(type == T)
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
        "map" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut result = Vec::with_capacity(arr.len());
                for item in arr.iter() {
                    eval(f, item, env, &mut |v| result.push(v));
                }
                output(Value::Array(Rc::new(result)));
            }
        }
        "select" => {
            if let Some(cond) = args.first() {
                let mut is_truthy = false;
                eval(cond, input, env, &mut |v| {
                    if v.is_truthy() {
                        is_truthy = true;
                    }
                });
                if is_truthy {
                    output(input.clone());
                }
            }
        }
        "add" => match input {
            Value::Array(arr) if !arr.is_empty() => {
                let mut acc = arr[0].clone();
                for item in &arr[1..] {
                    if let Some(result) = arith_values(&acc, &ArithOp::Add, item) {
                        acc = result;
                    }
                }
                output(acc);
            }
            Value::Array(_) => output(Value::Null),
            _ => {}
        },
        "any" => {
            if let Value::Array(arr) = input {
                if let Some(f) = args.first() {
                    let mut found = false;
                    for item in arr.iter() {
                        eval(f, item, env, &mut |v| {
                            if v.is_truthy() {
                                found = true;
                            }
                        });
                        if found {
                            break;
                        }
                    }
                    output(Value::Bool(found));
                } else {
                    let found = arr.iter().any(|v| v.is_truthy());
                    output(Value::Bool(found));
                }
            }
        }
        "all" => {
            if let Value::Array(arr) = input {
                if let Some(f) = args.first() {
                    let mut all_true = true;
                    for item in arr.iter() {
                        let mut item_true = false;
                        eval(f, item, env, &mut |v| {
                            if v.is_truthy() {
                                item_true = true;
                            }
                        });
                        if !item_true {
                            all_true = false;
                            break;
                        }
                    }
                    output(Value::Bool(all_true));
                } else {
                    let all_true = arr.iter().all(|v| v.is_truthy());
                    output(Value::Bool(all_true));
                }
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
        "to_entries" => {
            if let Value::Object(obj) = input {
                let entries: Vec<Value> = obj
                    .iter()
                    .map(|(k, v)| {
                        Value::Object(Rc::new(vec![
                            ("key".into(), Value::String(k.clone())),
                            ("value".into(), v.clone()),
                        ]))
                    })
                    .collect();
                output(Value::Array(Rc::new(entries)));
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
                output(Value::Object(Rc::new(obj)));
            }
        }
        "tostring" => match input {
            Value::String(_) => output(input.clone()),
            Value::Int(n) => output(Value::String(itoa::Buffer::new().format(*n).into())),
            Value::Double(f, _) => output(Value::String(ryu::Buffer::new().format(*f).into())),
            Value::Bool(b) => output(Value::String(if *b { "true" } else { "false" }.into())),
            Value::Null => output(Value::String("null".into())),
            Value::Array(_) | Value::Object(_) => {
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, input).unwrap();
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
            }
        }
        "ascii_upcase" => {
            if let Value::String(s) = input {
                output(Value::String(
                    s.chars().map(|c| c.to_ascii_uppercase()).collect(),
                ));
            }
        }
        "sort" => {
            if let Value::Array(arr) = input {
                let mut sorted: Vec<Value> = arr.as_ref().clone();
                sorted.sort_by(|a, b| values_order(a, b).unwrap_or(std::cmp::Ordering::Equal));
                output(Value::Array(Rc::new(sorted)));
            }
        }
        "sort_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut pairs: Vec<(Value, Value)> = arr
                    .iter()
                    .map(|item| {
                        let mut key = Value::Null;
                        eval(f, item, env, &mut |v| key = v);
                        (key, item.clone())
                    })
                    .collect();
                pairs.sort_by(|(a, _), (b, _)| {
                    values_order(a, b).unwrap_or(std::cmp::Ordering::Equal)
                });
                output(Value::Array(Rc::new(
                    pairs.into_iter().map(|(_, v)| v).collect(),
                )));
            }
        }
        "group_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut pairs: Vec<(Value, Value)> = arr
                    .iter()
                    .map(|item| {
                        let mut key = Value::Null;
                        eval(f, item, env, &mut |v| key = v);
                        (key, item.clone())
                    })
                    .collect();
                pairs.sort_by(|(a, _), (b, _)| {
                    values_order(a, b).unwrap_or(std::cmp::Ordering::Equal)
                });
                let mut groups: Vec<Value> = Vec::new();
                let mut current_key: Option<Value> = None;
                let mut current_group: Vec<Value> = Vec::new();
                for (key, val) in pairs {
                    if current_key.as_ref().is_some_and(|k| values_equal(k, &key)) {
                        current_group.push(val);
                    } else {
                        if !current_group.is_empty() {
                            groups.push(Value::Array(Rc::new(std::mem::take(&mut current_group))));
                        }
                        current_key = Some(key);
                        current_group.push(val);
                    }
                }
                if !current_group.is_empty() {
                    groups.push(Value::Array(Rc::new(current_group)));
                }
                output(Value::Array(Rc::new(groups)));
            }
        }
        "unique" => {
            if let Value::Array(arr) = input {
                let mut sorted: Vec<Value> = arr.as_ref().clone();
                sorted.sort_by(|a, b| values_order(a, b).unwrap_or(std::cmp::Ordering::Equal));
                sorted.dedup_by(|a, b| values_equal(a, b));
                output(Value::Array(Rc::new(sorted)));
            }
        }
        "unique_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut seen_keys: Vec<Value> = Vec::new();
                let mut result: Vec<Value> = Vec::new();
                for item in arr.iter() {
                    let mut key = Value::Null;
                    eval(f, item, env, &mut |v| key = v);
                    if !seen_keys.iter().any(|k| values_equal(k, &key)) {
                        seen_keys.push(key);
                        result.push(item.clone());
                    }
                }
                output(Value::Array(Rc::new(result)));
            }
        }
        "flatten" => {
            if let Value::Array(arr) = input {
                let depth = args.first().map_or(i64::MAX, |f| {
                    let mut d = Value::Null;
                    eval(f, input, env, &mut |v| d = v);
                    match d {
                        Value::Int(n) => n,
                        _ => i64::MAX,
                    }
                });
                let mut result = Vec::new();
                flatten_array(arr, depth, &mut result);
                output(Value::Array(Rc::new(result)));
            }
        }
        "first" => {
            if let Some(f) = args.first() {
                let mut found = false;
                eval(f, input, env, &mut |v| {
                    if !found {
                        output(v);
                        found = true;
                    }
                });
            } else if let Value::Array(arr) = input
                && let Some(v) = arr.first()
            {
                output(v.clone());
            }
        }
        "last" => {
            if let Some(f) = args.first() {
                let mut last = None;
                eval(f, input, env, &mut |v| last = Some(v));
                if let Some(v) = last {
                    output(v);
                }
            } else if let Value::Array(arr) = input
                && let Some(v) = arr.last()
            {
                output(v.clone());
            }
        }
        "reverse" => {
            if let Value::Array(arr) = input {
                let mut result: Vec<Value> = arr.as_ref().clone();
                result.reverse();
                output(Value::Array(Rc::new(result)));
            } else if let Value::String(s) = input {
                output(Value::String(s.chars().rev().collect()));
            }
        }
        "min" => {
            if let Value::Array(arr) = input {
                if let Some(min) = arr
                    .iter()
                    .min_by(|a, b| values_order(a, b).unwrap_or(std::cmp::Ordering::Equal))
                {
                    output(min.clone());
                } else {
                    output(Value::Null);
                }
            }
        }
        "max" => {
            if let Value::Array(arr) = input {
                if let Some(max) = arr
                    .iter()
                    .max_by(|a, b| values_order(a, b).unwrap_or(std::cmp::Ordering::Equal))
                {
                    output(max.clone());
                } else {
                    output(Value::Null);
                }
            }
        }
        "min_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut best: Option<(Value, Value)> = None;
                for item in arr.iter() {
                    let mut key = Value::Null;
                    eval(f, item, env, &mut |v| key = v);
                    if best.as_ref().is_none_or(|(bk, _)| {
                        values_order(&key, bk) == Some(std::cmp::Ordering::Less)
                    }) {
                        best = Some((key, item.clone()));
                    }
                }
                if let Some((_, v)) = best {
                    output(v);
                } else {
                    output(Value::Null);
                }
            }
        }
        "max_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut best: Option<(Value, Value)> = None;
                for item in arr.iter() {
                    let mut key = Value::Null;
                    eval(f, item, env, &mut |v| key = v);
                    if best.as_ref().is_none_or(|(bk, _)| {
                        values_order(&key, bk) == Some(std::cmp::Ordering::Greater)
                    }) {
                        best = Some((key, item.clone()));
                    }
                }
                if let Some((_, v)) = best {
                    output(v);
                } else {
                    output(Value::Null);
                }
            }
        }
        "del" => {
            // del(.field) — remove a field from an object
            if let Some(Filter::Field(name)) = args.first()
                && let Value::Object(obj) = input
            {
                let result: Vec<(String, Value)> =
                    obj.iter().filter(|(k, _)| k != name).cloned().collect();
                output(Value::Object(Rc::new(result)));
                return;
            }
            output(input.clone());
        }
        "contains" => {
            if let (Some(arg), _) = (args.first(), input) {
                let mut pattern = Value::Null;
                eval(arg, input, env, &mut |v| pattern = v);
                output(Value::Bool(value_contains(input, &pattern)));
            }
        }
        "ltrimstr" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut prefix = Value::Null;
                eval(arg, input, env, &mut |v| prefix = v);
                if let Value::String(p) = prefix {
                    output(Value::String(
                        s.strip_prefix(p.as_str()).unwrap_or(s).to_string(),
                    ));
                } else {
                    output(input.clone());
                }
            }
        }
        "rtrimstr" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut suffix = Value::Null;
                eval(arg, input, env, &mut |v| suffix = v);
                if let Value::String(p) = suffix {
                    output(Value::String(
                        s.strip_suffix(p.as_str()).unwrap_or(s).to_string(),
                    ));
                } else {
                    output(input.clone());
                }
            }
        }
        "startswith" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut prefix = Value::Null;
                eval(arg, input, env, &mut |v| prefix = v);
                if let Value::String(p) = prefix {
                    output(Value::Bool(s.starts_with(p.as_str())));
                }
            }
        }
        "endswith" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut suffix = Value::Null;
                eval(arg, input, env, &mut |v| suffix = v);
                if let Value::String(p) = suffix {
                    output(Value::Bool(s.ends_with(p.as_str())));
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
                    output(Value::Array(Rc::new(parts)));
                }
            }
        }
        "join" => {
            if let (Value::Array(arr), Some(arg)) = (input, args.first()) {
                let mut sep = Value::Null;
                eval(arg, input, env, &mut |v| sep = v);
                if let Value::String(p) = sep {
                    let strs: Vec<String> = arr
                        .iter()
                        .filter_map(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            Value::Int(n) => Some(itoa::Buffer::new().format(*n).into()),
                            Value::Double(f, _) => Some(ryu::Buffer::new().format(*f).into()),
                            Value::Null => Some(String::new()),
                            Value::Bool(b) => Some(if *b { "true" } else { "false" }.into()),
                            _ => None,
                        })
                        .collect();
                    output(Value::String(strs.join(&p)));
                }
            }
        }
        // --- range ---
        "range" => {
            match args.len() {
                1 => {
                    // range(n) → 0..n
                    eval(&args[0], input, env, &mut |nv| {
                        let n = to_f64(&nv);
                        let mut i = 0.0;
                        while i < n {
                            output(f64_to_value(i));
                            i += 1.0;
                        }
                    });
                }
                2 => {
                    // range(from; to)
                    eval(&args[0], input, env, &mut |from_v| {
                        eval(&args[1], input, env, &mut |to_v| {
                            let from = to_f64(&from_v);
                            let to = to_f64(&to_v);
                            let mut i = from;
                            while i < to {
                                output(f64_to_value(i));
                                i += 1.0;
                            }
                        });
                    });
                }
                3 => {
                    // range(from; to; step)
                    eval(&args[0], input, env, &mut |from_v| {
                        eval(&args[1], input, env, &mut |to_v| {
                            eval(&args[2], input, env, &mut |step_v| {
                                let from = to_f64(&from_v);
                                let to = to_f64(&to_v);
                                let step = to_f64(&step_v);
                                if step == 0.0 {
                                    return;
                                }
                                let mut i = from;
                                if step > 0.0 {
                                    while i < to {
                                        output(f64_to_value(i));
                                        i += step;
                                    }
                                } else {
                                    while i > to {
                                        output(f64_to_value(i));
                                        i += step;
                                    }
                                }
                            });
                        });
                    });
                }
                _ => {}
            }
        }
        // --- Math builtins (zero-arg, operate on input number) ---
        "floor" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.floor()));
            }
        }
        "ceil" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.ceil()));
            }
        }
        "round" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.round()));
            }
        }
        "trunc" | "truncate" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.trunc()));
            }
        }
        "fabs" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.abs(), None));
            }
        }
        "sqrt" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.sqrt(), None));
            }
        }
        "cbrt" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.cbrt(), None));
            }
        }
        "log" | "log_e" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.ln(), None));
            }
        }
        "log2" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.log2(), None));
            }
        }
        "log10" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.log10(), None));
            }
        }
        "logb" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(libc_logb(f), None));
            }
        }
        "exp" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.exp(), None));
            }
        }
        "exp2" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.exp2(), None));
            }
        }
        "sin" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.sin(), None));
            }
        }
        "cos" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.cos(), None));
            }
        }
        "tan" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.tan(), None));
            }
        }
        "asin" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.asin(), None));
            }
        }
        "acos" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.acos(), None));
            }
        }
        "atan" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.atan(), None));
            }
        }
        "sinh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.sinh(), None));
            }
        }
        "cosh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.cosh(), None));
            }
        }
        "tanh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.tanh(), None));
            }
        }
        "asinh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.asinh(), None));
            }
        }
        "acosh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.acosh(), None));
            }
        }
        "atanh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.atanh(), None));
            }
        }
        "significand" | "nearbyint" | "rint" => {
            if let Some(f) = input_as_f64(input) {
                let result = match name {
                    "significand" => {
                        if f == 0.0 {
                            0.0
                        } else {
                            let (_, exp) = libc_frexp(f);
                            f * (2.0_f64).powi(-(exp - 1))
                        }
                    }
                    _ => f.round(),
                };
                output(Value::Double(result, None));
            }
        }
        "scalb" => {
            // scalb(x; e) = x * 2^e — two-arg builtin
            if let (Some(base), Some(arg)) = (input_as_f64(input), args.first()) {
                let mut exp = 0i32;
                eval(arg, input, env, &mut |v| exp = to_f64(&v) as i32);
                output(f64_to_value(libc_ldexp(base, exp)));
            }
        }
        "exponent" => {
            if let Some(f) = input_as_f64(input) {
                let (_, exp) = libc_frexp(f);
                output(Value::Int(exp as i64));
            }
        }
        "j0" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(libc_j0(f), None));
            }
        }
        "j1" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(libc_j1(f), None));
            }
        }
        // Math constants/predicates
        "nan" => output(Value::Double(f64::NAN, None)),
        "infinite" | "inf" => output(Value::Double(f64::INFINITY, None)),
        "isnan" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_nan()));
            } else {
                output(Value::Bool(false));
            }
        }
        "isinfinite" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_infinite()));
            } else {
                output(Value::Bool(false));
            }
        }
        "isfinite" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_finite()));
            } else {
                output(Value::Bool(false));
            }
        }
        "isnormal" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_normal()));
            } else {
                output(Value::Bool(false));
            }
        }
        // Two-arg math
        "pow" => {
            if let (Some(base_f), Some(exp_f)) = (args.first(), args.get(1)) {
                let mut base = 0.0_f64;
                let mut exp = 0.0_f64;
                eval(base_f, input, env, &mut |v| base = to_f64(&v));
                eval(exp_f, input, env, &mut |v| exp = to_f64(&v));
                output(Value::Double(base.powf(exp), None));
            } else if args.len() == 1 {
                // pow(x; y) where input is piped
                // Actually: in jq, pow(a;b) takes two semicolon-separated args
                if let Some(f) = input_as_f64(input) {
                    let mut exp = 0.0_f64;
                    eval(&args[0], input, env, &mut |v| exp = to_f64(&v));
                    output(Value::Double(f.powf(exp), None));
                }
            }
        }
        "atan2" => {
            if let (Some(y_f), Some(x_f)) = (args.first(), args.get(1)) {
                let mut y = 0.0_f64;
                let mut x = 0.0_f64;
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                output(Value::Double(y.atan2(x), None));
            }
        }
        "remainder" => {
            if let (Some(x_f), Some(y_f)) = (args.first(), args.get(1)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                // IEEE remainder
                output(Value::Double(x - (x / y).round() * y, None));
            }
        }
        "hypot" => {
            if let (Some(x_f), Some(y_f)) = (args.first(), args.get(1)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                output(Value::Double(x.hypot(y), None));
            }
        }
        "fma" => {
            if let (Some(x_f), Some(y_f), Some(z_f)) = (args.first(), args.get(1), args.get(2)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                let mut z = 0.0_f64;
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                eval(z_f, input, env, &mut |v| z = to_f64(&v));
                output(Value::Double(x.mul_add(y, z), None));
            }
        }
        "abs" => match input {
            Value::Int(n) => output(
                n.checked_abs()
                    .map_or_else(|| Value::Double((*n as f64).abs(), None), Value::Int),
            ),
            Value::Double(f, _) => output(Value::Double(f.abs(), None)),
            _ => output(input.clone()),
        },
        // --- String builtins ---
        "trim" => {
            if let Value::String(s) = input {
                output(Value::String(s.trim().to_string()));
            }
        }
        "ltrim" => {
            if let Value::String(s) = input {
                output(Value::String(s.trim_start().to_string()));
            }
        }
        "rtrim" => {
            if let Value::String(s) = input {
                output(Value::String(s.trim_end().to_string()));
            }
        }
        "index" => {
            if let Some(arg) = args.first() {
                let mut needle = Value::Null;
                eval(arg, input, env, &mut |v| needle = v);
                match (input, &needle) {
                    (Value::String(s), Value::String(n)) => {
                        if let Some(byte_pos) = s.find(n.as_str()) {
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
                }
            }
        }
        "rindex" => {
            if let Some(arg) = args.first() {
                let mut needle = Value::Null;
                eval(arg, input, env, &mut |v| needle = v);
                match (input, &needle) {
                    (Value::String(s), Value::String(n)) => {
                        if let Some(byte_pos) = s.rfind(n.as_str()) {
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
                }
            }
        }
        "indices" | "_indices" => {
            if let Some(arg) = args.first() {
                let mut needle = Value::Null;
                eval(arg, input, env, &mut |v| needle = v);
                match (input, &needle) {
                    (Value::String(s), Value::String(n)) => {
                        let mut positions = Vec::new();
                        if !n.is_empty() {
                            let mut start = 0;
                            while let Some(byte_pos) = s[start..].find(n.as_str()) {
                                let abs_byte = start + byte_pos;
                                positions.push(Value::Int(s[..abs_byte].chars().count() as i64));
                                start = abs_byte + n.len();
                            }
                        }
                        output(Value::Array(Rc::new(positions)));
                    }
                    (Value::Array(arr), _) => {
                        let positions: Vec<Value> = arr
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| values_equal(v, &needle))
                            .map(|(i, _)| Value::Int(i as i64))
                            .collect();
                        output(Value::Array(Rc::new(positions)));
                    }
                    _ => output(Value::Array(Rc::new(Vec::new()))),
                }
            }
        }
        "explode" => {
            if let Value::String(s) = input {
                let codepoints: Vec<Value> = s.chars().map(|c| Value::Int(c as i64)).collect();
                output(Value::Array(Rc::new(codepoints)));
            }
        }
        "implode" => {
            if let Value::Array(arr) = input {
                let s: String = arr
                    .iter()
                    .filter_map(|v| match v {
                        Value::Int(n) => char::from_u32(*n as u32),
                        _ => None,
                    })
                    .collect();
                output(Value::String(s));
            }
        }
        "tojson" => {
            let mut buf = Vec::new();
            crate::output::write_compact(&mut buf, input).unwrap();
            output(Value::String(String::from_utf8(buf).unwrap_or_default()));
        }
        "fromjson" => {
            if let Value::String(s) = input {
                let padded = crate::simdjson::pad_buffer(s.as_bytes());
                if let Ok(val) = crate::simdjson::dom_parse_to_value(&padded, s.len()) {
                    output(val);
                }
            }
        }
        "utf8bytelength" => {
            if let Value::String(s) = input {
                output(Value::Int(s.len() as i64));
            }
        }
        "inside" => {
            if let Some(arg) = args.first() {
                let mut container = Value::Null;
                eval(arg, input, env, &mut |v| container = v);
                output(Value::Bool(value_contains(&container, input)));
            }
        }
        // --- Collection builtins ---
        "transpose" => {
            if let Value::Array(arr) = input {
                let max_len = arr
                    .iter()
                    .filter_map(|v| {
                        if let Value::Array(inner) = v {
                            Some(inner.len())
                        } else {
                            None
                        }
                    })
                    .max()
                    .unwrap_or(0);
                let mut result = Vec::with_capacity(max_len);
                for i in 0..max_len {
                    let col: Vec<Value> = arr
                        .iter()
                        .map(|v| {
                            if let Value::Array(inner) = v {
                                inner.get(i).cloned().unwrap_or(Value::Null)
                            } else {
                                Value::Null
                            }
                        })
                        .collect();
                    result.push(Value::Array(Rc::new(col)));
                }
                output(Value::Array(Rc::new(result)));
            }
        }
        "map_values" => {
            if let Some(f) = args.first() {
                match input {
                    Value::Object(obj) => {
                        let mut result = Vec::with_capacity(obj.len());
                        for (k, v) in obj.iter() {
                            let mut new_val = Value::Null;
                            eval(f, v, env, &mut |nv| new_val = nv);
                            result.push((k.clone(), new_val));
                        }
                        output(Value::Object(Rc::new(result)));
                    }
                    Value::Array(arr) => {
                        let mut result = Vec::with_capacity(arr.len());
                        for v in arr.iter() {
                            eval(f, v, env, &mut |nv| result.push(nv));
                        }
                        output(Value::Array(Rc::new(result)));
                    }
                    _ => output(input.clone()),
                }
            }
        }
        "limit" => {
            if args.len() == 2 {
                let mut n = 0i64;
                eval(&args[0], input, env, &mut |v| n = to_f64(&v) as i64);
                let mut count = 0i64;
                eval(&args[1], input, env, &mut |v| {
                    if count < n {
                        output(v);
                        count += 1;
                    }
                });
            }
        }
        "until" => {
            if args.len() == 2 {
                let mut current = input.clone();
                for _ in 0..1_000_000 {
                    let mut done = false;
                    eval(&args[0], &current, env, &mut |v| done = v.is_truthy());
                    if done {
                        break;
                    }
                    let mut next = current.clone();
                    eval(&args[1], &current, env, &mut |v| next = v);
                    if values_equal(&next, &current) {
                        break;
                    }
                    current = next;
                }
                output(current);
            }
        }
        "while" => {
            if args.len() == 2 {
                let mut current = input.clone();
                for _ in 0..1_000_000 {
                    let mut cont = false;
                    eval(&args[0], &current, env, &mut |v| cont = v.is_truthy());
                    if !cont {
                        break;
                    }
                    output(current.clone());
                    let mut next = current.clone();
                    eval(&args[1], &current, env, &mut |v| next = v);
                    if values_equal(&next, &current) {
                        break;
                    }
                    current = next;
                }
            }
        }
        "repeat" => {
            // repeat(f) = def repeat(f): f, repeat(f)
            // Applies f to the same input each time, producing infinite stream
            if let Some(f) = args.first() {
                for _ in 0..1_000_000 {
                    eval(f, input, env, output);
                }
            }
        }
        "isempty" => {
            if let Some(f) = args.first() {
                let mut found = false;
                eval(f, input, env, &mut |_| found = true);
                output(Value::Bool(!found));
            }
        }
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
                    output(set_path(input, &path_arr, &val));
                }
            }
        }
        "delpaths" => {
            if let Some(arg) = args.first() {
                let mut paths = Value::Null;
                eval(arg, input, env, &mut |v| paths = v);
                if let Value::Array(path_list) = paths {
                    let mut current = input.clone();
                    // Sort paths in reverse to delete deepest first
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
                }
            }
        }
        "paths" => {
            if args.is_empty() {
                // paths — enumerate all paths to leaf values
                enum_paths(input, &mut Vec::new(), output, None);
            } else {
                // paths(filter) — only paths where filter is truthy
                enum_paths(input, &mut Vec::new(), output, Some(&args[0]));
            }
        }
        "leaf_paths" => {
            enum_leaf_paths(input, &mut Vec::new(), output);
        }
        "path" => {
            // path(expr) — output path arrays for each result of expr
            if let Some(f) = args.first() {
                path_of(f, input, &mut Vec::new(), output);
            }
        }
        "builtins" => {
            let names = vec![
                "length",
                "utf8bytelength",
                "keys",
                "keys_unsorted",
                "values",
                "type",
                "empty",
                "not",
                "null",
                "true",
                "false",
                "numbers",
                "strings",
                "booleans",
                "nulls",
                "arrays",
                "objects",
                "iterables",
                "scalars",
                "map",
                "select",
                "add",
                "any",
                "all",
                "has",
                "to_entries",
                "from_entries",
                "with_entries",
                "tostring",
                "tonumber",
                "ascii_downcase",
                "ascii_upcase",
                "sort",
                "sort_by",
                "group_by",
                "unique",
                "unique_by",
                "flatten",
                "first",
                "last",
                "reverse",
                "min",
                "max",
                "min_by",
                "max_by",
                "del",
                "contains",
                "inside",
                "ltrimstr",
                "rtrimstr",
                "startswith",
                "endswith",
                "split",
                "join",
                "range",
                "floor",
                "ceil",
                "round",
                "sqrt",
                "pow",
                "log",
                "log2",
                "log10",
                "exp",
                "exp2",
                "fabs",
                "nan",
                "infinite",
                "isnan",
                "isinfinite",
                "isfinite",
                "isnormal",
                "abs",
                "trim",
                "ltrim",
                "rtrim",
                "index",
                "rindex",
                "indices",
                "explode",
                "implode",
                "tojson",
                "fromjson",
                "transpose",
                "map_values",
                "limit",
                "until",
                "while",
                "isempty",
                "getpath",
                "setpath",
                "delpaths",
                "paths",
                "leaf_paths",
                "builtins",
                "input",
                "debug",
                "error",
                "env",
                "ascii",
                "nth",
                "repeat",
                "recurse",
                "walk",
                "bsearch",
                "path",
                "todate",
                "fromdate",
                "now",
            ];
            let arr: Vec<Value> = names.iter().map(|n| Value::String(n.to_string())).collect();
            output(Value::Array(Rc::new(arr)));
        }
        "input" => {
            // TODO: requires input stream plumbing to read next JSON value from stdin
            // Producing no output is safer than identity (which gives wrong results)
        }
        "debug" => {
            if let Some(arg) = args.first() {
                let mut label = String::new();
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        label = s;
                    }
                });
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, input).unwrap();
                let json = String::from_utf8(buf).unwrap_or_default();
                if label.is_empty() {
                    eprintln!("[\"DEBUG:\",{json}]");
                } else {
                    eprintln!("[\"{label}\",{json}]");
                }
            } else {
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, input).unwrap();
                let json = String::from_utf8(buf).unwrap_or_default();
                eprintln!("[\"DEBUG:\",{json}]");
            }
            output(input.clone());
        }
        "error" => {
            let err_val = if let Some(arg) = args.first() {
                let mut msg = Value::Null;
                eval(arg, input, env, &mut |v| msg = v);
                msg
            } else {
                input.clone()
            };
            super::eval::LAST_ERROR.with(|e| *e.borrow_mut() = Some(err_val));
            // Produce no output (error in jq)
        }
        "env" | "$ENV" => {
            let vars: Vec<(String, Value)> = std::env::vars()
                .map(|(k, v)| (k, Value::String(v)))
                .collect();
            output(Value::Object(Rc::new(vars)));
        }
        "ascii" => {
            if let Value::String(s) = input
                && let Some(c) = s.chars().next()
            {
                output(Value::Int(c as i64));
            }
        }
        "nth" => {
            if args.len() == 2 {
                let mut n = 0i64;
                eval(&args[0], input, env, &mut |v| n = to_f64(&v) as i64);
                let mut count = 0i64;
                eval(&args[1], input, env, &mut |v| {
                    if count == n {
                        output(v);
                    }
                    count += 1;
                });
            } else if args.len() == 1 {
                // nth(n) operates on input as generator — take nth from .[]
                let mut n = 0i64;
                eval(&args[0], input, env, &mut |v| n = to_f64(&v) as i64);
                if let Value::Array(arr) = input
                    && n >= 0
                    && (n as usize) < arr.len()
                {
                    output(arr[n as usize].clone());
                }
            }
        }
        "recurse" => {
            if args.is_empty() {
                // recurse with no args = ..
                recurse(input, output);
            } else if args.len() == 1 {
                recurse_with_filter(&args[0], input, env, output, 100_000);
            } else if args.len() == 2 {
                // recurse(f; cond) — recurse while cond is truthy
                recurse_with_cond(&args[0], &args[1], input, env, output, 100_000);
            }
        }
        "walk" => {
            if let Some(f) = args.first() {
                fn walk_inner(value: &Value, f: &Filter, env: &Env, output: &mut dyn FnMut(Value)) {
                    // Bottom-up: first recurse children, then apply f
                    match value {
                        Value::Array(arr) => {
                            let mut new_arr = Vec::with_capacity(arr.len());
                            for v in arr.iter() {
                                walk_inner(v, f, env, &mut |walked| {
                                    new_arr.push(walked);
                                });
                            }
                            let reconstructed = Value::Array(Rc::new(new_arr));
                            eval(f, &reconstructed, env, output);
                        }
                        Value::Object(obj) => {
                            let mut new_obj = Vec::with_capacity(obj.len());
                            for (k, v) in obj.iter() {
                                walk_inner(v, f, env, &mut |walked| {
                                    new_obj.push((k.clone(), walked));
                                });
                            }
                            let reconstructed = Value::Object(Rc::new(new_obj));
                            eval(f, &reconstructed, env, output);
                        }
                        _ => {
                            eval(f, value, env, output);
                        }
                    }
                }
                walk_inner(input, f, env, output);
            }
        }
        "bsearch" => {
            if let (Value::Array(arr), Some(arg)) = (input, args.first()) {
                let mut target = Value::Null;
                eval(arg, input, env, &mut |v| target = v);
                // Binary search on sorted array
                let mut lo: i64 = 0;
                let mut hi: i64 = arr.len() as i64 - 1;
                let mut result: i64 = -(arr.len() as i64) - 1;
                while lo <= hi {
                    let mid = lo + (hi - lo) / 2;
                    match values_order(&arr[mid as usize], &target) {
                        Some(std::cmp::Ordering::Equal) => {
                            result = mid;
                            break;
                        }
                        Some(std::cmp::Ordering::Less) => lo = mid + 1,
                        _ => hi = mid - 1,
                    }
                }
                if result < 0 {
                    // Not found: return -insertion_point - 1
                    result = -lo - 1;
                }
                output(Value::Int(result));
            }
        }
        "IN" => {
            match args.len() {
                1 => {
                    // IN(generator) — test if input is in generator's outputs
                    let mut found = false;
                    eval(&args[0], input, env, &mut |v| {
                        if !found && values_equal(input, &v) {
                            found = true;
                        }
                    });
                    output(Value::Bool(found));
                }
                2 => {
                    // IN(stream; generator) — for each output of stream, test if in generator
                    eval(&args[0], input, env, &mut |sv| {
                        let mut found = false;
                        eval(&args[1], input, env, &mut |gv| {
                            if !found && values_equal(&sv, &gv) {
                                found = true;
                            }
                        });
                        output(Value::Bool(found));
                    });
                }
                _ => {}
            }
        }
        "with_entries" => {
            if let (Value::Object(obj), Some(f)) = (input, args.first()) {
                let entries: Vec<Value> = obj
                    .iter()
                    .map(|(k, v)| {
                        Value::Object(Rc::new(vec![
                            ("key".into(), Value::String(k.clone())),
                            ("value".into(), v.clone()),
                        ]))
                    })
                    .collect();
                let entries_val = Value::Array(Rc::new(entries));
                // map(f) | from_entries
                let mut mapped = Vec::new();
                if let Value::Array(arr) = &entries_val {
                    for item in arr.iter() {
                        eval(f, item, env, &mut |v| mapped.push(v));
                    }
                }
                let mut result_obj = Vec::new();
                for entry in &mapped {
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
                        result_obj.push((key, val));
                    }
                }
                output(Value::Object(Rc::new(result_obj)));
            }
        }
        // --- Date/time builtins (jiff) ---
        "todate" => {
            if let Some(ts) = input_as_f64(input)
                && let Some(s) = todate(ts as i64)
            {
                output(Value::String(s));
            }
        }
        "fromdate" => {
            if let Value::String(s) = input
                && let Some(ts) = fromdate(s)
            {
                output(Value::Int(ts));
            }
        }
        "now" => {
            output(Value::Double(now_timestamp(), None));
        }
        "strftime" => {
            if let (Some(arg), Some(ts)) = (args.first(), input_as_f64(input)) {
                let mut fmt = String::new();
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        fmt = s;
                    }
                });
                if let Some(s) = format_strftime_jiff(&fmt, ts as i64) {
                    output(Value::String(s));
                }
            }
        }
        _ => {
            // Unknown builtin — silently produce no output
        }
    }
}

fn flatten_array(arr: &[Value], depth: i64, result: &mut Vec<Value>) {
    for item in arr {
        if let Value::Array(inner) = item
            && depth > 0
        {
            flatten_array(inner, depth - 1, result);
            continue;
        }
        result.push(item.clone());
    }
}

fn value_contains(haystack: &Value, needle: &Value) -> bool {
    match (haystack, needle) {
        (Value::String(h), Value::String(n)) => h.contains(n.as_str()),
        (Value::Array(h), Value::Array(n)) => {
            n.iter().all(|nv| h.iter().any(|hv| value_contains(hv, nv)))
        }
        (Value::Object(h), Value::Object(n)) => n
            .iter()
            .all(|(nk, nv)| h.iter().any(|(hk, hv)| hk == nk && value_contains(hv, nv))),
        _ => values_equal(haystack, needle),
    }
}

fn recurse_with_filter(
    f: &Filter,
    value: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    output(value.clone());
    eval(f, value, env, &mut |v| {
        if v != Value::Null || matches!(value, Value::Array(_) | Value::Object(_)) {
            // Avoid infinite recursion on atoms producing null
            if !values_equal(&v, value) {
                recurse_with_filter(f, &v, env, output, limit - 1);
            }
        }
    });
}

fn recurse_with_cond(
    f: &Filter,
    cond: &Filter,
    value: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    let mut is_match = false;
    eval(cond, value, env, &mut |v| {
        if v.is_truthy() {
            is_match = true;
        }
    });
    if !is_match {
        return;
    }
    output(value.clone());
    eval(f, value, env, &mut |v| {
        if !values_equal(&v, value) {
            recurse_with_cond(f, cond, &v, env, output, limit - 1);
        }
    });
}
