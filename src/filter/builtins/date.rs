use crate::filter::{Env, Filter};
use crate::value::Value;

use super::super::eval::{LAST_ERROR, eval};
use super::super::value_ops::{
    bdtime_strftime, bdtime_to_epoch, epoch_to_bdtime, format_strftime_jiff, format_strftime_local,
    fromdate, input_as_f64, now_timestamp, strptime_to_bdtime, todate,
};

fn set_error(msg: String) {
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(Value::String(msg)));
}

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
            if let Some(arg) = args.first() {
                let mut fmt = String::new();
                let mut fmt_ok = false;
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        fmt = s;
                        fmt_ok = true;
                    }
                });
                if !fmt_ok {
                    set_error("strftime/1 requires a string format".to_string());
                    return;
                }
                // Input can be a number (epoch) or a broken-down time array
                if let Value::Array(arr) = input {
                    if let Some(s) = bdtime_strftime(arr, &fmt, true) {
                        output(Value::String(s));
                    } else {
                        set_error("strftime/1 requires parsed datetime inputs".to_string());
                    }
                } else if let Some(ts) = input_as_f64(input) {
                    if let Some(s) = format_strftime_jiff(&fmt, ts as i64) {
                        output(Value::String(s));
                    }
                } else if !matches!(input, Value::Null) {
                    set_error("strftime/1 requires parsed datetime inputs".to_string());
                }
            }
        }
        "gmtime" => {
            if let Some(ts) = input_as_f64(input)
                && let Some(arr) = epoch_to_bdtime(ts, true)
            {
                output(arr);
            }
        }
        "localtime" => {
            if let Some(ts) = input_as_f64(input)
                && let Some(arr) = epoch_to_bdtime(ts, false)
            {
                output(arr);
            }
        }
        "mktime" => {
            if let Value::Array(arr) = input {
                if let Some(epoch) = bdtime_to_epoch(arr) {
                    output(Value::Int(epoch));
                } else {
                    set_error("mktime requires parsed datetime inputs".to_string());
                }
            } else {
                set_error("mktime requires parsed datetime inputs".to_string());
            }
        }
        "strptime" => {
            if let Some(arg) = args.first()
                && let Value::String(s) = input
            {
                let mut fmt = String::new();
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        fmt = s;
                    }
                });
                if let Some(arr) = strptime_to_bdtime(s, &fmt) {
                    output(arr);
                }
            }
        }
        "strflocaltime" => {
            if let Some(arg) = args.first() {
                let mut fmts = Vec::new();
                let mut fmt_error = false;
                eval(arg, input, env, &mut |v| {
                    if let Value::String(s) = v {
                        fmts.push(s);
                    } else {
                        fmt_error = true;
                    }
                });
                if fmt_error {
                    set_error("strflocaltime/1 requires a string format".to_string());
                } else {
                    for fmt in &fmts {
                        match input {
                            Value::Array(arr) => {
                                if let Some(s) = bdtime_strftime(arr, fmt, false) {
                                    output(Value::String(s));
                                } else {
                                    set_error(
                                        "strflocaltime/1 requires parsed datetime inputs"
                                            .to_string(),
                                    );
                                }
                            }
                            _ => {
                                if let Some(ts) = input_as_f64(input)
                                    && let Some(s) = format_strftime_local(fmt, ts as i64)
                                {
                                    output(Value::String(s));
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}
