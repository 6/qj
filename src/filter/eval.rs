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
            _ => output(Value::Null),
        },

        Filter::Index(idx_filter) => {
            // Evaluate the index expression — iterate all outputs for generator semantics
            eval(idx_filter, input, &mut |idx| match (input, &idx) {
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
            });
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
            fn build_object(
                pairs: &[(ObjKey, Box<Filter>)],
                idx: usize,
                current: &mut Vec<(String, Value)>,
                input: &Value,
                output: &mut dyn FnMut(Value),
            ) {
                if idx == pairs.len() {
                    output(Value::Object(Rc::new(current.clone())));
                    return;
                }
                let (key, val_filter) = &pairs[idx];
                match key {
                    ObjKey::Name(s) => {
                        let key_str = s.clone();
                        eval(val_filter, input, &mut |v| {
                            current.push((key_str.clone(), v));
                            build_object(pairs, idx + 1, current, input, output);
                            current.pop();
                        });
                    }
                    ObjKey::Expr(expr) => {
                        eval(expr, input, &mut |kv| {
                            let key_str = match kv {
                                Value::String(s) => s,
                                _ => return,
                            };
                            eval(val_filter, input, &mut |v| {
                                current.push((key_str.clone(), v));
                                build_object(pairs, idx + 1, current, input, output);
                                current.pop();
                            });
                        });
                    }
                }
            }
            let mut current = Vec::with_capacity(pairs.len());
            build_object(pairs, 0, &mut current, input, output);
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
            eval(left, input, &mut |lval| {
                eval(right, input, &mut |rval| {
                    let result = compare_values(&lval, op, &rval);
                    output(Value::Bool(result));
                });
            });
        }

        Filter::Arith(left, op, right) => {
            eval(left, input, &mut |lval| {
                eval(right, input, &mut |rval| {
                    if let Some(result) = arith_values(&lval, op, &rval) {
                        output(result);
                    }
                });
            });
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
            eval(cond, input, &mut |cond_val| {
                if cond_val.is_truthy() {
                    eval(then_branch, input, output);
                } else if let Some(else_br) = else_branch {
                    eval(else_br, input, output);
                } else {
                    output(input.clone());
                }
            });
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
                            Value::Double(f, _) => result.push_str(ryu::Buffer::new().format(f)),
                            Value::Bool(b) => result.push_str(if b { "true" } else { "false" }),
                            Value::Null => result.push_str("null"),
                            Value::Array(_) | Value::Object(_) => {
                                let mut buf = Vec::new();
                                value_to_json_string(&mut buf, &v);
                                result.push_str(&String::from_utf8(buf).unwrap_or_default());
                            }
                        });
                    }
                }
            }
            output(Value::String(result));
        }

        Filter::Neg(inner) => {
            eval(inner, input, &mut |v| match v {
                Value::Int(n) => output(Value::Int(-n)),
                Value::Double(f, _) => output(Value::Double(-f, None)),
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
        (Value::Double(a, _), Value::Double(b, _)) => a == b,
        (Value::Int(a), Value::Double(b, _)) => (*a as f64) == *b,
        (Value::Double(a, _), Value::Int(b)) => *a == (*b as f64),
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

/// jq total ordering: null < false < true < numbers < strings < arrays < objects
fn type_order(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(false) => 1,
        Value::Bool(true) => 2,
        Value::Int(_) | Value::Double(..) => 3,
        Value::String(_) => 4,
        Value::Array(_) => 5,
        Value::Object(_) => 6,
    }
}

fn values_order(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    let lt = type_order(left);
    let rt = type_order(right);
    if lt != rt {
        return Some(lt.cmp(&rt));
    }
    match (left, right) {
        (Value::Null, Value::Null) => Some(std::cmp::Ordering::Equal),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Double(a, _), Value::Double(b, _)) => a.partial_cmp(b),
        (Value::Int(a), Value::Double(b, _)) => (*a as f64).partial_cmp(b),
        (Value::Double(a, _), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Array(a), Value::Array(b)) => {
            for (av, bv) in a.iter().zip(b.iter()) {
                match values_order(av, bv) {
                    Some(std::cmp::Ordering::Equal) => continue,
                    other => return other,
                }
            }
            Some(a.len().cmp(&b.len()))
        }
        (Value::Object(a), Value::Object(b)) => {
            // Compare by length first, then sorted keys+values
            match a.len().cmp(&b.len()) {
                std::cmp::Ordering::Equal => {
                    let mut ak: Vec<_> = a.iter().collect();
                    let mut bk: Vec<_> = b.iter().collect();
                    ak.sort_by(|(k1, _), (k2, _)| k1.cmp(k2));
                    bk.sort_by(|(k1, _), (k2, _)| k1.cmp(k2));
                    for ((ka, va), (kb, vb)) in ak.iter().zip(bk.iter()) {
                        match ka.cmp(kb) {
                            std::cmp::Ordering::Equal => {}
                            other => return Some(other),
                        }
                        match values_order(va, vb) {
                            Some(std::cmp::Ordering::Equal) => continue,
                            other => return other,
                        }
                    }
                    Some(std::cmp::Ordering::Equal)
                }
                other => Some(other),
            }
        }
        _ => Some(std::cmp::Ordering::Equal),
    }
}

fn arith_values(left: &Value, op: &ArithOp, right: &Value) -> Option<Value> {
    match op {
        ArithOp::Add => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Some(Value::Int(a.wrapping_add(*b))),
            (Value::Double(a, _), Value::Double(b, _)) => Some(Value::Double(a + b, None)),
            (Value::Int(a), Value::Double(b, _)) => Some(Value::Double(*a as f64 + b, None)),
            (Value::Double(a, _), Value::Int(b)) => Some(Value::Double(a + *b as f64, None)),
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
            (Value::Double(a, _), Value::Double(b, _)) => Some(Value::Double(a - b, None)),
            (Value::Int(a), Value::Double(b, _)) => Some(Value::Double(*a as f64 - b, None)),
            (Value::Double(a, _), Value::Int(b)) => Some(Value::Double(a - *b as f64, None)),
            (Value::Array(a), Value::Array(b)) => {
                let result: Vec<Value> = a
                    .iter()
                    .filter(|v| !b.iter().any(|bv| values_equal(v, bv)))
                    .cloned()
                    .collect();
                Some(Value::Array(Rc::new(result)))
            }
            _ => None,
        },
        ArithOp::Mul => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Some(Value::Int(a.wrapping_mul(*b))),
            (Value::Double(a, _), Value::Double(b, _)) => Some(Value::Double(a * b, None)),
            (Value::Int(a), Value::Double(b, _)) => Some(Value::Double(*a as f64 * b, None)),
            (Value::Double(a, _), Value::Int(b)) => Some(Value::Double(a * *b as f64, None)),
            (Value::Object(a), Value::Object(b)) => Some(object_recursive_merge(a, b)),
            (Value::String(s), Value::Int(n)) | (Value::Int(n), Value::String(s)) => {
                if *n <= 0 {
                    Some(Value::Null)
                } else {
                    Some(Value::String(s.repeat(*n as usize)))
                }
            }
            (Value::Null, _) | (_, Value::Null) => Some(Value::Null),
            _ => None,
        },
        ArithOp::Div => match (left, right) {
            (Value::Int(a), Value::Int(b)) if *b != 0 => {
                if a % b == 0 {
                    Some(Value::Int(a / b))
                } else {
                    Some(Value::Double(*a as f64 / *b as f64, None))
                }
            }
            (Value::Double(a, _), Value::Double(b, _)) => Some(Value::Double(a / b, None)),
            (Value::Int(a), Value::Double(b, _)) => Some(Value::Double(*a as f64 / b, None)),
            (Value::Double(a, _), Value::Int(b)) => Some(Value::Double(a / *b as f64, None)),
            (Value::String(s), Value::String(sep)) => {
                let parts: Vec<Value> = s
                    .split(sep.as_str())
                    .map(|part| Value::String(part.into()))
                    .collect();
                Some(Value::Array(Rc::new(parts)))
            }
            _ => None,
        },
        ArithOp::Mod => match (left, right) {
            (Value::Int(a), Value::Int(b)) if *b != 0 => Some(Value::Int(a % b)),
            (Value::Double(a, _), Value::Double(b, _)) => Some(Value::Double(a % b, None)),
            (Value::Int(a), Value::Double(b, _)) => Some(Value::Double(*a as f64 % b, None)),
            (Value::Double(a, _), Value::Int(b)) => Some(Value::Double(a % *b as f64, None)),
            _ => None,
        },
    }
}

fn object_recursive_merge(a: &Rc<Vec<(String, Value)>>, b: &Rc<Vec<(String, Value)>>) -> Value {
    let mut result: Vec<(String, Value)> = a.as_ref().clone();
    for (k, bv) in b.iter() {
        if let Some(existing) = result.iter_mut().find(|(ek, _)| ek == k) {
            // Recursive merge if both are objects
            if let (Value::Object(ea), Value::Object(eb)) = (&existing.1, bv) {
                existing.1 = object_recursive_merge(ea, eb);
            } else {
                existing.1 = bv.clone();
            }
        } else {
            result.push((k.clone(), bv.clone()));
        }
    }
    Value::Object(Rc::new(result))
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
            Value::String(s) => output(Value::Int(s.chars().count() as i64)),
            Value::Array(a) => output(Value::Int(a.len() as i64)),
            Value::Object(o) => output(Value::Int(o.len() as i64)),
            Value::Null => output(Value::Int(0)),
            Value::Int(n) => output(Value::Int(n.abs())),
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
                value_to_json_string(&mut buf, input);
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
                eval(arg, input, &mut |v| sep = v);
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
                    eval(&args[0], input, &mut |nv| {
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
                    eval(&args[0], input, &mut |from_v| {
                        eval(&args[1], input, &mut |to_v| {
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
                    eval(&args[0], input, &mut |from_v| {
                        eval(&args[1], input, &mut |to_v| {
                            eval(&args[2], input, &mut |step_v| {
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
                output(Value::Double(unsafe { logb(f) }, None));
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
                eval(arg, input, &mut |v| exp = to_f64(&v) as i32);
                output(f64_to_value(unsafe { ldexp(base, exp) }));
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
                output(Value::Double(unsafe { j0(f) }, None));
            }
        }
        "j1" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(unsafe { j1(f) }, None));
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
                eval(base_f, input, &mut |v| base = to_f64(&v));
                eval(exp_f, input, &mut |v| exp = to_f64(&v));
                output(Value::Double(base.powf(exp), None));
            } else if args.len() == 1 {
                // pow(x; y) where input is piped
                // Actually: in jq, pow(a;b) takes two semicolon-separated args
                if let Some(f) = input_as_f64(input) {
                    let mut exp = 0.0_f64;
                    eval(&args[0], input, &mut |v| exp = to_f64(&v));
                    output(Value::Double(f.powf(exp), None));
                }
            }
        }
        "atan2" => {
            if let (Some(y_f), Some(x_f)) = (args.first(), args.get(1)) {
                let mut y = 0.0_f64;
                let mut x = 0.0_f64;
                eval(y_f, input, &mut |v| y = to_f64(&v));
                eval(x_f, input, &mut |v| x = to_f64(&v));
                output(Value::Double(y.atan2(x), None));
            }
        }
        "remainder" => {
            if let (Some(x_f), Some(y_f)) = (args.first(), args.get(1)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                eval(x_f, input, &mut |v| x = to_f64(&v));
                eval(y_f, input, &mut |v| y = to_f64(&v));
                // IEEE remainder
                output(Value::Double(x - (x / y).round() * y, None));
            }
        }
        "hypot" => {
            if let (Some(x_f), Some(y_f)) = (args.first(), args.get(1)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                eval(x_f, input, &mut |v| x = to_f64(&v));
                eval(y_f, input, &mut |v| y = to_f64(&v));
                output(Value::Double(x.hypot(y), None));
            }
        }
        "fma" => {
            if let (Some(x_f), Some(y_f), Some(z_f)) = (args.first(), args.get(1), args.get(2)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                let mut z = 0.0_f64;
                eval(x_f, input, &mut |v| x = to_f64(&v));
                eval(y_f, input, &mut |v| y = to_f64(&v));
                eval(z_f, input, &mut |v| z = to_f64(&v));
                output(Value::Double(x.mul_add(y, z), None));
            }
        }
        "abs" => match input {
            Value::Int(n) => output(Value::Int(n.abs())),
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
                eval(arg, input, &mut |v| needle = v);
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
                eval(arg, input, &mut |v| needle = v);
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
                eval(arg, input, &mut |v| needle = v);
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
            value_to_json_string(&mut buf, input);
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
                eval(arg, input, &mut |v| container = v);
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
                            eval(f, v, &mut |nv| new_val = nv);
                            result.push((k.clone(), new_val));
                        }
                        output(Value::Object(Rc::new(result)));
                    }
                    Value::Array(arr) => {
                        let mut result = Vec::with_capacity(arr.len());
                        for v in arr.iter() {
                            eval(f, v, &mut |nv| result.push(nv));
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
                eval(&args[0], input, &mut |v| n = to_f64(&v) as i64);
                let mut count = 0i64;
                eval(&args[1], input, &mut |v| {
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
                    eval(&args[0], &current, &mut |v| done = v.is_truthy());
                    if done {
                        break;
                    }
                    let mut next = current.clone();
                    eval(&args[1], &current, &mut |v| next = v);
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
                    eval(&args[0], &current, &mut |v| cont = v.is_truthy());
                    if !cont {
                        break;
                    }
                    output(current.clone());
                    let mut next = current.clone();
                    eval(&args[1], &current, &mut |v| next = v);
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
                    eval(f, input, output);
                }
            }
        }
        "isempty" => {
            if let Some(f) = args.first() {
                let mut found = false;
                eval(f, input, &mut |_| found = true);
                output(Value::Bool(!found));
            }
        }
        "getpath" => {
            if let Some(arg) = args.first() {
                let mut path = Value::Null;
                eval(arg, input, &mut |v| path = v);
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
                eval(&args[0], input, &mut |v| path = v);
                eval(&args[1], input, &mut |v| val = v);
                if let Value::Array(path_arr) = path {
                    output(set_path(input, &path_arr, &val));
                }
            }
        }
        "delpaths" => {
            if let Some(arg) = args.first() {
                let mut paths = Value::Null;
                eval(arg, input, &mut |v| paths = v);
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
                eval(arg, input, &mut |v| {
                    if let Value::String(s) = v {
                        label = s;
                    }
                });
                let mut buf = Vec::new();
                value_to_json_string(&mut buf, input);
                let json = String::from_utf8(buf).unwrap_or_default();
                if label.is_empty() {
                    eprintln!("[\"DEBUG:\",{json}]");
                } else {
                    eprintln!("[\"{label}\",{json}]");
                }
            } else {
                let mut buf = Vec::new();
                value_to_json_string(&mut buf, input);
                let json = String::from_utf8(buf).unwrap_or_default();
                eprintln!("[\"DEBUG:\",{json}]");
            }
            output(input.clone());
        }
        "error" => {
            if let Some(arg) = args.first() {
                let mut _msg = Value::Null;
                eval(arg, input, &mut |v| _msg = v);
            }
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
                eval(&args[0], input, &mut |v| n = to_f64(&v) as i64);
                let mut count = 0i64;
                eval(&args[1], input, &mut |v| {
                    if count == n {
                        output(v);
                    }
                    count += 1;
                });
            } else if args.len() == 1 {
                // nth(n) operates on input as generator — take nth from .[]
                let mut n = 0i64;
                eval(&args[0], input, &mut |v| n = to_f64(&v) as i64);
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
                recurse_with_filter(&args[0], input, output, 100_000);
            } else if args.len() == 2 {
                // recurse(f; cond) — recurse while cond is truthy
                recurse_with_cond(&args[0], &args[1], input, output, 100_000);
            }
        }
        "bsearch" => {
            if let (Value::Array(arr), Some(arg)) = (input, args.first()) {
                let mut target = Value::Null;
                eval(arg, input, &mut |v| target = v);
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
                    eval(&args[0], input, &mut |v| {
                        if !found && values_equal(input, &v) {
                            found = true;
                        }
                    });
                    output(Value::Bool(found));
                }
                2 => {
                    // IN(stream; generator) — for each output of stream, test if in generator
                    eval(&args[0], input, &mut |sv| {
                        let mut found = false;
                        eval(&args[1], input, &mut |gv| {
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
                        eval(f, item, &mut |v| mapped.push(v));
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
        "todate" => {
            if let Some(ts) = input_as_f64(input)
                && let Some(tm) = unix_to_tm(ts as i64)
            {
                output(Value::String(format_strftime("%Y-%m-%dT%H:%M:%SZ", &tm)));
            }
        }
        "fromdate" => {
            if let Value::String(s) = input
                && let Some(ts) = parse_iso8601(s)
            {
                output(Value::Int(ts));
            }
        }
        "now" => {
            // now: current unix timestamp
            use std::time::{SystemTime, UNIX_EPOCH};
            if let Ok(dur) = SystemTime::now().duration_since(UNIX_EPOCH) {
                output(Value::Double(dur.as_secs_f64(), None));
            }
        }
        "strftime" => {
            if let (Some(arg), Some(ts)) = (args.first(), input_as_f64(input)) {
                let mut fmt = String::new();
                eval(arg, input, &mut |v| {
                    if let Value::String(s) = v {
                        fmt = s;
                    }
                });
                if let Some(tm) = unix_to_tm(ts as i64) {
                    output(Value::String(format_strftime(&fmt, &tm)));
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

fn to_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Double(f, _) => *f,
        _ => 0.0,
    }
}

fn input_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Double(f, _) => Some(*f),
        _ => None,
    }
}

fn f64_to_value(f: f64) -> Value {
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Value::Int(f as i64)
    } else {
        Value::Double(f, None)
    }
}

unsafe extern "C" {
    fn j0(x: f64) -> f64;
    fn j1(x: f64) -> f64;
    fn frexp(x: f64, exp: *mut i32) -> f64;
    fn logb(x: f64) -> f64;
    fn ldexp(x: f64, exp: i32) -> f64;
    fn gmtime_r(time: *const LibcTimeT, result: *mut LibcTm) -> *mut LibcTm;
    fn timegm(tm: *mut LibcTm) -> LibcTimeT;
    #[link_name = "strftime"]
    fn libc_strftime(s: *mut u8, max: usize, fmt: *const u8, tm: *const LibcTm) -> usize;
}

// Alias so we can use the same type as C's time_t (i64 on 64-bit platforms)
type LibcTimeT = i64;

#[repr(C)]
#[derive(Clone)]
struct LibcTm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const u8,
}

fn libc_frexp(f: f64) -> (f64, i32) {
    let mut exp: i32 = 0;
    let mantissa = unsafe { frexp(f, &mut exp) };
    (mantissa, exp)
}

fn new_libc_tm() -> LibcTm {
    LibcTm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: std::ptr::null(),
    }
}

fn unix_to_tm(ts: i64) -> Option<LibcTm> {
    let mut tm = new_libc_tm();
    let result = unsafe { gmtime_r(&ts, &mut tm) };
    if result.is_null() { None } else { Some(tm) }
}

fn tm_to_unix(tm: &mut LibcTm) -> i64 {
    unsafe { timegm(tm) }
}

fn format_strftime(fmt: &str, tm: &LibcTm) -> String {
    let mut buf = vec![0u8; 256];
    let fmt_cstr = format!("{fmt}\0");
    let len = unsafe { libc_strftime(buf.as_mut_ptr(), buf.len(), fmt_cstr.as_ptr(), tm) };
    if len == 0 && !fmt.is_empty() {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

fn value_to_json_string(buf: &mut Vec<u8>, v: &Value) {
    use std::io::Write;
    match v {
        Value::Null => buf.extend_from_slice(b"null"),
        Value::Bool(true) => buf.extend_from_slice(b"true"),
        Value::Bool(false) => buf.extend_from_slice(b"false"),
        Value::Int(n) => {
            let mut b = itoa::Buffer::new();
            buf.extend_from_slice(b.format(*n).as_bytes());
        }
        Value::Double(f, _) => {
            if f.is_nan() || f.is_infinite() {
                buf.extend_from_slice(b"null");
            } else if f.fract() == 0.0 && *f >= i64::MIN as f64 && *f <= i64::MAX as f64 {
                let mut b = itoa::Buffer::new();
                buf.extend_from_slice(b.format(*f as i64).as_bytes());
            } else {
                let mut b = ryu::Buffer::new();
                buf.extend_from_slice(b.format(*f).as_bytes());
            }
        }
        Value::String(s) => {
            buf.push(b'"');
            for byte in s.bytes() {
                match byte {
                    b'"' => buf.extend_from_slice(b"\\\""),
                    b'\\' => buf.extend_from_slice(b"\\\\"),
                    b'\n' => buf.extend_from_slice(b"\\n"),
                    b'\r' => buf.extend_from_slice(b"\\r"),
                    b'\t' => buf.extend_from_slice(b"\\t"),
                    0..=0x1f => write!(buf, "\\u{byte:04x}").unwrap(),
                    _ => buf.push(byte),
                }
            }
            buf.push(b'"');
        }
        Value::Array(arr) => {
            buf.push(b'[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                value_to_json_string(buf, item);
            }
            buf.push(b']');
        }
        Value::Object(obj) => {
            buf.push(b'{');
            for (i, (k, v)) in obj.iter().enumerate() {
                if i > 0 {
                    buf.push(b',');
                }
                // Write key as JSON string
                buf.push(b'"');
                for byte in k.bytes() {
                    match byte {
                        b'"' => buf.extend_from_slice(b"\\\""),
                        b'\\' => buf.extend_from_slice(b"\\\\"),
                        _ => buf.push(byte),
                    }
                }
                buf.push(b'"');
                buf.push(b':');
                value_to_json_string(buf, v);
            }
            buf.push(b'}');
        }
    }
}

fn set_path(value: &Value, path: &[Value], new_val: &Value) -> Value {
    if path.is_empty() {
        return new_val.clone();
    }
    let seg = &path[0];
    let rest = &path[1..];
    match (value, seg) {
        (Value::Object(obj), Value::String(k)) => {
            let mut result: Vec<(String, Value)> = obj.as_ref().clone();
            if let Some(existing) = result.iter_mut().find(|(ek, _)| ek == k) {
                existing.1 = set_path(&existing.1, rest, new_val);
            } else {
                result.push((k.clone(), set_path(&Value::Null, rest, new_val)));
            }
            Value::Object(Rc::new(result))
        }
        (Value::Array(arr), Value::Int(i)) => {
            let mut result = arr.as_ref().clone();
            let idx = if *i < 0 {
                (result.len() as i64 + i).max(0) as usize
            } else {
                *i as usize
            };
            while result.len() <= idx {
                result.push(Value::Null);
            }
            result[idx] = set_path(&result[idx], rest, new_val);
            Value::Array(Rc::new(result))
        }
        (Value::Null, Value::String(k)) => {
            let inner = set_path(&Value::Null, rest, new_val);
            Value::Object(Rc::new(vec![(k.clone(), inner)]))
        }
        (Value::Null, Value::Int(i)) => {
            let idx = (*i).max(0) as usize;
            let mut arr = vec![Value::Null; idx + 1];
            arr[idx] = set_path(&Value::Null, rest, new_val);
            Value::Array(Rc::new(arr))
        }
        _ => value.clone(),
    }
}

fn del_path(value: &Value, path: &[Value]) -> Value {
    if path.is_empty() {
        return Value::Null;
    }
    if path.len() == 1 {
        match (value, &path[0]) {
            (Value::Object(obj), Value::String(k)) => {
                let result: Vec<_> = obj.iter().filter(|(ek, _)| ek != k).cloned().collect();
                return Value::Object(Rc::new(result));
            }
            (Value::Array(arr), Value::Int(i)) => {
                let idx = if *i < 0 {
                    (arr.len() as i64 + i).max(0) as usize
                } else {
                    *i as usize
                };
                let mut result = arr.as_ref().clone();
                if idx < result.len() {
                    result.remove(idx);
                }
                return Value::Array(Rc::new(result));
            }
            _ => return value.clone(),
        }
    }
    let seg = &path[0];
    let rest = &path[1..];
    match (value, seg) {
        (Value::Object(obj), Value::String(k)) => {
            let mut result: Vec<(String, Value)> = obj.as_ref().clone();
            if let Some(existing) = result.iter_mut().find(|(ek, _)| ek == k) {
                existing.1 = del_path(&existing.1, rest);
            }
            Value::Object(Rc::new(result))
        }
        (Value::Array(arr), Value::Int(i)) => {
            let idx = if *i < 0 {
                (arr.len() as i64 + i).max(0) as usize
            } else {
                *i as usize
            };
            let mut result = arr.as_ref().clone();
            if idx < result.len() {
                result[idx] = del_path(&result[idx], rest);
            }
            Value::Array(Rc::new(result))
        }
        _ => value.clone(),
    }
}

fn enum_paths(
    value: &Value,
    current: &mut Vec<Value>,
    output: &mut dyn FnMut(Value),
    filter: Option<&Filter>,
) {
    match filter {
        Some(f) => {
            let mut is_match = false;
            eval(f, value, &mut |v| {
                if v.is_truthy() {
                    is_match = true;
                }
            });
            if is_match {
                output(Value::Array(Rc::new(current.clone())));
            }
        }
        None => {
            // paths without filter: emit all non-root paths to scalars and containers
        }
    }
    match value {
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                current.push(Value::Int(i as i64));
                if filter.is_none() {
                    output(Value::Array(Rc::new(current.clone())));
                }
                enum_paths(v, current, output, filter);
                current.pop();
            }
        }
        Value::Object(obj) => {
            for (k, v) in obj.iter() {
                current.push(Value::String(k.clone()));
                if filter.is_none() {
                    output(Value::Array(Rc::new(current.clone())));
                }
                enum_paths(v, current, output, filter);
                current.pop();
            }
        }
        _ => {}
    }
}

fn enum_leaf_paths(value: &Value, current: &mut Vec<Value>, output: &mut dyn FnMut(Value)) {
    match value {
        Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                current.push(Value::Int(i as i64));
                enum_leaf_paths(v, current, output);
                current.pop();
            }
        }
        Value::Object(obj) => {
            for (k, v) in obj.iter() {
                current.push(Value::String(k.clone()));
                enum_leaf_paths(v, current, output);
                current.pop();
            }
        }
        _ => {
            output(Value::Array(Rc::new(current.clone())));
        }
    }
}

fn path_of(
    filter: &Filter,
    input: &Value,
    current: &mut Vec<Value>,
    output: &mut dyn FnMut(Value),
) {
    match filter {
        Filter::Field(name) => {
            current.push(Value::String(name.clone()));
            output(Value::Array(Rc::new(current.clone())));
            current.pop();
        }
        Filter::Index(idx_f) => {
            eval(idx_f, input, &mut |idx| {
                current.push(idx);
                output(Value::Array(Rc::new(current.clone())));
                current.pop();
            });
        }
        Filter::Iterate => match input {
            Value::Array(arr) => {
                for i in 0..arr.len() {
                    current.push(Value::Int(i as i64));
                    output(Value::Array(Rc::new(current.clone())));
                    current.pop();
                }
            }
            Value::Object(obj) => {
                for (k, _) in obj.iter() {
                    current.push(Value::String(k.clone()));
                    output(Value::Array(Rc::new(current.clone())));
                    current.pop();
                }
            }
            _ => {}
        },
        Filter::Pipe(a, b) => {
            path_of(a, input, current, &mut |_path_val| {
                // For pipe, we just extend the path
            });
            // Simplified: just output based on the full pipe
            let saved_len = current.len();
            match a.as_ref() {
                Filter::Field(name) => {
                    current.push(Value::String(name.clone()));
                    let next = match input {
                        Value::Object(obj) => obj
                            .iter()
                            .find(|(k, _)| k == name)
                            .map(|(_, v)| v.clone())
                            .unwrap_or(Value::Null),
                        _ => Value::Null,
                    };
                    path_of(b, &next, current, output);
                    current.truncate(saved_len);
                }
                _ => {
                    // Fallback: just evaluate both sides
                    eval(filter, input, &mut |_| {
                        output(Value::Array(Rc::new(current.clone())));
                    });
                }
            }
        }
        Filter::Identity => {
            output(Value::Array(Rc::new(current.clone())));
        }
        _ => {}
    }
}

fn recurse_with_filter(f: &Filter, value: &Value, output: &mut dyn FnMut(Value), limit: usize) {
    if limit == 0 {
        return;
    }
    output(value.clone());
    eval(f, value, &mut |v| {
        if v != Value::Null || matches!(value, Value::Array(_) | Value::Object(_)) {
            // Avoid infinite recursion on atoms producing null
            if !values_equal(&v, value) {
                recurse_with_filter(f, &v, output, limit - 1);
            }
        }
    });
}

fn recurse_with_cond(
    f: &Filter,
    cond: &Filter,
    value: &Value,
    output: &mut dyn FnMut(Value),
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    let mut is_match = false;
    eval(cond, value, &mut |v| {
        if v.is_truthy() {
            is_match = true;
        }
    });
    if !is_match {
        return;
    }
    output(value.clone());
    eval(f, value, &mut |v| {
        if !values_equal(&v, value) {
            recurse_with_cond(f, cond, &v, output, limit - 1);
        }
    });
}

fn parse_iso8601(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: i32 = s[5..7].parse().ok()?;
    let d: i32 = s[8..10].parse().ok()?;
    let h: i32 = s[11..13].parse().ok()?;
    let min: i32 = s[14..16].parse().ok()?;
    let sec: i32 = s[17..19].parse().ok()?;

    let mut tm = new_libc_tm();
    tm.tm_year = y - 1900;
    tm.tm_mon = m - 1;
    tm.tm_mday = d;
    tm.tm_hour = h;
    tm.tm_min = min;
    tm.tm_sec = sec;
    tm.tm_isdst = 0;
    Some(tm_to_unix(&mut tm))
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

    // --- Operator precedence ---

    #[test]
    fn eval_precedence_mul_before_add() {
        assert_eq!(eval_one(&parse("1 + 2 * 3"), &Value::Null), Value::Int(7));
    }

    #[test]
    fn eval_precedence_div_before_sub() {
        assert_eq!(eval_one(&parse("10 - 6 / 2"), &Value::Null), Value::Int(7));
    }

    #[test]
    fn eval_precedence_complex() {
        // 2 * 3 + 4 * 5 = 6 + 20 = 26
        assert_eq!(
            eval_one(&parse("2 * 3 + 4 * 5"), &Value::Null),
            Value::Int(26)
        );
    }

    // --- Cross-type sort ordering ---

    #[test]
    fn eval_sort_mixed_types() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(3),
            Value::String("a".into()),
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(1),
        ]));
        let result = eval_one(&parse("sort"), &input);
        assert_eq!(
            result,
            Value::Array(Rc::new(vec![
                Value::Null,
                Value::Bool(false),
                Value::Bool(true),
                Value::Int(1),
                Value::Int(3),
                Value::String("a".into()),
            ]))
        );
    }

    #[test]
    fn eval_values_order_cross_type() {
        assert_eq!(
            values_order(&Value::Null, &Value::Bool(false)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            values_order(&Value::Bool(true), &Value::Int(0)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            values_order(&Value::Int(999), &Value::String("".into())),
            Some(std::cmp::Ordering::Less)
        );
    }

    #[test]
    fn eval_values_order_arrays() {
        let a = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]));
        let b = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(3)]));
        assert_eq!(values_order(&a, &b), Some(std::cmp::Ordering::Less));
        let c = Value::Array(Rc::new(vec![Value::Int(1)]));
        assert_eq!(values_order(&c, &a), Some(std::cmp::Ordering::Less));
    }

    #[test]
    fn eval_unique_sorts() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(3),
            Value::Int(1),
            Value::Int(2),
            Value::Int(1),
            Value::Int(3),
        ]));
        assert_eq!(
            eval_one(&parse("unique"), &input),
            Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
        );
    }

    // --- range() ---

    #[test]
    fn eval_range_single() {
        assert_eq!(
            eval_all(&parse("range(4)"), &Value::Null),
            vec![Value::Int(0), Value::Int(1), Value::Int(2), Value::Int(3)]
        );
    }

    #[test]
    fn eval_range_two_args() {
        assert_eq!(
            eval_all(&parse("range(2;5)"), &Value::Null),
            vec![Value::Int(2), Value::Int(3), Value::Int(4)]
        );
    }

    #[test]
    fn eval_range_three_args() {
        assert_eq!(
            eval_all(&parse("range(0;10;3)"), &Value::Null),
            vec![Value::Int(0), Value::Int(3), Value::Int(6), Value::Int(9)]
        );
    }

    #[test]
    fn eval_range_empty() {
        assert_eq!(eval_all(&parse("range(0)"), &Value::Null), vec![]);
    }

    // --- Math builtins ---

    #[test]
    fn eval_floor() {
        assert_eq!(
            eval_one(&parse("floor"), &Value::Double(3.7, None)),
            Value::Int(3)
        );
        assert_eq!(
            eval_one(&parse("floor"), &Value::Double(-1.2, None)),
            Value::Int(-2)
        );
    }

    #[test]
    fn eval_ceil() {
        assert_eq!(
            eval_one(&parse("ceil"), &Value::Double(3.2, None)),
            Value::Int(4)
        );
    }

    #[test]
    fn eval_round() {
        assert_eq!(
            eval_one(&parse("round"), &Value::Double(3.5, None)),
            Value::Int(4)
        );
        assert_eq!(
            eval_one(&parse("round"), &Value::Double(3.4, None)),
            Value::Int(3)
        );
    }

    #[test]
    fn eval_sqrt() {
        assert_eq!(
            eval_one(&parse("sqrt"), &Value::Int(9)),
            Value::Double(3.0, None)
        );
    }

    #[test]
    fn eval_fabs() {
        assert_eq!(
            eval_one(&parse("fabs"), &Value::Double(-5.5, None)),
            Value::Double(5.5, None)
        );
    }

    #[test]
    fn eval_nan_isnan() {
        let nan = eval_one(&parse("nan"), &Value::Null);
        assert!(matches!(nan, Value::Double(f, _) if f.is_nan()));
        assert_eq!(
            eval_one(&parse("nan | isnan"), &Value::Null),
            Value::Bool(true)
        );
    }

    #[test]
    fn eval_infinite() {
        assert_eq!(
            eval_one(&parse("infinite | isinfinite"), &Value::Null),
            Value::Bool(true)
        );
        assert_eq!(
            eval_one(&parse("1 | isinfinite"), &Value::Null),
            Value::Bool(false)
        );
    }

    #[test]
    fn eval_isfinite() {
        assert_eq!(
            eval_one(&parse("1 | isfinite"), &Value::Null),
            Value::Bool(true)
        );
    }

    #[test]
    fn eval_pow() {
        assert_eq!(
            eval_one(&parse("pow(2;10)"), &Value::Null),
            Value::Double(1024.0, None)
        );
    }

    #[test]
    fn eval_log_exp_roundtrip() {
        // exp(ln(x)) ≈ x
        let result = eval_one(&parse("log | exp"), &Value::Int(5));
        match result {
            Value::Double(f, _) => assert!((f - 5.0).abs() < 1e-10),
            Value::Int(5) => {} // also fine
            other => panic!("expected ~5.0, got {other:?}"),
        }
    }

    // --- length fixes ---

    #[test]
    fn eval_length_number_abs() {
        assert_eq!(eval_one(&parse("length"), &Value::Int(-5)), Value::Int(5));
        assert_eq!(eval_one(&parse("length"), &Value::Int(0)), Value::Int(0));
    }

    #[test]
    fn eval_length_double_abs() {
        assert_eq!(
            eval_one(&parse("length"), &Value::Double(-3.14, None)),
            Value::Double(3.14, None)
        );
    }

    #[test]
    fn eval_length_unicode_codepoints() {
        // "é" is 1 codepoint, 2 bytes
        assert_eq!(
            eval_one(&parse("length"), &Value::String("é".into())),
            Value::Int(1)
        );
        assert_eq!(
            eval_one(&parse("length"), &Value::String("abc".into())),
            Value::Int(3)
        );
    }

    // --- if with generator condition ---

    #[test]
    fn eval_if_generator_cond() {
        // if (1,2) > 1 then "yes" else "no" end → "no", "yes"
        let results = eval_all(
            &parse("if (1,2) > 1 then \"yes\" else \"no\" end"),
            &Value::Null,
        );
        assert_eq!(
            results,
            vec![Value::String("no".into()), Value::String("yes".into()),]
        );
    }

    #[test]
    fn eval_if_no_else_passthrough() {
        // if false then "x" end → input (pass-through)
        assert_eq!(
            eval_one(&parse("if false then \"x\" end"), &Value::Int(42)),
            Value::Int(42)
        );
    }

    // --- Object construct with generators ---

    #[test]
    fn eval_object_construct_generator() {
        let results = eval_all(&parse("{x: (1,2)}"), &Value::Null);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], obj(&[("x", Value::Int(1))]));
        assert_eq!(results[1], obj(&[("x", Value::Int(2))]));
    }

    #[test]
    fn eval_object_construct_multi_pair_generator() {
        // {a: (1,2), b: (3,4)} → 4 objects
        let results = eval_all(&parse("{a: (1,2), b: (3,4)}"), &Value::Null);
        assert_eq!(results.len(), 4);
    }

    // --- String builtins ---

    #[test]
    fn eval_split_empty() {
        assert_eq!(
            eval_one(&parse("split(\"\")"), &Value::String("abc".into())),
            Value::Array(Rc::new(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]))
        );
    }

    #[test]
    fn eval_ascii_downcase_only_ascii() {
        assert_eq!(
            eval_one(&parse("ascii_downcase"), &Value::String("ABCé".into())),
            Value::String("abcé".into())
        );
    }

    #[test]
    fn eval_explode() {
        assert_eq!(
            eval_one(&parse("explode"), &Value::String("abc".into())),
            Value::Array(Rc::new(vec![
                Value::Int(97),
                Value::Int(98),
                Value::Int(99)
            ]))
        );
    }

    #[test]
    fn eval_implode() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(97),
            Value::Int(98),
            Value::Int(99),
        ]));
        assert_eq!(
            eval_one(&parse("implode"), &input),
            Value::String("abc".into())
        );
    }

    #[test]
    fn eval_tojson() {
        let input = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(
            eval_one(&parse("tojson"), &input),
            Value::String("[1,2]".into())
        );
    }

    #[test]
    fn eval_utf8bytelength() {
        // "é" = 2 bytes in UTF-8
        assert_eq!(
            eval_one(&parse("utf8bytelength"), &Value::String("é".into())),
            Value::Int(2)
        );
        assert_eq!(
            eval_one(&parse("utf8bytelength"), &Value::String("abc".into())),
            Value::Int(3)
        );
    }

    #[test]
    fn eval_inside() {
        let input = Value::String("foo".into());
        assert_eq!(
            eval_one(&parse("inside(\"foobar\")"), &input),
            Value::Bool(true)
        );
        assert_eq!(
            eval_one(&parse("inside(\"bar\")"), &input),
            Value::Bool(false)
        );
    }

    #[test]
    fn eval_index_builtin() {
        assert_eq!(
            eval_one(&parse("index(\"b\")"), &Value::String("abc".into())),
            Value::Int(1)
        );
        assert_eq!(
            eval_one(&parse("index(\"z\")"), &Value::String("abc".into())),
            Value::Null
        );
    }

    #[test]
    fn eval_rindex_builtin() {
        assert_eq!(
            eval_one(&parse("rindex(\"o\")"), &Value::String("fooboo".into())),
            Value::Int(5)
        );
    }

    #[test]
    fn eval_indices_builtin() {
        assert_eq!(
            eval_one(&parse("indices(\"o\")"), &Value::String("foobar".into())),
            Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn eval_trim() {
        assert_eq!(
            eval_one(&parse("trim"), &Value::String("  hi  ".into())),
            Value::String("hi".into())
        );
        assert_eq!(
            eval_one(&parse("ltrim"), &Value::String("  hi  ".into())),
            Value::String("hi  ".into())
        );
        assert_eq!(
            eval_one(&parse("rtrim"), &Value::String("  hi  ".into())),
            Value::String("  hi".into())
        );
    }

    // --- String arithmetic ---

    #[test]
    fn eval_string_repeat() {
        assert_eq!(
            eval_one(&parse("\"ab\" * 3"), &Value::Null),
            Value::String("ababab".into())
        );
    }

    #[test]
    fn eval_string_repeat_zero() {
        assert_eq!(eval_one(&parse("\"ab\" * 0"), &Value::Null), Value::Null);
    }

    #[test]
    fn eval_string_divide() {
        assert_eq!(
            eval_one(&parse("\"a,b,c\" / \",\""), &Value::Null),
            Value::Array(Rc::new(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]))
        );
    }

    // --- Bug fixes ---

    #[test]
    fn eval_from_entries_capitalized() {
        let input = Value::Array(Rc::new(vec![Value::Object(Rc::new(vec![
            ("Key".into(), Value::String("x".into())),
            ("Value".into(), Value::Int(42)),
        ]))]));
        assert_eq!(
            eval_one(&parse("from_entries"), &input),
            obj(&[("x", Value::Int(42))])
        );
    }

    #[test]
    fn eval_values_iterates() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let results = eval_all(&parse("values"), &input);
        assert_eq!(results, vec![Value::Int(1), Value::Int(2)]);
    }

    #[test]
    fn eval_index_generator() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        assert_eq!(
            eval_all(&parse(".[0,2]"), &input),
            vec![Value::Int(10), Value::Int(30)]
        );
    }

    #[test]
    fn eval_array_subtraction() {
        let a = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        let b = Value::Array(Rc::new(vec![Value::Int(2)]));
        assert_eq!(
            arith_values(&a, &ArithOp::Sub, &b),
            Some(Value::Array(Rc::new(vec![Value::Int(1), Value::Int(3)])))
        );
    }

    #[test]
    fn eval_object_recursive_merge() {
        let result = eval_one(
            &parse("{\"a\":{\"b\":1}} * {\"a\":{\"c\":2}}"),
            &Value::Null,
        );
        // Should have a.b=1 and a.c=2
        if let Value::Object(outer) = &result {
            let a_val = &outer.iter().find(|(k, _)| k == "a").unwrap().1;
            if let Value::Object(inner) = a_val {
                assert_eq!(inner.len(), 2);
            } else {
                panic!("expected inner object");
            }
        } else {
            panic!("expected outer object");
        }
    }

    #[test]
    fn eval_float_modulo() {
        let result = arith_values(&Value::Double(10.5, None), &ArithOp::Mod, &Value::Int(3));
        match result {
            Some(Value::Double(f, _)) => assert!((f - 1.5).abs() < 1e-10),
            other => panic!("expected Double(1.5), got {other:?}"),
        }
    }

    #[test]
    fn eval_int_division_float_result() {
        // 1 / 3 should produce a float, not truncate to 0
        let result = arith_values(&Value::Int(1), &ArithOp::Div, &Value::Int(3));
        match result {
            Some(Value::Double(f, _)) => assert!((f - 1.0 / 3.0).abs() < 1e-10),
            other => panic!("expected Double(0.333...), got {other:?}"),
        }
    }

    #[test]
    fn eval_int_division_exact() {
        // 6 / 3 should produce Int(2), not Double
        assert_eq!(
            arith_values(&Value::Int(6), &ArithOp::Div, &Value::Int(3)),
            Some(Value::Int(2))
        );
    }

    #[test]
    fn eval_compare_generator() {
        // (1,2) > 1 should produce false, true
        let results = eval_all(&parse("(1,2) > 1"), &Value::Null);
        assert_eq!(results, vec![Value::Bool(false), Value::Bool(true)]);
    }

    #[test]
    fn eval_arith_generator() {
        // (1,2) + (10,20) should produce 11, 21, 12, 22
        let results = eval_all(&parse("(1,2) + (10,20)"), &Value::Null);
        assert_eq!(
            results,
            vec![
                Value::Int(11),
                Value::Int(21),
                Value::Int(12),
                Value::Int(22),
            ]
        );
    }

    // --- Collection builtins ---

    #[test]
    fn eval_transpose() {
        let input = Value::Array(Rc::new(vec![
            Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)])),
            Value::Array(Rc::new(vec![Value::Int(3), Value::Int(4)])),
        ]));
        assert_eq!(
            eval_one(&parse("transpose"), &input),
            Value::Array(Rc::new(vec![
                Value::Array(Rc::new(vec![Value::Int(1), Value::Int(3)])),
                Value::Array(Rc::new(vec![Value::Int(2), Value::Int(4)])),
            ]))
        );
    }

    #[test]
    fn eval_transpose_uneven() {
        // [[1],[2,3]] → [[1,2],[null,3]]
        let input = Value::Array(Rc::new(vec![
            Value::Array(Rc::new(vec![Value::Int(1)])),
            Value::Array(Rc::new(vec![Value::Int(2), Value::Int(3)])),
        ]));
        let result = eval_one(&parse("transpose"), &input);
        assert_eq!(
            result,
            Value::Array(Rc::new(vec![
                Value::Array(Rc::new(vec![Value::Int(1), Value::Int(2)])),
                Value::Array(Rc::new(vec![Value::Null, Value::Int(3)])),
            ]))
        );
    }

    #[test]
    fn eval_map_values_object() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(
            eval_one(&parse("map_values(. + 10)"), &input),
            obj(&[("a", Value::Int(11)), ("b", Value::Int(12))])
        );
    }

    #[test]
    fn eval_limit() {
        assert_eq!(
            eval_all(&parse("limit(3; range(100))"), &Value::Null),
            vec![Value::Int(0), Value::Int(1), Value::Int(2)]
        );
    }

    #[test]
    fn eval_until() {
        assert_eq!(
            eval_one(&parse("0 | until(. >= 5; . + 1)"), &Value::Null),
            Value::Int(5)
        );
    }

    #[test]
    fn eval_while() {
        let input = Value::Null;
        let results = eval_all(&parse("[1 | while(. < 8; . * 2)]"), &input);
        assert_eq!(
            results,
            vec![Value::Array(Rc::new(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(4),
            ]))]
        );
    }

    #[test]
    fn eval_isempty_true() {
        assert_eq!(
            eval_one(&parse("isempty(empty)"), &Value::Null),
            Value::Bool(true)
        );
    }

    #[test]
    fn eval_isempty_false() {
        assert_eq!(
            eval_one(&parse("isempty(range(3))"), &Value::Null),
            Value::Bool(false)
        );
    }

    #[test]
    fn eval_getpath() {
        let input = obj(&[("a", obj(&[("b", Value::Int(42))]))]);
        assert_eq!(
            eval_one(&parse("getpath([\"a\",\"b\"])"), &input),
            Value::Int(42)
        );
    }

    #[test]
    fn eval_getpath_missing() {
        let input = obj(&[("a", Value::Int(1))]);
        assert_eq!(eval_one(&parse("getpath([\"x\"])"), &input), Value::Null);
    }

    #[test]
    fn eval_setpath() {
        let input = obj(&[("a", obj(&[("b", Value::Int(1))]))]);
        let result = eval_one(&parse("setpath([\"a\",\"b\"]; 99)"), &input);
        assert_eq!(result, obj(&[("a", obj(&[("b", Value::Int(99))]))]));
    }

    #[test]
    fn eval_delpaths() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        assert_eq!(
            eval_one(&parse("delpaths([[\"a\"]])"), &input),
            obj(&[("b", Value::Int(2))])
        );
    }

    #[test]
    fn eval_paths_no_filter() {
        let input = obj(&[("a", Value::Int(1)), ("b", obj(&[("c", Value::Int(2))]))]);
        let results = eval_all(&parse("paths"), &input);
        assert_eq!(results.len(), 3); // ["a"], ["b"], ["b","c"]
    }

    #[test]
    fn eval_bsearch_found() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
            Value::Int(5),
        ]));
        assert_eq!(eval_one(&parse("bsearch(3)"), &input), Value::Int(2));
    }

    #[test]
    fn eval_bsearch_not_found() {
        let input = Value::Array(Rc::new(vec![Value::Int(1), Value::Int(3), Value::Int(5)]));
        // 2 would go at index 1, so returns -(1)-1 = -2
        assert_eq!(eval_one(&parse("bsearch(2)"), &input), Value::Int(-2));
    }

    #[test]
    fn eval_in_builtin() {
        assert_eq!(
            eval_one(&parse("IN(2, 3)"), &Value::Int(3)),
            Value::Bool(true)
        );
        assert_eq!(
            eval_one(&parse("IN(2, 3)"), &Value::Int(5)),
            Value::Bool(false)
        );
    }

    #[test]
    fn eval_with_entries() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let result = eval_one(&parse("with_entries(select(.value > 1))"), &input);
        assert_eq!(result, obj(&[("b", Value::Int(2))]));
    }

    #[test]
    fn eval_abs() {
        assert_eq!(eval_one(&parse("abs"), &Value::Int(-42)), Value::Int(42));
        assert_eq!(eval_one(&parse("abs"), &Value::Int(42)), Value::Int(42));
    }

    #[test]
    fn eval_debug_passthrough() {
        // debug should return the input unchanged
        assert_eq!(eval_one(&parse("debug"), &Value::Int(42)), Value::Int(42));
    }

    #[test]
    fn eval_error_no_output() {
        assert_eq!(eval_all(&parse("error"), &Value::Int(42)), vec![]);
    }

    #[test]
    fn eval_nth() {
        assert_eq!(
            eval_one(&parse("nth(2; range(5))"), &Value::Null),
            Value::Int(2)
        );
    }

    #[test]
    fn eval_repeat() {
        // repeat(f) applies f to same input each time: f, repeat(f)
        let results = eval_all(&parse("limit(3; 5 | repeat(. + 1))"), &Value::Null);
        assert_eq!(results, vec![Value::Int(6), Value::Int(6), Value::Int(6)]);
    }

    #[test]
    fn eval_recurse_with_filter_and_cond() {
        let results = eval_all(&parse("2 | recurse(. * .; . < 100)"), &Value::Null);
        assert_eq!(results, vec![Value::Int(2), Value::Int(4), Value::Int(16)]);
    }

    #[test]
    fn eval_string_interp_with_array() {
        // Build StringInterp AST directly since parser doesn't support \(...) yet
        use crate::filter::StringPart;
        let filter = Filter::StringInterp(vec![
            StringPart::Lit("items: ".to_string()),
            StringPart::Expr(Filter::Literal(Value::Array(Rc::new(vec![
                Value::Int(1),
                Value::Int(2),
            ])))),
        ]);
        let result = eval_one(&filter, &Value::Null);
        assert_eq!(result, Value::String("items: [1,2]".to_string()));
    }

    #[test]
    fn eval_string_interp_with_object() {
        use crate::filter::StringPart;
        let filter = Filter::StringInterp(vec![
            StringPart::Lit("obj: ".to_string()),
            StringPart::Expr(Filter::Literal(Value::Object(Rc::new(vec![(
                "a".to_string(),
                Value::Int(1),
            )])))),
        ]);
        let result = eval_one(&filter, &Value::Null);
        assert_eq!(result, Value::String(r#"obj: {"a":1}"#.to_string()));
    }

    #[test]
    fn eval_tostring_array() {
        let result = eval_one(&parse("[1,2,3] | tostring"), &Value::Null);
        assert_eq!(result, Value::String("[1,2,3]".to_string()));
    }

    #[test]
    fn eval_tostring_object() {
        let result = eval_one(&parse(r#"{"a":1} | tostring"#), &Value::Null);
        assert_eq!(result, Value::String(r#"{"a":1}"#.to_string()));
    }

    #[test]
    fn eval_logb() {
        let result = eval_one(&parse("1 | logb"), &Value::Null);
        assert_eq!(result, Value::Double(0.0, None));
        let result = eval_one(&parse("8 | logb"), &Value::Null);
        assert_eq!(result, Value::Double(3.0, None));
    }

    #[test]
    fn eval_scalb() {
        let result = eval_one(&parse("2 | scalb(3)"), &Value::Null);
        assert_eq!(result, Value::Int(16));
        let result = eval_one(&parse("1 | scalb(10)"), &Value::Null);
        assert_eq!(result, Value::Int(1024));
    }

    #[test]
    fn eval_env() {
        // env should return a non-empty object
        let result = eval_one(&parse("env | keys | length"), &Value::Null);
        match result {
            Value::Int(n) => assert!(n > 0, "env should have entries"),
            _ => panic!("expected int from env | keys | length"),
        }
    }

    // --- Helper function tests ---

    #[test]
    fn test_to_f64() {
        assert_eq!(to_f64(&Value::Int(5)), 5.0);
        assert_eq!(to_f64(&Value::Double(3.14, None)), 3.14);
        assert_eq!(to_f64(&Value::Null), 0.0);
    }

    #[test]
    fn test_f64_to_value_int() {
        assert_eq!(f64_to_value(5.0), Value::Int(5));
        assert_eq!(f64_to_value(0.0), Value::Int(0));
        assert_eq!(f64_to_value(-3.0), Value::Int(-3));
    }

    #[test]
    fn test_f64_to_value_double() {
        assert_eq!(f64_to_value(3.14), Value::Double(3.14, None));
    }

    #[test]
    fn test_frexp() {
        let (m, e) = libc_frexp(0.0);
        assert_eq!(m, 0.0);
        assert_eq!(e, 0);

        let (m, e) = libc_frexp(1.0);
        assert!((m - 0.5).abs() < 1e-10);
        assert_eq!(e, 1);

        let (m, e) = libc_frexp(-0.5);
        assert!((m - (-0.5)).abs() < 1e-10);
        assert_eq!(e, 0);

        let (m, _) = libc_frexp(f64::INFINITY);
        assert!(m.is_infinite());

        let (m, _) = libc_frexp(f64::NAN);
        assert!(m.is_nan());
    }

    #[test]
    fn test_unix_to_tm_epoch() {
        let tm = unix_to_tm(0).unwrap();
        assert_eq!(tm.tm_year, 70); // years since 1900
        assert_eq!(tm.tm_mon, 0); // 0-indexed
        assert_eq!(tm.tm_mday, 1);
        assert_eq!(tm.tm_hour, 0);
        assert_eq!(tm.tm_min, 0);
        assert_eq!(tm.tm_sec, 0);
    }

    #[test]
    fn test_unix_to_tm_known() {
        // 2024-01-15 11:30:45 UTC = 1705318245
        let tm = unix_to_tm(1705318245).unwrap();
        assert_eq!(tm.tm_year + 1900, 2024);
        assert_eq!(tm.tm_mon + 1, 1);
        assert_eq!(tm.tm_mday, 15);
        assert_eq!(tm.tm_hour, 11);
        assert_eq!(tm.tm_min, 30);
        assert_eq!(tm.tm_sec, 45);
    }

    #[test]
    fn test_unix_to_tm_negative() {
        // Before epoch: 1969-12-31 23:59:59 = -1
        let tm = unix_to_tm(-1).unwrap();
        assert_eq!(tm.tm_year + 1900, 1969);
        assert_eq!(tm.tm_mon + 1, 12);
        assert_eq!(tm.tm_mday, 31);
        assert_eq!(tm.tm_hour, 23);
        assert_eq!(tm.tm_min, 59);
        assert_eq!(tm.tm_sec, 59);
    }

    #[test]
    fn test_parse_iso8601() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("short"), None);
    }

    #[test]
    fn test_parse_iso8601_roundtrip() {
        let ts = 1705318245_i64;
        let tm = unix_to_tm(ts).unwrap();
        let s = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        );
        assert_eq!(parse_iso8601(&s), Some(ts));
    }

    #[test]
    fn test_format_strftime() {
        let tm = unix_to_tm(0).unwrap();
        assert_eq!(format_strftime("%Y-%m-%d", &tm), "1970-01-01");
        assert_eq!(format_strftime("%A", &tm), "Thursday");
        assert_eq!(format_strftime("%j", &tm), "001");
    }

    #[test]
    fn test_value_to_json_string() {
        let mut buf = Vec::new();
        value_to_json_string(&mut buf, &Value::Int(42));
        assert_eq!(String::from_utf8(buf).unwrap(), "42");

        let mut buf = Vec::new();
        value_to_json_string(&mut buf, &Value::String("he\"llo".into()));
        assert_eq!(String::from_utf8(buf).unwrap(), r#""he\"llo""#);

        let mut buf = Vec::new();
        value_to_json_string(&mut buf, &Value::Null);
        assert_eq!(String::from_utf8(buf).unwrap(), "null");
    }

    #[test]
    fn test_set_path_creates_nested() {
        let result = set_path(&Value::Null, &[Value::String("a".into())], &Value::Int(1));
        assert_eq!(result, obj(&[("a", Value::Int(1))]));
    }

    #[test]
    fn test_del_path_object() {
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let result = del_path(&input, &[Value::String("a".into())]);
        assert_eq!(result, obj(&[("b", Value::Int(2))]));
    }

    #[test]
    fn test_del_path_array() {
        let input = Value::Array(Rc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        let result = del_path(&input, &[Value::Int(1)]);
        assert_eq!(
            result,
            Value::Array(Rc::new(vec![Value::Int(10), Value::Int(30)]))
        );
    }

    #[test]
    fn test_object_recursive_merge_fn() {
        let a = Rc::new(vec![(
            "x".to_string(),
            Value::Object(Rc::new(vec![("y".to_string(), Value::Int(1))])),
        )]);
        let b = Rc::new(vec![(
            "x".to_string(),
            Value::Object(Rc::new(vec![("z".to_string(), Value::Int(2))])),
        )]);
        let result = object_recursive_merge(&a, &b);
        if let Value::Object(obj) = result {
            let x = &obj.iter().find(|(k, _)| k == "x").unwrap().1;
            if let Value::Object(inner) = x {
                assert_eq!(inner.len(), 2);
            } else {
                panic!("expected nested object");
            }
        } else {
            panic!("expected object");
        }
    }
}
