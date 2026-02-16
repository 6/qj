//! Lazy evaluator wrapper that operates on `FlatValue` to avoid materializing
//! the full `Value` tree for NDJSON lines.
//!
//! The key optimization: field chain navigation stays as FlatValue (zero
//! allocation) and only materializes at the point where a concrete Value is
//! needed (output boundary, complex computation, etc.).

use crate::filter::{Env, Filter, ObjKey};
use crate::flat_value::FlatValue;
use crate::value::Value;
use std::sync::Arc;

/// Result of navigating a filter on a FlatValue.
///
/// `Flat` means we successfully navigated without materializing.
/// `Values` means we had to materialize (or the filter produced multiple outputs).
enum NavResult<'a> {
    /// Single FlatValue result — still zero-copy.
    Flat(FlatValue<'a>),
    /// Materialized Value results.
    Values(Vec<Value>),
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

        Filter::Pipe(left, right) => match eval_flat_nav(left, flat, env) {
            NavResult::Flat(mid) => eval_flat_nav(right, mid, env),
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

        Filter::Alternative(left, right) => {
            match eval_flat_nav(left, flat, env) {
                NavResult::Flat(child) => {
                    if child.is_truthy() {
                        NavResult::Flat(child)
                    } else {
                        eval_flat_nav(right, flat, env)
                    }
                }
                NavResult::Values(values) => {
                    // Alternative: first truthy output wins
                    let truthy: Vec<Value> = values.into_iter().filter(|v| v.is_truthy()).collect();
                    if !truthy.is_empty() {
                        NavResult::Values(truthy)
                    } else {
                        eval_flat_nav(right, flat, env)
                    }
                }
            }
        }

        Filter::Try(inner) => {
            // Try: suppress errors, treat as navigation
            let result = eval_flat_nav(inner, flat, env);
            let _ = crate::filter::eval::take_last_error();
            result
        }

        Filter::Literal(v) => NavResult::Values(vec![v.clone()]),

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
        Filter::Builtin(name, args) if args.is_empty() => {
            matches!(name.as_str(), "length" | "type" | "keys" | "not")
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
            // Evaluate condition — need to materialize for complex conditions
            let value = flat.to_value();
            let mut is_truthy = false;
            crate::filter::eval::eval_filter_with_env(cond, &value, env, &mut |v| {
                if v.is_truthy() {
                    is_truthy = true;
                }
            });
            if is_truthy {
                output(value);
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

        Filter::Builtin(name, args) if name == "not" && args.is_empty() => {
            output(Value::Bool(!flat.is_truthy()));
        }

        Filter::Comma(filters) => {
            for f in filters {
                eval_flat(f, flat, env, output);
            }
        }

        Filter::Literal(v) => {
            output(v.clone());
        }

        Filter::Try(inner) => {
            eval_flat(inner, flat, env, output);
            // Try suppresses errors — clear any set by the inner expression
            let _ = crate::filter::eval::take_last_error();
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

    #[test]
    fn reduce_fallback() {
        assert_equiv("reduce .[] as $x (0; . + $x)", b"[1,2,3,4,5]");
    }

    #[test]
    fn try_operator() {
        assert_equiv(".foo?", br#"{"foo":1}"#);
        assert_equiv(".foo?", b"42");
    }

    // --- Literal ---

    #[test]
    fn literal_in_filter() {
        assert_equiv("42", br#"{"a":1}"#);
        assert_equiv(r#""hello""#, b"null");
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
