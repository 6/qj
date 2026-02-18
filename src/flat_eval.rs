//! Lazy evaluator wrapper that operates on `FlatValue` to avoid materializing
//! the full `Value` tree for NDJSON lines.
//!
//! The key optimization: field chain navigation stays as FlatValue (zero
//! allocation) and only materializes at the point where a concrete Value is
//! needed (output boundary, complex computation, etc.).

use crate::filter::eval::set_last_error;
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
                set_last_error(Value::String(format!(
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
                set_last_error(Value::String(format!(
                    "{} is not iterable",
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
                    set_last_error(Value::String(format!(
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
                    // Stop if an error was raised (match regular eval Pipe behavior)
                    if crate::filter::eval::has_last_error() {
                        break;
                    }
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
                    // Stop if an error was raised (match regular eval Pipe behavior)
                    if crate::filter::eval::has_last_error() {
                        break;
                    }
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

/// Serialize a FlatValue directly to compact JSON without materializing to Value.
/// This avoids all intermediate Value tree allocation.
fn write_compact_flat(w: &mut Vec<u8>, flat: FlatValue<'_>) {
    if flat.is_null() {
        w.extend_from_slice(b"null");
    } else if let Some(b) = flat.as_bool() {
        w.extend_from_slice(if b { b"true" } else { b"false" });
    } else if let Some(i) = flat.as_int() {
        let mut buf = itoa::Buffer::new();
        w.extend_from_slice(buf.format(i).as_bytes());
    } else if let Some((f, raw)) = flat.as_f64() {
        if let Some(text) = raw {
            w.extend_from_slice(text.as_bytes());
        } else {
            let mut buf = ryu::Buffer::new();
            w.extend_from_slice(buf.format_finite(f).as_bytes());
        }
    } else if let Some(s) = flat.as_str() {
        let _ = crate::output::write_json_string(w, s);
    } else if flat.is_array() {
        w.push(b'[');
        let mut first = true;
        for elem in flat.array_iter() {
            if !first {
                w.push(b',');
            }
            first = false;
            write_compact_flat(w, elem);
        }
        w.push(b']');
    } else if flat.is_object() {
        w.push(b'{');
        let mut first = true;
        for (k, v) in flat.object_iter() {
            if !first {
                w.push(b',');
            }
            first = false;
            let _ = crate::output::write_json_string(w, k);
            w.push(b':');
            write_compact_flat(w, v);
        }
        w.push(b'}');
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
                        // Stop if an error was raised (match regular eval behavior)
                        if crate::filter::eval::has_last_error() {
                            return;
                        }
                        eval_flat(right, child, env, output);
                    }
                }
                NavResult::Values(values) => {
                    for v in &values {
                        // Stop if an error was raised (match regular eval behavior)
                        if crate::filter::eval::has_last_error() {
                            return;
                        }
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
            // If an error occurred during collection, don't produce the array
            // (let the error propagate to try/catch)
            if !crate::filter::eval::has_last_error() {
                output(Value::Array(Arc::new(arr)));
            }
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
            // Non-iterable types: no output, no error.
            // Note: regular eval sets an error here, but flat_eval is only
            // used for NDJSON where input is always an object. Keeping the
            // error-free behavior avoids interactions with Alternative/Try
            // error clearing that differ between the two eval paths.
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
            let f = &args[0];
            if flat.is_array() {
                let mut result = Vec::new();
                for elem in flat.array_iter() {
                    eval_flat(f, elem, env, &mut |v| result.push(v));
                }
                output(Value::Array(Arc::new(result)));
            } else if flat.is_object() {
                // Materialize and use regular eval for map on objects.
                // Object values can be any type, and eval_flat's error state
                // can diverge from regular eval when processing scalars
                // through compound filters (Pipe/Alternative/etc).
                let value = flat.to_value();
                crate::filter::eval::eval_filter_with_env(filter, &value, env, output);
            } else {
                let value = flat.to_value();
                crate::filter::eval::set_last_error(Value::String(format!(
                    "Cannot iterate over {} ({})",
                    value.type_name(),
                    value.short_desc()
                )));
            }
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
                    // Match normal eval: default to Null if inner filter produces nothing
                    let mut new_val = Value::Null;
                    eval_flat(f, v, env, &mut |nv| new_val = nv);
                    result.push((k.to_string(), new_val));
                }
                output(Value::Object(Arc::new(result)));
            } else {
                // Normal eval passes through scalars; match that behavior
                output(flat.to_value());
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
            } else if flat.is_null() {
                output(Value::Null);
            } else {
                let value = flat.to_value();
                crate::filter::eval::set_last_error(Value::String(format!(
                    "{} ({}) has no keys",
                    value.type_name(),
                    value.short_desc()
                )));
            }
        }

        Filter::Builtin(name, args) if name == "tojson" && args.is_empty() => {
            let mut buf = Vec::new();
            write_compact_flat(&mut buf, flat);
            output(Value::String(String::from_utf8(buf).unwrap_or_default()));
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
            eval_flat(cond, flat, env, &mut |cond_val| {
                if cond_val.is_truthy() {
                    eval_flat(then_branch, flat, env, output);
                } else if let Some(else_br) = else_branch {
                    eval_flat(else_br, flat, env, output);
                } else {
                    output(flat.to_value());
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

        Filter::Builtin(name, args) if name == "sort_by" && args.len() == 1 => {
            if flat.is_array() {
                let f = &args[0];
                // Collect (sort_key, index) pairs from flat space
                let elems: Vec<FlatValue<'_>> = flat.array_iter().collect();
                let mut pairs: Vec<(Vec<Value>, usize)> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, elem)| {
                        let mut keys = Vec::new();
                        eval_flat(f, *elem, env, &mut |v| keys.push(v));
                        (keys, i)
                    })
                    .collect();
                pairs.sort_by(|(a, _), (b, _)| {
                    for (ak, bk) in a.iter().zip(b.iter()) {
                        let ord = crate::filter::values_order(ak, bk)
                            .unwrap_or(std::cmp::Ordering::Equal);
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    a.len().cmp(&b.len())
                });
                // Materialize only in sorted order
                let sorted: Vec<Value> = pairs
                    .into_iter()
                    .map(|(_, i)| elems[i].to_value())
                    .collect();
                output(Value::Array(Arc::new(sorted)));
            } else {
                let value = flat.to_value();
                crate::filter::eval::set_last_error(Value::String(format!(
                    "{} ({}) cannot be sorted, as it is not an array",
                    value.type_name(),
                    value.short_desc()
                )));
            }
        }

        Filter::Builtin(name, args) if name == "group_by" && args.len() == 1 => {
            if flat.is_array() {
                let f = &args[0];
                let elems: Vec<FlatValue<'_>> = flat.array_iter().collect();
                let mut pairs: Vec<(Value, usize)> = elems
                    .iter()
                    .enumerate()
                    .map(|(i, elem)| {
                        let mut key = Value::Null;
                        eval_flat(f, *elem, env, &mut |v| key = v);
                        (key, i)
                    })
                    .collect();
                pairs.sort_by(|(a, _), (b, _)| {
                    crate::filter::values_order(a, b).unwrap_or(std::cmp::Ordering::Equal)
                });
                // Group consecutive equal keys
                let mut groups: Vec<Value> = Vec::new();
                let mut current_group: Vec<Value> = Vec::new();
                let mut current_key: Option<&Value> = None;
                for (key, idx) in &pairs {
                    if current_key.is_some_and(|k| {
                        crate::filter::values_order(k, key) != Some(std::cmp::Ordering::Equal)
                    }) {
                        groups.push(Value::Array(Arc::new(std::mem::take(&mut current_group))));
                    }
                    current_key = Some(key);
                    current_group.push(elems[*idx].to_value());
                }
                if !current_group.is_empty() {
                    groups.push(Value::Array(Arc::new(current_group)));
                }
                output(Value::Array(Arc::new(groups)));
            } else {
                let value = flat.to_value();
                crate::filter::eval::set_last_error(Value::String(format!(
                    "{} ({}) cannot be grouped, as it is not an array",
                    value.type_name(),
                    value.short_desc()
                )));
            }
        }

        Filter::PostfixSlice(base, start_f, end_f) => {
            // Evaluate base via flat eval
            eval_flat(base, flat, env, &mut |base_val| {
                let start_val = start_f.as_ref().map(|f| {
                    let mut v = Value::Null;
                    eval_flat(f, flat, env, &mut |val| v = val);
                    if let Value::Double(f, _) = &v
                        && f.is_finite()
                    {
                        return Value::Int(f.floor() as i64);
                    }
                    v
                });
                let end_val = end_f.as_ref().map(|f| {
                    let mut v = Value::Null;
                    eval_flat(f, flat, env, &mut |val| v = val);
                    if let Value::Double(f, _) = &v
                        && f.is_finite()
                    {
                        return Value::Int(f.ceil() as i64);
                    }
                    v
                });
                match &base_val {
                    Value::Array(arr) => {
                        let len = arr.len() as i64;
                        let s =
                            crate::filter::eval::resolve_slice_index(start_val.as_ref(), 0, len);
                        let e =
                            crate::filter::eval::resolve_slice_index(end_val.as_ref(), len, len);
                        if s < e {
                            output(Value::Array(Arc::new(arr[s as usize..e as usize].to_vec())));
                        } else {
                            output(Value::Array(Arc::new(vec![])));
                        }
                    }
                    Value::String(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let len = chars.len() as i64;
                        let start =
                            crate::filter::eval::resolve_slice_index(start_val.as_ref(), 0, len);
                        let end =
                            crate::filter::eval::resolve_slice_index(end_val.as_ref(), len, len);
                        if start < end {
                            let sliced: String =
                                chars[start as usize..end as usize].iter().collect();
                            output(Value::String(sliced));
                        } else {
                            output(Value::String(String::new()));
                        }
                    }
                    Value::Null => output(Value::Null),
                    _ => {
                        crate::filter::eval::set_last_error(Value::String(format!(
                            "{} cannot be sliced",
                            base_val.type_name()
                        )));
                    }
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

    // --- Compare ---

    #[test]
    fn compare_gt() {
        assert_equiv(".a > 0", br#"{"a":5}"#);
        assert_equiv(".a > 0", br#"{"a":0}"#);
        assert_equiv(".a > 0", br#"{"a":-1}"#);
    }

    #[test]
    fn compare_lt() {
        assert_equiv(".a < 10", br#"{"a":5}"#);
        assert_equiv(".a < 10", br#"{"a":10}"#);
        assert_equiv(".a < 10", br#"{"a":15}"#);
    }

    #[test]
    fn compare_eq() {
        assert_equiv(r#".name == "alice""#, br#"{"name":"alice"}"#);
        assert_equiv(r#".name == "alice""#, br#"{"name":"bob"}"#);
    }

    #[test]
    fn compare_ne() {
        assert_equiv(".a != .b", br#"{"a":1,"b":2}"#);
        assert_equiv(".a != .b", br#"{"a":1,"b":1}"#);
    }

    #[test]
    fn compare_ge_le() {
        assert_equiv(".x >= 5", br#"{"x":5}"#);
        assert_equiv(".x >= 5", br#"{"x":4}"#);
        assert_equiv(".x <= 5", br#"{"x":5}"#);
        assert_equiv(".x <= 5", br#"{"x":6}"#);
    }

    #[test]
    fn compare_null() {
        assert_equiv(".missing > 0", br#"{"a":1}"#);
    }

    #[test]
    fn compare_cross_type() {
        assert_equiv(".a > .b", br#"{"a":1,"b":"hello"}"#);
    }

    // --- BoolOp ---

    #[test]
    fn bool_and() {
        assert_equiv(".a > 0 and .b > 0", br#"{"a":1,"b":2}"#);
        assert_equiv(".a > 0 and .b > 0", br#"{"a":0,"b":2}"#);
        assert_equiv(".a > 0 and .b > 0", br#"{"a":1,"b":0}"#);
    }

    #[test]
    fn bool_or() {
        assert_equiv(".a > 0 or .b > 0", br#"{"a":0,"b":0}"#);
        assert_equiv(".a > 0 or .b > 0", br#"{"a":1,"b":0}"#);
        assert_equiv(".a > 0 or .b > 0", br#"{"a":0,"b":1}"#);
    }

    #[test]
    fn bool_short_circuit() {
        // And: short-circuits when left is false
        assert_equiv("false and true", b"null");
        // Or: short-circuits when left is true
        assert_equiv("true or false", b"null");
    }

    // --- Arith ---

    #[test]
    fn arith_add() {
        assert_equiv(".a + .b", br#"{"a":10,"b":20}"#);
    }

    #[test]
    fn arith_sub() {
        assert_equiv(".a - .b", br#"{"a":10,"b":3}"#);
    }

    #[test]
    fn arith_mul_div_mod() {
        assert_equiv(".a * .b", br#"{"a":6,"b":7}"#);
        assert_equiv(".a / .b", br#"{"a":10,"b":3}"#);
        assert_equiv(".a % .b", br#"{"a":10,"b":3}"#);
    }

    #[test]
    fn arith_string_concat() {
        assert_equiv(r#".a + .b"#, br#"{"a":"hello","b":" world"}"#);
    }

    #[test]
    fn arith_in_condition() {
        assert_equiv(".a + .b > 10", br#"{"a":6,"b":7}"#);
        assert_equiv(".a + .b > 10", br#"{"a":3,"b":4}"#);
    }

    // --- Neg ---

    #[test]
    fn neg_int() {
        assert_equiv("-.a", br#"{"a":42}"#);
        assert_equiv("-.a", br#"{"a":-5}"#);
    }

    #[test]
    fn neg_float() {
        assert_equiv("-.a", br#"{"a":3.14}"#);
    }

    // --- Select with flat Compare (Select in eval_flat_nav) ---

    #[test]
    fn select_compare_in_pipe() {
        // This tests Select in eval_flat_nav returning Flat(flat)
        assert_equiv(
            ".[] | select(.x > 0) | .name",
            br#"[{"x":1,"name":"a"},{"x":0,"name":"b"},{"x":5,"name":"c"}]"#,
        );
    }

    #[test]
    fn select_and_construct() {
        assert_equiv(
            r#".[] | select(.x > 0) | {name, x}"#,
            br#"[{"x":1,"name":"a","extra":true},{"x":0,"name":"b","extra":false}]"#,
        );
    }

    #[test]
    fn select_complex_condition() {
        assert_equiv(
            r#".[] | select(.x > 0 and .name != "skip")"#,
            br#"[{"x":1,"name":"a"},{"x":2,"name":"skip"},{"x":0,"name":"c"}]"#,
        );
    }

    #[test]
    fn select_all_filtered() {
        assert_equiv(".[] | select(.x > 100)", br#"[{"x":1},{"x":2},{"x":3}]"#);
    }

    // --- tojson ---

    #[test]
    fn tojson_string() {
        assert_equiv("tojson", br#""hello""#);
    }

    #[test]
    fn tojson_int() {
        assert_equiv("tojson", b"42");
    }

    #[test]
    fn tojson_null() {
        assert_equiv("tojson", b"null");
    }

    #[test]
    fn tojson_bool() {
        assert_equiv("tojson", b"true");
        assert_equiv("tojson", b"false");
    }

    #[test]
    fn tojson_array() {
        assert_equiv("tojson", b"[1,2,3]");
    }

    #[test]
    fn tojson_object() {
        assert_equiv("tojson", br#"{"a":1,"b":"two"}"#);
    }

    #[test]
    fn tojson_nested() {
        assert_equiv("tojson", br#"{"a":{"b":[1,true,null]}}"#);
    }

    #[test]
    fn tojson_escaped_string() {
        assert_equiv("tojson", br#""hello \"world\"""#);
    }

    #[test]
    fn tojson_per_field() {
        assert_equiv("map_values(tojson)", br#"{"a":1,"b":"two","c":null}"#);
    }

    #[test]
    fn tojson_in_pipe() {
        assert_equiv(".a | tojson", br#"{"a":{"x":1,"y":[2,3]}}"#);
    }

    // --- Def handler ---

    #[test]
    fn def_simple() {
        assert_equiv("def f: .a; f", br#"{"a":42,"b":99}"#);
    }

    #[test]
    fn def_with_args() {
        assert_equiv(
            r#"def hi(x): if x > 0 then "yes" else "no" end; hi(.a)"#,
            br#"{"a":5}"#,
        );
    }

    #[test]
    fn def_with_iterate() {
        assert_equiv("def double: . * 2; [.[] | double]", b"[1,2,3]");
    }

    // --- IfThenElse handler ---

    #[test]
    fn if_then_else() {
        assert_equiv(r#"if .x > 0 then "pos" else "non-pos" end"#, br#"{"x":5}"#);
        assert_equiv(r#"if .x > 0 then "pos" else "non-pos" end"#, br#"{"x":-1}"#);
    }

    #[test]
    fn if_then_no_else() {
        assert_equiv("if .x > 0 then .x end", br#"{"x":5}"#);
    }

    #[test]
    fn elif_chain() {
        assert_equiv(
            r#"if .x > 10 then "big" elif .x > 0 then "small" else "zero" end"#,
            br#"{"x":15}"#,
        );
        assert_equiv(
            r#"if .x > 10 then "big" elif .x > 0 then "small" else "zero" end"#,
            br#"{"x":5}"#,
        );
        assert_equiv(
            r#"if .x > 10 then "big" elif .x > 0 then "small" else "zero" end"#,
            br#"{"x":0}"#,
        );
    }

    // --- Bind handler ---

    #[test]
    fn bind_simple() {
        assert_equiv(". as $s | $s.a + $s.b", br#"{"a":10,"b":20}"#);
    }

    #[test]
    fn bind_in_iterate() {
        assert_equiv(
            ".[] | . as $s | {name: $s.name, double: ($s.x * 2)}",
            br#"[{"name":"a","x":1},{"name":"b","x":2}]"#,
        );
    }

    // --- write_compact_flat ---

    #[test]
    fn write_compact_flat_scalars() {
        let cases: &[(&[u8], &str)] = &[
            (b"null", "null"),
            (b"true", "true"),
            (b"false", "false"),
            (b"42", "42"),
            (b"-7", "-7"),
            (b"3.14", "3.14"),
            (br#""hello""#, r#""hello""#),
        ];
        for (json, expected) in cases {
            let buf = pad_buffer(json);
            let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
            let mut out = Vec::new();
            write_compact_flat(&mut out, flat_buf.root());
            assert_eq!(
                std::str::from_utf8(&out).unwrap(),
                *expected,
                "write_compact_flat mismatch for {:?}",
                std::str::from_utf8(json).unwrap()
            );
        }
    }

    #[test]
    fn write_compact_flat_containers() {
        let cases: &[(&[u8], &str)] = &[
            (b"[1,2,3]", "[1,2,3]"),
            (b"[]", "[]"),
            (br#"{"a":1,"b":2}"#, r#"{"a":1,"b":2}"#),
            (br#"{}"#, "{}"),
            (br#"{"a":[1,{"b":true}]}"#, r#"{"a":[1,{"b":true}]}"#),
        ];
        for (json, expected) in cases {
            let buf = pad_buffer(json);
            let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
            let mut out = Vec::new();
            write_compact_flat(&mut out, flat_buf.root());
            assert_eq!(
                std::str::from_utf8(&out).unwrap(),
                *expected,
                "write_compact_flat mismatch for {:?}",
                std::str::from_utf8(json).unwrap()
            );
        }
    }

    #[test]
    fn write_compact_flat_escaped_string() {
        let json = br#""hello\nworld""#;
        let buf = pad_buffer(json);
        let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
        let mut out = Vec::new();
        write_compact_flat(&mut out, flat_buf.root());
        assert_eq!(std::str::from_utf8(&out).unwrap(), r#""hello\nworld""#);
    }

    // --- sort_by ---

    #[test]
    fn sort_by_numeric() {
        assert_equiv(
            "sort_by(.x)",
            br#"[{"x":3,"n":"c"},{"x":1,"n":"a"},{"x":2,"n":"b"}]"#,
        );
    }

    #[test]
    fn sort_by_string() {
        assert_equiv("sort_by(.x)", br#"[{"x":"b"},{"x":"a"},{"x":"c"}]"#);
    }

    #[test]
    fn sort_by_last() {
        assert_equiv(
            "sort_by(.x) | .[-1]",
            br#"[{"x":3,"n":"c"},{"x":1,"n":"a"}]"#,
        );
    }

    // --- group_by ---

    #[test]
    fn group_by_basic() {
        assert_equiv(
            "group_by(.t)",
            br#"[{"t":1,"n":"a"},{"t":2,"n":"b"},{"t":1,"n":"c"}]"#,
        );
    }

    #[test]
    fn group_by_length() {
        assert_equiv(
            "group_by(.t) | length",
            br#"[{"t":"a"},{"t":"b"},{"t":"a"},{"t":"c"},{"t":"b"}]"#,
        );
    }

    // --- PostfixSlice ---

    #[test]
    fn postfix_slice_array() {
        assert_equiv(".[:3]", b"[1,2,3,4,5]");
        assert_equiv(".[2:4]", b"[1,2,3,4,5]");
        assert_equiv(".[3:]", b"[1,2,3,4,5]");
    }

    #[test]
    fn postfix_slice_string() {
        assert_equiv(".[1:3]", br#""hello""#);
    }

    #[test]
    fn postfix_slice_negative() {
        assert_equiv(".[-2:]", b"[1,2,3,4,5]");
    }

    #[test]
    fn postfix_slice_in_pipe() {
        assert_equiv("[.[] | .x][:2]", br#"[{"x":1},{"x":2},{"x":3}]"#);
    }

    // -----------------------------------------------------------------------
    // Exhaustive differential tests for ALL independently-handled arms
    //
    // Every match arm in eval_flat() that does NOT fall through to the
    // catch-all materializer must produce identical output to the normal
    // evaluator for all JSON types. This prevents the class of bug where
    // a fix in one eval path doesn't get mirrored in the other
    // (e.g., the map-on-objects bug, map_values scalar passthrough).
    // -----------------------------------------------------------------------

    const DIVERSE_INPUTS: &[&[u8]] = &[
        b"null",
        b"true",
        b"false",
        b"0",
        b"42",
        b"-1",
        b"3.14",
        br#""hello""#,
        br#""""#,
        b"[]",
        b"[1,2,3]",
        br#"[1,"two",null,true,[5],{"a":6}]"#,
        b"{}",
        br#"{"a":1,"b":2}"#,
        br#"{"a":null,"b":"hi","c":[1,2],"d":{"x":1}}"#,
    ];

    #[test]
    fn differential_map() {
        let filters = ["map(.)", "map(type)", "map(. + 1)", "map(length)"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_map_values() {
        let filters = ["map_values(.)", "map_values(type)", "map_values(. + 1)"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_length() {
        for input in DIVERSE_INPUTS {
            assert_equiv("length", input);
        }
    }

    #[test]
    fn differential_type() {
        for input in DIVERSE_INPUTS {
            assert_equiv("type", input);
        }
    }

    #[test]
    fn differential_keys() {
        for input in DIVERSE_INPUTS {
            assert_equiv("keys", input);
        }
    }

    #[test]
    fn differential_tojson() {
        for input in DIVERSE_INPUTS {
            assert_equiv("tojson", input);
        }
    }

    #[test]
    fn differential_sort_by() {
        let inputs: &[&[u8]] = &[
            b"[]",
            b"[1,2,3]",
            br#"[{"x":3},{"x":1},{"x":2}]"#,
            br#"[{"x":"b"},{"x":"a"}]"#,
            // Non-array inputs — flat_eval and normal eval should agree
            b"null",
            b"42",
            br#""hello""#,
            br#"{"a":1}"#,
        ];
        for input in inputs {
            assert_equiv("sort_by(.x)", input);
        }
    }

    #[test]
    fn differential_group_by() {
        let inputs: &[&[u8]] = &[
            b"[]",
            br#"[{"t":1},{"t":2},{"t":1}]"#,
            // Non-array inputs
            b"null",
            b"42",
            br#""hello""#,
            br#"{"a":1}"#,
        ];
        for input in inputs {
            assert_equiv("group_by(.t)", input);
        }
    }

    #[test]
    fn differential_postfix_slice() {
        let filters = [".[1:3]", ".[:2]", ".[-2:]"];
        let inputs: &[&[u8]] = &[b"[1,2,3,4,5]", br#""hello""#, b"null", b"[]", br#""""#];
        for filter in &filters {
            for input in inputs {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_composite_pipes() {
        // Filters that combine builtins handled by flat_eval
        let cases: &[(&str, &[u8])] = &[
            ("map(type) | length", br#"{"a":1,"b":null,"c":"hi"}"#),
            ("map(length)", br#"{"a":[1,2],"b":[3]}"#),
            ("keys | map(length)", br#"{"ab":1,"cde":2}"#),
            ("map(. + 1) | map(. * 2)", b"[1,2,3]"),
            (".x | map(type)", br#"{"x":[1,"a",null]}"#),
            (".x | length", br#"{"x":[1,2,3]}"#),
            (".x | keys", br#"{"x":{"b":2,"a":1}}"#),
            ("map(tojson)", b"[1,null,true]"),
            ("map(tojson)", br#"{"a":1,"b":null}"#),
            // Pipe + Select
            (".[] | select(. > 1)", b"[1,2,3]"),
            (".[] | select(type == \"string\")", br#"[1,"a",null,"b"]"#),
            // Pipe + Alternative
            (".x // \"default\"", br#"{"a":1}"#),
            (".x // [] | .[]", br#"{"x":[1,2]}"#),
            // Pipe + Try
            (".[] | try .x", br#"[{"x":1},2,"hi"]"#),
            // ArrayConstruct + Pipe
            ("[.[] | . + 1]", b"[1,2,3]"),
            ("[.[] | type]", br#"{"a":1,"b":"hi"}"#),
            // ObjectConstruct + Pipe
            ("{a: .x, b: (.y + 1)}", br#"{"x":1,"y":2}"#),
            // Reduce + Pipe
            ("reduce .[] as $x (0; . + $x)", b"[1,2,3]"),
            // IfThenElse + Pipe
            (
                ".[] | if . > 2 then \"big\" else \"small\" end",
                b"[1,2,3,4]",
            ),
        ];
        for (filter, input) in cases {
            assert_equiv(filter, input);
        }
    }

    // --- Remaining independently-handled arms ---

    #[test]
    fn differential_identity() {
        for input in DIVERSE_INPUTS {
            assert_equiv(".", input);
        }
    }

    #[test]
    fn differential_field() {
        let filters = [".a", ".b", ".x", ".missing"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_iterate() {
        for input in DIVERSE_INPUTS {
            assert_equiv(".[]", input);
        }
    }

    #[test]
    fn differential_index() {
        let filters = [".[0]", ".[1]", ".[-1]", ".[99]"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_literal() {
        // Literal doesn't depend on input, but flat_eval handles it directly
        let filters = ["null", "true", "false", "42", r#""hello""#];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_comma() {
        let filters = ["., .", ".a, .b", "type, length"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_pipe() {
        let filters = [". | .", ". | type", ". | length", ".a | .b"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_select() {
        let filters = [
            "select(. != null)",
            "select(type == \"number\")",
            "select(. > 0)",
            "select(true)",
            "select(false)",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_alternative() {
        let filters = [
            ". // \"fallback\"",
            ".x // \"default\"",
            "null // 42",
            "false // true",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_try() {
        let filters = ["try .x", "try .[]", "try (. + 1)", "try error"];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_if_then_else() {
        let filters = [
            "if . then \"truthy\" else \"falsy\" end",
            "if type == \"array\" then length else 0 end",
            "if . == null then \"nil\" elif . == true then \"yes\" else \"other\" end",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_not() {
        for input in DIVERSE_INPUTS {
            assert_equiv("not", input);
        }
    }

    #[test]
    fn differential_neg() {
        for input in DIVERSE_INPUTS {
            assert_equiv("-(. // 0)", input);
        }
    }

    #[test]
    fn differential_compare() {
        let filters = [
            ". == null",
            ". != 0",
            ". < 10",
            ". <= \"hello\"",
            ". > false",
            ". >= []",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_bool_op() {
        let filters = [
            ". and true",
            ". or false",
            "(. != null) and (. != false)",
            "(. == null) or (. == false)",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_arith() {
        // Use try to suppress errors from type mismatches (so we compare behavior, not just errors)
        let filters = [
            "try (. + 1)",
            "try (. - 1)",
            "try (. * 2)",
            "try (. / 2)",
            "try (. % 3)",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_array_construct() {
        let filters = ["[.]", "[., .]", "[.[] | . + 1]", "[type, length]"];
        let inputs: &[&[u8]] = &[
            b"null",
            b"42",
            br#""hello""#,
            b"[1,2,3]",
            br#"{"a":1,"b":2}"#,
        ];
        for filter in &filters {
            for input in inputs {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_object_construct() {
        let filters = ["{a: 1}", "{a: ., b: type}", "{a: length}", "{x: .a, y: .b}"];
        let inputs: &[&[u8]] = &[
            b"null",
            b"42",
            br#""hello""#,
            b"[1,2,3]",
            br#"{"a":1,"b":2}"#,
        ];
        for filter in &filters {
            for input in inputs {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_bind() {
        let filters = [
            ". as $x | $x",
            ". as $x | type",
            "(.a // 0) as $x | . as $y | {x: $x}",
        ];
        for filter in &filters {
            for input in DIVERSE_INPUTS {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_reduce() {
        let inputs: &[&[u8]] = &[
            b"[]",
            b"[1,2,3]",
            b"[1,2,3,4,5]",
            br#"["a","b","c"]"#,
            br#"{"a":1,"b":2}"#,
            b"null",
            b"42",
        ];
        let filters = [
            "reduce .[] as $x (0; . + $x)",
            "reduce .[] as $x (\"\"; . + $x)",
            "reduce .[] as $x ([]; . + [$x])",
        ];
        for filter in &filters {
            for input in inputs {
                assert_equiv(filter, input);
            }
        }
    }

    #[test]
    fn differential_def() {
        let filters = [
            "def double: . * 2; double",
            "def addone: . + 1; [.[] | addone]",
            "def mytype: type; mytype",
        ];
        let inputs: &[&[u8]] = &[b"null", b"42", br#""hello""#, b"[1,2,3]", br#"{"a":1}"#];
        for filter in &filters {
            for input in inputs {
                assert_equiv(filter, input);
            }
        }
    }
}
