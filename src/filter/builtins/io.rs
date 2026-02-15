use crate::filter::{Env, Filter};
use crate::value::Value;
use std::rc::Rc;

use super::super::eval::eval;

pub(super) fn eval_io(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "builtins" => {
            // Returns "name/arity" strings for all builtins
            let builtins = vec![
                ("length", 0),
                ("utf8bytelength", 0),
                ("keys", 0),
                ("keys_unsorted", 0),
                ("values", 0),
                ("type", 0),
                ("empty", 0),
                ("not", 0),
                ("null", 0),
                ("true", 0),
                ("false", 0),
                ("numbers", 0),
                ("strings", 0),
                ("booleans", 0),
                ("nulls", 0),
                ("arrays", 0),
                ("objects", 0),
                ("iterables", 0),
                ("scalars", 0),
                ("map", 1),
                ("select", 1),
                ("add", 0),
                ("any", 0),
                ("any", 1),
                ("any", 2),
                ("all", 0),
                ("all", 1),
                ("all", 2),
                ("has", 1),
                ("to_entries", 0),
                ("from_entries", 0),
                ("with_entries", 1),
                ("tostring", 0),
                ("tonumber", 0),
                ("toboolean", 0),
                ("ascii_downcase", 0),
                ("ascii_upcase", 0),
                ("sort", 0),
                ("sort_by", 1),
                ("group_by", 1),
                ("unique", 0),
                ("unique_by", 1),
                ("flatten", 0),
                ("flatten", 1),
                ("first", 0),
                ("first", 1),
                ("last", 0),
                ("last", 1),
                ("reverse", 0),
                ("min", 0),
                ("max", 0),
                ("min_by", 1),
                ("max_by", 1),
                ("del", 1),
                ("contains", 1),
                ("inside", 1),
                ("ltrimstr", 1),
                ("rtrimstr", 1),
                ("startswith", 1),
                ("endswith", 1),
                ("split", 1),
                ("join", 1),
                ("range", 1),
                ("range", 2),
                ("range", 3),
                ("floor", 0),
                ("ceil", 0),
                ("round", 0),
                ("sqrt", 0),
                ("pow", 2),
                ("log", 0),
                ("log2", 0),
                ("log10", 0),
                ("exp", 0),
                ("exp2", 0),
                ("fabs", 0),
                ("nan", 0),
                ("infinite", 0),
                ("isnan", 0),
                ("isinfinite", 0),
                ("isfinite", 0),
                ("isnormal", 0),
                ("abs", 0),
                ("trim", 0),
                ("ltrim", 0),
                ("rtrim", 0),
                ("index", 1),
                ("rindex", 1),
                ("indices", 1),
                ("explode", 0),
                ("implode", 0),
                ("tojson", 0),
                ("fromjson", 0),
                ("transpose", 0),
                ("map_values", 1),
                ("limit", 2),
                ("until", 2),
                ("while", 2),
                ("isempty", 1),
                ("getpath", 1),
                ("setpath", 2),
                ("delpaths", 1),
                ("paths", 0),
                ("paths", 1),
                ("leaf_paths", 0),
                ("builtins", 0),
                ("input", 0),
                ("debug", 0),
                ("debug", 1),
                ("error", 0),
                ("error", 1),
                ("env", 0),
                ("ascii", 0),
                ("nth", 1),
                ("nth", 2),
                ("repeat", 1),
                ("recurse", 0),
                ("recurse", 1),
                ("recurse", 2),
                ("walk", 1),
                ("bsearch", 1),
                ("path", 1),
                ("todate", 0),
                ("fromdate", 0),
                ("now", 0),
                ("test", 1),
                ("test", 2),
                ("match", 1),
                ("match", 2),
                ("capture", 1),
                ("capture", 2),
                ("scan", 1),
                ("sub", 2),
                ("sub", 3),
                ("gsub", 2),
                ("gsub", 3),
                ("splits", 1),
                ("splits", 2),
                ("@base64", 0),
                ("@base64d", 0),
                ("@uri", 0),
                ("@csv", 0),
                ("@tsv", 0),
                ("@html", 0),
                ("@sh", 0),
                ("@json", 0),
                ("@text", 0),
                ("in", 1),
                ("IN", 1),
                ("IN", 2),
                ("pick", 1),
                ("combinations", 0),
                ("combinations", 1),
                ("ascii_upcase", 0),
                ("ascii_downcase", 0),
                ("logb", 0),
                ("scalb", 2),
                ("significand", 0),
                ("lgamma", 0),
                ("tgamma", 0),
                ("j0", 0),
                ("j1", 0),
                ("rint", 0),
                ("nearbyint", 0),
                ("atan", 0),
                ("atan", 2),
                ("acos", 0),
                ("asin", 0),
                ("cos", 0),
                ("sin", 0),
                ("tan", 0),
                ("cbrt", 0),
                ("remainder", 2),
                ("fma", 3),
                ("drem", 2),
                ("ldexp", 2),
                ("frexp", 0),
                ("modf", 0),
                ("input", 0),
                ("inputs", 0),
                ("strftime", 1),
                ("gmtime", 0),
                ("localtime", 0),
                ("mktime", 0),
                ("strptime", 1),
                ("strflocaltime", 1),
                ("have_decnum", 0),
                ("have_literal_numbers", 0),
                ("@urid", 0),
                ("trimstr", 1),
            ];
            let arr: Vec<Value> = builtins
                .iter()
                .map(|(name, arity)| Value::String(format!("{name}/{arity}").into()))
                .collect();
            output(Value::Array(Rc::new(arr)));
        }
        "input" => {
            let val = super::super::eval::INPUT_QUEUE.with(|q| q.borrow_mut().pop_front());
            if let Some(v) = val {
                output(v);
            } else {
                // jq signals break when no more input is available
                super::super::eval::LAST_ERROR
                    .with(|e| *e.borrow_mut() = Some(Value::String("break".into())));
            }
        }
        "inputs" => {
            let values: Vec<Value> =
                super::super::eval::INPUT_QUEUE.with(|q| q.borrow_mut().drain(..).collect());
            for v in values {
                output(v);
            }
        }
        "debug" => {
            if let Some(arg) = args.first() {
                let mut label = String::new();
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        label = s.to_string();
                    }
                });
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, input, false).unwrap();
                let json = String::from_utf8(buf).unwrap_or_default();
                if label.is_empty() {
                    eprintln!("[\"DEBUG:\",{json}]");
                } else {
                    eprintln!("[\"{label}\",{json}]");
                }
            } else {
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, input, false).unwrap();
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
            super::super::eval::LAST_ERROR.with(|e| *e.borrow_mut() = Some(err_val));
        }
        "have_decnum" | "have_literal_numbers" => {
            // qj uses i64/f64, not arbitrary precision decimals
            output(Value::Bool(false));
        }
        "env" | "$ENV" => {
            let vars: Vec<(String, Value)> = std::env::vars()
                .map(|(k, v)| (k, Value::String(v.into())))
                .collect();
            output(Value::Object(Rc::new(vars)));
        }
        _ => {}
    }
}
