//! Lazy evaluator wrapper that operates on `FlatValue` to avoid materializing
//! the full `Value` tree for NDJSON lines.
//!
//! The key optimization: field chain navigation stays as FlatValue (zero
//! allocation) and only materializes at the point where a concrete Value is
//! needed (output boundary, complex computation, etc.).

use crate::filter::{BoolOp, Env, Filter, ObjKey, Pattern};
use crate::flat_value::FlatValue;
use crate::value::Value;
use std::collections::HashSet;
use std::sync::Arc;

/// Result of navigating a filter on a FlatValue.
///
/// `Flat` means we successfully navigated without materializing.
/// `FlatMany` means the filter produced multiple FlatValue outputs (e.g. Iterate).
/// `Values` means we had to materialize.
enum NavResult<'a> {
    /// Single FlatValue result — still zero-copy.
    Flat(FlatValue<'a>),
    /// Multiple FlatValue results — still zero-copy (e.g. from `.[]`).
    FlatMany(Vec<FlatValue<'a>>),
    /// Materialized Value results.
    Values(Vec<Value>),
}

/// Check if any variable bound by `pattern` is referenced in `filter`.
fn pattern_var_used_in(pattern: &Pattern, filter: &Filter) -> bool {
    let mut pat_vars = HashSet::new();
    crate::filter::collect_pattern_var_refs(pattern, &mut pat_vars);
    let mut filter_vars = HashSet::new();
    filter.collect_var_refs(&mut filter_vars);
    pat_vars.iter().any(|v| filter_vars.contains(v))
}

/// Count how many values `source` would produce from `flat` without materializing.
/// Returns `Some(count)` for navigable patterns (`.[]`, `.field[]`), `None` otherwise.
fn flat_source_count(filter: &Filter, flat: FlatValue<'_>, env: &Env) -> Option<usize> {
    match filter {
        Filter::Iterate => flat.len(),
        Filter::Pipe(left, right) => match eval_flat_nav(left, flat, env) {
            NavResult::Flat(child) => flat_source_count(right, child, env),
            NavResult::FlatMany(children) => {
                // Sum counts across all children
                let mut total = 0;
                for child in children {
                    total += flat_source_count(right, child, env)?;
                }
                Some(total)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Try to evaluate a filter as flat navigation, avoiding materialization.
///
/// For field chains and simple navigation, returns `NavResult::Flat`.
/// For anything that requires computation, falls back to producing Values.
fn eval_flat_nav<'a>(filter: &Filter, flat: FlatValue<'a>, env: &Env) -> NavResult<'a> {
    match filter {
        Filter::Identity => NavResult::Flat(flat),

        Filter::Field(name) => {
            if flat.is_object() {
                match flat.get_field(name) {
                    Some(child) => NavResult::Flat(child),
                    None => NavResult::Values(vec![Value::Null]),
                }
            } else if flat.is_null() {
                NavResult::Values(vec![Value::Null])
            } else {
                crate::filter::eval::set_last_error(Value::String(format!(
                    "Cannot index {} with string \"{}\"",
                    flat.type_name(),
                    name
                )));
                NavResult::Values(vec![])
            }
        }

        Filter::Iterate => {
            if flat.is_array() {
                NavResult::FlatMany(flat.array_iter().collect())
            } else if flat.is_object() {
                NavResult::FlatMany(flat.object_iter().map(|(_, v)| v).collect())
            } else {
                crate::filter::eval::set_last_error(Value::String(format!(
                    "Cannot iterate over {}",
                    flat.type_name()
                )));
                NavResult::Values(vec![])
            }
        }

        Filter::Index(idx_filter) => match idx_filter.as_ref() {
            Filter::Literal(Value::Int(i)) => {
                if flat.is_array() {
                    let len = flat.len().unwrap_or(0);
                    let idx = if *i < 0 { len as i64 + i } else { *i } as usize;
                    if idx < len {
                        match flat.get_index(idx) {
                            Some(child) => NavResult::Flat(child),
                            None => NavResult::Values(vec![Value::Null]),
                        }
                    } else {
                        NavResult::Values(vec![Value::Null])
                    }
                } else if flat.is_null() {
                    NavResult::Values(vec![Value::Null])
                } else {
                    crate::filter::eval::set_last_error(Value::String(format!(
                        "Cannot index {} with number",
                        flat.type_name()
                    )));
                    NavResult::Values(vec![])
                }
            }
            // Dynamic index: materialize and delegate
            _ => {
                let value = flat.to_value();
                let mut results = Vec::new();
                crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |out| {
                    results.push(out);
                });
                NavResult::Values(results)
            }
        },

        Filter::Pipe(left, right) => match eval_flat_nav(left, flat, env) {
            NavResult::Flat(mid) => eval_flat_nav(right, mid, env),
            NavResult::FlatMany(mids) => {
                let mut all_flat = Vec::new();
                let mut all_values = Vec::new();
                for mid in mids {
                    match eval_flat_nav(right, mid, env) {
                        NavResult::Flat(child) => all_flat.push(child),
                        NavResult::FlatMany(children) => all_flat.extend(children),
                        NavResult::Values(values) => all_values.extend(values),
                    }
                }
                if all_values.is_empty() {
                    NavResult::FlatMany(all_flat)
                } else {
                    let mut combined: Vec<Value> =
                        all_flat.into_iter().map(|f| f.to_value()).collect();
                    combined.extend(all_values);
                    NavResult::Values(combined)
                }
            }
            NavResult::Values(values) => {
                let mut results = Vec::new();
                for v in &values {
                    crate::filter::eval::eval_filter_with_env(right, v, env, &mut |out| {
                        results.push(out);
                    });
                }
                NavResult::Values(results)
            }
        },

        Filter::Alternative(left, right) => match eval_flat_nav(left, flat, env) {
            NavResult::Flat(child) => {
                if child.is_truthy() {
                    NavResult::Flat(child)
                } else {
                    eval_flat_nav(right, flat, env)
                }
            }
            NavResult::FlatMany(children) => {
                let truthy: Vec<FlatValue> =
                    children.into_iter().filter(|c| c.is_truthy()).collect();
                if !truthy.is_empty() {
                    NavResult::FlatMany(truthy)
                } else {
                    eval_flat_nav(right, flat, env)
                }
            }
            NavResult::Values(values) => {
                let truthy: Vec<Value> = values.into_iter().filter(|v| v.is_truthy()).collect();
                if !truthy.is_empty() {
                    NavResult::Values(truthy)
                } else {
                    eval_flat_nav(right, flat, env)
                }
            }
        },

        Filter::Try(inner) => {
            // Try: suppress errors, treat as navigation
            let result = eval_flat_nav(inner, flat, env);
            let _ = crate::filter::eval::take_last_error();
            result
        }

        Filter::Literal(v) => NavResult::Values(vec![v.clone()]),

        Filter::Def {
            name,
            params,
            body,
            rest,
        } => {
            let func = crate::filter::UserFunc {
                params: params.clone(),
                body: (**body).clone(),
                closure_env: env.clone(),
                is_def: true,
            };
            let new_env = env.bind_func(name.clone(), params.len(), func);
            eval_flat_nav(rest, flat, &new_env)
        }

        Filter::Select(cond) => {
            let mut is_truthy = false;
            eval_flat(cond, flat, env, &mut |v| {
                if v.is_truthy() {
                    is_truthy = true;
                }
            });
            if is_truthy {
                NavResult::Flat(flat)
            } else {
                NavResult::Values(vec![])
            }
        }

        // For anything else: materialize and delegate
        _ => {
            let value = flat.to_value();
            let mut results = Vec::new();
            crate::filter::eval::eval_filter_with_env(filter, &value, env, &mut |out| {
                results.push(out);
            });
            NavResult::Values(results)
        }
    }
}

/// Recursive helper for ObjectConstruct: produces Cartesian product of
/// generator outputs across all entries.
fn eval_flat_obj_entries(
    entries: &[(ObjKey, Box<Filter>)],
    flat: FlatValue<'_>,
    env: &Env,
    partial: &mut Vec<(String, Value)>,
    output: &mut dyn FnMut(Value),
) {
    if entries.is_empty() {
        output(Value::Object(Arc::new(partial.clone())));
        return;
    }

    let (key, val_filter) = &entries[0];
    let rest = &entries[1..];

    // Resolve key — may itself be a generator for ObjKey::Expr
    match key {
        ObjKey::Name(name) => {
            eval_flat(val_filter, flat, env, &mut |v| {
                partial.push((name.clone(), v));
                eval_flat_obj_entries(rest, flat, env, partial, output);
                partial.pop();
            });
        }
        ObjKey::Expr(expr) => {
            let value = flat.to_value();
            crate::filter::eval::eval_filter_with_env(expr, &value, env, &mut |key_val| {
                if let Value::String(key_str) = key_val {
                    eval_flat(val_filter, flat, env, &mut |v| {
                        partial.push((key_str.clone(), v));
                        eval_flat_obj_entries(rest, flat, env, partial, output);
                        partial.pop();
                    });
                }
            });
        }
    }
}

/// Check whether a filter is safe for flat evaluation in single-document mode.
///
/// Returns true if the filter only uses operations that flat eval handles
/// natively. Filters that could produce type errors (e.g., `.field` on an
/// array) are still safe here — flat eval silences them, which matches jq
/// behavior in NDJSON. For single-doc, we only call this for filters where
/// the top-level structure is known to be compatible (object construction,
/// pipes of field chains, etc.).
///
/// This is conservative: it returns false for any filter that would fall
/// through to the regular evaluator's catch-all, since that path may set
/// thread-local errors that the caller expects to check.
pub fn is_flat_safe(filter: &Filter) -> bool {
    match filter {
        Filter::Identity | Filter::Literal(_) | Filter::Iterate | Filter::Select(_) => true,
        Filter::Field(_) | Filter::Index(_) => true,
        Filter::Pipe(l, r) => is_flat_safe(l) && is_flat_safe(r),
        Filter::Comma(fs) => fs.iter().all(is_flat_safe),
        Filter::ObjectConstruct(entries) => entries
            .iter()
            .all(|(key, val)| matches!(key, ObjKey::Name(_)) && is_flat_safe(val)),
        Filter::ArrayConstruct(inner) => is_flat_safe(inner),
        Filter::Alternative(l, r) => is_flat_safe(l) && is_flat_safe(r),
        Filter::Try(inner) => is_flat_safe(inner),
        Filter::Not(inner) => is_flat_safe(inner),
        // Reduce: source must be flat-safe (passed to eval_flat). Init goes
        // through eval_flat too (literals handled directly, exotic expressions
        // fall through to catch-all). Update runs via eval_filter_with_env on
        // a materialized accumulator, so it doesn't need the check.
        Filter::Reduce(source, _pattern, init, _update) => {
            is_flat_safe(source) && is_flat_safe(init)
        }
        Filter::Builtin(name, args) if args.is_empty() => {
            matches!(name.as_str(), "length" | "type" | "keys")
        }
        Filter::Builtin(name, args) if args.len() == 1 => {
            matches!(name.as_str(), "map" | "map_values") && is_flat_safe(&args[0])
        }
        _ => false,
    }
}

/// Evaluate a filter with a FlatValue input, producing Value outputs.
///
/// This is the main entry point for lazy NDJSON evaluation. It navigates
/// the flat buffer as much as possible, only materializing when needed.
pub fn eval_flat(filter: &Filter, flat: FlatValue<'_>, env: &Env, output: &mut dyn FnMut(Value)) {
    match filter {
        Filter::Identity => {
            output(flat.to_value());
        }

        Filter::Field(name) => {
            if flat.is_object() {
                match flat.get_field(name) {
                    Some(child) => output(child.to_value()),
                    None => output(Value::Null),
                }
            } else if flat.is_null() {
                output(Value::Null);
            } else {
                crate::filter::eval::set_last_error(Value::String(format!(
                    "Cannot index {} with string \"{}\"",
                    flat.type_name(),
                    name
                )));
            }
        }

        Filter::Pipe(left, right) => {
            // Try to navigate left side without materializing
            match eval_flat_nav(left, flat, env) {
                NavResult::Flat(child) => {
                    eval_flat(right, child, env, output);
                }
                NavResult::FlatMany(children) => {
                    for child in children {
                        eval_flat(right, child, env, output);
                    }
                }
                NavResult::Values(values) => {
                    for v in &values {
                        crate::filter::eval::eval_filter_with_env(right, v, env, output);
                    }
                }
            }
        }

        Filter::ObjectConstruct(entries) => {
            // Recursive Cartesian product: each entry can produce multiple key-value
            // pairs (generators), and we need one output object per combination.
            let mut partial = Vec::with_capacity(entries.len());
            eval_flat_obj_entries(entries, flat, env, &mut partial, output);
        }

        Filter::ArrayConstruct(inner) => {
            let mut arr = Vec::new();
            eval_flat(inner, flat, env, &mut |v| arr.push(v));
            output(Value::Array(Arc::new(arr)));
        }

        Filter::Iterate => {
            if flat.is_array() {
                for elem in flat.array_iter() {
                    output(elem.to_value());
                }
            } else if flat.is_object() {
                for (_, val) in flat.object_iter() {
                    output(val.to_value());
                }
            }
            // else: error in jq (no output)
        }

        Filter::Select(cond) => {
            let mut is_truthy = false;
            eval_flat(cond, flat, env, &mut |v| {
                if v.is_truthy() {
                    is_truthy = true;
                }
            });
            if is_truthy {
                output(flat.to_value());
            }
        }

        Filter::Alternative(left, right) => match eval_flat_nav(left, flat, env) {
            NavResult::Flat(child) => {
                if child.is_truthy() {
                    output(child.to_value());
                } else {
                    eval_flat(right, flat, env, output);
                }
            }
            NavResult::FlatMany(children) => {
                let truthy: Vec<FlatValue> =
                    children.into_iter().filter(|c| c.is_truthy()).collect();
                if !truthy.is_empty() {
                    for child in truthy {
                        output(child.to_value());
                    }
                } else {
                    eval_flat(right, flat, env, output);
                }
            }
            NavResult::Values(values) => {
                let truthy: Vec<Value> = values.into_iter().filter(|v| v.is_truthy()).collect();
                if !truthy.is_empty() {
                    for v in truthy {
                        output(v);
                    }
                } else {
                    eval_flat(right, flat, env, output);
                }
            }
        },

        Filter::Builtin(name, args) if name == "map" && args.len() == 1 => {
            if flat.is_array() {
                let f = &args[0];
                let mut result = Vec::new();
                for elem in flat.array_iter() {
                    eval_flat(f, elem, env, &mut |v| result.push(v));
                }
                output(Value::Array(Arc::new(result)));
            }
            // else: non-array → no output (matches jq)
        }

        Filter::Builtin(name, args) if name == "map_values" && args.len() == 1 => {
            let f = &args[0];
            if flat.is_array() {
                let mut result = Vec::new();
                for elem in flat.array_iter() {
                    eval_flat(f, elem, env, &mut |v| result.push(v));
                }
                output(Value::Array(Arc::new(result)));
            } else if flat.is_object() {
                let mut result = Vec::new();
                for (k, v) in flat.object_iter() {
                    eval_flat(f, v, env, &mut |new_v| {
                        result.push((k.to_string(), new_v));
                    });
                }
                output(Value::Object(Arc::new(result)));
            }
        }

        Filter::Builtin(name, args) if name == "length" && args.is_empty() => {
            match flat.tag() {
                crate::simdjson::TAG_ARRAY_START | crate::simdjson::TAG_OBJECT_START => {
                    output(Value::Int(flat.len().unwrap_or(0) as i64));
                }
                crate::simdjson::TAG_STRING => {
                    // jq counts Unicode codepoints, not bytes
                    let s = flat.as_str().unwrap();
                    output(Value::Int(s.chars().count() as i64));
                }
                crate::simdjson::TAG_NULL => {
                    output(Value::Int(0));
                }
                _ => {
                    // Materialize and delegate for error handling
                    let value = flat.to_value();
                    crate::filter::eval::eval_filter_with_env(filter, &value, env, output);
                }
            }
        }

        Filter::Builtin(name, args) if name == "type" && args.is_empty() => {
            output(Value::String(flat.type_name().to_string()));
        }

        Filter::Builtin(name, args) if name == "keys" && args.is_empty() => {
            if flat.is_object() {
                let mut keys: Vec<String> =
                    flat.object_iter().map(|(k, _)| k.to_string()).collect();
                keys.sort();
                output(Value::Array(Arc::new(
                    keys.into_iter().map(Value::String).collect(),
                )));
            } else if flat.is_array() {
                let len = flat.len().unwrap_or(0);
                output(Value::Array(Arc::new(
                    (0..len as i64).map(Value::Int).collect(),
                )));
            } else {
                let value = flat.to_value();
                crate::filter::eval::eval_filter_with_env(filter, &value, env, output);
            }
        }

        Filter::Comma(filters) => {
            for f in filters {
                eval_flat(f, flat, env, output);
            }
        }

        Filter::Literal(v) => {
            output(v.clone());
        }

        Filter::Reduce(source, pattern, init, update) => {
            // Evaluate init via flat eval — literals like `0` are handled directly
            // without materializing the document; exotic init expressions fall
            // through to eval_flat's catch-all which materializes as needed.
            let mut acc = Value::Null;
            eval_flat(init, flat, env, &mut |v| acc = v);

            if !pattern_var_used_in(pattern, update) {
                // Dead variable: pattern var is never referenced in update.
                // Try to count source elements without materializing them.
                if let Some(count) = flat_source_count(source, flat, env) {
                    // Zero-materialization path: just run update N times.
                    for _ in 0..count {
                        let cur = acc.clone();
                        crate::filter::eval::eval_filter_with_env(update, &cur, env, &mut |v| {
                            acc = v
                        });
                    }
                } else {
                    // Uncountable source (e.g., contains select): materialize
                    // elements but skip match_pattern/binding.
                    eval_flat(source, flat, env, &mut |_val| {
                        let cur = acc.clone();
                        crate::filter::eval::eval_filter_with_env(update, &cur, env, &mut |v| {
                            acc = v
                        });
                    });
                }
            } else {
                // Live variable: materialize each element and bind pattern.
                eval_flat(source, flat, env, &mut |val| {
                    if let Some(new_env) = crate::filter::eval::match_pattern(pattern, &val, env) {
                        let cur = acc.clone();
                        crate::filter::eval::eval_filter_with_env(
                            update,
                            &cur,
                            &new_env,
                            &mut |v| acc = v,
                        );
                    }
                });
            }
            output(acc);
        }

        Filter::Try(inner) => {
            eval_flat(inner, flat, env, output);
            // Try suppresses errors — clear any set by the inner expression
            let _ = crate::filter::eval::take_last_error();
        }

        Filter::Not(inner) => {
            eval_flat(inner, flat, env, &mut |v| {
                output(Value::Bool(!v.is_truthy()));
            });
        }

        Filter::Def {
            name,
            params,
            body,
            rest,
        } => {
            let func = crate::filter::UserFunc {
                params: params.clone(),
                body: (**body).clone(),
                closure_env: env.clone(),
                is_def: true,
            };
            let new_env = env.bind_func(name.clone(), params.len(), func);
            eval_flat(rest, flat, &new_env, output);
        }

        Filter::IfThenElse(cond, then_branch, else_branch) => {
            let value = flat.to_value();
            crate::filter::eval::eval_filter_with_env(cond, &value, env, &mut |cond_val| {
                if cond_val.is_truthy() {
                    crate::filter::eval::eval_filter_with_env(then_branch, &value, env, output);
                } else if let Some(else_br) = else_branch {
                    crate::filter::eval::eval_filter_with_env(else_br, &value, env, output);
                } else {
                    output(value.clone());
                }
            });
        }

        Filter::Bind(expr, pattern, body) => {
            let value = flat.to_value();
            crate::filter::eval::eval_filter_with_env(expr, &value, env, &mut |val| {
                if let Some(new_env) = crate::filter::eval::match_pattern(pattern, &val, env) {
                    crate::filter::eval::eval_filter_with_env(body, &value, &new_env, output);
                }
            });
        }

        Filter::Compare(left, op, right) => {
            eval_flat(right, flat, env, &mut |rval| {
                eval_flat(left, flat, env, &mut |lval| {
                    output(Value::Bool(crate::filter::compare_values(&lval, op, &rval)));
                });
            });
        }

        Filter::BoolOp(left, op, right) => {
            let mut lval = Value::Null;
            eval_flat(left, flat, env, &mut |v| lval = v);
            match op {
                BoolOp::And => {
                    if lval.is_truthy() {
                        let mut rval = Value::Null;
                        eval_flat(right, flat, env, &mut |v| rval = v);
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
                        eval_flat(right, flat, env, &mut |v| rval = v);
                        output(Value::Bool(rval.is_truthy()));
                    }
                }
            }
        }

        Filter::Arith(left, op, right) => {
            eval_flat(right, flat, env, &mut |rval| {
                eval_flat(
                    left,
                    flat,
                    env,
                    &mut |lval| match crate::filter::arith_values(&lval, op, &rval) {
                        Ok(result) => output(result),
                        Err(msg) => {
                            crate::filter::eval::set_last_error(Value::String(msg));
                        }
                    },
                );
            });
        }

        Filter::Neg(inner) => {
            eval_flat(inner, flat, env, &mut |v| match v {
                Value::Int(n) => output(
                    n.checked_neg()
                        .map_or_else(|| Value::Double(-(n as f64), None), Value::Int),
                ),
                Value::Double(f, _) => output(Value::Double(-f, None)),
                _ => {
                    crate::filter::eval::set_last_error(Value::String(format!(
                        "{} cannot be negated",
                        v.type_name()
                    )));
                }
            });
        }

        // Fall back to regular evaluator for everything else
        _ => {
            let value = flat.to_value();
            crate::filter::eval::eval_filter_with_env(filter, &value, env, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::lexer::lex;
    use crate::filter::parser::parse;
    use crate::simdjson::{dom_parse_to_flat_buf, dom_parse_to_value, pad_buffer};

    /// Helper: parse a jq filter string.
    fn parse_filter(s: &str) -> Filter {
        let tokens = lex(s).unwrap();
        parse(&tokens).unwrap()
    }

    /// Helper: evaluate with regular evaluator (for comparison).
    fn eval_regular(filter: &Filter, json: &[u8]) -> Vec<Value> {
        let buf = pad_buffer(json);
        let value = dom_parse_to_value(&buf, json.len()).unwrap();
        let env = Env::empty();
        let mut results = Vec::new();
        crate::filter::eval::eval_filter_with_env(filter, &value, &env, &mut |v| {
            results.push(v);
        });
        results
    }

    /// Helper: evaluate with flat evaluator.
    fn eval_with_flat(filter: &Filter, json: &[u8]) -> Vec<Value> {
        let buf = pad_buffer(json);
        let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
        let env = Env::empty();
        let mut results = Vec::new();
        eval_flat(filter, flat_buf.root(), &env, &mut |v| {
            results.push(v);
        });
        results
    }

    /// Assert flat eval produces same output as regular eval.
    fn assert_equiv(filter_str: &str, json: &[u8]) {
        let filter = parse_filter(filter_str);
        let regular = eval_regular(&filter, json);
        let flat = eval_with_flat(&filter, json);
        assert_eq!(
            flat,
            regular,
            "flat eval mismatch for filter {:?} on {:?}",
            filter_str,
            std::str::from_utf8(json).unwrap_or("<binary>")
        );
    }

    // --- Simple field access ---

    #[test]
    fn field_access() {
        assert_equiv(".name", br#"{"name":"alice","age":30}"#);
    }

    #[test]
    fn nested_field_access() {
        assert_equiv(".a.b.c", br#"{"a":{"b":{"c":42}}}"#);
    }

    #[test]
    fn missing_field() {
        assert_equiv(".missing", br#"{"name":"alice"}"#);
    }

    #[test]
    fn field_on_null() {
        assert_equiv(".x", b"null");
    }

    // --- Identity ---

    #[test]
    fn identity() {
        assert_equiv(".", br#"{"a":1,"b":2}"#);
    }

    #[test]
    fn identity_scalar() {
        assert_equiv(".", b"42");
    }

    // --- Pipe chains ---

    #[test]
    fn pipe_field_chain() {
        assert_equiv(".payload.commits", br#"{"payload":{"commits":[1,2,3]}}"#);
    }

    #[test]
    fn pipe_field_then_length() {
        assert_equiv(".items | length", br#"{"items":[1,2,3,4,5]}"#);
    }

    #[test]
    fn pipe_field_length_nested() {
        assert_equiv(
            ".payload.commits | length",
            br#"{"payload":{"commits":["a","b"]}}"#,
        );
    }

    // --- Object construction ---

    #[test]
    fn object_construct() {
        assert_equiv(
            "{type, name: .user.name}",
            br#"{"type":"PushEvent","user":{"name":"alice"},"other":"data"}"#,
        );
    }

    #[test]
    fn object_construct_nested() {
        assert_equiv(
            "{type, repo: .repo.name, actor: .actor.login}",
            br#"{"type":"PushEvent","repo":{"name":"cool/repo"},"actor":{"login":"alice"},"id":123}"#,
        );
    }

    #[test]
    fn object_construct_generator() {
        // Generator in object value should produce multiple objects
        assert_equiv("{x: (.a, .b)}", br#"{"a":1,"b":2}"#);
    }

    // --- Select ---

    #[test]
    fn select_eq() {
        assert_equiv(
            r#"select(.type == "PushEvent")"#,
            br#"{"type":"PushEvent","id":1}"#,
        );
    }

    #[test]
    fn select_eq_no_match() {
        assert_equiv(
            r#"select(.type == "PushEvent")"#,
            br#"{"type":"WatchEvent","id":2}"#,
        );
    }

    // --- Alternative ---

    #[test]
    fn alternative_present() {
        assert_equiv(r#".x // "default""#, br#"{"x":"hello"}"#);
    }

    #[test]
    fn alternative_missing() {
        assert_equiv(r#".x // "default""#, br#"{"y":1}"#);
    }

    #[test]
    fn alternative_null() {
        assert_equiv(r#".x // "default""#, br#"{"x":null}"#);
    }

    #[test]
    fn alternative_false() {
        assert_equiv(r#".x // "default""#, br#"{"x":false}"#);
    }

    // --- Pipe with alternative and length (the benchmark filter) ---

    #[test]
    fn benchmark_filter_with_commits() {
        assert_equiv(
            "{type, commits: (.payload.commits // [] | length)}",
            br#"{"type":"PushEvent","payload":{"commits":["a","b","c"]}}"#,
        );
    }

    #[test]
    fn benchmark_filter_without_commits() {
        assert_equiv(
            "{type, commits: (.payload.commits // [] | length)}",
            br#"{"type":"WatchEvent","payload":{}}"#,
        );
    }

    #[test]
    fn benchmark_filter_null_payload() {
        assert_equiv(
            "{type, commits: (.payload.commits // [] | length)}",
            br#"{"type":"IssueEvent","payload":{"commits":null}}"#,
        );
    }

    // --- Iterate ---

    #[test]
    fn iterate_array() {
        assert_equiv(".[] | .name", br#"[{"name":"a"},{"name":"b"}]"#);
    }

    #[test]
    fn iterate_object() {
        assert_equiv(".[]", br#"{"a":1,"b":2}"#);
    }

    // --- Array construction ---

    #[test]
    fn array_construct() {
        assert_equiv("[.items[]]", br#"{"items":[1,2,3]}"#);
    }

    // --- Builtins ---

    #[test]
    fn builtin_length_array() {
        assert_equiv("length", b"[1,2,3]");
    }

    #[test]
    fn builtin_length_object() {
        assert_equiv("length", br#"{"a":1,"b":2}"#);
    }

    #[test]
    fn builtin_length_string() {
        assert_equiv("length", br#""hello""#);
    }

    #[test]
    fn builtin_length_null() {
        assert_equiv("length", b"null");
    }

    #[test]
    fn builtin_type() {
        assert_equiv("type", br#"{"a":1}"#);
        assert_equiv("type", b"[1]");
        assert_equiv("type", b"42");
        assert_equiv("type", b"true");
        assert_equiv("type", b"null");
        assert_equiv("type", br#""hi""#);
    }

    #[test]
    fn builtin_keys() {
        assert_equiv("keys", br#"{"b":2,"a":1}"#);
    }

    #[test]
    fn builtin_keys_array() {
        assert_equiv("keys", b"[10,20,30]");
    }

    #[test]
    fn builtin_not() {
        assert_equiv("not", b"true");
        assert_equiv("not", b"false");
        assert_equiv("not", b"null");
    }

    // --- Comma ---

    #[test]
    fn comma_fields() {
        assert_equiv(".a, .b", br#"{"a":1,"b":2}"#);
    }

    // --- Complex fallback ---

    // --- Reduce ---

    #[test]
    fn reduce_sum() {
        assert_equiv("reduce .[] as $x (0; . + $x)", b"[1,2,3,4,5]");
    }

    #[test]
    fn reduce_string_concat() {
        assert_equiv(r#"reduce .[] as $x (""; . + $x)"#, br#"["a","b","c"]"#);
    }

    #[test]
    fn reduce_object_iteration() {
        assert_equiv("reduce .[] as $x (0; . + $x)", br#"{"a":1,"b":2,"c":3}"#);
    }

    #[test]
    fn reduce_with_field_source() {
        assert_equiv(
            "reduce .items[] as $x (0; . + $x)",
            br#"{"items":[10,20,30]}"#,
        );
    }

    #[test]
    fn reduce_empty_array() {
        assert_equiv("reduce .[] as $x (0; . + $x)", b"[]");
    }

    #[test]
    fn reduce_nested_pattern() {
        assert_equiv(
            r#"reduce .[] as {name: $n} (""; . + $n)"#,
            br#"[{"name":"a"},{"name":"b"}]"#,
        );
    }

    #[test]
    fn reduce_dead_var_counting() {
        // Pattern var $x is unused in update — takes zero-materialization path
        assert_equiv("reduce .[] as $x (0; . + 1)", b"[1,2,3,4,5]");
        assert_equiv("reduce .[] as $x (0; . + 1)", br#"{"a":1,"b":2}"#);
        assert_equiv("reduce .[] as $x (0; . + 1)", b"[]");
    }

    #[test]
    fn reduce_dead_var_field_source() {
        // Dead variable with field chain source
        assert_equiv(
            "reduce .items[] as $x (0; . + 1)",
            br#"{"items":[10,20,30]}"#,
        );
    }

    #[test]
    fn try_operator() {
        assert_equiv(".foo?", br#"{"foo":1}"#);
        assert_equiv(".foo?", b"42");
    }

    // --- Map ---

    #[test]
    fn map_field() {
        assert_equiv("map(.name)", br#"[{"name":"a"},{"name":"b"}]"#);
    }

    #[test]
    fn map_construct() {
        assert_equiv(
            "map({name, age})",
            br#"[{"name":"a","age":1,"extra":true},{"name":"b","age":2,"extra":false}]"#,
        );
    }

    #[test]
    fn map_length() {
        assert_equiv("map(length)", br#"[[1,2],[3],[4,5,6]]"#);
    }

    #[test]
    fn map_empty_array() {
        assert_equiv("map(.x)", b"[]");
    }

    #[test]
    fn map_nested_pipe() {
        assert_equiv("map(.a | .b)", br#"[{"a":{"b":1}},{"a":{"b":2}}]"#);
    }

    // --- Map values ---

    #[test]
    fn map_values_object() {
        assert_equiv("map_values(. + 1)", br#"{"a":1,"b":2,"c":3}"#);
    }

    #[test]
    fn map_values_array() {
        assert_equiv("map_values(. + 10)", b"[1,2,3]");
    }

    // --- Literal ---

    #[test]
    fn literal_in_filter() {
        assert_equiv("42", br#"{"a":1}"#);
        assert_equiv(r#""hello""#, b"null");
    }

    // --- is_flat_safe ---

    #[test]
    fn flat_safe_simple_filters() {
        assert!(is_flat_safe(&parse_filter(".")));
        assert!(is_flat_safe(&parse_filter(".foo")));
        assert!(is_flat_safe(&parse_filter(".foo.bar")));
        assert!(is_flat_safe(&parse_filter(".[]")));
        assert!(is_flat_safe(&parse_filter("length")));
        assert!(is_flat_safe(&parse_filter("type")));
        assert!(is_flat_safe(&parse_filter("keys")));
        assert!(is_flat_safe(&parse_filter("not")));
        assert!(is_flat_safe(&parse_filter("42")));
        assert!(is_flat_safe(&parse_filter(r#""hello""#)));
    }

    #[test]
    fn flat_safe_compound_filters() {
        assert!(is_flat_safe(&parse_filter(".a | .b")));
        assert!(is_flat_safe(&parse_filter("{type, name: .user.name}")));
        assert!(is_flat_safe(&parse_filter("[.[] | .x]")));
        assert!(is_flat_safe(&parse_filter(".a, .b")));
        assert!(is_flat_safe(&parse_filter(r#".x // "default""#)));
        assert!(is_flat_safe(&parse_filter(".foo?")));
        assert!(is_flat_safe(&parse_filter(r#"select(.type == "Push")"#)));
    }

    #[test]
    fn flat_safe_map() {
        assert!(is_flat_safe(&parse_filter("map(.x)")));
        assert!(is_flat_safe(&parse_filter("map_values(.x)")));
    }

    #[test]
    fn flat_safe_reduce() {
        // reduce with flat-safe source and init
        assert!(is_flat_safe(&parse_filter("reduce .[] as $x (0; . + $x)")));
        assert!(is_flat_safe(&parse_filter("reduce .[] as $x (0; . + 1)")));
        assert!(is_flat_safe(&parse_filter(
            "reduce .items[] as $x (0; . + $x)"
        )));
        // reduce with non-flat-safe source
        assert!(!is_flat_safe(&parse_filter(
            "reduce (if . then .[] else empty end) as $x (0; . + $x)"
        )));
    }

    #[test]
    fn flat_safe_rejects_unsupported() {
        // if-then-else is not flat-safe
        assert!(!is_flat_safe(&parse_filter("if .x then .a else .b end")));
        // def is not flat-safe
        assert!(!is_flat_safe(&parse_filter("def f: .; f")));
        // Unsupported builtins
        assert!(!is_flat_safe(&parse_filter("to_entries")));
        assert!(!is_flat_safe(&parse_filter("sort")));
        assert!(!is_flat_safe(&parse_filter("group_by(.x)")));
        // map with non-flat-safe inner
        assert!(!is_flat_safe(&parse_filter("map(if . then 1 else 0 end)")));
    }

    // --- Error handling ---

    #[test]
    fn field_on_non_object_sets_error() {
        // .foo on an array should produce no output but set an error
        let filter = parse_filter(".foo");
        let result = eval_with_flat(&filter, b"[1,2,3]");
        assert!(result.is_empty());
        // Error should have been set
        let err = crate::filter::eval::take_last_error();
        assert!(err.is_some(), "expected error for .foo on array");
    }

    #[test]
    fn try_clears_error() {
        // .foo? on a non-object should produce no output and NO error
        let filter = parse_filter(".foo?");
        let result = eval_with_flat(&filter, b"[1,2,3]");
        assert!(result.is_empty());
        let err = crate::filter::eval::take_last_error();
        assert!(err.is_none(), "try should have cleared the error");
    }

    // --- Mixed complex ---

    #[test]
    fn mixed_object_with_fallback() {
        // Object construction where some values use complex expressions
        assert_equiv(
            r#"{type, count: (.items | length), label: "test"}"#,
            br#"{"type":"event","items":[1,2,3],"extra":"ignored"}"#,
        );
    }
}
