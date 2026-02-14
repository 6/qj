use crate::filter::{Env, Filter};
use crate::value::Value;

use super::super::eval::eval;
use super::super::value_ops::{
    format_strftime_jiff, fromdate, input_as_f64, now_timestamp, todate,
};

pub(super) fn eval_date(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "todate" => {
            if let Some(ts) = input_as_f64(input)
                && let Some(s) = todate(ts as i64)
            {
                output(Value::String(s));
            }
        }
        "fromdate" => {
            if let Value::String(s) = input
                && let Some(ts) = fromdate(s)
            {
                output(Value::Int(ts));
            }
        }
        "now" => {
            output(Value::Double(now_timestamp(), None));
        }
        "strftime" => {
            if let (Some(arg), Some(ts)) = (args.first(), input_as_f64(input)) {
                let mut fmt = String::new();
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        fmt = s;
                    }
                });
                if let Some(s) = format_strftime_jiff(&fmt, ts as i64) {
                    output(Value::String(s));
                }
            }
        }
        _ => {}
    }
}
