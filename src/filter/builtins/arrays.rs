use crate::filter::{ArithOp, Env, Filter};
use crate::value::Value;
use std::rc::Rc;

use super::super::eval::{LAST_ERROR, eval};
use super::super::value_ops::{arith_values, recurse, to_f64, values_equal, values_order};

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
                    if let Ok(result) = arith_values(&acc, &ArithOp::Add, item) {
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
                if let Some(f) = args.first() {
                    eval(f, input, env, &mut |d| {
                        let depth = match d {
                            Value::Int(n) => n,
                            _ => i64::MAX,
                        };
                        let mut result = Vec::new();
                        flatten_array(arr, depth, &mut result);
                        output(Value::Array(Rc::new(result)));
                    });
                } else {
                    let mut result = Vec::new();
                    flatten_array(arr, i64::MAX, &mut result);
                    output(Value::Array(Rc::new(result)));
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
                eval(&args[0], input, env, &mut |n_val| {
                    let n = to_f64(&n_val) as i64;
                    if n < 0 {
                        LAST_ERROR.with(|e| {
                            *e.borrow_mut() =
                                Some(Value::String("limit doesn't support negative count".into()));
                        });
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
                        LAST_ERROR.with(|e| {
                            *e.borrow_mut() =
                                Some(Value::String("skip doesn't support negative count".into()));
                        });
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
        "nth" => {
            if args.len() == 2 {
                // nth(indices; generator) — for each index, output the nth value from generator
                eval(&args[0], input, env, &mut |idx_val| {
                    let n = to_f64(&idx_val) as i64;
                    if n < 0 {
                        LAST_ERROR.with(|e| {
                            *e.borrow_mut() =
                                Some(Value::String("nth doesn't support negative count".into()));
                        });
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
        },
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
                        output(Value::Array(Rc::new(combo)));
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
                        output(Value::Array(Rc::new(combo)));
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
