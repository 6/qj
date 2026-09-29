//! Deep structural operations — `jv_equal`, `jv_cmp`, `jv_contains` — with a
//! bounded native stack.
//!
//! jq recurses in C (on an 8 MB main stack) and accepts 10000 levels of
//! nesting in its input; Rust frames are larger and worker threads smaller.
//! These functions recurse up to [`MAX_RECURSION`] levels (fast, no
//! allocation for ordinary data) and continue with an explicit stack below
//! that, so arbitrarily deep values cannot overflow the thread's stack. The
//! results are exactly those of the C recursion.

use std::cmp::Ordering;

use super::{Object, Str, Value};

/// Levels handled by native recursion before switching to explicit stacks.
const MAX_RECURSION: u32 = 48;

// ------------------------------------------------------------------ equal

/// `jv_equal`.
pub(super) fn equal(a: &Value, b: &Value) -> bool {
    equal_rec(a, b, 0)
}

fn equal_rec(a: &Value, b: &Value, depth: u32) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        // (jq's pointer fast path only applies to literals, which are never
        // NaN and so compare equal to themselves anyway.)
        (Value::Number(x), Value::Number(y)) => x.equal(y),
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            // jv_equal's fast path: same storage and length, offsets ignored.
            if x.same_storage(y) {
                return true;
            }
            // jvp_array_equal
            if x.len() != y.len() {
                return false;
            }
            if depth >= MAX_RECURSION {
                return equal_iter(a, b);
            }
            x.iter()
                .zip(y.iter())
                .all(|(p, q)| equal_rec(p, q, depth + 1))
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.ptr_eq(y) {
                return true;
            }
            // jvp_object_equal
            if x.len() != y.len() {
                return false;
            }
            if depth >= MAX_RECURSION {
                return equal_iter(a, b);
            }
            x.iter()
                .all(|(k, v)| y.get(k).is_some_and(|w| equal_rec(v, w, depth + 1)))
        }
        _ => false,
    }
}

fn equal_iter(a: &Value, b: &Value) -> bool {
    let mut work: Vec<(&Value, &Value)> = vec![(a, b)];
    while let Some((a, b)) = work.pop() {
        match (a, b) {
            (Value::Array(x), Value::Array(y)) => {
                if x.same_storage(y) {
                    continue;
                }
                if x.len() != y.len() {
                    return false;
                }
                work.extend(x.iter().zip(y.iter()));
            }
            (Value::Object(x), Value::Object(y)) => {
                if x.ptr_eq(y) {
                    continue;
                }
                if x.len() != y.len() {
                    return false;
                }
                for (k, v) in x.iter() {
                    match y.get(k) {
                        Some(w) => work.push((v, w)),
                        None => return false,
                    }
                }
            }
            // Anything else is a scalar comparison (no recursion).
            _ => {
                if !equal_rec(a, b, MAX_RECURSION) {
                    return false;
                }
            }
        }
    }
    true
}

// ---------------------------------------------------------------- compare

/// `jv_cmp`; with `total`, NaN == NaN (a consistent order for sorting).
pub(super) fn compare(a: &Value, b: &Value, total: bool) -> Ordering {
    cmp_rec(a, b, total, 0)
}

fn sorted_keys(o: &Object) -> Vec<&Str> {
    let mut keys: Vec<&Str> = o.keys().collect();
    keys.sort();
    keys
}

fn cmp_scalar(a: &Value, b: &Value, total: bool) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            match (x.is_nan(), y.is_nan()) {
                (true, true) if total => Ordering::Equal,
                // jv_cmp(jv_null(), b): null < number
                (true, _) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => x.compare(y),
            }
        }
        (Value::String(x), Value::String(y)) => x.cmp(y),
        // null, false, true: there's only one of each of these values
        _ => Ordering::Equal,
    }
}

fn cmp_rec(a: &Value, b: &Value, total: bool, depth: u32) -> Ordering {
    let (ka, kb) = (a.kind(), b.kind());
    if ka != kb {
        return ka.cmp(&kb);
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            if depth >= MAX_RECURSION {
                return cmp_iter(a, b, total);
            }
            // Lexical ordering of arrays
            for (p, q) in x.iter().zip(y.iter()) {
                let r = cmp_rec(p, q, total, depth + 1);
                if r != Ordering::Equal {
                    return r;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            if depth >= MAX_RECURSION {
                return cmp_iter(a, b, total);
            }
            // Sorted key lists first, then the values key by key.
            let kx = sorted_keys(x);
            let r = kx.cmp(&sorted_keys(y));
            if r != Ordering::Equal {
                return r;
            }
            for k in kx {
                let r = cmp_rec(&x[k], &y[k], total, depth + 1);
                if r != Ordering::Equal {
                    return r;
                }
            }
            Ordering::Equal
        }
        _ => cmp_scalar(a, b, total),
    }
}

/// `jv_cmp` with explicit frames. In jv_cmp the first non-equal
/// sub-comparison decides the whole result, which makes this simple.
fn cmp_iter(a: &Value, b: &Value, total: bool) -> Ordering {
    enum Frame<'a> {
        Arr(&'a [Value], &'a [Value], usize),
        Obj(&'a Object, &'a Object, Vec<&'a Str>, usize),
    }
    let mut stack: Vec<Frame<'_>> = Vec::new();
    let mut cur: Option<(&Value, &Value)> = Some((a, b));
    loop {
        if let Some((a, b)) = cur.take() {
            let (ka, kb) = (a.kind(), b.kind());
            if ka != kb {
                return ka.cmp(&kb);
            }
            match (a, b) {
                (Value::Array(x), Value::Array(y)) => {
                    stack.push(Frame::Arr(x.as_slice(), y.as_slice(), 0));
                }
                (Value::Object(x), Value::Object(y)) => {
                    let kx = sorted_keys(x);
                    let r = kx.cmp(&sorted_keys(y));
                    if r != Ordering::Equal {
                        return r;
                    }
                    stack.push(Frame::Obj(x, y, kx, 0));
                }
                _ => {
                    let r = cmp_scalar(a, b, total);
                    if r != Ordering::Equal {
                        return r;
                    }
                }
            }
        }
        let Some(top) = stack.last_mut() else {
            return Ordering::Equal;
        };
        match top {
            Frame::Arr(x, y, i) => {
                let (xs, ys): (&[Value], &[Value]) = (x, y);
                if *i < xs.len() && *i < ys.len() {
                    cur = Some((&xs[*i], &ys[*i]));
                    *i += 1;
                } else {
                    let r = xs.len().cmp(&ys.len());
                    stack.pop();
                    if r != Ordering::Equal {
                        return r;
                    }
                }
            }
            Frame::Obj(x, y, keys, i) => {
                if *i < keys.len() {
                    let (xo, yo): (&Object, &Object) = (x, y);
                    let k: &Str = keys[*i];
                    cur = Some((&xo[k], &yo[k]));
                    *i += 1;
                } else {
                    stack.pop();
                }
            }
        }
    }
}

// --------------------------------------------------------------- contains

/// `jv_contains`.
pub(super) fn contains(a: &Value, b: &Value) -> bool {
    contains_rec(a, b, 0)
}

fn contains_scalar(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => {
            y.is_empty() || memchr::memmem::find(x.as_bytes(), y.as_bytes()).is_some()
        }
        _ => equal(a, b),
    }
}

fn contains_rec(a: &Value, b: &Value, depth: u32) -> bool {
    if a.kind() != b.kind() {
        return false;
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            if depth >= MAX_RECURSION {
                return contains_iter(a, b);
            }
            y.iter().all(|(k, bv)| match x.get(k) {
                Some(av) => contains_rec(av, bv, depth + 1),
                // a missing key is an invalid value: never contains anything
                None => false,
            })
        }
        (Value::Array(x), Value::Array(y)) => {
            if depth >= MAX_RECURSION {
                return contains_iter(a, b);
            }
            y.iter()
                .all(|bv| x.iter().any(|av| contains_rec(av, bv, depth + 1)))
        }
        _ => contains_scalar(a, b),
    }
}

/// `jv_contains` with explicit frames (a frame is resumed with the result
/// of the sub-question it asked).
fn contains_iter(a: &Value, b: &Value) -> bool {
    enum Frame<'a> {
        /// every (key, value) of `b` must be contained in `a[key]`
        Obj(&'a Object, &'a Object, usize),
        /// every element of `b` must be contained in some element of `a`
        Arr(&'a [Value], &'a [Value], usize, usize),
    }
    enum Step<'a> {
        Done(bool),
        Pushed(Frame<'a>),
    }
    fn start<'a>(a: &'a Value, b: &'a Value) -> Step<'a> {
        if a.kind() != b.kind() {
            return Step::Done(false);
        }
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => Step::Pushed(Frame::Obj(x, y, 0)),
            (Value::Array(x), Value::Array(y)) => {
                Step::Pushed(Frame::Arr(x.as_slice(), y.as_slice(), 0, 0))
            }
            _ => Step::Done(contains_scalar(a, b)),
        }
    }
    let mut stack: Vec<Frame<'_>> = Vec::new();
    // The result of the most recent sub-question, `None` for a new frame.
    let mut answer: Option<bool> = match start(a, b) {
        Step::Done(r) => return r,
        Step::Pushed(f) => {
            stack.push(f);
            None
        }
    };
    loop {
        let top = stack.last_mut().expect("a frame is active");
        // Either the next sub-question for this frame, or its final result.
        let next: Result<(&Value, &Value), bool> = match top {
            Frame::Obj(x, y, i) => {
                if answer == Some(false) {
                    Err(false)
                } else if *i < y.len() {
                    let (xo, yo): (&Object, &Object) = (x, y);
                    let (k, bv) = yo.get_index(*i).expect("in range");
                    *i += 1;
                    match xo.get(k) {
                        Some(av) => Ok((av, bv)),
                        None => Err(false),
                    }
                } else {
                    Err(true)
                }
            }
            Frame::Arr(x, y, bi, ai) => {
                match answer {
                    Some(true) => {
                        // found a container for y[bi]; next element of y
                        *bi += 1;
                        *ai = 0;
                    }
                    Some(false) => *ai += 1,
                    None => {}
                }
                let (xs, ys): (&[Value], &[Value]) = (x, y);
                if *bi >= ys.len() {
                    Err(true)
                } else if *ai >= xs.len() {
                    Err(false)
                } else {
                    Ok((&xs[*ai], &ys[*bi]))
                }
            }
        };
        match next {
            Ok((x, y)) => match start(x, y) {
                Step::Done(r) => answer = Some(r),
                Step::Pushed(f) => {
                    stack.push(f);
                    answer = None;
                }
            },
            Err(r) => {
                stack.pop();
                if stack.is_empty() {
                    return r;
                }
                answer = Some(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jq::value::parse_sized;

    fn jv(s: &str) -> Value {
        parse_sized(s.as_bytes()).unwrap()
    }

    /// The iterative versions must agree with the recursive ones.
    #[test]
    fn iterative_matches_recursive() {
        let vals = [
            "null",
            "1",
            "nan",
            "\"a\"",
            "[]",
            "[1]",
            "[1,2]",
            "[[1],[2,3]]",
            "[nan]",
            "{}",
            "{\"a\":1}",
            "{\"a\":[1,{\"b\":2}]}",
            "{\"b\":1,\"a\":2}",
            "{\"a\":2,\"b\":1}",
            "[\"foobar\",{\"x\":\"yz\"}]",
            "[\"bar\"]",
            "{\"a\":{\"x\":\"y\"}}",
            "[1,[2,[3]]]",
            "[1,[2,[4]]]",
            "{\"a\":null}",
            "[true,false]",
            "[false,true]",
        ];
        for a in vals {
            for b in vals {
                let (x, y) = (jv(a), jv(b));
                assert_eq!(equal_iter(&x, &y), equal_rec(&x, &y, 0), "equal {a} {b}");
                for total in [false, true] {
                    assert_eq!(
                        cmp_iter(&x, &y, total),
                        cmp_rec(&x, &y, total, 0),
                        "cmp {a} {b}"
                    );
                }
                assert_eq!(
                    contains_iter(&x, &y),
                    contains_rec(&x, &y, 0),
                    "contains {a} {b}"
                );
            }
        }
    }
}
