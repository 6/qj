/// jq filter evaluator — produces zero or more output Values per input.
///
/// Uses generator semantics: each filter operation calls `output` for
/// each result, avoiding intermediate Vec allocations.
use crate::filter::{ArithOp, BoolOp, CmpOp, Env, Filter, ObjKey};
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// Last error value set by `error` / `error(msg)` builtins.
    pub(super) static LAST_ERROR: RefCell<Option<Value>> = const { RefCell::new(None) };
}

/// Public entry point — creates an empty env for top-level evaluation.
pub fn eval_filter(filter: &Filter, input: &Value, output: &mut dyn FnMut(Value)) {
    eval(filter, input, &Env::empty(), output);
}

/// Evaluate a filter against an input value, calling `output` for each result.
pub fn eval(filter: &Filter, input: &Value, env: &Env, output: &mut dyn FnMut(Value)) {
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
            eval(idx_filter, input, env, &mut |idx| match (input, &idx) {
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
            eval(left, input, env, &mut |intermediate| {
                eval(right, &intermediate, env, output);
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
            eval(cond, input, env, &mut |v| {
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
                env: &Env,
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
                        eval(val_filter, input, env, &mut |v| {
                            current.push((key_str.clone(), v));
                            build_object(pairs, idx + 1, current, input, env, output);
                            current.pop();
                        });
                    }
                    ObjKey::Expr(expr) => {
                        eval(expr, input, env, &mut |kv| {
                            let key_str = match kv {
                                Value::String(s) => s,
                                _ => return,
                            };
                            eval(val_filter, input, env, &mut |v| {
                                current.push((key_str.clone(), v));
                                build_object(pairs, idx + 1, current, input, env, output);
                                current.pop();
                            });
                        });
                    }
                }
            }
            let mut current = Vec::with_capacity(pairs.len());
            build_object(pairs, 0, &mut current, input, env, output);
        }

        Filter::ArrayConstruct(expr) => {
            let mut arr = Vec::new();
            eval(expr, input, env, &mut |v| {
                arr.push(v);
            });
            output(Value::Array(Rc::new(arr)));
        }

        Filter::Literal(val) => output(val.clone()),

        Filter::Compare(left, op, right) => {
            eval(left, input, env, &mut |lval| {
                eval(right, input, env, &mut |rval| {
                    let result = compare_values(&lval, op, &rval);
                    output(Value::Bool(result));
                });
            });
        }

        Filter::Arith(left, op, right) => {
            eval(left, input, env, &mut |lval| {
                eval(right, input, env, &mut |rval| {
                    if let Some(result) = arith_values(&lval, op, &rval) {
                        output(result);
                    }
                });
            });
        }

        Filter::Comma(items) => {
            for item in items {
                eval(item, input, env, output);
            }
        }

        Filter::Recurse => {
            recurse(input, output);
        }

        Filter::Builtin(name, args) => {
            super::builtins::eval_builtin(name, args, input, env, output);
        }

        Filter::Not(inner) => {
            eval(inner, input, env, &mut |v| {
                output(Value::Bool(!v.is_truthy()));
            });
        }

        Filter::BoolOp(left, op, right) => {
            let mut lval = Value::Null;
            eval(left, input, env, &mut |v| lval = v);
            match op {
                BoolOp::And => {
                    if lval.is_truthy() {
                        let mut rval = Value::Null;
                        eval(right, input, env, &mut |v| rval = v);
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
                        eval(right, input, env, &mut |v| rval = v);
                        output(Value::Bool(rval.is_truthy()));
                    }
                }
            }
        }

        Filter::IfThenElse(cond, then_branch, else_branch) => {
            eval(cond, input, env, &mut |cond_val| {
                if cond_val.is_truthy() {
                    eval(then_branch, input, env, output);
                } else if let Some(else_br) = else_branch {
                    eval(else_br, input, env, output);
                } else {
                    output(input.clone());
                }
            });
        }

        Filter::Alternative(left, right) => {
            let mut lval = Value::Null;
            let mut got_value = false;
            eval(left, input, env, &mut |v| {
                if !got_value {
                    lval = v;
                    got_value = true;
                }
            });
            if got_value && lval != Value::Null && lval != Value::Bool(false) {
                output(lval);
            } else {
                eval(right, input, env, output);
            }
        }

        Filter::Try(inner) => {
            // Try: suppress errors, just produce no output on failure.
            LAST_ERROR.with(|e| e.borrow_mut().take());
            eval(inner, input, env, output);
            LAST_ERROR.with(|e| e.borrow_mut().take());
        }

        Filter::TryCatch(body, handler) => {
            LAST_ERROR.with(|e| e.borrow_mut().take());
            let mut had_output = false;
            eval(body, input, env, &mut |v| {
                had_output = true;
                output(v);
            });
            if !had_output {
                let err_val = LAST_ERROR.with(|e| e.borrow_mut().take());
                let catch_input = err_val.unwrap_or_else(|| input.clone());
                eval(handler, &catch_input, env, output);
            }
        }

        Filter::StringInterp(parts) => {
            let mut result = String::new();
            for part in parts {
                match part {
                    crate::filter::StringPart::Lit(s) => result.push_str(s),
                    crate::filter::StringPart::Expr(f) => {
                        eval(f, input, env, &mut |v| match v {
                            Value::String(s) => result.push_str(&s),
                            Value::Int(n) => result.push_str(itoa::Buffer::new().format(n)),
                            Value::Double(f, _) => result.push_str(ryu::Buffer::new().format(f)),
                            Value::Bool(b) => result.push_str(if b { "true" } else { "false" }),
                            Value::Null => result.push_str("null"),
                            Value::Array(_) | Value::Object(_) => {
                                let mut buf = Vec::new();
                                crate::output::write_compact(&mut buf, &v).unwrap();
                                result.push_str(&String::from_utf8(buf).unwrap_or_default());
                            }
                        });
                    }
                }
            }
            output(Value::String(result));
        }

        Filter::Neg(inner) => {
            eval(inner, input, env, &mut |v| match v {
                Value::Int(n) => output(Value::Int(-n)),
                Value::Double(f, _) => output(Value::Double(-f, None)),
                _ => {}
            });
        }

        Filter::Slice(start_f, end_f) => {
            let start_val = start_f.as_ref().map(|f| {
                let mut v = Value::Null;
                eval(f, input, env, &mut |val| v = val);
                v
            });
            let end_val = end_f.as_ref().map(|f| {
                let mut v = Value::Null;
                eval(f, input, env, &mut |val| v = val);
                v
            });

            match input {
                Value::Array(arr) => {
                    let len = arr.len() as i64;
                    let s = resolve_slice_index(start_val.as_ref(), 0, len);
                    let e = resolve_slice_index(end_val.as_ref(), len, len);
                    if s < e {
                        output(Value::Array(Rc::new(arr[s as usize..e as usize].to_vec())));
                    } else {
                        output(Value::Array(Rc::new(vec![])));
                    }
                }
                Value::String(s) => {
                    let chars: Vec<char> = s.chars().collect();
                    let len = chars.len() as i64;
                    let start = resolve_slice_index(start_val.as_ref(), 0, len);
                    let end = resolve_slice_index(end_val.as_ref(), len, len);
                    if start < end {
                        let sliced: String = chars[start as usize..end as usize].iter().collect();
                        output(Value::String(sliced));
                    } else {
                        output(Value::String(String::new()));
                    }
                }
                _ => output(Value::Null),
            }
        }

        Filter::Var(name) => {
            if let Some(val) = env.get_var(name) {
                output(val.clone());
            } else {
                // Fall through to builtins for special variables like $ENV
                super::builtins::eval_builtin(name, &[], input, env, output);
            }
        }

        Filter::Bind(expr, name, body) => {
            eval(expr, input, env, &mut |val| {
                let new_env = env.bind_var(name.clone(), val);
                eval(body, input, &new_env, output);
            });
        }

        Filter::Reduce(source, var, init, update) => {
            let mut acc = Value::Null;
            eval(init, input, env, &mut |v| acc = v);

            eval(source, input, env, &mut |val| {
                let new_env = env.bind_var(var.clone(), val);
                let cur = acc.clone();
                eval(update, &cur, &new_env, &mut |v| acc = v);
            });

            output(acc);
        }

        Filter::Foreach(source, var, init, update, extract) => {
            let mut acc = Value::Null;
            eval(init, input, env, &mut |v| acc = v);

            eval(source, input, env, &mut |val| {
                let new_env = env.bind_var(var.clone(), val);
                let cur = acc.clone();
                eval(update, &cur, &new_env, &mut |v| acc = v);
                if let Some(ext) = extract {
                    eval(ext, &acc, &new_env, output);
                } else {
                    output(acc.clone());
                }
            });
        }
    }
}

/// Resolve a slice index: handle negatives (wrap with len), clamp to [0, len].
fn resolve_slice_index(val: Option<&Value>, default: i64, len: i64) -> i64 {
    let idx = match val {
        Some(Value::Int(n)) => *n,
        Some(Value::Double(f, _)) => *f as i64,
        _ => return default.clamp(0, len),
    };
    let resolved = if idx < 0 { len + idx } else { idx };
    resolved.clamp(0, len)
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

pub(super) fn arith_values(left: &Value, op: &ArithOp, right: &Value) -> Option<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn eval_one(filter: &Filter, input: &Value) -> Value {
        let mut results = Vec::new();
        eval_filter(filter, input, &mut |v| results.push(v));
        assert_eq!(results.len(), 1, "expected 1 result, got {:?}", results);
        results.into_iter().next().unwrap()
    }

    fn eval_all(filter: &Filter, input: &Value) -> Vec<Value> {
        let mut results = Vec::new();
        eval_filter(filter, input, &mut |v| results.push(v));
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
