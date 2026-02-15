/// jq builtin functions — dispatcher to category sub-modules.
mod arrays;
mod date;
mod format;
mod io;
mod math;
mod paths;
mod regex;
mod strings;
mod types;

use crate::filter::{Env, Filter};
use crate::value::Value;

/// Set a runtime error value. Shared helper for all builtin modules.
pub(super) fn set_error(msg: String) {
    super::eval::LAST_ERROR.with(|e| *e.borrow_mut() = Some(Value::String(msg)));
}

pub(super) fn eval_builtin(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        // Type operations and membership
        "length" | "type" | "empty" | "not" | "null" | "true" | "false" | "numbers" | "strings"
        | "booleans" | "nulls" | "arrays" | "objects" | "iterables" | "scalars" | "has"
        | "contains" | "inside" | "in" | "to_entries" | "from_entries" => {
            types::eval_types(name, args, input, env, output)
        }

        // String operations
        "tostring" | "tonumber" | "toboolean" | "ascii_downcase" | "ascii_upcase" | "ltrimstr"
        | "rtrimstr" | "trimstr" | "startswith" | "endswith" | "split" | "join" | "trim"
        | "ltrim" | "rtrim" | "index" | "rindex" | "indices" | "_indices" | "explode"
        | "implode" | "tojson" | "fromjson" | "utf8bytelength" | "ascii" => {
            strings::eval_strings(name, args, input, env, output)
        }

        // Array/collection operations
        "keys" | "keys_unsorted" | "values" | "map" | "select" | "add" | "any" | "all" | "sort"
        | "sort_by" | "group_by" | "unique" | "unique_by" | "flatten" | "first" | "last"
        | "reverse" | "min" | "max" | "min_by" | "max_by" | "del" | "transpose" | "map_values"
        | "limit" | "skip" | "until" | "while" | "repeat" | "isempty" | "nth" | "recurse"
        | "walk" | "bsearch" | "IN" | "INDEX" | "JOIN" | "pick" | "with_entries"
        | "combinations" => arrays::eval_arrays(name, args, input, env, output),

        // Math operations
        "range" | "floor" | "ceil" | "round" | "trunc" | "truncate" | "fabs" | "sqrt" | "cbrt"
        | "log" | "log_e" | "log2" | "log10" | "logb" | "exp" | "exp2" | "sin" | "cos" | "tan"
        | "asin" | "acos" | "atan" | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh"
        | "significand" | "nearbyint" | "rint" | "scalb" | "exponent" | "j0" | "j1" | "nan"
        | "infinite" | "inf" | "isnan" | "isinfinite" | "isfinite" | "isnormal" | "pow"
        | "atan2" | "remainder" | "hypot" | "fma" | "abs" => {
            math::eval_math(name, args, input, env, output)
        }

        // Path operations
        "getpath" | "setpath" | "delpaths" | "paths" | "leaf_paths" | "path" => {
            paths::eval_paths(name, args, input, env, output)
        }

        // Regex operations
        "test" | "match" | "capture" | "scan" | "sub" | "gsub" | "splits" => {
            regex::eval_regex(name, args, input, env, output)
        }

        // Date/time operations
        "todate" | "fromdate" | "now" | "strftime" | "gmtime" | "localtime" | "mktime"
        | "strptime" | "strflocaltime" => date::eval_date(name, args, input, env, output),

        // Format strings
        "@json" | "@text" | "@html" | "@uri" | "@urid" | "@csv" | "@tsv" | "@sh" | "@base64"
        | "@base64d" => format::eval_format(name, args, input, env, output),

        // I/O and introspection
        "builtins"
        | "input"
        | "debug"
        | "error"
        | "env"
        | "$ENV"
        | "have_decnum"
        | "have_literal_numbers" => io::eval_io(name, args, input, env, output),

        _ => {
            // Unknown builtin — silently produce no output
        }
    }
}
