use crate::filter::{Env, Filter};
use crate::value::Value;

use super::super::eval::eval;
use super::super::value_ops::{
    f64_to_value, input_as_f64, libc_frexp, libc_j0, libc_j1, libc_ldexp, libc_logb, to_f64,
};

pub(super) fn eval_math(
    name: &str,
    args: &[Filter],
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    match name {
        "range" => match args.len() {
            1 => {
                eval(&args[0], input, env, &mut |nv| {
                    let n = to_f64(&nv);
                    let mut i = 0.0;
                    while i < n {
                        output(f64_to_value(i));
                        i += 1.0;
                    }
                });
            }
            2 => {
                eval(&args[0], input, env, &mut |from_v| {
                    eval(&args[1], input, env, &mut |to_v| {
                        let from = to_f64(&from_v);
                        let to = to_f64(&to_v);
                        let mut i = from;
                        while i < to {
                            output(f64_to_value(i));
                            i += 1.0;
                        }
                    });
                });
            }
            3 => {
                eval(&args[0], input, env, &mut |from_v| {
                    eval(&args[1], input, env, &mut |to_v| {
                        eval(&args[2], input, env, &mut |step_v| {
                            let from = to_f64(&from_v);
                            let to = to_f64(&to_v);
                            let step = to_f64(&step_v);
                            if step == 0.0 {
                                return;
                            }
                            let mut i = from;
                            if step > 0.0 {
                                while i < to {
                                    output(f64_to_value(i));
                                    i += step;
                                }
                            } else {
                                while i > to {
                                    output(f64_to_value(i));
                                    i += step;
                                }
                            }
                        });
                    });
                });
            }
            _ => {}
        },
        "floor" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.floor()));
            }
        }
        "ceil" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.ceil()));
            }
        }
        "round" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.round()));
            }
        }
        "trunc" | "truncate" => {
            if let Some(f) = input_as_f64(input) {
                output(f64_to_value(f.trunc()));
            }
        }
        "fabs" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.abs(), None));
            }
        }
        "sqrt" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.sqrt(), None));
            }
        }
        "cbrt" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.cbrt(), None));
            }
        }
        "log" | "log_e" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.ln(), None));
            }
        }
        "log2" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.log2(), None));
            }
        }
        "log10" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.log10(), None));
            }
        }
        "logb" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(libc_logb(f), None));
            }
        }
        "exp" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.exp(), None));
            }
        }
        "exp2" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.exp2(), None));
            }
        }
        "sin" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.sin(), None));
            }
        }
        "cos" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.cos(), None));
            }
        }
        "tan" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.tan(), None));
            }
        }
        "asin" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.asin(), None));
            }
        }
        "acos" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.acos(), None));
            }
        }
        "atan" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.atan(), None));
            }
        }
        "sinh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.sinh(), None));
            }
        }
        "cosh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.cosh(), None));
            }
        }
        "tanh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.tanh(), None));
            }
        }
        "asinh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.asinh(), None));
            }
        }
        "acosh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.acosh(), None));
            }
        }
        "atanh" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(f.atanh(), None));
            }
        }
        "significand" | "nearbyint" | "rint" => {
            if let Some(f) = input_as_f64(input) {
                let result = match name {
                    "significand" => {
                        if f == 0.0 {
                            0.0
                        } else {
                            let (_, exp) = libc_frexp(f);
                            f * (2.0_f64).powi(-(exp - 1))
                        }
                    }
                    _ => f.round(),
                };
                output(Value::Double(result, None));
            }
        }
        "scalb" => {
            if let (Some(base), Some(arg)) = (input_as_f64(input), args.first()) {
                let mut exp = 0i32;
                eval(arg, input, env, &mut |v| exp = to_f64(&v) as i32);
                output(f64_to_value(libc_ldexp(base, exp)));
            }
        }
        "exponent" => {
            if let Some(f) = input_as_f64(input) {
                let (_, exp) = libc_frexp(f);
                output(Value::Int(exp as i64));
            }
        }
        "j0" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(libc_j0(f), None));
            }
        }
        "j1" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Double(libc_j1(f), None));
            }
        }
        "nan" => output(Value::Double(f64::NAN, None)),
        "infinite" | "inf" => output(Value::Double(f64::INFINITY, None)),
        "isnan" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_nan()));
            } else {
                output(Value::Bool(false));
            }
        }
        "isinfinite" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_infinite()));
            } else {
                output(Value::Bool(false));
            }
        }
        "isfinite" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_finite()));
            } else {
                output(Value::Bool(false));
            }
        }
        "isnormal" => {
            if let Some(f) = input_as_f64(input) {
                output(Value::Bool(f.is_normal()));
            } else {
                output(Value::Bool(false));
            }
        }
        "pow" => {
            if let (Some(base_f), Some(exp_f)) = (args.first(), args.get(1)) {
                let mut base = 0.0_f64;
                let mut exp = 0.0_f64;
                eval(base_f, input, env, &mut |v| base = to_f64(&v));
                eval(exp_f, input, env, &mut |v| exp = to_f64(&v));
                output(Value::Double(base.powf(exp), None));
            } else if args.len() == 1
                && let Some(f) = input_as_f64(input)
            {
                let mut exp = 0.0_f64;
                eval(&args[0], input, env, &mut |v| exp = to_f64(&v));
                output(Value::Double(f.powf(exp), None));
            }
        }
        "atan2" => {
            if let (Some(y_f), Some(x_f)) = (args.first(), args.get(1)) {
                let mut y = 0.0_f64;
                let mut x = 0.0_f64;
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                output(Value::Double(y.atan2(x), None));
            }
        }
        "remainder" => {
            if let (Some(x_f), Some(y_f)) = (args.first(), args.get(1)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                output(Value::Double(x - (x / y).round() * y, None));
            }
        }
        "hypot" => {
            if let (Some(x_f), Some(y_f)) = (args.first(), args.get(1)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                output(Value::Double(x.hypot(y), None));
            }
        }
        "fma" => {
            if let (Some(x_f), Some(y_f), Some(z_f)) = (args.first(), args.get(1), args.get(2)) {
                let mut x = 0.0_f64;
                let mut y = 0.0_f64;
                let mut z = 0.0_f64;
                eval(x_f, input, env, &mut |v| x = to_f64(&v));
                eval(y_f, input, env, &mut |v| y = to_f64(&v));
                eval(z_f, input, env, &mut |v| z = to_f64(&v));
                output(Value::Double(x.mul_add(y, z), None));
            }
        }
        "abs" => match input {
            Value::Int(n) => output(
                n.checked_abs()
                    .map_or_else(|| Value::Double((*n as f64).abs(), None), Value::Int),
            ),
            Value::Double(f, _) => output(Value::Double(f.abs(), None)),
            _ => output(input.clone()),
        },
        _ => {}
    }
}
