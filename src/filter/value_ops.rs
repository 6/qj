/// Numeric utilities, math FFI, date/time helpers (jiff), path operations,
/// and pure value-manipulation functions used by both eval.rs and builtins.
use crate::filter::{ArithOp, CmpOp, Env, Filter};
use crate::value::Value;
use std::rc::Rc;

use super::eval::eval;

// ---------------------------------------------------------------------------
// Numeric helpers
// ---------------------------------------------------------------------------

pub(super) fn to_f64(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Double(f, _) => *f,
        _ => 0.0,
    }
}

pub(super) fn input_as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(n) => Some(*n as f64),
        Value::Double(f, _) => Some(*f),
        _ => None,
    }
}

pub(super) fn f64_to_value(f: f64) -> Value {
    // Use strict < for upper bound: i64::MAX as f64 rounds up to 2^63 which
    // doesn't fit in i64, so `f as i64` would saturate to i64::MAX.
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f < i64::MAX as f64 {
        Value::Int(f as i64)
    } else {
        Value::Double(f, None)
    }
}

// ---------------------------------------------------------------------------
// Math FFI (libc)
// ---------------------------------------------------------------------------

unsafe extern "C" {
    fn j0(x: f64) -> f64;
    fn j1(x: f64) -> f64;
    fn frexp(x: f64, exp: *mut i32) -> f64;
    fn logb(x: f64) -> f64;
    fn ldexp(x: f64, exp: i32) -> f64;
}

pub(super) fn libc_frexp(f: f64) -> (f64, i32) {
    let mut exp: i32 = 0;
    let mantissa = unsafe { frexp(f, &mut exp) };
    (mantissa, exp)
}

pub(super) fn libc_j0(f: f64) -> f64 {
    unsafe { j0(f) }
}

pub(super) fn libc_j1(f: f64) -> f64 {
    unsafe { j1(f) }
}

pub(super) fn libc_logb(f: f64) -> f64 {
    unsafe { logb(f) }
}

pub(super) fn libc_ldexp(f: f64, exp: i32) -> f64 {
    unsafe { ldexp(f, exp) }
}

// ---------------------------------------------------------------------------
// Date/time helpers (jiff)
// ---------------------------------------------------------------------------

use jiff::Timestamp;

pub(super) fn todate(secs: i64) -> Option<String> {
    let ts = Timestamp::from_second(secs).ok()?;
    Some(
        ts.to_zoned(jiff::tz::TimeZone::UTC)
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

pub(super) fn fromdate(s: &str) -> Option<i64> {
    // Try Timestamp first (handles "2024-01-15T11:30:45Z" etc)
    if let Ok(ts) = s.parse::<Timestamp>() {
        return Some(ts.as_second());
    }
    // Fallback: try parsing as civil datetime and assume UTC
    if let Ok(dt) = s.parse::<jiff::civil::DateTime>() {
        return dt
            .to_zoned(jiff::tz::TimeZone::UTC)
            .ok()
            .map(|z| z.timestamp().as_second());
    }
    None
}

pub(super) fn now_timestamp() -> f64 {
    let ts = Timestamp::now();
    ts.as_second() as f64 + ts.subsec_nanosecond() as f64 / 1_000_000_000.0
}

pub(super) fn format_strftime_jiff(fmt: &str, secs: i64) -> Option<String> {
    let ts = Timestamp::from_second(secs).ok()?;
    Some(
        ts.to_zoned(jiff::tz::TimeZone::UTC)
            .strftime(fmt)
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Path operations
// ---------------------------------------------------------------------------

pub(super) fn set_path(value: &Value, path: &[Value], new_val: &Value) -> Value {
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

pub(super) fn del_path(value: &Value, path: &[Value]) -> Value {
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

pub(super) fn get_path(value: &Value, path: &[Value]) -> Value {
    let mut current = value.clone();
    for seg in path {
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
    current
}

pub(super) fn enum_paths(
    value: &Value,
    current: &mut Vec<Value>,
    output: &mut dyn FnMut(Value),
    filter: Option<&Filter>,
) {
    let env = Env::empty();
    match filter {
        Some(f) => {
            let mut is_match = false;
            eval(f, value, &env, &mut |v| {
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

pub(super) fn enum_leaf_paths(
    value: &Value,
    current: &mut Vec<Value>,
    output: &mut dyn FnMut(Value),
) {
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

pub(super) fn path_of(
    filter: &Filter,
    input: &Value,
    current: &mut Vec<Value>,
    output: &mut dyn FnMut(Value),
) {
    let env = Env::empty();
    match filter {
        Filter::Field(name) => {
            current.push(Value::String(name.clone()));
            output(Value::Array(Rc::new(current.clone())));
            current.pop();
        }
        Filter::Index(idx_f) => {
            eval(idx_f, input, &env, &mut |idx| {
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
            let saved_len = current.len();
            // Collect all paths from LHS, navigate to each, then resolve RHS paths
            let mut lhs_paths: Vec<Vec<Value>> = Vec::new();
            path_of(a, input, current, &mut |p| {
                if let Value::Array(arr) = p {
                    lhs_paths.push(arr.as_ref().clone());
                }
            });
            for lhs_path in &lhs_paths {
                current.truncate(saved_len);
                current.extend_from_slice(lhs_path);
                let next = get_path(input, lhs_path);
                path_of(b, &next, current, output);
            }
            current.truncate(saved_len);
        }
        Filter::Identity => {
            output(Value::Array(Rc::new(current.clone())));
        }
        Filter::Select(cond) => {
            let mut is_match = false;
            eval(cond, input, &env, &mut |v| {
                if v.is_truthy() {
                    is_match = true;
                }
            });
            if is_match {
                output(Value::Array(Rc::new(current.clone())));
            }
        }
        Filter::Comma(items) => {
            for item in items {
                path_of(item, input, current, output);
            }
        }
        Filter::Recurse => {
            fn recurse_paths(
                value: &Value,
                current: &mut Vec<Value>,
                output: &mut dyn FnMut(Value),
            ) {
                output(Value::Array(Rc::new(current.clone())));
                match value {
                    Value::Array(arr) => {
                        for (i, v) in arr.iter().enumerate() {
                            current.push(Value::Int(i as i64));
                            recurse_paths(v, current, output);
                            current.pop();
                        }
                    }
                    Value::Object(obj) => {
                        for (k, v) in obj.iter() {
                            current.push(Value::String(k.clone()));
                            recurse_paths(v, current, output);
                            current.pop();
                        }
                    }
                    _ => {}
                }
            }
            recurse_paths(input, current, output);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Value comparison, ordering, arithmetic, and traversal
// ---------------------------------------------------------------------------

pub(super) fn compare_values(left: &Value, op: &CmpOp, right: &Value) -> bool {
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

pub(super) fn values_equal(left: &Value, right: &Value) -> bool {
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

pub(super) fn values_order(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
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

/// Format a Value for use in error messages (compact representation).
fn value_desc(v: &Value) -> String {
    v.short_desc()
}

pub(super) fn arith_values(left: &Value, op: &ArithOp, right: &Value) -> Result<Value, String> {
    match op {
        ArithOp::Add => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(a
                .checked_add(*b)
                .map_or_else(|| Value::Double(*a as f64 + *b as f64, None), Value::Int)),
            (Value::Double(a, _), Value::Double(b, _)) => Ok(Value::Double(a + b, None)),
            (Value::Int(a), Value::Double(b, _)) => Ok(Value::Double(*a as f64 + b, None)),
            (Value::Double(a, _), Value::Int(b)) => Ok(Value::Double(a + *b as f64, None)),
            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{a}{b}"))),
            (Value::Array(a), Value::Array(b)) => {
                let mut result = Vec::with_capacity(a.len() + b.len());
                result.extend_from_slice(a);
                result.extend_from_slice(b);
                Ok(Value::Array(Rc::new(result)))
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
                Ok(Value::Object(Rc::new(result)))
            }
            (Value::Null, other) | (other, Value::Null) => Ok(other.clone()),
            _ => Err(format!(
                "{} and {} cannot be added",
                left.type_name(),
                right.type_name()
            )),
        },
        ArithOp::Sub => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(a
                .checked_sub(*b)
                .map_or_else(|| Value::Double(*a as f64 - *b as f64, None), Value::Int)),
            (Value::Double(a, _), Value::Double(b, _)) => Ok(Value::Double(a - b, None)),
            (Value::Int(a), Value::Double(b, _)) => Ok(Value::Double(*a as f64 - b, None)),
            (Value::Double(a, _), Value::Int(b)) => Ok(Value::Double(a - *b as f64, None)),
            (Value::Array(a), Value::Array(b)) => {
                let result: Vec<Value> = a
                    .iter()
                    .filter(|v| !b.iter().any(|bv| values_equal(v, bv)))
                    .cloned()
                    .collect();
                Ok(Value::Array(Rc::new(result)))
            }
            _ => Err(format!(
                "{} ({}) and {} ({}) cannot be subtracted",
                left.type_name(),
                value_desc(left),
                right.type_name(),
                value_desc(right)
            )),
        },
        ArithOp::Mul => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(a
                .checked_mul(*b)
                .map_or_else(|| Value::Double(*a as f64 * *b as f64, None), Value::Int)),
            (Value::Double(a, _), Value::Double(b, _)) => Ok(Value::Double(a * b, None)),
            (Value::Int(a), Value::Double(b, _)) => Ok(Value::Double(*a as f64 * b, None)),
            (Value::Double(a, _), Value::Int(b)) => Ok(Value::Double(a * *b as f64, None)),
            (Value::Object(a), Value::Object(b)) => Ok(object_recursive_merge(a, b)),
            (Value::String(s), Value::Int(n)) | (Value::Int(n), Value::String(s)) => {
                if *n <= 0 {
                    Ok(Value::Null)
                } else {
                    Ok(Value::String(s.repeat(*n as usize)))
                }
            }
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            _ => Err(format!(
                "{} and {} cannot be multiplied",
                left.type_name(),
                right.type_name()
            )),
        },
        ArithOp::Div => match (left, right) {
            // Integer division by zero
            (Value::Int(_), Value::Int(b)) if *b == 0 => Err(format!(
                "number ({}) and number (0) cannot be divided because the divisor is zero",
                value_desc(left)
            )),
            (Value::Int(a), Value::Int(b)) => {
                // i64::MIN / -1 overflows (panics in debug, wraps in release)
                if let Some(q) = a.checked_div(*b) {
                    if a % b == 0 {
                        Ok(Value::Int(q))
                    } else {
                        Ok(Value::Double(*a as f64 / *b as f64, None))
                    }
                } else {
                    Ok(Value::Double(*a as f64 / *b as f64, None))
                }
            }
            // Float division by zero
            (Value::Double(_, _), Value::Double(b, _)) if *b == 0.0 => Err(format!(
                "number ({}) and number (0) cannot be divided because the divisor is zero",
                value_desc(left)
            )),
            (Value::Int(_), Value::Double(b, _)) if *b == 0.0 => Err(format!(
                "number ({}) and number (0) cannot be divided because the divisor is zero",
                value_desc(left)
            )),
            (Value::Double(_, _), Value::Int(b)) if *b == 0 => Err(format!(
                "number ({}) and number (0) cannot be divided because the divisor is zero",
                value_desc(left)
            )),
            (Value::Double(a, _), Value::Double(b, _)) => Ok(Value::Double(a / b, None)),
            (Value::Int(a), Value::Double(b, _)) => Ok(Value::Double(*a as f64 / b, None)),
            (Value::Double(a, _), Value::Int(b)) => Ok(Value::Double(a / *b as f64, None)),
            (Value::String(s), Value::String(sep)) => {
                let parts: Vec<Value> = s
                    .split(sep.as_str())
                    .map(|part| Value::String(part.into()))
                    .collect();
                Ok(Value::Array(Rc::new(parts)))
            }
            _ => Err(format!(
                "{} and {} cannot be divided",
                left.type_name(),
                right.type_name()
            )),
        },
        ArithOp::Mod => match (left, right) {
            // Integer modulo by zero
            (Value::Int(_), Value::Int(b)) if *b == 0 => Err(format!(
                "number ({}) and number (0) cannot be divided (remainder) because the divisor is zero",
                value_desc(left)
            )),
            // i64::MIN % -1 can panic in debug mode; mathematically it's 0
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.checked_rem(*b).unwrap_or(0))),
            // Float modulo by zero
            (Value::Double(_, _), Value::Double(b, _)) if *b == 0.0 => Err(format!(
                "number ({}) and number (0) cannot be divided (remainder) because the divisor is zero",
                value_desc(left)
            )),
            (Value::Int(_), Value::Double(b, _)) if *b == 0.0 => Err(format!(
                "number ({}) and number (0) cannot be divided (remainder) because the divisor is zero",
                value_desc(left)
            )),
            (Value::Double(_, _), Value::Int(b)) if *b == 0 => Err(format!(
                "number ({}) and number (0) cannot be divided (remainder) because the divisor is zero",
                value_desc(left)
            )),
            (Value::Double(a, _), Value::Double(b, _)) => Ok(Value::Double(a % b, None)),
            (Value::Int(a), Value::Double(b, _)) => Ok(Value::Double(*a as f64 % b, None)),
            (Value::Double(a, _), Value::Int(b)) => Ok(Value::Double(a % *b as f64, None)),
            _ => Err(format!(
                "{} and {} cannot be divided (remainder)",
                left.type_name(),
                right.type_name()
            )),
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

pub(super) fn recurse(value: &Value, output: &mut dyn FnMut(Value)) {
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

pub(super) fn value_contains(haystack: &Value, needle: &Value) -> bool {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(pairs: &[(&str, Value)]) -> Value {
        Value::Object(Rc::new(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        ))
    }

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
    fn test_todate_epoch() {
        assert_eq!(todate(0), Some("1970-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_todate_known() {
        assert_eq!(todate(1705318245), Some("2024-01-15T11:30:45Z".to_string()));
    }

    #[test]
    fn test_fromdate() {
        assert_eq!(fromdate("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(fromdate("2024-01-15T11:30:45Z"), Some(1705318245));
    }

    #[test]
    fn test_fromdate_roundtrip() {
        let ts = 1705318245_i64;
        let s = todate(ts).unwrap();
        assert_eq!(fromdate(&s), Some(ts));
    }

    #[test]
    fn test_format_strftime() {
        assert_eq!(
            format_strftime_jiff("%Y-%m-%d", 0),
            Some("1970-01-01".to_string())
        );
        assert_eq!(format_strftime_jiff("%A", 0), Some("Thursday".to_string()));
        assert_eq!(format_strftime_jiff("%j", 0), Some("001".to_string()));
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
    fn f64_to_value_at_i64_max_boundary() {
        // 2^63 = 9223372036854775808.0 is above i64::MAX → must stay Double
        let v = f64_to_value(9223372036854775808.0);
        assert!(
            matches!(v, Value::Double(..)),
            "2^63 should be Double, got {v:?}"
        );
    }

    #[test]
    fn f64_to_value_normal_int() {
        assert_eq!(f64_to_value(42.0), Value::Int(42));
        assert_eq!(f64_to_value(-1.0), Value::Int(-1));
    }

    #[test]
    fn f64_to_value_i64_min() {
        // -2^63 is exactly representable and fits in i64
        assert_eq!(f64_to_value(i64::MIN as f64), Value::Int(i64::MIN));
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
