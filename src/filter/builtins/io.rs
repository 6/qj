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
                "walk",
                "bsearch",
                "path",
                "todate",
                "fromdate",
                "now",
                "test",
                "match",
                "capture",
                "scan",
                "sub",
                "gsub",
                "splits",
                "@base64",
                "@base64d",
                "@uri",
                "@csv",
                "@tsv",
                "@html",
                "@sh",
                "@json",
                "@text",
                "in",
                "combinations",
            ];
            let arr: Vec<Value> = names.iter().map(|n| Value::String(n.to_string())).collect();
            output(Value::Array(Rc::new(arr)));
        }
        "input" => {
            // TODO: requires input stream plumbing
        }
        "debug" => {
            if let Some(arg) = args.first() {
                let mut label = String::new();
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        label = s;
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
        "env" | "$ENV" => {
            let vars: Vec<(String, Value)> = std::env::vars()
                .map(|(k, v)| (k, Value::String(v)))
                .collect();
            output(Value::Object(Rc::new(vars)));
        }
        _ => {}
    }
}
