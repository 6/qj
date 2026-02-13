/// jq filter evaluator — produces zero or more output Values per input.
///
/// Uses generator semantics: each filter operation calls `output` for
/// each result, avoiding intermediate Vec allocations.
use crate::filter::{ArithOp, BoolOp, CmpOp, Filter, ObjKey};
use crate::value::Value;
use std::rc::Rc;

/// Evaluate a filter against an input value, calling `output` for each result.
pub fn eval(filter: &Filter, input: &Value, output: &mut dyn FnMut(Value)) {
    match filter {
        Filter::Identity => output(input.clone()),

        Filter::Field(name) => match input {
            Value::Object(obj) => {
                for (k, v) in obj.iter() {
                    if k == name {
                        output(v.clone());
                        return;
                    }
                }
                output(Value::Null);
            }
            Value::Null => output(Value::Null),
            _ => {} // jq produces error on non-object, we silently drop
        },

        Filter::Index(idx_filter) => {
            // Evaluate the index expression
            let mut idx_val = None;
            eval(idx_filter, input, &mut |v| {
                if idx_val.is_none() {
                    idx_val = Some(v);
                }
            });
            let Some(idx) = idx_val else { return };
            match (input, &idx) {
                (Value::Array(arr), Value::Int(i)) => {
                    let index = if *i < 0 { arr.len() as i64 + i } else { *i };
                    if index >= 0 && (index as usize) < arr.len() {
                        output(arr[index as usize].clone());
                    } else {
                        output(Value::Null);
                    }
                }
                (Value::Object(obj), Value::String(key)) => {
                    for (k, v) in obj.iter() {
                        if k == key {
                            output(v.clone());
                            return;
                        }
                    }
                    output(Value::Null);
                }
                (Value::Null, _) => output(Value::Null),
                _ => {}
            }
        }

        Filter::Pipe(left, right) => {
            eval(left, input, &mut |intermediate| {
                eval(right, &intermediate, output);
            });
        }

        Filter::Iterate => match input {
            Value::Array(arr) => {
                for v in arr.iter() {
                    output(v.clone());
                }
            }
            Value::Object(obj) => {
                for (_, v) in obj.iter() {
                    output(v.clone());
                }
            }
            Value::Null => {}
            _ => {}
        },

        Filter::Select(cond) => {
            let mut is_truthy = false;
            eval(cond, input, &mut |v| {
                if v.is_truthy() {
                    is_truthy = true;
                }
            });
            if is_truthy {
                output(input.clone());
            }
        }

        Filter::ObjectConstruct(pairs) => {
            let mut obj = Vec::with_capacity(pairs.len());
            for (key, val_filter) in pairs {
                let key_str = match key {
                    ObjKey::Name(s) => s.clone(),
                    ObjKey::Expr(expr) => {
                        let mut k = String::new();
                        eval(expr, input, &mut |v| {
                            if let Value::String(s) = v {
                                k = s;
                            }
                        });
                        k
                    }
                };
                let mut val = Value::Null;
                eval(val_filter, input, &mut |v| {
                    val = v;
                });
                obj.push((key_str, val));
            }
            output(Value::Object(Rc::new(obj)));
        }

        Filter::ArrayConstruct(expr) => {
            let mut arr = Vec::new();
            eval(expr, input, &mut |v| {
                arr.push(v);
            });
            output(Value::Array(Rc::new(arr)));
        }

        Filter::Literal(val) => output(val.clone()),

        Filter::Compare(left, op, right) => {
            let mut lval = Value::Null;
            eval(left, input, &mut |v| lval = v);
            let mut rval = Value::Null;
            eval(right, input, &mut |v| rval = v);
            let result = compare_values(&lval, op, &rval);
            output(Value::Bool(result));
        }

        Filter::Arith(left, op, right) => {
            let mut lval = Value::Null;
            eval(left, input, &mut |v| lval = v);
            let mut rval = Value::Null;
            eval(right, input, &mut |v| rval = v);
            if let Some(result) = arith_values(&lval, op, &rval) {
                output(result);
            }
        }

        Filter::Comma(items) => {
            for item in items {
                eval(item, input, output);
            }
        }

        Filter::Recurse => {
            recurse(input, output);
        }

        Filter::Builtin(name, args) => {
            eval_builtin(name, args, input, output);
        }

        Filter::Not(inner) => {
            eval(inner, input, &mut |v| {
                output(Value::Bool(!v.is_truthy()));
            });
        }

        Filter::BoolOp(left, op, right) => {
            let mut lval = Value::Null;
            eval(left, input, &mut |v| lval = v);
            match op {
                BoolOp::And => {
                    if lval.is_truthy() {
                        let mut rval = Value::Null;
                        eval(right, input, &mut |v| rval = v);
                        output(Value::Bool(rval.is_truthy()));
                    } else {
                        output(Value::Bool(false));
                    }
                }
                BoolOp::Or => {
                    if lval.is_truthy() {
                        output(Value::Bool(true));
                    } else {
                        let mut rval = Value::Null;
                        eval(right, input, &mut |v| rval = v);
                        output(Value::Bool(rval.is_truthy()));
                    }
                }
            }
        }

        Filter::IfThenElse(cond, then_branch, else_branch) => {
            let mut cond_val = Value::Null;
            eval(cond, input, &mut |v| cond_val = v);
            if cond_val.is_truthy() {
                eval(then_branch, input, output);
            } else if let Some(else_br) = else_branch {
                eval(else_br, input, output);
            } else {
                output(input.clone());
            }
        }

        Filter::Alternative(left, right) => {
            let mut lval = Value::Null;
            let mut got_value = false;
            eval(left, input, &mut |v| {
                if !got_value {
                    lval = v;
                    got_value = true;
                }
            });
            if got_value && lval != Value::Null && lval != Value::Bool(false) {
                output(lval);
            } else {
                eval(right, input, output);
            }
        }

        Filter::Try(inner) => {
            // Try: suppress errors, just produce no output on failure.
            // Since we don't use Result in eval, "errors" are represented
            // as producing no output, which Try already handles.
            eval(inner, input, output);
        }

        Filter::StringInterp(parts) => {
            let mut result = String::new();
            for part in parts {
                match part {
                    crate::filter::StringPart::Lit(s) => result.push_str(s),
                    crate::filter::StringPart::Expr(f) => {
                        eval(f, input, &mut |v| match v {
                            Value::String(s) => result.push_str(&s),
                            Value::Int(n) => result.push_str(itoa::Buffer::new().format(n)),
                            Value::Double(f) => result.push_str(ryu::Buffer::new().format(f)),
                            Value::Bool(b) => result.push_str(if b { "true" } else { "false" }),
                            Value::Null => result.push_str("null"),
                            _ => {} // arrays/objects: skip for now
                        });
                    }
                }
            }
            output(Value::String(result));
        }

        Filter::Neg(inner) => {
            eval(inner, input, &mut |v| match v {
                Value::Int(n) => output(Value::Int(-n)),
                Value::Double(f) => output(Value::Double(-f)),
                _ => {}
            });
        }
    }
}

fn compare_values(left: &Value, op: &CmpOp, right: &Value) -> bool {
    match op {
        CmpOp::Eq => values_equal(left, right),
        CmpOp::Ne => !values_equal(left, right),
        CmpOp::Lt => values_order(left, right) == Some(std::cmp::Ordering::Less),
        CmpOp::Le => matches!(
            values_order(left, right),
            Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
        ),
        CmpOp::Gt => values_order(left, right) == Some(std::cmp::Ordering::Greater),
        CmpOp::Ge => matches!(
            values_order(left, right),
            Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
        ),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Int(a), Value::Int(b)) => a == b,
        (Value::Double(a), Value::Double(b)) => a == b,
        (Value::Int(a), Value::Double(b)) => (*a as f64) == *b,
        (Value::Double(a), Value::Int(b)) => *a == (*b as f64),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            Rc::ptr_eq(a, b)
                || (a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y)))
        }
        (Value::Object(a), Value::Object(b)) => {
            Rc::ptr_eq(a, b)
                || (a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2)))
        }
        _ => false,
    }
}

fn values_order(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Double(a), Value::Double(b)) => a.partial_cmp(b),
        (Value::Int(a), Value::Double(b)) => (*a as f64).partial_cmp(b),
        (Value::Double(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

fn arith_values(left: &Value, op: &ArithOp, right: &Value) -> Option<Value> {
    match op {
        ArithOp::Add => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Some(Value::Int(a.wrapping_add(*b))),
            (Value::Double(a), Value::Double(b)) => Some(Value::Double(a + b)),
            (Value::Int(a), Value::Double(b)) => Some(Value::Double(*a as f64 + b)),
            (Value::Double(a), Value::Int(b)) => Some(Value::Double(a + *b as f64)),
            (Value::String(a), Value::String(b)) => Some(Value::String(format!("{a}{b}"))),
            (Value::Array(a), Value::Array(b)) => {
                let mut result = Vec::with_capacity(a.len() + b.len());
                result.extend_from_slice(a);
                result.extend_from_slice(b);
                Some(Value::Array(Rc::new(result)))
            }
            (Value::Object(a), Value::Object(b)) => {
                // Shallow merge: b's keys override a's
                let mut result: Vec<(String, Value)> = a.as_ref().clone();
                for (k, v) in b.iter() {
                    if let Some(existing) = result.iter_mut().find(|(ek, _)| ek == k) {
                        existing.1 = v.clone();
                    } else {
                        result.push((k.clone(), v.clone()));
                    }
                }
                Some(Value::Object(Rc::new(result)))
            }
            (Value::Null, other) | (other, Value::Null) => Some(other.clone()),
            _ => None,
        },
        ArithOp::Sub => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Some(Value::Int(a.wrapping_sub(*b))),
            (Value::Double(a), Value::Double(b)) => Some(Value::Double(a - b)),
            (Value::Int(a), Value::Double(b)) => Some(Value::Double(*a as f64 - b)),
            (Value::Double(a), Value::Int(b)) => Some(Value::Double(a - *b as f64)),
            _ => None,
        },
        ArithOp::Mul => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Some(Value::Int(a.wrapping_mul(*b))),
            (Value::Double(a), Value::Double(b)) => Some(Value::Double(a * b)),
            (Value::Int(a), Value::Double(b)) => Some(Value::Double(*a as f64 * b)),
            (Value::Double(a), Value::Int(b)) => Some(Value::Double(a * *b as f64)),
            _ => None,
        },
        ArithOp::Div => match (left, right) {
            (Value::Int(a), Value::Int(b)) if *b != 0 => Some(Value::Int(a / b)),
            (Value::Double(a), Value::Double(b)) => Some(Value::Double(a / b)),
            (Value::Int(a), Value::Double(b)) => Some(Value::Double(*a as f64 / b)),
            (Value::Double(a), Value::Int(b)) => Some(Value::Double(a / *b as f64)),
            _ => None,
        },
        ArithOp::Mod => match (left, right) {
            (Value::Int(a), Value::Int(b)) if *b != 0 => Some(Value::Int(a % b)),
            _ => None,
        },
    }
}

fn recurse(value: &Value, output: &mut dyn FnMut(Value)) {
    output(value.clone());
    match value {
        Value::Array(arr) => {
            for v in arr.iter() {
                recurse(v, output);
            }
        }
        Value::Object(obj) => {
            for (_, v) in obj.iter() {
                recurse(v, output);
            }
        }
        _ => {}
    }
}

fn eval_builtin(name: &str, args: &[Filter], input: &Value, output: &mut dyn FnMut(Value)) {
    match name {
        "length" => match input {
            Value::String(s) => output(Value::Int(s.len() as i64)),
            Value::Array(a) => output(Value::Int(a.len() as i64)),
            Value::Object(o) => output(Value::Int(o.len() as i64)),
            Value::Null => output(Value::Int(0)),
            Value::Int(_) | Value::Double(_) | Value::Bool(_) => {
                // jq returns absolute value for numbers, error for bool
                // We'll output null for unsupported
                output(Value::Null);
            }
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
                output(Value::Array(Rc::new(
                    obj.iter().map(|(_, v)| v.clone()).collect(),
                )));
            }
            Value::Array(arr) => {
                output(Value::Array(arr.clone()));
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
        "map" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut result = Vec::with_capacity(arr.len());
                for item in arr.iter() {
                    eval(f, item, &mut |v| result.push(v));
                }
                output(Value::Array(Rc::new(result)));
            }
        }
        "select" => {
            if let Some(cond) = args.first() {
                let mut is_truthy = false;
                eval(cond, input, &mut |v| {
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
                        eval(f, item, &mut |v| {
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
                        eval(f, item, &mut |v| {
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
                eval(key_filter, input, &mut |v| key_val = v);
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
                            .find(|(k, _)| k == "key" || k == "name")
                            .map(|(_, v)| match v {
                                Value::String(s) => s.clone(),
                                Value::Int(n) => n.to_string(),
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        let val = fields
                            .iter()
                            .find(|(k, _)| k == "value")
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
            Value::Double(f) => output(Value::String(ryu::Buffer::new().format(*f).into())),
            Value::Bool(b) => output(Value::String(if *b { "true" } else { "false" }.into())),
            Value::Null => output(Value::String("null".into())),
            _ => output(input.clone()), // arrays/objects: would need JSON serialization
        },
        "tonumber" => match input {
            Value::Int(_) | Value::Double(_) => output(input.clone()),
            Value::String(s) => {
                if let Ok(n) = s.parse::<i64>() {
                    output(Value::Int(n));
                } else if let Ok(f) = s.parse::<f64>() {
                    output(Value::Double(f));
                }
            }
            _ => {}
        },
        "ascii_downcase" => {
            if let Value::String(s) = input {
                output(Value::String(s.to_lowercase()));
            }
        }
        "ascii_upcase" => {
            if let Value::String(s) = input {
                output(Value::String(s.to_uppercase()));
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
                        eval(f, item, &mut |v| key = v);
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
                        eval(f, item, &mut |v| key = v);
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
                let mut result: Vec<Value> = Vec::new();
                for item in arr.iter() {
                    if !result.iter().any(|v| values_equal(v, item)) {
                        result.push(item.clone());
                    }
                }
                output(Value::Array(Rc::new(result)));
            }
        }
        "unique_by" => {
            if let (Value::Array(arr), Some(f)) = (input, args.first()) {
                let mut seen_keys: Vec<Value> = Vec::new();
                let mut result: Vec<Value> = Vec::new();
                for item in arr.iter() {
                    let mut key = Value::Null;
                    eval(f, item, &mut |v| key = v);
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
                    eval(f, input, &mut |v| d = v);
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
                eval(f, input, &mut |v| {
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
                eval(f, input, &mut |v| last = Some(v));
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
                    eval(f, item, &mut |v| key = v);
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
                    eval(f, item, &mut |v| key = v);
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
                eval(arg, input, &mut |v| pattern = v);
                output(Value::Bool(value_contains(input, &pattern)));
            }
        }
        "ltrimstr" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut prefix = Value::Null;
                eval(arg, input, &mut |v| prefix = v);
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
                eval(arg, input, &mut |v| suffix = v);
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
                eval(arg, input, &mut |v| prefix = v);
                if let Value::String(p) = prefix {
                    output(Value::Bool(s.starts_with(p.as_str())));
                }
            }
        }
        "endswith" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut suffix = Value::Null;
                eval(arg, input, &mut |v| suffix = v);
                if let Value::String(p) = suffix {
                    output(Value::Bool(s.ends_with(p.as_str())));
                }
            }
        }
        "split" => {
            if let (Value::String(s), Some(arg)) = (input, args.first()) {
                let mut sep = Value::Null;
                eval(arg, input, &mut |v| sep = v);
                if let Value::String(p) = sep {
                    let parts: Vec<Value> =
                        s.split(&p).map(|part| Value::String(part.into())).collect();
                    output(Value::Array(Rc::new(parts)));
                }
            }
        }
        "join" => {
            if let (Value::Array(arr), Some(arg)) = (input, args.first()) {
                let mut sep = Value::Null;
                eval(arg, input, &mut |v| sep = v);
                if let Value::String(p) = sep {
                    let strs: Vec<String> = arr
                        .iter()
                        .filter_map(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            Value::Int(n) => Some(itoa::Buffer::new().format(*n).into()),
                            Value::Double(f) => Some(ryu::Buffer::new().format(*f).into()),
                            Value::Null => Some(String::new()),
                            Value::Bool(b) => Some(if *b { "true" } else { "false" }.into()),
                            _ => None,
                        })
                        .collect();
                    output(Value::String(strs.join(&p)));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_one(filter: &Filter, input: &Value) -> Value {
        let mut results = Vec::new();
        eval(filter, input, &mut |v| results.push(v));
        assert_eq!(results.len(), 1, "expected 1 result, got {:?}", results);
        results.into_iter().next().unwrap()
    }

    fn eval_all(filter: &Filter, input: &Value) -> Vec<Value> {
        let mut results = Vec::new();
        eval(filter, input, &mut |v| results.push(v));
        results
    }

    fn parse(s: &str) -> Filter {
        crate::filter::parse(s).unwrap()
    }

    fn obj(pairs: &[(&str, Value)]) -> Value {
        Value::Object(Rc::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ))
    }

    #[test]
    fn eval_identity() {
        let v = Value::Int(42);
        assert_eq!(eval_one(&parse("."), &v), Value::Int(42));
    }

    #[test]
    fn eval_field() {
        let input = obj(&[("name", Value::String("alice".into()))]);
        assert_eq!(
            eval_one(&parse(".name"), &input),
            Value::String("alice".into())
        );
    }

    #[test]
    fn eval_nested_field() {
        let input = obj(&[("a", obj(&[("b", Value::Int(1))]))]);
        assert_eq!(eval_one(&parse(".a.b"), &input), Value::Int(1));
    }

    #[test]
    fn eval_missing_field() {
        let input = obj(&[("x", Value::Int(1))]);
        assert_eq!(eval_one(&parse(".y"), &input), Value::Null);
    }

    #[test]
    fn eval_iterate_array() {
        let input = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_all(&parse(".[]"), &input),
            vec![Value::Int(1), Value::Int(2), Value::Int(3)]
        );
    }

    #[test]
    fn eval_iterate_object() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let results = eval_all(&parse(".[]"), &input);
        assert_eq!(results, vec![Value::Int(1), Value::Int(2)]);
    }

    #[test]
    fn eval_pipe() {
        let input = Value::Array(Rc::new(vec![
            obj(&[("name", Value::String("alice".into()))]),
            obj(&[("name", Value::String("bob".into()))]),
        ]));
        assert_eq!(
            eval_all(&parse(".[] | .name"), &input),
            vec![Value::String("alice".into()), Value::String("bob".into()),]
        );
    }

    #[test]
    fn eval_select() {
        let input = Value::Array(Rc::new(vec![
            obj(&[("x", Value::Int(1))]),
            obj(&[("x", Value::Int(5))]),
            obj(&[("x", Value::Int(3))]),
        ]));
        let results = eval_all(&parse(".[] | select(.x > 2)"), &input);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn eval_object_construct() {
        let input = obj(&[
            ("name", Value::String("alice".into())),
            ("age", Value::Int(30)),
        ]);
        let result = eval_one(&parse("{name: .name}"), &input);
        assert_eq!(result, obj(&[("name", Value::String("alice".into()))]));
    }

    #[test]
    fn eval_array_construct() {
        let input = Value::Array(Rc::new(vec![
            obj(&[("x", Value::Int(1))]),
            obj(&[("x", Value::Int(2))]),
        ]));
        let result = eval_one(&parse("[.[] | .x]"), &input);
        assert_eq!(
            result,
            Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn eval_index() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        assert_eq!(eval_one(&parse(".[1]"), &input), Value::Int(20));
    }

    #[test]
    fn eval_negative_index() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        assert_eq!(eval_one(&parse(".[-1]"), &input), Value::Int(30));
    }

    #[test]
    fn eval_arithmetic() {
        let input = obj(&[("x", Value::Int(10))]);
        assert_eq!(eval_one(&parse(".x + 5"), &input), Value::Int(15));
        assert_eq!(eval_one(&parse(".x - 3"), &input), Value::Int(7));
        assert_eq!(eval_one(&parse(".x * 2"), &input), Value::Int(20));
    }

    #[test]
    fn eval_comparison() {
        let input = obj(&[("x", Value::Int(5))]);
        assert_eq!(eval_one(&parse(".x > 3"), &input), Value::Bool(true));
        assert_eq!(eval_one(&parse(".x < 3"), &input), Value::Bool(false));
        assert_eq!(eval_one(&parse(".x == 5"), &input), Value::Bool(true));
    }

    #[test]
    fn eval_comma() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(
            eval_all(&parse(".a, .b"), &input),
            vec![Value::Int(1), Value::Int(2)]
        );
    }

    #[test]
    fn eval_length() {
        assert_eq!(
            eval_one(
                &parse("length"),
                &Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]))
            ),
            Value::Int(2)
        );
        assert_eq!(
            eval_one(&parse("length"), &Value::String("hello".into())),
            Value::Int(5)
        );
    }

    #[test]
    fn eval_keys() {
        let input = obj(&[("b", Value::Int(2)), ("a", Value::Int(1))]);
        assert_eq!(
            eval_one(&parse("keys"), &input),
            Value::Array(Rc::new(vec![
                Value::String("a".into()),
                Value::String("b".into())
            ]))
        );
    }

    #[test]
    fn eval_type() {
        assert_eq!(
            eval_one(&parse("type"), &Value::Int(42)),
            Value::String("number".into())
        );
    }

    #[test]
    fn eval_if_then_else() {
        assert_eq!(
            eval_one(
                &parse("if . > 5 then \"big\" else \"small\" end"),
                &Value::Int(10)
            ),
            Value::String("big".into())
        );
    }

    #[test]
    fn eval_alternative() {
        assert_eq!(eval_one(&parse(".x // 42"), &obj(&[])), Value::Int(42));
        assert_eq!(
            eval_one(&parse(".x // 42"), &obj(&[("x", Value::Int(7))])),
            Value::Int(7)
        );
    }

    #[test]
    fn eval_map() {
        let input = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&parse("map(. + 10)"), &input),
            Value::Array(Rc::new(vec![
                Value::Int(11),
                Value::Int(12),
                Value::Int(13)
            ]))
        );
    }

    #[test]
    fn eval_add() {
        let input = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(eval_one(&parse("add"), &input), Value::Int(6));
    }

    #[test]
    fn eval_has() {
        let input = obj(&[("name", Value::String("alice".into()))]);
        assert_eq!(eval_one(&parse("has(\"name\")"), &input), Value::Bool(true));
        assert_eq!(eval_one(&parse("has(\"age\")"), &input), Value::Bool(false));
    }

    #[test]
    fn eval_sort() {
        let input = Value::Array(Rc::new(vec![Value::Int(3), Value::Int(1), Value::Int(2)]));
        assert_eq!(
            eval_one(&parse("sort"), &input),
            Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
        );
    }
}
