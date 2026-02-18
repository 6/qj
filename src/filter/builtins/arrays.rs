use crate::filter::{ArithOp, Env, Filter};
use crate::value::Value;
use std::sync::Arc;

use super::super::eval::{LAST_ERROR, eval};
use super::super::value_ops::{arith_values, recurse, to_f64, values_equal, values_order};
use super::set_error;

/// Maximum iterations for `until`, `while`, and `repeat` builtins.
const MAX_LOOP_ITERATIONS: usize = 1_000_000;

pub(super) fn eval_arrays(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
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
                output(Value::Array(Arc::new(keys)));
            }
            Value::Array(arr) => {
                let keys: Vec<Value> = (0..arr.len() as i64).map(Value::Int).collect();
                output(Value::Array(Arc::new(keys)));
            }
            Value::Null => output(Value::Null),
            _ => {
                set_error(format!(
                    "{} ({}) has no keys",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        },
        "values" => {
            // values = select(. != null): passes through any non-null input
            if !matches!(input, Value::Null) {
                output(input.clone());
            }
        }
        "map" => {
            if let Some(f) = args.first() {
                match input {
                    Value::Array(arr) => {
                        let mut result = Vec::with_capacity(arr.len());
                        for item in arr.iter() {
                            eval(f, item, env, &mut |v| result.push(v));
                        }
                        output(Value::Array(Arc::new(result)));
                    }
                    Value::Object(obj) => {
                        // jq: map(f) on objects applies f to each value, returns array
                        let mut result = Vec::with_capacity(obj.len());
                        for (_, v) in obj.iter() {
                            eval(f, v, env, &mut |v| result.push(v));
                        }
                        output(Value::Array(Arc::new(result)));
                    }
                    _ => {}
                }
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
        "add" => {
            if !args.is_empty() {
                // add(f) — reduce f as $x (null; . + $x)
                let mut acc = Value::Null;
                let mut has_val = false;
                for arg in args {
                    eval(arg, input, env, &mut |v| {
                        if !has_val {
                            acc = v;
                            has_val = true;
                        } else if let Ok(result) = arith_values(&acc, &ArithOp::Add, &v) {
                            acc = result;
                        }
                    });
                }
                output(acc);
            } else {
                match input {
                    Value::Array(arr) if !arr.is_empty() => {
                        let mut acc = arr[0].clone();
                        for item in &arr[1..] {
                            if let Ok(result) = arith_values(&acc, &ArithOp::Add, item) {
                                acc = result;
                            }
                        }
                        output(acc);
                    }
                    Value::Array(_) => output(Value::Null),
                    _ => {
                        set_error(format!(
                            "Cannot iterate over {} ({})",
                            input.type_name(),
                            input.short_desc()
                        ));
                    }
                }
            }
        }
        "any" => match args.len() {
            // any(generator; filter) — 2-arg form
            2 => {
                let mut found = false;
                eval(&args[0], input, env, &mut |item| {
                    if !found {
                        eval(&args[1], &item, env, &mut |v| {
                            if v.is_truthy() {
                                found = true;
                            }
                        });
                    }
                });
                output(Value::Bool(found));
            }
            // any(filter) — 1-arg form on array input
            1 => {
                if let Value::Array(arr) = input {
                    let mut found = false;
                    for item in arr.iter() {
                        eval(&args[0], item, env, &mut |v| {
                            if v.is_truthy() {
                                found = true;
                            }
                        });
                        if found {
                            break;
                        }
                    }
                    output(Value::Bool(found));
                }
            }
            // any — 0-arg form on array input
            _ => {
                if let Value::Array(arr) = input {
                    let found = arr.iter().any(|v| v.is_truthy());
                    output(Value::Bool(found));
                }
            }
        },
        "all" => match args.len() {
            // all(generator; filter) — 2-arg form
            2 => {
                let mut all_true = true;
                eval(&args[0], input, env, &mut |item| {
                    if all_true {
                        let mut item_true = false;
                        eval(&args[1], &item, env, &mut |v| {
                            if v.is_truthy() {
                                item_true = true;
                            }
                        });
                        if !item_true {
                            all_true = false;
                        }
                    }
                });
                output(Value::Bool(all_true));
            }
            // all(filter) — 1-arg form on array input
            1 => {
                if let Value::Array(arr) = input {
                    let mut all_true = true;
                    for item in arr.iter() {
                        let mut item_true = false;
                        eval(&args[0], item, env, &mut |v| {
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
                }
            }
            // all — 0-arg form on array input
            _ => {
                if let Value::Array(arr) = input {
                    let all_true = arr.iter().all(|v| v.is_truthy());
                    output(Value::Bool(all_true));
                }
            }
        },
        "sort" => {
            if let Value::Array(arr) = input {
                let mut sorted: Vec<Value> = arr.as_ref().clone();
                sorted.sort_by(|a, b| values_order(a, b).unwrap_or(std::cmp::Ordering::Equal));
                output(Value::Array(Arc::new(sorted)));
            } else {
                set_error(format!(
                    "{} ({}) cannot be sorted, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "sort_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut pairs: Vec<(Vec<Value>, Value)> = arr
                    .iter()
                    .map(|item| {
                        let mut keys = Vec::new();
                        eval(f, item, env, &mut |v| keys.push(v));
                        (keys, item.clone())
                    })
                    .collect();
                pairs.sort_by(|(a, _), (b, _)| {
                    // Lexicographic comparison of key tuples
                    for (ak, bk) in a.iter().zip(b.iter()) {
                        let ord = values_order(ak, bk).unwrap_or(std::cmp::Ordering::Equal);
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    a.len().cmp(&b.len())
                });
                output(Value::Array(Arc::new(
                    pairs.into_iter().map(|(_, v)| v).collect(),
                )));
            } else if !matches!(input, Value::Array(_)) {
                set_error(format!(
                    "{} ({}) cannot be sorted, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
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
                            groups.push(Value::Array(Arc::new(std::mem::take(&mut current_group))));
                        }
                        current_key = Some(key);
                        current_group.push(val);
                    }
                }
                if !current_group.is_empty() {
                    groups.push(Value::Array(Arc::new(current_group)));
                }
                output(Value::Array(Arc::new(groups)));
            } else if !matches!(input, Value::Array(_)) {
                set_error(format!(
                    "{} ({}) cannot be grouped, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "unique" => {
            if let Value::Array(arr) = input {
                let mut sorted: Vec<Value> = arr.as_ref().clone();
                sorted.sort_by(|a, b| values_order(a, b).unwrap_or(std::cmp::Ordering::Equal));
                sorted.dedup_by(|a, b| values_equal(a, b));
                output(Value::Array(Arc::new(sorted)));
            } else {
                set_error(format!(
                    "{} ({}) cannot be unique'd, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
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
                output(Value::Array(Arc::new(result)));
            } else if !matches!(input, Value::Array(_)) {
                set_error(format!(
                    "{} ({}) cannot be unique'd, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "flatten" => {
            // jq: flatten works on arrays directly, on objects extracts values first
            let arr: Option<&[Value]> = match input {
                Value::Array(arr) => Some(arr.as_ref()),
                Value::Object(obj) => {
                    // Extract values into a temp array, then flatten that
                    let values: Vec<Value> = obj.iter().map(|(_, v)| v.clone()).collect();
                    let temp = values; // moved into scope
                    // Use a closure to handle the temp lifetime
                    let do_flatten = |arr: &[Value], depth: i64, output: &mut dyn FnMut(Value)| {
                        let mut result = Vec::new();
                        flatten_array(arr, depth, &mut result);
                        output(Value::Array(Arc::new(result)));
                    };
                    if let Some(f) = args.first() {
                        eval(f, input, env, &mut |d| {
                            let depth = match d {
                                Value::Int(n) => {
                                    if n < 0 {
                                        set_error("flatten depth must not be negative".to_string());
                                        return;
                                    }
                                    n
                                }
                                _ => i64::MAX,
                            };
                            do_flatten(&temp, depth, output);
                        });
                    } else {
                        do_flatten(&temp, i64::MAX, output);
                    }
                    None // already handled
                }
                _ => {
                    set_error(format!(
                        "{} ({}) cannot be flattened, as it is not an array",
                        input.type_name(),
                        input.short_desc()
                    ));
                    None
                }
            };
            if let Some(arr) = arr {
                if let Some(f) = args.first() {
                    eval(f, input, env, &mut |d| {
                        let depth = match d {
                            Value::Int(n) => {
                                if n < 0 {
                                    set_error("flatten depth must not be negative".to_string());
                                    return;
                                }
                                n
                            }
                            _ => i64::MAX,
                        };
                        let mut result = Vec::new();
                        flatten_array(arr, depth, &mut result);
                        output(Value::Array(Arc::new(result)));
                    });
                } else {
                    let mut result = Vec::new();
                    flatten_array(arr, i64::MAX, &mut result);
                    output(Value::Array(Arc::new(result)));
                }
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
            } else {
                match input {
                    Value::Array(arr) => {
                        output(arr.first().cloned().unwrap_or(Value::Null));
                    }
                    Value::Null => output(Value::Null),
                    _ => {
                        set_error(format!("Cannot index {} with number", input.type_name()));
                    }
                }
            }
        }
        "last" => {
            if let Some(f) = args.first() {
                let mut last = None;
                eval(f, input, env, &mut |v| last = Some(v));
                if let Some(v) = last {
                    output(v);
                }
            } else {
                match input {
                    Value::Array(arr) => {
                        output(arr.last().cloned().unwrap_or(Value::Null));
                    }
                    Value::Null => output(Value::Null),
                    _ => {
                        set_error(format!("Cannot index {} with number", input.type_name()));
                    }
                }
            }
        }
        "reverse" => match input {
            Value::Array(arr) => {
                let mut result: Vec<Value> = arr.as_ref().clone();
                result.reverse();
                output(Value::Array(Arc::new(result)));
            }
            Value::String(s) => {
                output(Value::String(s.chars().rev().collect()));
            }
            Value::Null => output(Value::Array(Arc::new(vec![]))),
            _ => {
                set_error(format!(
                    "{} ({}) cannot be reversed, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        },
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
            } else {
                set_error(format!(
                    "Cannot iterate over {} ({})",
                    input.type_name(),
                    input.short_desc()
                ));
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
            } else {
                set_error(format!(
                    "Cannot iterate over {} ({})",
                    input.type_name(),
                    input.short_desc()
                ));
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
            } else if !matches!(input, Value::Array(_)) {
                set_error(format!(
                    "Cannot iterate over {} ({})",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "max_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut best: Option<(Value, Value)> = None;
                for item in arr.iter() {
                    let mut key = Value::Null;
                    eval(f, item, env, &mut |v| key = v);
                    if best.as_ref().is_none_or(|(bk, _)| {
                        matches!(
                            values_order(&key, bk),
                            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                        )
                    }) {
                        best = Some((key, item.clone()));
                    }
                }
                if let Some((_, v)) = best {
                    output(v);
                } else {
                    output(Value::Null);
                }
            } else if !matches!(input, Value::Array(_)) {
                set_error(format!(
                    "Cannot iterate over {} ({})",
                    input.type_name(),
                    input.short_desc()
                ));
            }
        }
        "del" => {
            if let Some(path_f) = args.first() {
                // Collect all paths to delete, resolving negative indices
                let mut paths: Vec<Vec<Value>> = Vec::new();
                super::super::value_ops::path_of_env(
                    path_f,
                    input,
                    &mut Vec::new(),
                    env,
                    &mut |path_val| {
                        if let Value::Array(arr) = path_val {
                            paths.push(arr.as_ref().clone());
                        }
                    },
                );
                if paths.is_empty() {
                    output(input.clone());
                    return;
                }
                // Resolve negative indices at each level relative to current container
                for path in &mut paths {
                    let mut container = input;
                    for seg in path.iter_mut() {
                        if let Value::Int(i) = seg {
                            if *i < 0
                                && let Value::Array(arr) = container
                            {
                                *i = (arr.len() as i64 + *i).max(0);
                            }
                            if let Value::Array(arr) = container {
                                let idx = *i as usize;
                                if idx < arr.len() {
                                    container = &arr[idx];
                                }
                            }
                        } else if let Value::String(k) = seg
                            && let Value::Object(obj) = container
                            && let Some((_, v)) = obj.iter().find(|(ek, _)| ek == k)
                        {
                            container = v;
                        }
                    }
                }
                // Sort in reverse order so deletions don't shift indices
                paths.sort_by(|a, b| {
                    super::super::value_ops::values_order(
                        &Value::Array(Arc::new(b.clone())),
                        &Value::Array(Arc::new(a.clone())),
                    )
                    .unwrap_or(std::cmp::Ordering::Equal)
                });
                // Deduplicate paths (a path that's a prefix of another already deletes it)
                paths.dedup();
                let mut result = input.clone();
                for path in &paths {
                    result = super::super::value_ops::del_path(&result, path);
                }
                output(result);
            } else {
                output(input.clone());
            }
        }
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
                    result.push(Value::Array(Arc::new(col)));
                }
                output(Value::Array(Arc::new(result)));
            } else {
                set_error(format!(
                    "{} ({}) cannot be transposed, as it is not an array",
                    input.type_name(),
                    input.short_desc()
                ));
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
                        output(Value::Object(Arc::new(result)));
                    }
                    Value::Array(arr) => {
                        let mut result = Vec::with_capacity(arr.len());
                        for v in arr.iter() {
                            eval(f, v, env, &mut |nv| result.push(nv));
                        }
                        output(Value::Array(Arc::new(result)));
                    }
                    _ => output(input.clone()),
                }
            }
        }
        "limit" => {
            if args.len() == 2 {
                eval(&args[0], input, env, &mut |n_val| {
                    let n = to_f64(&n_val) as i64;
                    if n < 0 {
                        set_error("limit doesn't support negative count".into());
                        return;
                    }
                    let mut count = 0i64;
                    eval(&args[1], input, env, &mut |v| {
                        if count < n {
                            output(v);
                            count += 1;
                        }
                    });
                    // Clear errors from generator values past the limit
                    if count >= n {
                        LAST_ERROR.with(|e| e.borrow_mut().take());
                    }
                });
            }
        }
        "skip" => {
            if args.len() == 2 {
                eval(&args[0], input, env, &mut |n_val| {
                    let n = to_f64(&n_val) as i64;
                    if n < 0 {
                        set_error("skip doesn't support negative count".into());
                        return;
                    }
                    let mut count = 0i64;
                    eval(&args[1], input, env, &mut |v| {
                        if count >= n {
                            output(v);
                        }
                        count += 1;
                    });
                });
            }
        }
        "until" => {
            if args.len() == 2 {
                let mut current = input.clone();
                for _ in 0..MAX_LOOP_ITERATIONS {
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
                for _ in 0..MAX_LOOP_ITERATIONS {
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
            if let Some(f) = args.first() {
                for _ in 0..MAX_LOOP_ITERATIONS {
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
        "nth" => {
            if args.len() == 2 {
                // nth(indices; generator) — for each index, output the nth value from generator
                eval(&args[0], input, env, &mut |idx_val| {
                    let n = to_f64(&idx_val) as i64;
                    if n < 0 {
                        set_error("nth doesn't support negative indices".into());
                        return;
                    }
                    let mut count = 0i64;
                    eval(&args[1], input, env, &mut |v| {
                        if count == n {
                            output(v);
                        }
                        count += 1;
                    });
                });
            } else if args.len() == 1 {
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
                recurse(input, output);
            } else if args.len() == 1 {
                recurse_with_filter(&args[0], input, env, output, 100_000);
            } else if args.len() == 2 {
                recurse_with_cond(&args[0], &args[1], input, env, output, 100_000);
            }
        }
        "walk" => {
            if let Some(f) = args.first() {
                fn walk_inner(value: &Value, f: &Filter, env: &Env, output: &mut dyn FnMut(Value)) {
                    match value {
                        Value::Array(arr) => {
                            let mut new_arr = Vec::with_capacity(arr.len());
                            for v in arr.iter() {
                                // Take only first output from recursive walk
                                let mut first = None;
                                walk_inner(v, f, env, &mut |walked| {
                                    if first.is_none() {
                                        first = Some(walked);
                                    }
                                });
                                if let Some(walked) = first {
                                    new_arr.push(walked);
                                }
                                // No output → element removed (e.g., select filtered it)
                            }
                            let reconstructed = Value::Array(Arc::new(new_arr));
                            eval(f, &reconstructed, env, output);
                        }
                        Value::Object(obj) => {
                            let mut new_obj = Vec::with_capacity(obj.len());
                            for (k, v) in obj.iter() {
                                // Take only first output from recursive walk
                                let mut first = None;
                                walk_inner(v, f, env, &mut |walked| {
                                    if first.is_none() {
                                        first = Some(walked);
                                    }
                                });
                                if let Some(walked) = first {
                                    new_obj.push((k.clone(), walked));
                                }
                                // No output → key removed (e.g., select filtered it)
                            }
                            let reconstructed = Value::Object(Arc::new(new_obj));
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
            if let Some(arg) = args.first() {
                match input {
                    Value::Array(arr) => {
                        eval(arg, input, env, &mut |target| {
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
                                result = -lo - 1;
                            }
                            output(Value::Int(result));
                        });
                    }
                    _ => {
                        set_error(format!(
                            "{} ({}) cannot be searched from",
                            input.type_name(),
                            input.short_desc()
                        ));
                    }
                }
            }
        }
        "IN" => match args.len() {
            1 => {
                let mut found = false;
                eval(&args[0], input, env, &mut |v| {
                    if !found && values_equal(input, &v) {
                        found = true;
                    }
                });
                output(Value::Bool(found));
            }
            2 => {
                // IN(g; s): check if any value from g is found in s
                let mut found = false;
                eval(&args[0], input, env, &mut |sv| {
                    if !found {
                        eval(&args[1], input, env, &mut |gv| {
                            if !found && values_equal(&sv, &gv) {
                                found = true;
                            }
                        });
                    }
                });
                output(Value::Bool(found));
            }
            _ => {}
        },
        "with_entries" => {
            if let (Value::Object(obj), Some(f)) = (input, args.first()) {
                let entries: Vec<Value> = obj
                    .iter()
                    .map(|(k, v)| {
                        Value::Object(Arc::new(vec![
                            ("key".into(), Value::String(k.clone())),
                            ("value".into(), v.clone()),
                        ]))
                    })
                    .collect();
                let entries_val = Value::Array(Arc::new(entries));
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
                output(Value::Object(Arc::new(result_obj)));
            }
        }
        "combinations" => {
            if let Value::Array(arr) = input {
                if args.is_empty() {
                    let arrays: Vec<&[Value]> = arr
                        .iter()
                        .filter_map(|v| {
                            if let Value::Array(a) = v {
                                Some(a.as_ref().as_slice())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if arrays.is_empty() {
                        return;
                    }
                    let mut indices = vec![0usize; arrays.len()];
                    loop {
                        let combo: Vec<Value> = indices
                            .iter()
                            .enumerate()
                            .map(|(i, &j)| arrays[i][j].clone())
                            .collect();
                        output(Value::Array(Arc::new(combo)));
                        let mut carry = true;
                        for k in (0..indices.len()).rev() {
                            if carry {
                                indices[k] += 1;
                                if indices[k] < arrays[k].len() {
                                    carry = false;
                                } else {
                                    indices[k] = 0;
                                }
                            }
                        }
                        if carry {
                            break;
                        }
                    }
                } else {
                    let mut n = 0i64;
                    eval(&args[0], input, env, &mut |v| n = to_f64(&v) as i64);
                    if n <= 0 {
                        return;
                    }
                    let arrays: Vec<&[Value]> = (0..n).map(|_| arr.as_ref().as_slice()).collect();
                    let mut indices = vec![0usize; n as usize];
                    loop {
                        let combo: Vec<Value> = indices
                            .iter()
                            .enumerate()
                            .map(|(i, &j)| arrays[i][j].clone())
                            .collect();
                        output(Value::Array(Arc::new(combo)));
                        let mut carry = true;
                        for k in (0..indices.len()).rev() {
                            if carry {
                                indices[k] += 1;
                                if indices[k] < arrays[k].len() {
                                    carry = false;
                                } else {
                                    indices[k] = 0;
                                }
                            }
                        }
                        if carry {
                            break;
                        }
                    }
                }
            }
        }
        "pick" => {
            // pick(pathexpr) — constructs an object/array containing only the specified paths
            if let Some(path_f) = args.first() {
                // Use path() to get paths, then copy values via setpath
                let mut acc = Value::Null;
                let mut had_error = false;
                super::super::value_ops::path_of_env(
                    path_f,
                    input,
                    &mut Vec::new(),
                    env,
                    &mut |path_val| {
                        if had_error {
                            return;
                        }
                        if let Value::Array(path_arr) = &path_val {
                            let val = super::super::value_ops::get_path(input, path_arr);
                            match super::super::value_ops::set_path(&acc, path_arr, &val) {
                                Ok(v) => acc = v,
                                Err(msg) => {
                                    set_error(msg);
                                    had_error = true;
                                }
                            }
                        }
                    },
                );
                if !had_error {
                    output(acc);
                }
            }
        }
        "INDEX" => match args.len() {
            // INDEX(stream; idx_expr) — build lookup dict from stream
            2 => {
                let mut result: Vec<(String, Value)> = Vec::new();
                eval(&args[0], input, env, &mut |item| {
                    eval(&args[1], &item, env, &mut |key| {
                        let key_str = match &key {
                            Value::String(s) => s.clone(),
                            _ => {
                                let mut buf = Vec::new();
                                crate::output::write_compact(&mut buf, &key, false).unwrap();
                                String::from_utf8(buf).unwrap_or_default()
                            }
                        };
                        // Remove any existing entry with same key, then add new
                        result.retain(|(k, _)| k != &key_str);
                        result.push((key_str, item.clone()));
                    });
                });
                output(Value::Object(Arc::new(result)));
            }
            // INDEX(idx_expr) — .[] as input
            1 => {
                let mut result: Vec<(String, Value)> = Vec::new();
                if let Value::Array(arr) = input {
                    for item in arr.iter() {
                        eval(&args[0], item, env, &mut |key| {
                            let key_str = match &key {
                                Value::String(s) => s.clone(),
                                _ => {
                                    let mut buf = Vec::new();
                                    crate::output::write_compact(&mut buf, &key, false).unwrap();
                                    String::from_utf8(buf).unwrap_or_default()
                                }
                            };
                            result.retain(|(k, _)| k != &key_str);
                            result.push((key_str, item.clone()));
                        });
                    }
                }
                output(Value::Object(Arc::new(result)));
            }
            _ => {}
        },
        "JOIN" => {
            // JOIN(idx; key_expr) — join with lookup table
            // idx is a filter that produces an object (the index)
            // For each input element, looks up key_expr in the index
            if args.len() == 2 {
                // First evaluate the index
                let mut index = Value::Null;
                eval(&args[0], input, env, &mut |v| index = v);
                // Then iterate over input and join, collecting results
                let mut results = Vec::new();
                if let Value::Array(arr) = input {
                    for item in arr.iter() {
                        eval(&args[1], item, env, &mut |key| {
                            let key_str = match &key {
                                Value::String(s) => s.clone(),
                                _ => {
                                    let mut buf = Vec::new();
                                    crate::output::write_compact(&mut buf, &key, false).unwrap();
                                    String::from_utf8(buf).unwrap_or_default()
                                }
                            };
                            let lookup = if let Value::Object(obj) = &index {
                                obj.iter()
                                    .find(|(k, _)| k == &key_str)
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or(Value::Null)
                            } else {
                                Value::Null
                            };
                            results.push(Value::Array(Arc::new(vec![item.clone(), lookup])));
                        });
                    }
                }
                output(Value::Array(Arc::new(results)));
            }
        }
        _ => {}
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
        if (v != Value::Null || matches!(value, Value::Array(_) | Value::Object(_)))
            && !values_equal(&v, value)
        {
            recurse_with_filter(f, &v, env, output, limit - 1);
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
