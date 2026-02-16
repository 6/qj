/// jq filter evaluator — produces zero or more output Values per input.
///
/// Uses generator semantics: each filter operation calls `output` for
/// each result, avoiding intermediate Vec allocations.
use crate::filter::{ArithOp, AssignOp, BoolOp, Env, Filter, ObjKey, Pattern, PatternKey};
use crate::value::Value;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Arc;

const MAX_EVAL_DEPTH: usize = 256;

use super::value_ops::{arith_values, compare_values, recurse};

/// Match a destructuring pattern against a value (lenient mode for `as`).
/// Always succeeds — missing fields/indices produce null.
pub fn match_pattern(pattern: &Pattern, value: &Value, env: &Env) -> Option<Env> {
    match_pattern_inner(pattern, value, env, false)
}

/// Match a destructuring pattern against a value (strict mode for `?//`).
/// Returns None if the value's structure doesn't match the pattern.
fn try_match_pattern(pattern: &Pattern, value: &Value, env: &Env) -> Option<Env> {
    match_pattern_inner(pattern, value, env, true)
}

fn match_pattern_inner(pattern: &Pattern, value: &Value, env: &Env, strict: bool) -> Option<Env> {
    match pattern {
        Pattern::Var(name) => Some(env.bind_var(name.clone(), value.clone())),
        Pattern::Array(patterns) => {
            if strict && !matches!(value, Value::Array(_)) {
                return None;
            }
            let mut new_env = env.clone();
            for (i, pat) in patterns.iter().enumerate() {
                let elem = match value {
                    Value::Array(arr) => arr.get(i).cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                new_env = match_pattern_inner(pat, &elem, &new_env, strict)?;
            }
            Some(new_env)
        }
        Pattern::Object(pairs) => {
            if strict && !matches!(value, Value::Object(_)) {
                return None;
            }
            let mut new_env = env.clone();
            for (key, pat) in pairs {
                let (key_str, bind_var) = match key {
                    PatternKey::Name(s) => (s.clone(), None),
                    PatternKey::Var(s) => {
                        // $x shorthand: key is the variable name without $
                        let k = s.strip_prefix('$').unwrap_or(s).to_string();
                        // If the sub-pattern is not the same variable, also bind $x
                        let bind = if *pat != Pattern::Var(s.clone()) {
                            Some(s.clone())
                        } else {
                            None
                        };
                        (k, bind)
                    }
                    PatternKey::Expr(expr) => {
                        // Computed key: evaluate expression to get key string
                        let mut result = String::new();
                        eval(expr, value, &new_env, &mut |v| {
                            if let Value::String(s) = v {
                                result = s;
                            }
                        });
                        (result, None)
                    }
                };
                let field_val = match value {
                    Value::Object(obj) => obj
                        .iter()
                        .find(|(k, _)| k == &key_str)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                // Bind the variable to the full field value before destructuring
                if let Some(var_name) = bind_var {
                    new_env = new_env.bind_var(var_name, field_val.clone());
                }
                new_env = match_pattern_inner(pat, &field_val, &new_env, strict)?;
            }
            Some(new_env)
        }
    }
}

/// Collect all variable names from a pattern.
fn collect_pattern_vars(pattern: &Pattern, vars: &mut Vec<String>) {
    match pattern {
        Pattern::Var(name) => {
            if !vars.contains(name) {
                vars.push(name.clone());
            }
        }
        Pattern::Array(patterns) => {
            for pat in patterns {
                collect_pattern_vars(pat, vars);
            }
        }
        Pattern::Object(pairs) => {
            for (_, pat) in pairs {
                collect_pattern_vars(pat, vars);
            }
        }
    }
}

thread_local! {
    /// Last error value set by `error` / `error(msg)` builtins.
    pub(super) static LAST_ERROR: RefCell<Option<Value>> = const { RefCell::new(None) };
    /// Break signal for label-break unwinding.
    static BREAK_SIGNAL: RefCell<Option<String>> = const { RefCell::new(None) };
    /// Input queue for `input`/`inputs` builtins.
    pub(super) static INPUT_QUEUE: RefCell<VecDeque<Value>> = const { RefCell::new(VecDeque::new()) };
    /// Current eval() recursion depth for stack overflow protection.
    static EVAL_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// RAII guard that decrements the eval depth counter on drop.
struct EvalDepthGuard;

impl Drop for EvalDepthGuard {
    fn drop(&mut self) {
        EVAL_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

/// Take the last error value, if any, clearing the thread-local state.
/// Called after evaluation to check for uncaught runtime errors.
pub fn take_last_error() -> Option<Value> {
    LAST_ERROR.with(|e| e.borrow_mut().take())
}

/// Set a runtime error (used by flat_eval for type errors).
pub fn set_last_error(err: Value) {
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(err));
}

/// Set the input queue for `input`/`inputs` builtins.
pub fn set_input_queue(values: VecDeque<Value>) {
    INPUT_QUEUE.with(|q| *q.borrow_mut() = values);
}

/// Take back the input queue (returns remaining unconsumed values).
pub fn take_input_queue() -> VecDeque<Value> {
    INPUT_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

/// Public entry point — creates an empty env for top-level evaluation.
pub fn eval_filter(filter: &Filter, input: &Value, output: &mut dyn FnMut(Value)) {
    // Clear stale state from any previous evaluation
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
    BREAK_SIGNAL.with(|b| *b.borrow_mut() = None);
    eval(filter, input, &Env::empty(), output);
}

/// Public entry point with a pre-populated environment (for --arg / --argjson).
pub fn eval_filter_with_env(
    filter: &Filter,
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    // Clear stale state from any previous evaluation
    LAST_ERROR.with(|e| *e.borrow_mut() = None);
    BREAK_SIGNAL.with(|b| *b.borrow_mut() = None);
    eval(filter, input, env, output);
}

/// Evaluate a filter against an input value, calling `output` for each result.
pub fn eval(filter: &Filter, input: &Value, env: &Env, output: &mut dyn FnMut(Value)) {
    // Check for break signal — stop producing output during label-break unwind.
    if BREAK_SIGNAL.with(|b| b.borrow().is_some()) {
        return;
    }

    // Recursion depth limit — prevents stack overflow from infinite recursion.
    EVAL_DEPTH.with(|d| d.set(d.get() + 1));
    let _guard = EvalDepthGuard;
    if EVAL_DEPTH.with(|d| d.get()) > MAX_EVAL_DEPTH {
        LAST_ERROR.with(|e| {
            *e.borrow_mut() = Some(Value::String(format!(
                "Evaluation depth limit exceeded ({MAX_EVAL_DEPTH})"
            )));
        });
        return;
    }
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
            Value::Null => output(Value::Null),
            _ => {
                LAST_ERROR.with(|e| {
                    *e.borrow_mut() = Some(Value::String(format!(
                        "Cannot index {} with string \"{}\"",
                        input.type_name(),
                        name
                    )));
                });
            }
        },

        Filter::Index(idx_filter) => {
            // Evaluate the index expression — iterate all outputs for generator semantics
            eval(idx_filter, input, env, &mut |idx| {
                // Truncate float indices to integer (jq behavior)
                let idx = match &idx {
                    Value::Double(f, _) if f.is_nan() => {
                        // .[nan] → null for arrays
                        if matches!(input, Value::Array(_) | Value::Null) {
                            output(Value::Null);
                        }
                        return;
                    }
                    Value::Double(f, _) if f.is_finite() => Value::Int(*f as i64),
                    _ => idx,
                };
                match (input, &idx) {
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
                    _ => {
                        LAST_ERROR.with(|e| {
                            let idx_desc = match &idx {
                                Value::String(s) => format!("string \"{}\"", s),
                                _ => idx.type_name().to_string(),
                            };
                            *e.borrow_mut() = Some(Value::String(format!(
                                "Cannot index {} with {}",
                                input.type_name(),
                                idx_desc
                            )));
                        });
                    }
                }
            });
        }

        Filter::Pipe(left, right) => {
            eval(left, input, env, &mut |intermediate| {
                // Stop if an error was raised (e.g., by `error` builtin)
                if LAST_ERROR.with(|e| e.borrow().is_some()) {
                    return;
                }
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
            Value::Null => {
                LAST_ERROR.with(|e| {
                    *e.borrow_mut() =
                        Some(Value::String("null is not iterable (null)".to_string()));
                });
            }
            _ => {
                LAST_ERROR.with(|e| {
                    *e.borrow_mut() = Some(Value::String(format!(
                        "Cannot iterate over {} ({})",
                        input.type_name(),
                        input.short_desc()
                    )));
                });
            }
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
                    output(Value::Object(Arc::new(current.clone())));
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
            // If an error occurred during collection, don't produce the array
            // (let the error propagate to try/catch)
            let has_error = LAST_ERROR.with(|e| e.borrow().is_some());
            if !has_error {
                output(Value::Array(Arc::new(arr)));
            }
        }

        Filter::Literal(val) => output(val.clone()),

        Filter::Compare(left, op, right) => {
            // jq nesting: RHS is outer loop, LHS is inner loop
            eval(right, input, env, &mut |rval| {
                eval(left, input, env, &mut |lval| {
                    let result = compare_values(&lval, op, &rval);
                    output(Value::Bool(result));
                });
            });
        }

        Filter::Arith(left, op, right) => {
            // jq nesting: RHS is outer loop, LHS is inner loop
            eval(right, input, env, &mut |rval| {
                eval(
                    left,
                    input,
                    env,
                    &mut |lval| match arith_values(&lval, op, &rval) {
                        Ok(result) => output(result),
                        Err(msg) => {
                            LAST_ERROR.with(|e| {
                                *e.borrow_mut() = Some(Value::String(msg));
                            });
                        }
                    },
                );
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
            // Check user-defined functions before builtins
            if let Some(func) = env.get_func(name, args.len()) {
                eval_user_func(name, args.len(), func, args, input, env, output);
            } else {
                super::builtins::eval_builtin(name, args, input, env, output);
            }
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
            // Collect all outputs from left, filter to truthy (not null/false).
            // If any truthy values exist, output them all; otherwise eval right.
            let mut truthy_vals = Vec::new();
            eval(left, input, env, &mut |v| {
                if v != Value::Null && v != Value::Bool(false) {
                    truthy_vals.push(v);
                }
            });
            if !truthy_vals.is_empty() {
                for v in truthy_vals {
                    output(v);
                }
            } else {
                eval(right, input, env, output);
            }
        }

        Filter::Try(inner) => {
            // Try: suppress body errors but preserve downstream errors.
            // Downstream errors occur inside the output callback (after body
            // successfully produces a value), so we capture them separately.
            LAST_ERROR.with(|e| e.borrow_mut().take());
            let mut downstream_error: Option<Value> = None;
            eval(inner, input, env, &mut |v| {
                // Body produced a value — clear any partial body error
                LAST_ERROR.with(|e| e.borrow_mut().take());
                output(v);
                // Capture error set by downstream processing
                if let Some(err) = LAST_ERROR.with(|e| e.borrow_mut().take()) {
                    downstream_error = Some(err);
                }
            });
            // Suppress remaining body error
            LAST_ERROR.with(|e| e.borrow_mut().take());
            // Restore downstream error
            if let Some(err) = downstream_error {
                LAST_ERROR.with(|e| *e.borrow_mut() = Some(err));
            }
        }

        Filter::TryCatch(body, handler) => {
            LAST_ERROR.with(|e| e.borrow_mut().take());
            let mut downstream_error: Option<Value> = None;
            eval(body, input, env, &mut |v| {
                LAST_ERROR.with(|e| e.borrow_mut().take());
                output(v);
                if let Some(err) = LAST_ERROR.with(|e| e.borrow_mut().take()) {
                    downstream_error = Some(err);
                }
            });
            if let Some(err_val) = LAST_ERROR.with(|e| e.borrow_mut().take()) {
                eval(handler, &err_val, env, output);
            }
            // Restore downstream error
            if let Some(err) = downstream_error {
                LAST_ERROR.with(|e| *e.borrow_mut() = Some(err));
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
                                crate::output::write_compact(&mut buf, &v, false).unwrap();
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
                Value::Int(n) => output(
                    n.checked_neg()
                        .map_or_else(|| Value::Double(-(n as f64), None), Value::Int),
                ),
                Value::Double(f, _) => output(Value::Double(-f, None)),
                _ => {
                    LAST_ERROR.with(|e| {
                        *e.borrow_mut() = Some(Value::String(format!(
                            "{} ({}) cannot be negated",
                            v.type_name(),
                            v.short_desc()
                        )));
                    });
                }
            });
        }

        Filter::Slice(start_f, end_f) => {
            let start_val = start_f.as_ref().map(|f| {
                let mut v = Value::Null;
                eval(f, input, env, &mut |val| v = val);
                // jq floors the start of a slice
                if let Value::Double(f, _) = &v
                    && f.is_finite()
                {
                    return Value::Int(f.floor() as i64);
                }
                v
            });
            let end_val = end_f.as_ref().map(|f| {
                let mut v = Value::Null;
                eval(f, input, env, &mut |val| v = val);
                // jq ceils the end of a slice
                if let Value::Double(f, _) = &v
                    && f.is_finite()
                {
                    return Value::Int(f.ceil() as i64);
                }
                v
            });

            match input {
                Value::Array(arr) => {
                    let len = arr.len() as i64;
                    let s = resolve_slice_index(start_val.as_ref(), 0, len);
                    let e = resolve_slice_index(end_val.as_ref(), len, len);
                    if s < e {
                        output(Value::Array(Arc::new(arr[s as usize..e as usize].to_vec())));
                    } else {
                        output(Value::Array(Arc::new(vec![])));
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
                Value::Null => output(Value::Null),
                _ => {
                    LAST_ERROR.with(|e| {
                        *e.borrow_mut() = Some(Value::String(format!(
                            "{} ({}) cannot be sliced",
                            input.type_name(),
                            input.short_desc()
                        )));
                    });
                }
            }
        }

        Filter::Var(name) => {
            if name == "$__loc__" {
                // $__loc__ returns {"file":"<top-level>","line":1}
                let loc = Value::Object(Arc::new(vec![
                    ("file".to_string(), Value::String("<top-level>".to_string())),
                    ("line".to_string(), Value::Int(1)),
                ]));
                output(loc);
            } else if let Some(val) = env.get_var(name) {
                output(val.clone());
            } else {
                // Fall through to builtins for special variables like $ENV
                super::builtins::eval_builtin(name, &[], input, env, output);
            }
        }

        Filter::Bind(expr, pattern, body) => {
            eval(expr, input, env, &mut |val| {
                if let Some(new_env) = match_pattern(pattern, &val, env) {
                    eval(body, input, &new_env, output);
                }
            });
        }

        Filter::Reduce(source, pattern, init, update) => {
            let mut acc = Value::Null;
            eval(init, input, env, &mut |v| acc = v);

            eval(source, input, env, &mut |val| {
                if let Some(new_env) = match_pattern(pattern, &val, env) {
                    let cur = acc.clone();
                    eval(update, &cur, &new_env, &mut |v| acc = v);
                }
            });

            output(acc);
        }

        Filter::Foreach(source, pattern, init, update, extract) => {
            // Support generators in init: each init value runs a separate foreach
            let mut init_vals = Vec::new();
            eval(init, input, env, &mut |v| init_vals.push(v));
            if init_vals.is_empty() {
                init_vals.push(Value::Null);
            }
            for init_val in init_vals {
                let mut acc = init_val;
                eval(source, input, env, &mut |val| {
                    if BREAK_SIGNAL.with(|b| b.borrow().is_some()) {
                        return;
                    }
                    if let Some(new_env) = match_pattern(pattern, &val, env) {
                        let cur = acc.clone();
                        eval(update, &cur, &new_env, &mut |v| acc = v);
                        if let Some(ext) = extract {
                            eval(ext, &acc, &new_env, output);
                        } else {
                            output(acc.clone());
                        }
                    }
                });
            }
        }

        Filter::Assign(path_filter, op, rhs) => {
            eval_assign(path_filter, *op, rhs, input, env, output);
        }

        Filter::Def {
            name,
            params,
            body,
            rest,
        } => {
            // Register the function in the environment. Recursion is handled
            // at call time in eval_user_func, which re-registers the function
            // (with an updated closure_env) in its own body environment.
            let func = super::UserFunc {
                params: params.clone(),
                body: (**body).clone(),
                closure_env: env.clone(),
                is_def: true,
            };
            let new_env = env.bind_func(name.clone(), params.len(), func);
            eval(rest, input, &new_env, output);
        }

        Filter::AltBind(expr, patterns, body) => {
            // Collect all variable names from all patterns so unmatched vars get null
            let mut all_vars = Vec::new();
            for pat in patterns {
                collect_pattern_vars(pat, &mut all_vars);
            }
            // Try each pattern left-to-right, use first that matches
            eval(expr, input, env, &mut |val| {
                // Pre-initialize all vars to null
                let mut base_env = env.clone();
                for var in &all_vars {
                    base_env = base_env.bind_var(var.clone(), Value::Null);
                }
                for pat in patterns {
                    if let Some(new_env) = try_match_pattern(pat, &val, &base_env) {
                        eval(body, input, &new_env, output);
                        return;
                    }
                }
                // No pattern matched — error
                LAST_ERROR.with(|e| {
                    *e.borrow_mut() = Some(Value::String(
                        "No pattern matched in ?// expression".to_string(),
                    ));
                });
            });
        }

        Filter::PostfixIndex(base, idx_expr) => {
            // A[B] — evaluate base against input, then evaluate idx_expr against
            // original input, and index the base result with each idx result.
            eval(base, input, env, &mut |base_val| {
                eval(idx_expr, input, env, &mut |idx| {
                    // Same indexing logic as Filter::Index
                    let idx = match &idx {
                        Value::Double(f, _) if f.is_nan() => {
                            if matches!(base_val, Value::Array(_) | Value::Null) {
                                output(Value::Null);
                            }
                            return;
                        }
                        Value::Double(f, _) if f.is_finite() => Value::Int(*f as i64),
                        _ => idx,
                    };
                    match (&base_val, &idx) {
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
                        _ => {
                            LAST_ERROR.with(|e| {
                                let idx_desc = match &idx {
                                    Value::String(s) => format!("string \"{}\"", s),
                                    _ => idx.type_name().to_string(),
                                };
                                *e.borrow_mut() = Some(Value::String(format!(
                                    "Cannot index {} with {}",
                                    base_val.type_name(),
                                    idx_desc
                                )));
                            });
                        }
                    }
                });
            });
        }

        Filter::PostfixSlice(base, start_f, end_f) => {
            // A[s:e] — evaluate base, s, e all against original input.
            eval(base, input, env, &mut |base_val| {
                let start_val = start_f.as_ref().map(|f| {
                    let mut v = Value::Null;
                    eval(f, input, env, &mut |val| v = val);
                    if let Value::Double(f, _) = &v
                        && f.is_finite()
                    {
                        return Value::Int(f.floor() as i64);
                    }
                    v
                });
                let end_val = end_f.as_ref().map(|f| {
                    let mut v = Value::Null;
                    eval(f, input, env, &mut |val| v = val);
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
                        let s = resolve_slice_index(start_val.as_ref(), 0, len);
                        let e = resolve_slice_index(end_val.as_ref(), len, len);
                        if s < e {
                            output(Value::Array(Arc::new(arr[s as usize..e as usize].to_vec())));
                        } else {
                            output(Value::Array(Arc::new(vec![])));
                        }
                    }
                    Value::String(s) => {
                        let chars: Vec<char> = s.chars().collect();
                        let len = chars.len() as i64;
                        let start = resolve_slice_index(start_val.as_ref(), 0, len);
                        let end = resolve_slice_index(end_val.as_ref(), len, len);
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
                        LAST_ERROR.with(|e| {
                            *e.borrow_mut() = Some(Value::String(format!(
                                "{} ({}) cannot be sliced",
                                base_val.type_name(),
                                base_val.short_desc()
                            )));
                        });
                    }
                }
            });
        }

        Filter::Label(name, body) => {
            eval(body, input, env, &mut |v| {
                if BREAK_SIGNAL.with(|b| b.borrow().is_none()) {
                    output(v);
                }
            });
            // Clear break signal if it matches our label
            BREAK_SIGNAL.with(|b| {
                if b.borrow().as_deref() == Some(name.as_str()) {
                    *b.borrow_mut() = None;
                }
            });
        }

        Filter::Break(name) => {
            BREAK_SIGNAL.with(|b| {
                *b.borrow_mut() = Some(name.clone());
            });
        }
    }
}

/// Evaluate a user-defined function call.
///
/// jq function parameters are **filters** (unevaluated AST), not values.
/// `def f(x): x | x` means `x` is a filter evaluated each time in the body.
///
/// Implementation: each parameter is bound as a zero-arg user function whose
/// body is the argument filter and whose closure captures the caller's environment.
/// When the body references a param name, it calls that zero-arg function,
/// which evaluates the arg filter in the caller's context.
///
/// For `$param` style: evaluate the arg filter once, bind result as a variable.
fn eval_user_func(
    func_name: &str,
    func_arity: usize,
    func: &super::UserFunc,
    args: &[Filter],
    input: &Value,
    caller_env: &Env,
    output: &mut dyn FnMut(Value),
) {
    // Start from the function's closure environment.
    let mut body_env = func.closure_env.clone();

    // For real `def` functions (not filter parameter wrappers), register the
    // function in its own body environment so recursive calls can find it.
    // We create a copy with the current body_env as closure, which enables
    // the tying-the-knot pattern for recursion.
    if func.is_def {
        let self_func = super::UserFunc {
            params: func.params.clone(),
            body: func.body.clone(),
            closure_env: body_env.clone(),
            is_def: true,
        };
        body_env = body_env.bind_func(func_name.to_string(), func_arity, self_func);
    }

    // Bind each parameter
    for (param_name, arg_filter) in func.params.iter().zip(args.iter()) {
        if param_name.starts_with('$') {
            // $param sugar: evaluate the arg once, bind as a variable
            let mut val = Value::Null;
            eval(arg_filter, input, caller_env, &mut |v| val = v);
            body_env = body_env.bind_var(param_name.clone(), val);
        } else {
            // Filter parameter: bind as a zero-arg function in the body environment.
            // The function's body is the arg filter, and its closure is the caller's env.
            let param_func = super::UserFunc {
                params: vec![],
                body: arg_filter.clone(),
                closure_env: caller_env.clone(),
                is_def: false,
            };
            body_env = body_env.bind_func(param_name.clone(), 0, param_func);
        }
    }

    eval(&func.body, input, &body_env, output);
}

/// Evaluate an assignment expression: `path_filter op= rhs`.
fn eval_assign(
    path_filter: &Filter,
    op: AssignOp,
    rhs: &Filter,
    input: &Value,
    env: &Env,
    output: &mut dyn FnMut(Value),
) {
    // For `= expr` where expr is a generator, produce one output per RHS value.
    if op == AssignOp::Set {
        // Collect all RHS values (generators produce multiple)
        let mut rhs_vals = Vec::new();
        eval(rhs, input, env, &mut |v| rhs_vals.push(v));

        for rhs_val in rhs_vals {
            let set_updater = |_current: &Value| -> Option<Value> { Some(rhs_val.clone()) };
            if is_update_path_supported(path_filter) {
                if let Some(result) = update_recursive(path_filter, input, env, &set_updater) {
                    output(result);
                }
            } else {
                eval_assign_via_paths(path_filter, input, &set_updater, output);
            }
        }
        return;
    }

    // Build the leaf updater closure based on the operation type.
    // Returns Some(new_value) or None (for deletion, e.g. |= empty).
    type Updater<'a> = Box<dyn Fn(&Value) -> Option<Value> + 'a>;
    let updater: Updater<'_> = match op {
        AssignOp::Update => Box::new(|current: &Value| {
            let mut result = None;
            eval(rhs, current, env, &mut |v| {
                if result.is_none() {
                    result = Some(v);
                }
            });
            result // None means |= empty → deletion
        }),
        AssignOp::Alt => Box::new(|current: &Value| {
            // //= only updates if current value is null or false
            if matches!(current, Value::Null | Value::Bool(false)) {
                let mut result = None;
                eval(rhs, input, env, &mut |v| {
                    if result.is_none() {
                        result = Some(v);
                    }
                });
                Some(result.unwrap_or_else(|| current.clone()))
            } else {
                Some(current.clone())
            }
        }),
        _ => {
            // +=, -=, *=, /=, %=
            let arith_op = match op {
                AssignOp::Add => ArithOp::Add,
                AssignOp::Sub => ArithOp::Sub,
                AssignOp::Mul => ArithOp::Mul,
                AssignOp::Div => ArithOp::Div,
                AssignOp::Mod => ArithOp::Mod,
                _ => unreachable!(),
            };
            Box::new(move |current: &Value| {
                let mut result = None;
                // Evaluate rhs against the ORIGINAL input (not the current path value)
                eval(rhs, input, env, &mut |rhs_val| {
                    if result.is_none() {
                        match arith_values(current, &arith_op, &rhs_val) {
                            Ok(v) => result = Some(v),
                            Err(msg) => {
                                LAST_ERROR.with(|e| {
                                    *e.borrow_mut() = Some(Value::String(msg));
                                });
                            }
                        }
                    }
                });
                result
            })
        }
    };

    // Fast path: recursive single-pass update (O(N) for iterators).
    if is_update_path_supported(path_filter) {
        if let Some(result) = update_recursive(path_filter, input, env, &*updater) {
            output(result);
        }
        return;
    }

    // Slow path: collect paths, apply updates one by one (O(N²) for iterators).
    eval_assign_via_paths(path_filter, input, &*updater, output);
}

/// Check if a path filter can be handled by the fast recursive update.
fn is_update_path_supported(f: &Filter) -> bool {
    match f {
        Filter::Identity | Filter::Field(_) | Filter::Iterate | Filter::Select(_) => true,
        Filter::Index(_) | Filter::Slice(_, _) => true,
        Filter::PostfixIndex(a, _) | Filter::PostfixSlice(a, _, _) => is_update_path_supported(a),
        Filter::Pipe(a, b) => is_update_path_supported(a) && is_update_path_supported(b),
        Filter::Comma(items) => items.iter().all(is_update_path_supported),
        _ => false,
    }
}

/// Recursively apply update through a path filter in O(N) for iterators.
///
/// Instead of collecting all paths and doing N separate set_path calls (each
/// cloning the entire root — O(N²)), this navigates the path structure and
/// updates each container in a single pass.
///
/// Returns `Some(updated_value)` or `None` (deletion at this level).
fn update_recursive(
    path_filter: &Filter,
    input: &Value,
    env: &Env,
    updater: &dyn Fn(&Value) -> Option<Value>,
) -> Option<Value> {
    match path_filter {
        Filter::Identity => updater(input),

        Filter::Field(name) => match input {
            Value::Object(obj) => {
                let mut result: Vec<(String, Value)> = Vec::with_capacity(obj.len());
                let mut found = false;
                for (k, v) in obj.iter() {
                    if k == name && !found {
                        found = true;
                        if let Some(new_v) = updater(v) {
                            result.push((k.clone(), new_v));
                        }
                        // None → delete this key
                    } else {
                        result.push((k.clone(), v.clone()));
                    }
                }
                if !found {
                    if let Some(new_v) = updater(&Value::Null) {
                        result.push((name.clone(), new_v));
                    } else if LAST_ERROR.with(|e| e.borrow().is_some()) {
                        return None; // propagate error
                    }
                }
                Some(Value::Object(Arc::new(result)))
            }
            Value::Null => {
                if let Some(new_v) = updater(&Value::Null) {
                    Some(Value::Object(Arc::new(vec![(name.clone(), new_v)])))
                } else if LAST_ERROR.with(|e| e.borrow().is_some()) {
                    None // propagate error
                } else {
                    Some(Value::Null)
                }
            }
            _ => Some(input.clone()),
        },

        Filter::Iterate => match input {
            Value::Array(arr) => {
                let mut result = Vec::with_capacity(arr.len());
                for elem in arr.iter() {
                    if let Some(new_elem) = updater(elem) {
                        result.push(new_elem);
                    }
                    // None → element deleted
                }
                Some(Value::Array(Arc::new(result)))
            }
            Value::Object(obj) => {
                let mut result = Vec::with_capacity(obj.len());
                for (k, v) in obj.iter() {
                    if let Some(new_v) = updater(v) {
                        result.push((k.clone(), new_v));
                    }
                }
                Some(Value::Object(Arc::new(result)))
            }
            _ => Some(input.clone()),
        },

        Filter::Index(idx_f) => {
            let mut raw_indices = Vec::new();
            eval(idx_f, input, env, &mut |v| raw_indices.push(v));

            // Truncate float indices to int, handle NaN
            let mut indices = Vec::new();
            for v in raw_indices {
                match &v {
                    Value::Double(f, _) if f.is_nan() => {
                        LAST_ERROR.with(|e| {
                            *e.borrow_mut() = Some(Value::String(
                                "Cannot set array element at NaN index".into(),
                            ));
                        });
                        return None;
                    }
                    Value::Double(f, _) if f.is_finite() => {
                        indices.push(Value::Int(*f as i64));
                    }
                    _ => indices.push(v),
                }
            }

            match input {
                Value::Array(arr) => {
                    let mut result = arr.as_ref().clone();
                    let mut to_delete = Vec::new();

                    for idx_val in &indices {
                        if let Value::Int(i) = idx_val {
                            let idx = if *i < 0 {
                                (result.len() as i64 + i).max(0) as usize
                            } else {
                                *i as usize
                            };
                            while result.len() <= idx {
                                result.push(Value::Null);
                            }
                            match updater(&result[idx]) {
                                Some(new_v) => result[idx] = new_v,
                                None => to_delete.push(idx),
                            }
                        }
                    }

                    to_delete.sort_unstable();
                    to_delete.dedup();
                    for idx in to_delete.into_iter().rev() {
                        if idx < result.len() {
                            result.remove(idx);
                        }
                    }

                    Some(Value::Array(Arc::new(result)))
                }
                Value::Object(obj) => {
                    let mut result: Vec<(String, Value)> = obj.as_ref().clone();
                    let mut keys_to_delete = Vec::new();

                    for idx_val in &indices {
                        if let Value::String(k) = idx_val {
                            if let Some(entry) = result.iter_mut().find(|(ek, _)| ek == k) {
                                match updater(&entry.1) {
                                    Some(new_v) => entry.1 = new_v,
                                    None => keys_to_delete.push(k.clone()),
                                }
                            } else if let Some(new_v) = updater(&Value::Null) {
                                result.push((k.clone(), new_v));
                            }
                        }
                    }

                    result.retain(|(k, _)| !keys_to_delete.contains(k));

                    Some(Value::Object(Arc::new(result)))
                }
                Value::Null => {
                    if let Some(idx_val) = indices.first() {
                        match idx_val {
                            Value::Int(i) => {
                                if *i < 0 {
                                    LAST_ERROR.with(|e| {
                                        *e.borrow_mut() = Some(Value::String(
                                            "Out of bounds negative array index".into(),
                                        ));
                                    });
                                    return None;
                                }
                                if *i > 1_000_000 {
                                    LAST_ERROR.with(|e| {
                                        *e.borrow_mut() =
                                            Some(Value::String("Array index too large".into()));
                                    });
                                    return None;
                                }
                                let idx = *i as usize;
                                let mut arr = vec![Value::Null; idx + 1];
                                if let Some(new_v) = updater(&Value::Null) {
                                    arr[idx] = new_v;
                                } else if LAST_ERROR.with(|e| e.borrow().is_some()) {
                                    return None;
                                }
                                Some(Value::Array(Arc::new(arr)))
                            }
                            Value::String(k) => {
                                if let Some(new_v) = updater(&Value::Null) {
                                    Some(Value::Object(Arc::new(vec![(k.clone(), new_v)])))
                                } else if LAST_ERROR.with(|e| e.borrow().is_some()) {
                                    None
                                } else {
                                    Some(Value::Null)
                                }
                            }
                            _ => Some(Value::Null),
                        }
                    } else {
                        Some(Value::Null)
                    }
                }
                _ => Some(input.clone()),
            }
        }

        Filter::Slice(start_f, end_f) => {
            match input {
                Value::String(_) => {
                    LAST_ERROR.with(|e| {
                        *e.borrow_mut() = Some(Value::String("Cannot update string slices".into()));
                    });
                    None
                }
                Value::Array(arr) => {
                    let len = arr.len() as i64;
                    let s = resolve_slice_index(
                        start_f
                            .as_ref()
                            .map(|f| {
                                let mut v = Value::Null;
                                eval(f, input, env, &mut |val| v = val);
                                // floor for start
                                if let Value::Double(fv, _) = &v
                                    && fv.is_finite()
                                {
                                    return Value::Int(fv.floor() as i64);
                                }
                                v
                            })
                            .as_ref(),
                        0,
                        len,
                    );
                    let e = resolve_slice_index(
                        end_f
                            .as_ref()
                            .map(|f| {
                                let mut v = Value::Null;
                                eval(f, input, env, &mut |val| v = val);
                                // ceil for end
                                if let Value::Double(fv, _) = &v
                                    && fv.is_finite()
                                {
                                    return Value::Int(fv.ceil() as i64);
                                }
                                v
                            })
                            .as_ref(),
                        len,
                        len,
                    );
                    let s = s as usize;
                    let e = e as usize;
                    if let Some(new_val) =
                        updater(&Value::Array(Arc::new(arr[s..e.min(arr.len())].to_vec())))
                    {
                        let mut result = Vec::new();
                        result.extend_from_slice(&arr[..s]);
                        if let Value::Array(new_arr) = &new_val {
                            result.extend_from_slice(new_arr);
                        } else {
                            result.push(new_val);
                        }
                        if e < arr.len() {
                            result.extend_from_slice(&arr[e..]);
                        }
                        Some(Value::Array(Arc::new(result)))
                    } else {
                        // Deletion: remove the slice
                        let mut result = Vec::new();
                        result.extend_from_slice(&arr[..s]);
                        if e < arr.len() {
                            result.extend_from_slice(&arr[e..]);
                        }
                        Some(Value::Array(Arc::new(result)))
                    }
                }
                _ => Some(input.clone()),
            }
        }

        Filter::PostfixIndex(base, idx) => {
            // PostfixIndex(base, idx) — navigate base, then update with Index
            let idx_filter = Filter::Index(idx.clone());
            update_recursive(base, input, env, &|val: &Value| -> Option<Value> {
                update_recursive(&idx_filter, val, env, updater)
            })
        }

        Filter::PostfixSlice(base, s, e) => {
            // PostfixSlice(base, s, e) — navigate base, then update with Slice
            let slice_filter = Filter::Slice(s.clone(), e.clone());
            update_recursive(base, input, env, &|val: &Value| -> Option<Value> {
                update_recursive(&slice_filter, val, env, updater)
            })
        }

        Filter::Pipe(a, b) => {
            // a |= (b |= rhs): navigate to a, then recursively update b within
            update_recursive(a, input, env, &|val: &Value| -> Option<Value> {
                update_recursive(b, val, env, updater)
            })
        }

        Filter::Select(cond) => {
            let mut is_match = false;
            eval(cond, input, env, &mut |v| {
                if v.is_truthy() {
                    is_match = true;
                }
            });
            if is_match {
                updater(input)
            } else {
                Some(input.clone())
            }
        }

        Filter::Comma(items) => {
            let mut result = input.clone();
            for item in items {
                if let Some(updated) = update_recursive(item, &result, env, updater) {
                    result = updated;
                }
            }
            Some(result)
        }

        _ => unreachable!("checked by is_update_path_supported"),
    }
}

/// Fallback path-based assignment for patterns not supported by recursive update.
fn eval_assign_via_paths(
    path_filter: &Filter,
    input: &Value,
    updater: &dyn Fn(&Value) -> Option<Value>,
    output: &mut dyn FnMut(Value),
) {
    use super::value_ops;

    let mut paths: Vec<Vec<Value>> = Vec::new();
    value_ops::path_of(path_filter, input, &mut Vec::new(), &mut |p| {
        if let Value::Array(arr) = p {
            paths.push(arr.as_ref().clone());
        }
    });

    let mut result = input.clone();
    let mut deletions: Vec<Vec<Value>> = Vec::new();

    for path in &paths {
        let current = value_ops::get_path(&result, path);
        match updater(&current) {
            Some(new_val) => match value_ops::set_path(&result, path, &new_val) {
                Ok(v) => result = v,
                Err(msg) => {
                    LAST_ERROR.with(|e| *e.borrow_mut() = Some(Value::String(msg)));
                    return;
                }
            },
            None => {
                deletions.push(path.clone());
            }
        }
    }

    // Process deletions in reverse index order to avoid shifting
    deletions.sort_by(|a, b| match (a.last(), b.last()) {
        (Some(Value::Int(ai)), Some(Value::Int(bi))) => bi.cmp(ai),
        _ => std::cmp::Ordering::Equal,
    });
    for path in &deletions {
        result = value_ops::del_path(&result, path);
    }

    output(result);
}

/// Resolve a slice index: handle negatives (wrap with len), clamp to [0, len].
pub(crate) fn resolve_slice_index(val: Option<&Value>, default: i64, len: i64) -> i64 {
    let idx = match val {
        Some(Value::Int(n)) => *n,
        Some(Value::Double(f, _)) if f.is_finite() => *f as i64,
        // NaN, Infinity, or non-numeric → use default
        _ => return default.clamp(0, len),
    };
    let resolved = if idx < 0 { len + idx } else { idx };
    resolved.clamp(0, len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::value_ops::values_order;

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
        Value::Object(Arc::new(
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
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
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
        let input = Value::Array(Arc::new(vec![
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
        let input = Value::Array(Arc::new(vec![
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
        let input = Value::Array(Arc::new(vec![
            obj(&[("x", Value::Int(1))]),
            obj(&[("x", Value::Int(2))]),
        ]));
        let result = eval_one(&parse("[.[] | .x]"), &input);
        assert_eq!(
            result,
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn eval_index() {
        let input = Value::Array(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        assert_eq!(eval_one(&parse(".[1]"), &input), Value::Int(20));
    }

    #[test]
    fn eval_negative_index() {
        let input = Value::Array(Arc::new(vec![
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
                &Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
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
            Value::Array(Arc::new(vec![
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
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&parse("map(. + 10)"), &input),
            Value::Array(Arc::new(vec![
                Value::Int(11),
                Value::Int(12),
                Value::Int(13)
            ]))
        );
    }

    #[test]
    fn eval_add() {
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
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
        let input = Value::Array(Arc::new(vec![Value::Int(3), Value::Int(1), Value::Int(2)]));
        assert_eq!(
            eval_one(&parse("sort"), &input),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
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
        let input = Value::Array(Arc::new(vec![
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
            Value::Array(Arc::new(vec![
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
        let a = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]));
        let b = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(3)]));
        assert_eq!(values_order(&a, &b), Some(std::cmp::Ordering::Less));
        let c = Value::Array(Arc::new(vec![Value::Int(1)]));
        assert_eq!(values_order(&c, &a), Some(std::cmp::Ordering::Less));
    }

    #[test]
    fn eval_unique_sorts() {
        let input = Value::Array(Arc::new(vec![
            Value::Int(3),
            Value::Int(1),
            Value::Int(2),
            Value::Int(1),
            Value::Int(3),
        ]));
        assert_eq!(
            eval_one(&parse("unique"), &input),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]))
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
            Value::Array(Arc::new(vec![
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
            Value::Array(Arc::new(vec![
                Value::Int(97),
                Value::Int(98),
                Value::Int(99)
            ]))
        );
    }

    #[test]
    fn eval_implode() {
        let input = Value::Array(Arc::new(vec![
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
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]));
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
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
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
        assert_eq!(
            eval_one(&parse("\"ab\" * 0"), &Value::Null),
            Value::String(String::new())
        );
    }

    #[test]
    fn eval_string_divide() {
        assert_eq!(
            eval_one(&parse("\"a,b,c\" / \",\""), &Value::Null),
            Value::Array(Arc::new(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]))
        );
    }

    // --- Bug fixes ---

    #[test]
    fn eval_from_entries_capitalized() {
        let input = Value::Array(Arc::new(vec![Value::Object(Arc::new(vec![
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
        // values acts as select(. != null), passing through non-null input
        let input = obj(&[("a", Value::Int(1)), ("b", Value::Int(2))]);
        let results = eval_all(&parse("values"), &input);
        assert_eq!(results, vec![input.clone()]);
        // nulls are filtered out
        let null_results = eval_all(&parse("values"), &Value::Null);
        assert_eq!(null_results, vec![] as Vec<Value>);
    }

    #[test]
    fn eval_index_generator() {
        let input = Value::Array(Arc::new(vec![
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
        let a = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        let b = Value::Array(Arc::new(vec![Value::Int(2)]));
        assert_eq!(
            arith_values(&a, &ArithOp::Sub, &b),
            Ok(Value::Array(Arc::new(vec![Value::Int(1), Value::Int(3)])))
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
            Ok(Value::Double(f, _)) => assert!((f - 1.5).abs() < 1e-10),
            other => panic!("expected Double(1.5), got {other:?}"),
        }
    }

    #[test]
    fn eval_int_division_float_result() {
        // 1 / 3 should produce a float, not truncate to 0
        let result = arith_values(&Value::Int(1), &ArithOp::Div, &Value::Int(3));
        match result {
            Ok(Value::Double(f, _)) => assert!((f - 1.0 / 3.0).abs() < 1e-10),
            other => panic!("expected Double(0.333...), got {other:?}"),
        }
    }

    #[test]
    fn eval_int_division_exact() {
        // 6 / 3 should produce Int(2), not Double
        assert_eq!(
            arith_values(&Value::Int(6), &ArithOp::Div, &Value::Int(3)),
            Ok(Value::Int(2))
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
        // (1,2) + (10,20): RHS outer loop → 11, 12, 21, 22 (matches jq)
        let results = eval_all(&parse("(1,2) + (10,20)"), &Value::Null);
        assert_eq!(
            results,
            vec![
                Value::Int(11),
                Value::Int(12),
                Value::Int(21),
                Value::Int(22),
            ]
        );
    }

    // --- Collection builtins ---

    #[test]
    fn eval_transpose() {
        let input = Value::Array(Arc::new(vec![
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])),
            Value::Array(Arc::new(vec![Value::Int(3), Value::Int(4)])),
        ]));
        assert_eq!(
            eval_one(&parse("transpose"), &input),
            Value::Array(Arc::new(vec![
                Value::Array(Arc::new(vec![Value::Int(1), Value::Int(3)])),
                Value::Array(Arc::new(vec![Value::Int(2), Value::Int(4)])),
            ]))
        );
    }

    #[test]
    fn eval_transpose_uneven() {
        // [[1],[2,3]] → [[1,2],[null,3]]
        let input = Value::Array(Arc::new(vec![
            Value::Array(Arc::new(vec![Value::Int(1)])),
            Value::Array(Arc::new(vec![Value::Int(2), Value::Int(3)])),
        ]));
        let result = eval_one(&parse("transpose"), &input);
        assert_eq!(
            result,
            Value::Array(Arc::new(vec![
                Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])),
                Value::Array(Arc::new(vec![Value::Null, Value::Int(3)])),
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
            vec![Value::Array(Arc::new(vec![
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
        let input = Value::Array(Arc::new(vec![
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
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(3), Value::Int(5)]));
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
            StringPart::Expr(Filter::Literal(Value::Array(Arc::new(vec![
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
            StringPart::Expr(Filter::Literal(Value::Object(Arc::new(vec![(
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

    // --- Phase 2: variables ---

    #[test]
    fn eval_var_basic() {
        let f = parse(". as $x | $x");
        assert_eq!(eval_one(&f, &Value::Int(42)), Value::Int(42));
    }

    #[test]
    fn eval_var_shadowing() {
        // Inner binding shadows outer
        let f = parse("1 as $x | 2 as $x | $x");
        assert_eq!(eval_one(&f, &Value::Null), Value::Int(2));
    }

    #[test]
    fn eval_var_multiple_outputs() {
        // Generator in binding produces multiple outputs
        let f = parse(".[] as $x | $x * $x");
        let input = Value::Array(Arc::new(vec![Value::Int(2), Value::Int(3)]));
        assert_eq!(eval_all(&f, &input), vec![Value::Int(4), Value::Int(9)]);
    }

    #[test]
    fn eval_var_in_arith() {
        let f = parse(".a as $x | .b as $y | $x + $y");
        let input = Value::Object(Arc::new(vec![
            ("a".into(), Value::Int(10)),
            ("b".into(), Value::Int(20)),
        ]));
        assert_eq!(eval_one(&f, &input), Value::Int(30));
    }

    #[test]
    fn eval_var_undefined() {
        // Undefined variable produces no output (falls through to builtins)
        let f = parse("$nonexistent");
        let results = eval_all(&f, &Value::Null);
        // $nonexistent isn't a known builtin either, so no output
        assert!(results.is_empty());
    }

    #[test]
    fn eval_env_var() {
        // $ENV should still work after variable support was added
        let f = parse("$ENV | type");
        assert_eq!(eval_one(&f, &Value::Null), Value::String("object".into()));
    }

    // --- Phase 2: slicing ---

    #[test]
    fn eval_slice_array() {
        let f = parse(".[1:3]");
        let input = Value::Array(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
            Value::Int(40),
        ]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(20), Value::Int(30)]))
        );
    }

    #[test]
    fn eval_slice_array_no_start() {
        let f = parse(".[:2]");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn eval_slice_array_no_end() {
        let f = parse(".[1:]");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(2), Value::Int(3)]))
        );
    }

    #[test]
    fn eval_slice_array_negative() {
        let f = parse(".[-2:]");
        let input = Value::Array(Arc::new(vec![
            Value::Int(1),
            Value::Int(2),
            Value::Int(3),
            Value::Int(4),
        ]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(3), Value::Int(4)]))
        );
    }

    #[test]
    fn eval_slice_array_empty_range() {
        let f = parse(".[3:1]");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(eval_one(&f, &input), Value::Array(Arc::new(vec![])));
    }

    #[test]
    fn eval_slice_array_out_of_bounds() {
        let f = parse(".[0:100]");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn eval_slice_string() {
        let f = parse(".[1:4]");
        assert_eq!(
            eval_one(&f, &Value::String("abcdef".into())),
            Value::String("bcd".into())
        );
    }

    #[test]
    fn eval_slice_string_negative() {
        let f = parse(".[-3:]");
        assert_eq!(
            eval_one(&f, &Value::String("abcdef".into())),
            Value::String("def".into())
        );
    }

    #[test]
    fn eval_slice_null_input() {
        let f = parse(".[0:1]");
        assert_eq!(eval_one(&f, &Value::Null), Value::Null);
    }

    // --- Phase 2: elif ---

    #[test]
    fn eval_elif_first_branch() {
        let f = parse(r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#);
        assert_eq!(eval_one(&f, &Value::Int(-5)), Value::String("neg".into()));
    }

    #[test]
    fn eval_elif_middle_branch() {
        let f = parse(r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#);
        assert_eq!(eval_one(&f, &Value::Int(0)), Value::String("zero".into()));
    }

    #[test]
    fn eval_elif_else_branch() {
        let f = parse(r#"if . < 0 then "neg" elif . == 0 then "zero" else "pos" end"#);
        assert_eq!(eval_one(&f, &Value::Int(5)), Value::String("pos".into()));
    }

    #[test]
    fn eval_elif_three_branches() {
        let f = parse(
            r#"if . == 1 then "one" elif . == 2 then "two" elif . == 3 then "three" else "other" end"#,
        );
        assert_eq!(eval_one(&f, &Value::Int(2)), Value::String("two".into()));
        assert_eq!(eval_one(&f, &Value::Int(3)), Value::String("three".into()));
        assert_eq!(eval_one(&f, &Value::Int(99)), Value::String("other".into()));
    }

    // --- Phase 2: try-catch ---

    #[test]
    fn eval_try_keyword_success() {
        let f = parse("try .a");
        let input = Value::Object(Arc::new(vec![("a".into(), Value::Int(1))]));
        assert_eq!(eval_one(&f, &input), Value::Int(1));
    }

    #[test]
    fn eval_try_keyword_error() {
        let f = parse("try error");
        let results = eval_all(&f, &Value::Null);
        assert!(results.is_empty(), "try should suppress error");
    }

    #[test]
    fn eval_try_catch_no_error() {
        let f = parse("try . catch .");
        assert_eq!(eval_one(&f, &Value::Int(42)), Value::Int(42));
    }

    #[test]
    fn eval_try_catch_with_error() {
        let f = parse(r#"try error("boom") catch ."#);
        assert_eq!(eval_one(&f, &Value::Null), Value::String("boom".into()));
    }

    #[test]
    fn eval_try_catch_error_no_arg() {
        // `error` with no arg uses input as error value
        let f = parse("try error catch .");
        assert_eq!(eval_one(&f, &Value::Int(99)), Value::Int(99));
    }

    // --- Phase 2: reduce ---

    #[test]
    fn eval_reduce_sum() {
        let f = parse("reduce .[] as $x (0; . + $x)");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(eval_one(&f, &input), Value::Int(6));
    }

    #[test]
    fn eval_reduce_empty() {
        let f = parse("reduce .[] as $x (0; . + $x)");
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(eval_one(&f, &input), Value::Int(0));
    }

    #[test]
    fn eval_reduce_build_array() {
        let f = parse("reduce .[] as $x ([]; . + [$x * 2])");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(2), Value::Int(4), Value::Int(6)]))
        );
    }

    // --- Phase 2: foreach ---

    #[test]
    fn eval_foreach_running_sum() {
        let f = parse("[foreach .[] as $x (0; . + $x)]");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(3), Value::Int(6)]))
        );
    }

    #[test]
    fn eval_foreach_with_extract() {
        let f = parse("[foreach .[] as $x (0; . + $x; . * 10)]");
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![
                Value::Int(10),
                Value::Int(30),
                Value::Int(60)
            ]))
        );
    }

    #[test]
    fn eval_foreach_empty() {
        let f = parse("[foreach .[] as $x (0; . + $x)]");
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(eval_one(&f, &input), Value::Array(Arc::new(vec![])));
    }

    // --- Phase 2: walk ---

    #[test]
    fn eval_walk_numbers() {
        let f = parse("walk(if type == \"number\" then . + 1 else . end)");
        let input = Value::Array(Arc::new(vec![
            Value::Int(1),
            Value::Array(Arc::new(vec![Value::Int(2)])),
        ]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Array(Arc::new(vec![
                Value::Int(2),
                Value::Array(Arc::new(vec![Value::Int(3)])),
            ]))
        );
    }

    #[test]
    fn eval_walk_scalar() {
        let f = parse("walk(. + 1)");
        assert_eq!(eval_one(&f, &Value::Int(5)), Value::Int(6));
    }

    #[test]
    fn eval_walk_object() {
        let f = parse(r#"walk(if type == "string" then "X" else . end)"#);
        let input = Value::Object(Arc::new(vec![("a".into(), Value::String("hello".into()))]));
        assert_eq!(
            eval_one(&f, &input),
            Value::Object(Arc::new(vec![("a".into(), Value::String("X".into()))]))
        );
    }

    // --- Integer overflow promotion tests ---

    #[test]
    fn overflow_add_promotes_to_double() {
        let f = parse(". + 1");
        let result = eval_one(&f, &Value::Int(i64::MAX));
        // i64::MAX + 1 overflows i64 → should promote to f64
        match result {
            Value::Double(v, _) => {
                // At f64 precision, i64::MAX as f64 rounds to 2^63 = 9223372036854775808.0
                // Adding 1.0 doesn't change it (1 < ULP of 1024), so result ≈ 2^63
                assert!((v - 9.223372036854776e18).abs() < 1e4);
            }
            other => panic!("expected Double, got {other:?}"),
        }
    }

    #[test]
    fn overflow_sub_promotes_to_double() {
        let f = parse(". - 1");
        let result = eval_one(&f, &Value::Int(i64::MIN));
        // i64::MIN - 1 overflows → promotes to f64, but at f64 precision
        // -2^63 - 1.0 rounds back to -2^63 (1 < ULP of 1024), so the
        // output formatter may convert it back to integer i64::MIN.
        match result {
            Value::Double(v, _) => assert!((v - (-9.223372036854776e18)).abs() < 1e4),
            Value::Int(n) => assert_eq!(n, i64::MIN),
            other => panic!("expected Double or Int, got {other:?}"),
        }
    }

    #[test]
    fn overflow_mul_promotes_to_double() {
        let f = parse(". * 2");
        let result = eval_one(&f, &Value::Int(i64::MAX));
        match result {
            Value::Double(v, _) => assert!(v > i64::MAX as f64),
            other => panic!("expected Double, got {other:?}"),
        }
    }

    #[test]
    fn overflow_neg_promotes_to_double() {
        // -i64::MIN overflows since |i64::MIN| > i64::MAX
        let f = parse("-.");
        let result = eval_one(&f, &Value::Int(i64::MIN));
        match result {
            Value::Double(v, _) => assert!(v > 0.0),
            other => panic!("expected Double, got {other:?}"),
        }
    }

    #[test]
    fn no_overflow_stays_int() {
        let f = parse(". + 1");
        assert_eq!(eval_one(&f, &Value::Int(100)), Value::Int(101));

        let f = parse(". - 1");
        assert_eq!(eval_one(&f, &Value::Int(-100)), Value::Int(-101));

        let f = parse(". * 2");
        assert_eq!(eval_one(&f, &Value::Int(50)), Value::Int(100));

        let f = parse("-.");
        assert_eq!(eval_one(&f, &Value::Int(-42)), Value::Int(42));
    }

    #[test]
    fn overflow_div_i64_min_by_neg1() {
        // i64::MIN / -1 = i64::MAX + 1, overflows → promote to f64
        let f = parse(". / -1");
        let result = eval_one(&f, &Value::Int(i64::MIN));
        match result {
            Value::Double(v, _) => assert!(v > 0.0, "should be positive: {v}"),
            other => panic!("expected Double, got {other:?}"),
        }
    }

    #[test]
    fn overflow_mod_i64_min_by_neg1() {
        // i64::MIN % -1 is mathematically 0, must not panic
        let f = parse(". % -1");
        assert_eq!(eval_one(&f, &Value::Int(i64::MIN)), Value::Int(0));
    }

    #[test]
    fn abs_i64_min_promotes_to_double() {
        // |i64::MIN| > i64::MAX, so abs must promote to f64
        let f = parse("abs");
        let result = eval_one(&f, &Value::Int(i64::MIN));
        match result {
            Value::Double(v, _) => assert!(v > 0.0),
            other => panic!("expected Double, got {other:?}"),
        }
    }

    #[test]
    fn length_i64_min_promotes_to_double() {
        // length on negative int = abs, same overflow
        let f = parse("length");
        let result = eval_one(&f, &Value::Int(i64::MIN));
        match result {
            Value::Double(v, _) => assert!(v > 0.0),
            other => panic!("expected Double, got {other:?}"),
        }
    }

    #[test]
    fn floor_large_double_stays_double() {
        // 2^63 as f64 should stay Double, not saturate to i64::MAX
        let f = parse("floor");
        let result = eval_one(&f, &Value::Double(9223372036854775808.0, None));
        match result {
            Value::Double(v, _) => assert_eq!(v, 9223372036854775808.0),
            other => panic!("expected Double for 2^63, got {other:?}"),
        }
    }

    #[test]
    fn div_normal_stays_int() {
        assert_eq!(eval_one(&parse(". / 2"), &Value::Int(10)), Value::Int(5));
    }

    #[test]
    fn mod_normal() {
        assert_eq!(eval_one(&parse(". % 3"), &Value::Int(10)), Value::Int(1));
    }

    // --- Division / modulo by zero ---

    #[test]
    fn int_div_by_zero_error() {
        // Division by zero produces an error (no output, sets LAST_ERROR)
        assert!(eval_all(&parse("1 / 0"), &Value::Null).is_empty());
    }

    #[test]
    fn int_mod_by_zero_error() {
        assert!(eval_all(&parse("1 % 0"), &Value::Null).is_empty());
    }

    #[test]
    fn float_div_by_zero_error() {
        // 1.0 / 0.0 now produces a catchable error, not Infinity
        assert!(eval_all(&parse("1.0 / 0.0"), &Value::Null).is_empty());
    }

    #[test]
    fn float_zero_div_zero_error() {
        // 0.0 / 0.0 now produces a catchable error, not NaN
        assert!(eval_all(&parse("0.0 / 0.0"), &Value::Null).is_empty());
    }

    #[test]
    fn div_by_zero_catchable() {
        // Division by zero error is catchable with try-catch
        let result = eval_all(&parse("try (1/0) catch ."), &Value::Null);
        assert_eq!(result.len(), 1);
        match &result[0] {
            Value::String(s) => assert!(s.contains("cannot be divided"), "got: {s}"),
            other => panic!("expected error string, got {other:?}"),
        }
    }

    #[test]
    fn mod_by_zero_catchable() {
        let result = eval_all(&parse("try (1%0) catch ."), &Value::Null);
        assert_eq!(result.len(), 1);
        match &result[0] {
            Value::String(s) => assert!(s.contains("remainder"), "got: {s}"),
            other => panic!("expected error string, got {other:?}"),
        }
    }

    // --- range edge cases ---

    #[test]
    fn range_step_zero_no_output() {
        assert!(eval_all(&parse("range(0; 10; 0)"), &Value::Null).is_empty());
    }

    #[test]
    fn range_nan_no_output() {
        assert!(eval_all(&parse("range(nan)"), &Value::Null).is_empty());
    }

    #[test]
    fn range_negative_step() {
        let results = eval_all(&parse("range(5; 0; -2)"), &Value::Null);
        assert_eq!(results, vec![Value::Int(5), Value::Int(3), Value::Int(1)]);
    }

    // --- implode edge cases ---

    #[test]
    fn implode_negative_codepoint_replaced() {
        // Negative i64 → out of range → replacement char U+FFFD (jq behavior)
        let input = Value::Array(Arc::new(vec![Value::Int(-1)]));
        assert_eq!(
            eval_one(&parse("implode"), &input),
            Value::String("\u{FFFD}".into())
        );
    }

    #[test]
    fn implode_valid_codepoints() {
        let input = Value::Array(Arc::new(vec![Value::Int(65), Value::Int(66)]));
        assert_eq!(
            eval_one(&parse("implode"), &input),
            Value::String("AB".into())
        );
    }

    // --- tonumber edge cases ---

    #[test]
    fn tonumber_overflow_i64_falls_to_f64() {
        // "99999999999999999999" overflows i64 → parsed as f64
        let input = Value::String("99999999999999999999".into());
        let result = eval_one(&parse("tonumber"), &input);
        match result {
            Value::Double(v, _) => assert!(v > 1e19),
            Value::Int(_) => panic!("should be f64 for overflowing string"),
            other => panic!("expected number, got {other:?}"),
        }
    }

    #[test]
    fn tonumber_already_int() {
        assert_eq!(
            eval_one(&parse("tonumber"), &Value::Int(42)),
            Value::Int(42)
        );
    }

    // --- ceil/round on large doubles ---

    #[test]
    fn ceil_large_double_stays_double() {
        let f = parse("ceil");
        let result = eval_one(&f, &Value::Double(9223372036854775808.0, None));
        match result {
            Value::Double(v, _) => assert_eq!(v, 9223372036854775808.0),
            other => panic!("expected Double for 2^63, got {other:?}"),
        }
    }

    #[test]
    fn round_large_double_stays_double() {
        let f = parse("round");
        let result = eval_one(&f, &Value::Double(9223372036854775808.0, None));
        match result {
            Value::Double(v, _) => assert_eq!(v, 9223372036854775808.0),
            other => panic!("expected Double for 2^63, got {other:?}"),
        }
    }

    // --- limit edge cases ---

    #[test]
    fn limit_zero_no_output() {
        assert!(eval_all(&parse("limit(0; range(10))"), &Value::Null).is_empty());
    }

    #[test]
    fn limit_normal() {
        let results = eval_all(&parse("limit(3; range(10))"), &Value::Null);
        assert_eq!(results, vec![Value::Int(0), Value::Int(1), Value::Int(2)]);
    }

    // --- String builtin edge cases ---

    #[test]
    fn split_empty_string() {
        // split("") → individual characters (matches jq behavior, no empty edge strings)
        let input = Value::String("abc".into());
        let result = eval_one(&parse(r#"split("")"#), &input);
        assert_eq!(
            result,
            Value::Array(Arc::new(vec![
                Value::String("a".into()),
                Value::String("b".into()),
                Value::String("c".into()),
            ]))
        );
    }

    #[test]
    fn split_no_match() {
        let input = Value::String("hello".into());
        let result = eval_one(&parse(r#"split("xyz")"#), &input);
        assert_eq!(
            result,
            Value::Array(Arc::new(vec![Value::String("hello".into())]))
        );
    }

    #[test]
    fn join_empty_array() {
        let input = Value::Array(Arc::new(vec![]));
        let result = eval_one(&parse(r#"join(",")"#), &input);
        assert_eq!(result, Value::String("".into()));
    }

    #[test]
    fn ltrimstr_no_match() {
        let input = Value::String("hello".into());
        let result = eval_one(&parse(r#"ltrimstr("xyz")"#), &input);
        assert_eq!(result, Value::String("hello".into()));
    }

    #[test]
    fn rtrimstr_no_match() {
        let input = Value::String("hello".into());
        let result = eval_one(&parse(r#"rtrimstr("xyz")"#), &input);
        assert_eq!(result, Value::String("hello".into()));
    }

    #[test]
    fn ascii_downcase_non_ascii() {
        // Non-ASCII chars should pass through unchanged
        let input = Value::String("Héllo".into());
        let result = eval_one(&parse("ascii_downcase"), &input);
        assert_eq!(result, Value::String("héllo".into()));
    }

    #[test]
    fn tostring_types() {
        assert_eq!(
            eval_one(&parse("tostring"), &Value::Int(42)),
            Value::String("42".into())
        );
        assert_eq!(
            eval_one(&parse("tostring"), &Value::Null),
            Value::String("null".into())
        );
        assert_eq!(
            eval_one(&parse("tostring"), &Value::Bool(true)),
            Value::String("true".into())
        );
        // String input → unchanged
        assert_eq!(
            eval_one(&parse("tostring"), &Value::String("hi".into())),
            Value::String("hi".into())
        );
    }

    // --- Array builtin edge cases ---

    #[test]
    fn add_empty_array() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(eval_one(&parse("add"), &input), Value::Null);
    }

    #[test]
    fn add_single_element() {
        let input = Value::Array(Arc::new(vec![Value::Int(5)]));
        assert_eq!(eval_one(&parse("add"), &input), Value::Int(5));
    }

    #[test]
    fn add_strings() {
        let input = Value::Array(Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
        ]));
        assert_eq!(eval_one(&parse("add"), &input), Value::String("ab".into()));
    }

    #[test]
    fn flatten_already_flat() {
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(
            eval_one(&parse("flatten"), &input),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn flatten_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("flatten"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn flatten_depth_zero() {
        // flatten(0) should not flatten at all
        let inner = Value::Array(Arc::new(vec![Value::Int(1)]));
        let input = Value::Array(Arc::new(vec![inner.clone()]));
        assert_eq!(
            eval_one(&parse("flatten(0)"), &input),
            Value::Array(Arc::new(vec![inner]))
        );
    }

    #[test]
    fn first_empty_array() {
        let input = Value::Array(Arc::new(vec![]));
        assert!(eval_all(&parse("first"), &input).is_empty());
    }

    #[test]
    fn last_empty_array() {
        let input = Value::Array(Arc::new(vec![]));
        assert!(eval_all(&parse("last"), &input).is_empty());
    }

    #[test]
    fn reverse_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("reverse"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn reverse_string() {
        let input = Value::String("abc".into());
        assert_eq!(
            eval_one(&parse("reverse"), &input),
            Value::String("cba".into())
        );
    }

    #[test]
    fn sort_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("sort"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn unique_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("unique"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn group_by_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("group_by(.)"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn min_by_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(eval_one(&parse("min_by(.)"), &input), Value::Null);
    }

    #[test]
    fn max_by_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(eval_one(&parse("max_by(.)"), &input), Value::Null);
    }

    #[test]
    fn transpose_ragged() {
        // [[1,2],[3]] → [[1,3],[2,null]]
        let input = Value::Array(Arc::new(vec![
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])),
            Value::Array(Arc::new(vec![Value::Int(3)])),
        ]));
        let result = eval_one(&parse("transpose"), &input);
        assert_eq!(
            result,
            Value::Array(Arc::new(vec![
                Value::Array(Arc::new(vec![Value::Int(1), Value::Int(3)])),
                Value::Array(Arc::new(vec![Value::Int(2), Value::Null])),
            ]))
        );
    }

    #[test]
    fn transpose_empty() {
        let input = Value::Array(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("transpose"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    // --- Path operation edge cases ---

    #[test]
    fn getpath_missing() {
        let input = Value::Object(Arc::new(vec![("a".into(), Value::Int(1))]));
        assert_eq!(eval_one(&parse(r#"getpath(["b"])"#), &input), Value::Null);
    }

    #[test]
    fn getpath_deep_missing() {
        assert_eq!(
            eval_one(&parse(r#"getpath(["a","b","c"])"#), &Value::Null),
            Value::Null
        );
    }

    #[test]
    fn getpath_empty_path() {
        let input = Value::Int(42);
        assert_eq!(eval_one(&parse("getpath([])"), &input), Value::Int(42));
    }

    #[test]
    fn setpath_creates_nested() {
        let result = eval_one(&parse(r#"setpath(["a","b"]; 1)"#), &Value::Null);
        // Should create {"a":{"b":1}}
        if let Value::Object(obj) = &result {
            if let Some((_, Value::Object(inner))) = obj.iter().find(|(k, _)| k == "a") {
                assert_eq!(
                    inner.iter().find(|(k, _)| k == "b").map(|(_, v)| v),
                    Some(&Value::Int(1))
                );
                return;
            }
        }
        panic!("expected {{\"a\":{{\"b\":1}}}}, got {result:?}");
    }

    // --- Type conversion edge cases ---

    #[test]
    fn fromjson_invalid() {
        let input = Value::String("not json".into());
        assert!(eval_all(&parse("fromjson"), &input).is_empty());
    }

    #[test]
    fn fromjson_valid() {
        let input = Value::String(r#"{"a":1}"#.into());
        let result = eval_one(&parse("fromjson"), &input);
        assert_eq!(
            result,
            Value::Object(Arc::new(vec![("a".into(), Value::Int(1))]))
        );
    }

    #[test]
    fn tojson_roundtrip() {
        let input = Value::Array(Arc::new(vec![Value::Int(1), Value::Bool(true)]));
        let json_str = eval_one(&parse("tojson"), &input);
        let roundtrip = eval_one(&parse("fromjson"), &json_str);
        assert_eq!(roundtrip, input);
    }

    // --- contains / inside edge cases ---

    #[test]
    fn contains_partial_object() {
        let input = Value::Object(Arc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ]));
        assert_eq!(
            eval_one(&parse(r#"contains({"a":1})"#), &input),
            Value::Bool(true)
        );
    }

    #[test]
    fn contains_missing_key() {
        let input = Value::Object(Arc::new(vec![("a".into(), Value::Int(1))]));
        assert_eq!(
            eval_one(&parse(r#"contains({"x":1})"#), &input),
            Value::Bool(false)
        );
    }

    // --- select / empty edge cases ---

    #[test]
    fn select_false_no_output() {
        assert!(eval_all(&parse("select(false)"), &Value::Int(1)).is_empty());
    }

    #[test]
    fn select_null_no_output() {
        assert!(eval_all(&parse("select(null)"), &Value::Int(1)).is_empty());
    }

    #[test]
    fn empty_no_output() {
        assert!(eval_all(&parse("empty"), &Value::Int(1)).is_empty());
    }

    // --- type / null checks ---

    #[test]
    fn type_of_all_types() {
        assert_eq!(
            eval_one(&parse("type"), &Value::Null),
            Value::String("null".into())
        );
        assert_eq!(
            eval_one(&parse("type"), &Value::Bool(true)),
            Value::String("boolean".into())
        );
        assert_eq!(
            eval_one(&parse("type"), &Value::Int(1)),
            Value::String("number".into())
        );
        assert_eq!(
            eval_one(&parse("type"), &Value::Double(1.5, None)),
            Value::String("number".into())
        );
        assert_eq!(
            eval_one(&parse("type"), &Value::String("x".into())),
            Value::String("string".into())
        );
        assert_eq!(
            eval_one(&parse("type"), &Value::Array(Arc::new(vec![]))),
            Value::String("array".into())
        );
        assert_eq!(
            eval_one(&parse("type"), &Value::Object(Arc::new(vec![]))),
            Value::String("object".into())
        );
    }

    #[test]
    fn length_all_types() {
        assert_eq!(eval_one(&parse("length"), &Value::Null), Value::Int(0));
        assert_eq!(
            eval_one(&parse("length"), &Value::String("hello".into())),
            Value::Int(5)
        );
        assert_eq!(
            eval_one(
                &parse("length"),
                &Value::Array(Arc::new(vec![Value::Int(1)]))
            ),
            Value::Int(1)
        );
        assert_eq!(
            eval_one(&parse("length"), &Value::Object(Arc::new(vec![]))),
            Value::Int(0)
        );
        // length of number = abs value
        assert_eq!(eval_one(&parse("length"), &Value::Int(-5)), Value::Int(5));
    }

    // --- keys / values on empty ---

    #[test]
    fn keys_empty_object() {
        let input = Value::Object(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("keys"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn values_empty_object() {
        let input = Value::Object(Arc::new(vec![]));
        assert_eq!(
            eval_one(&parse("[.[]]"), &input),
            Value::Array(Arc::new(vec![]))
        );
    }

    #[test]
    fn keys_array() {
        let input = Value::Array(Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
        ]));
        assert_eq!(
            eval_one(&parse("keys"), &input),
            Value::Array(Arc::new(vec![Value::Int(0), Value::Int(1)]))
        );
    }

    // --- Negative array indexing ---

    #[test]
    fn array_index_negative() {
        let input = Value::Array(Arc::new(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
        ]));
        assert_eq!(eval_one(&parse(".[-1]"), &input), Value::Int(30));
        assert_eq!(eval_one(&parse(".[-2]"), &input), Value::Int(20));
    }

    #[test]
    fn array_index_out_of_bounds() {
        let input = Value::Array(Arc::new(vec![Value::Int(1)]));
        assert_eq!(eval_one(&parse(".[99]"), &input), Value::Null);
    }

    #[test]
    fn field_on_null() {
        assert_eq!(eval_one(&parse(".x"), &Value::Null), Value::Null);
    }

    #[test]
    fn field_on_wrong_type() {
        // .x on a number produces an error (no output), matching jq
        let mut results = Vec::new();
        eval_filter(&parse(".x"), &Value::Int(5), &mut |v| results.push(v));
        assert!(results.is_empty(), "expected no output, got {:?}", results);
    }

    // --- Explode/implode roundtrip ---

    #[test]
    fn explode_implode_roundtrip() {
        let input = Value::String("Hello".into());
        let result = eval_one(&parse("explode | implode"), &input);
        assert_eq!(result, input);
    }

    // --- until / while safety ---

    #[test]
    fn until_terminates() {
        assert_eq!(
            eval_one(&parse("0 | until(. >= 5; . + 1)"), &Value::Null),
            Value::Int(5)
        );
    }

    // --- Alternative operator ---

    #[test]
    fn alternative_null() {
        assert_eq!(eval_one(&parse("null // 42"), &Value::Null), Value::Int(42));
    }

    #[test]
    fn alternative_false() {
        assert_eq!(
            eval_one(&parse("false // 42"), &Value::Null),
            Value::Int(42)
        );
    }

    #[test]
    fn alternative_non_null() {
        assert_eq!(eval_one(&parse("1 // 42"), &Value::Null), Value::Int(1));
    }

    #[test]
    fn eval_filter_clears_stale_last_error() {
        // Deliberately set a stale error
        LAST_ERROR.with(|e| *e.borrow_mut() = Some(Value::String("stale".into())));
        // eval_filter should clear it before evaluating
        eval_filter(&parse("."), &Value::Int(1), &mut |_| {});
        let has_error = LAST_ERROR.with(|e| e.borrow().is_some());
        assert!(!has_error, "LAST_ERROR should be cleared after eval_filter");
    }

    #[test]
    fn eval_filter_with_env_clears_stale_error() {
        LAST_ERROR.with(|e| *e.borrow_mut() = Some(Value::String("stale".into())));
        eval_filter_with_env(&parse("."), &Value::Int(1), &Env::empty(), &mut |_| {});
        let has_error = LAST_ERROR.with(|e| e.borrow().is_some());
        assert!(
            !has_error,
            "LAST_ERROR should be cleared after eval_filter_with_env"
        );
    }
}
