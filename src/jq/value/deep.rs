//! Deep structural operations — `jv_equal`, `jv_cmp`, `jv_contains` — with a
//! bounded native stack.
//!
//! jq recurses in C, and gives up past [`MAX_DEPTH`] levels: `jv_equal` and
//! `jv_contains` return -1 and `jv_cmp` `INT_MIN`, which is [`TooDeep`] here.
//! The check comes first at every level, before the kinds are compared, and
//! the first unequal (or too deep) sub-comparison in jq's order decides the
//! result, so these walk the values in exactly jq's order. Rust frames are
//! larger than C's, so these functions recurse up to [`MAX_RECURSION`]
//! levels (fast, no allocation for ordinary data) and continue with an
//! explicit stack below that.

use std::cmp::Ordering;

use super::{Object, Str, Value};

/// jq's `MAX_EQUAL_DEPTH`, `MAX_CMP_DEPTH` and `MAX_CONTAINS_DEPTH` (and
/// `MAX_OBJECT_MERGE_DEPTH`): a comparison that reaches a level deeper than
/// this stops.
pub const MAX_DEPTH: u32 = 10000;

/// A comparison that went deeper than [`MAX_DEPTH`]: `jv_equal`'s and
/// `jv_contains`' -1, `jv_cmp`'s `INT_MIN`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooDeep;

/// Levels handled by native recursion before switching to explicit stacks.
const MAX_RECURSION: u32 = 48;

// ------------------------------------------------------------------ equal

/// `jv_equal`.
pub(super) fn equal(a: &Value, b: &Value) -> Result<bool, TooDeep> {
    // QJ_JQ_COMPAT=1: jq recurses, and dies when this comparison is deeper
    // than its C stack allows.
    crate::compat::comparing_equal(a, b);
    equal_rec(a, b, 0)
}

/// `jvp_equal(a, b, depth)`.
fn equal_rec(a: &Value, b: &Value, depth: u32) -> Result<bool, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    match (a, b) {
        (Value::Null, Value::Null) => Ok(true),
        (Value::Bool(x), Value::Bool(y)) => Ok(x == y),
        // (jq's pointer fast path only applies to literals, which are never
        // NaN and so compare equal to themselves anyway.)
        (Value::Number(x), Value::Number(y)) => Ok(x.equal(y)),
        (Value::String(x), Value::String(y)) => Ok(x == y),
        (Value::Array(x), Value::Array(y)) => {
            // jv_equal's fast path: same storage and length, offsets ignored.
            if x.same_storage(y) {
                return Ok(true);
            }
            // jvp_array_equal
            if x.len() != y.len() {
                return Ok(false);
            }
            if depth >= MAX_RECURSION {
                return equal_iter(a, b, depth);
            }
            for (p, q) in x.iter().zip(y.iter()) {
                if !equal_rec(p, q, depth + 1)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.ptr_eq(y) {
                return Ok(true);
            }
            if depth >= MAX_RECURSION {
                return equal_iter(a, b, depth);
            }
            // jvp_object_equal: the sizes are compared last.
            for (k, v) in x.iter() {
                match y.get(k) {
                    Some(w) => {
                        if !equal_rec(v, w, depth + 1)? {
                            return Ok(false);
                        }
                    }
                    None => return Ok(false),
                }
            }
            Ok(x.len() == y.len())
        }
        _ => Ok(false),
    }
}

/// [`equal_rec`] with explicit frames. Any `false` or [`TooDeep`] decides
/// the whole comparison.
fn equal_iter(a: &Value, b: &Value, depth: u32) -> Result<bool, TooDeep> {
    enum Frame<'a> {
        Arr(&'a [Value], &'a [Value], usize),
        Obj(&'a Object, &'a Object, usize),
    }
    // Each frame with the depth of its elements.
    let mut stack: Vec<(Frame<'_>, u32)> = Vec::new();
    let mut cur: Option<(&Value, &Value, u32)> = Some((a, b, depth));
    loop {
        if let Some((a, b, depth)) = cur.take() {
            if depth > MAX_DEPTH {
                return Err(TooDeep);
            }
            match (a, b) {
                (Value::Array(x), Value::Array(y)) => {
                    if !x.same_storage(y) {
                        if x.len() != y.len() {
                            return Ok(false);
                        }
                        stack.push((Frame::Arr(x.as_slice(), y.as_slice(), 0), depth + 1));
                    }
                }
                (Value::Object(x), Value::Object(y)) => {
                    if !x.ptr_eq(y) {
                        stack.push((Frame::Obj(x, y, 0), depth + 1));
                    }
                }
                // Anything else is a scalar comparison (no recursion).
                _ => {
                    if !equal_rec(a, b, depth)? {
                        return Ok(false);
                    }
                }
            }
        }
        let Some((top, depth)) = stack.last_mut() else {
            return Ok(true);
        };
        let depth = *depth;
        match top {
            Frame::Arr(x, y, i) => {
                let (xs, ys): (&[Value], &[Value]) = (x, y);
                if *i < xs.len() {
                    cur = Some((&xs[*i], &ys[*i], depth));
                    *i += 1;
                } else {
                    stack.pop();
                }
            }
            Frame::Obj(x, y, i) => {
                let (xo, yo): (&Object, &Object) = (x, y);
                if let Some((k, v)) = xo.get_index(*i) {
                    match yo.get(k) {
                        Some(w) => cur = Some((v, w, depth)),
                        None => return Ok(false),
                    }
                    *i += 1;
                } else {
                    if xo.len() != yo.len() {
                        return Ok(false);
                    }
                    stack.pop();
                }
            }
        }
    }
}

// ---------------------------------------------------------------- compare

/// `jv_cmp`; with `total`, NaN == NaN (a consistent order for sorting).
pub(super) fn compare(a: &Value, b: &Value, total: bool) -> Result<Ordering, TooDeep> {
    // QJ_JQ_COMPAT=1: as in `equal`. jq's `sort_cmp` calls `jv_cmp` too, so
    // the check belongs here rather than at each caller.
    crate::compat::comparing_order(a, b);
    cmp_rec(a, b, total, 0)
}

fn sorted_keys(o: &Object) -> Vec<&Str> {
    let mut keys: Vec<&Str> = o.keys().collect();
    keys.sort();
    keys
}

/// `jvp_cmp(keys_a, keys_b, depth)` on two objects' sorted keys (`jv_keys`):
/// the arrays are at `depth` and the keys a level deeper.
fn cmp_keys(kx: &[&Str], ky: &[&Str], depth: u32) -> Result<Ordering, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    for (p, q) in kx.iter().zip(ky.iter()) {
        if depth + 1 > MAX_DEPTH {
            return Err(TooDeep);
        }
        let r = p.cmp(q);
        if r != Ordering::Equal {
            return Ok(r);
        }
    }
    Ok(kx.len().cmp(&ky.len()))
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

/// `jvp_cmp(a, b, depth)`.
fn cmp_rec(a: &Value, b: &Value, total: bool, depth: u32) -> Result<Ordering, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    let (ka, kb) = (a.kind(), b.kind());
    if ka != kb {
        return Ok(ka.cmp(&kb));
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            if depth >= MAX_RECURSION {
                return cmp_iter(a, b, total, depth);
            }
            // Lexical ordering of arrays
            for (p, q) in x.iter().zip(y.iter()) {
                let r = cmp_rec(p, q, total, depth + 1)?;
                if r != Ordering::Equal {
                    return Ok(r);
                }
            }
            Ok(x.len().cmp(&y.len()))
        }
        (Value::Object(x), Value::Object(y)) => {
            if depth >= MAX_RECURSION {
                return cmp_iter(a, b, total, depth);
            }
            // Sorted key lists first, then the values key by key.
            let kx = sorted_keys(x);
            let r = cmp_keys(&kx, &sorted_keys(y), depth + 1)?;
            if r != Ordering::Equal {
                return Ok(r);
            }
            for k in kx {
                let r = cmp_rec(&x[k], &y[k], total, depth + 1)?;
                if r != Ordering::Equal {
                    return Ok(r);
                }
            }
            Ok(Ordering::Equal)
        }
        _ => Ok(cmp_scalar(a, b, total)),
    }
}

/// `jv_cmp` with explicit frames. In jv_cmp the first non-equal (or too
/// deep) sub-comparison decides the whole result, which makes this simple.
fn cmp_iter(a: &Value, b: &Value, total: bool, depth: u32) -> Result<Ordering, TooDeep> {
    enum Frame<'a> {
        Arr(&'a [Value], &'a [Value], usize),
        Obj(&'a Object, &'a Object, Vec<&'a Str>, usize),
    }
    // Each frame with the depth of its elements.
    let mut stack: Vec<(Frame<'_>, u32)> = Vec::new();
    let mut cur: Option<(&Value, &Value, u32)> = Some((a, b, depth));
    loop {
        if let Some((a, b, depth)) = cur.take() {
            if depth > MAX_DEPTH {
                return Err(TooDeep);
            }
            let (ka, kb) = (a.kind(), b.kind());
            if ka != kb {
                return Ok(ka.cmp(&kb));
            }
            match (a, b) {
                (Value::Array(x), Value::Array(y)) => {
                    stack.push((Frame::Arr(x.as_slice(), y.as_slice(), 0), depth + 1));
                }
                (Value::Object(x), Value::Object(y)) => {
                    let kx = sorted_keys(x);
                    let r = cmp_keys(&kx, &sorted_keys(y), depth + 1)?;
                    if r != Ordering::Equal {
                        return Ok(r);
                    }
                    stack.push((Frame::Obj(x, y, kx, 0), depth + 1));
                }
                _ => {
                    let r = cmp_scalar(a, b, total);
                    if r != Ordering::Equal {
                        return Ok(r);
                    }
                }
            }
        }
        let Some((top, depth)) = stack.last_mut() else {
            return Ok(Ordering::Equal);
        };
        let depth = *depth;
        match top {
            Frame::Arr(x, y, i) => {
                let (xs, ys): (&[Value], &[Value]) = (x, y);
                if *i < xs.len() && *i < ys.len() {
                    cur = Some((&xs[*i], &ys[*i], depth));
                    *i += 1;
                } else {
                    let r = xs.len().cmp(&ys.len());
                    stack.pop();
                    if r != Ordering::Equal {
                        return Ok(r);
                    }
                }
            }
            Frame::Obj(x, y, keys, i) => {
                if *i < keys.len() {
                    let (xo, yo): (&Object, &Object) = (x, y);
                    let k: &Str = keys[*i];
                    cur = Some((&xo[k], &yo[k], depth));
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
pub(super) fn contains(a: &Value, b: &Value) -> Result<bool, TooDeep> {
    // QJ_JQ_COMPAT=1: as in `equal`, with jq's search over the elements of an
    // array (see `src/compat/depth.rs`).
    crate::compat::containing(a, b);
    contains_rec(a, b, 0)
}

fn contains_scalar(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => {
            y.is_empty() || memchr::memmem::find(x.as_bytes(), y.as_bytes()).is_some()
        }
        // jv_equal at depth 0 on scalars: never too deep.
        _ => equal(a, b).unwrap_or(true),
    }
}

/// `jvp_contains(a_val, b_val, depth)` where `a_val` is the invalid value
/// `jv_object_get` gives for a key `a` doesn't have: its kind differs.
fn contains_missing(depth: u32) -> Result<bool, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    Ok(false)
}

/// `jvp_contains(a, b, depth)`.
fn contains_rec(a: &Value, b: &Value, depth: u32) -> Result<bool, TooDeep> {
    if depth > MAX_DEPTH {
        return Err(TooDeep);
    }
    if a.kind() != b.kind() {
        return Ok(false);
    }
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            if depth >= MAX_RECURSION {
                return contains_iter(a, b, depth);
            }
            for (k, bv) in y.iter() {
                let r = match x.get(k) {
                    Some(av) => contains_rec(av, bv, depth + 1)?,
                    None => contains_missing(depth + 1)?,
                };
                if !r {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::Array(x), Value::Array(y)) => {
            if depth >= MAX_RECURSION {
                return contains_iter(a, b, depth);
            }
            for bv in y.iter() {
                let mut found = false;
                for av in x.iter() {
                    if contains_rec(av, bv, depth + 1)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(contains_scalar(a, b)),
    }
}

/// `jv_contains` with explicit frames (a frame is resumed with the result
/// of the sub-question it asked). [`TooDeep`] decides the whole question.
fn contains_iter(a: &Value, b: &Value, depth: u32) -> Result<bool, TooDeep> {
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
    fn start<'a>(a: &'a Value, b: &'a Value, depth: u32) -> Result<Step<'a>, TooDeep> {
        if depth > MAX_DEPTH {
            return Err(TooDeep);
        }
        if a.kind() != b.kind() {
            return Ok(Step::Done(false));
        }
        Ok(match (a, b) {
            (Value::Object(x), Value::Object(y)) => Step::Pushed(Frame::Obj(x, y, 0)),
            (Value::Array(x), Value::Array(y)) => {
                Step::Pushed(Frame::Arr(x.as_slice(), y.as_slice(), 0, 0))
            }
            _ => Step::Done(contains_scalar(a, b)),
        })
    }
    // Each frame with the depth of its elements.
    let mut stack: Vec<(Frame<'_>, u32)> = Vec::new();
    // The result of the most recent sub-question, `None` for a new frame.
    let mut answer: Option<bool> = match start(a, b, depth)? {
        Step::Done(r) => return Ok(r),
        Step::Pushed(f) => {
            stack.push((f, depth + 1));
            None
        }
    };
    loop {
        let (top, depth) = stack.last_mut().expect("a frame is active");
        let depth = *depth;
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
                        None => Err(contains_missing(depth)?),
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
            Ok((x, y)) => match start(x, y, depth)? {
                Step::Done(r) => answer = Some(r),
                Step::Pushed(f) => {
                    stack.push((f, depth + 1));
                    answer = None;
                }
            },
            Err(r) => {
                stack.pop();
                if stack.is_empty() {
                    return Ok(r);
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
                for depth in [0, MAX_DEPTH - 2, MAX_DEPTH - 1, MAX_DEPTH, MAX_DEPTH + 1] {
                    assert_eq!(
                        equal_iter(&x, &y, depth),
                        equal_rec(&x, &y, depth),
                        "equal {a} {b} at {depth}"
                    );
                    for total in [false, true] {
                        assert_eq!(
                            cmp_iter(&x, &y, total, depth),
                            cmp_rec(&x, &y, total, depth),
                            "cmp {a} {b} at {depth}"
                        );
                    }
                    assert_eq!(
                        contains_iter(&x, &y, depth),
                        contains_rec(&x, &y, depth),
                        "contains {a} {b} at {depth}"
                    );
                }
            }
        }
    }

    /// `n` arrays around `inner`.
    fn nest(n: usize, inner: &str) -> Value {
        let mut v = jv(inner);
        for _ in 0..n {
            v = Value::Array([v].into_iter().collect());
        }
        v
    }

    fn arr(items: [Value; 2]) -> Value {
        Value::Array(items.into_iter().collect())
    }

    fn obj(items: &[(&str, Value)]) -> Value {
        let mut o = Object::new();
        for (k, v) in items {
            o.insert(Str::from(*k), v.clone());
        }
        Value::Object(o)
    }

    /// jq 1.8.2 checks the depth before anything else, and the first
    /// sub-comparison that is unequal or too deep decides.
    #[test]
    fn depth_limits() {
        let deep = |n| nest(n, "1");
        // A value nested 10000 levels has its innermost element at depth
        // 10000, which is still compared.
        assert_eq!(equal(&deep(10000), &deep(10000)), Ok(true));
        assert_eq!(equal(&deep(10001), &deep(10001)), Err(TooDeep));
        assert_eq!(
            compare(&deep(10000), &deep(10000), false),
            Ok(Ordering::Equal)
        );
        assert_eq!(compare(&deep(10001), &deep(10001), false), Err(TooDeep));
        assert_eq!(contains(&deep(10000), &deep(10000)), Ok(true));
        assert_eq!(contains(&deep(10001), &deep(10001)), Err(TooDeep));
        // Kinds are compared after the depth: still too deep.
        assert_eq!(
            equal(&nest(10001, "1"), &nest(10001, "\"a\"")),
            Err(TooDeep)
        );
        // An earlier difference decides first; a later one doesn't.
        let a = arr([jv("0"), deep(10001)]);
        let b = arr([jv("1"), deep(10001)]);
        assert_eq!(equal(&a, &b), Ok(false));
        assert_eq!(compare(&a, &b, false), Ok(Ordering::Less));
        let a = arr([deep(10001), jv("0")]);
        let b = arr([deep(10001), jv("1")]);
        assert_eq!(equal(&a, &b), Err(TooDeep));
        assert_eq!(compare(&a, &b, false), Err(TooDeep));
        // Objects compare their sizes last in jv_equal.
        let a = obj(&[("a", deep(10001))]);
        let b = obj(&[("a", deep(10001)), ("b", jv("1"))]);
        assert_eq!(equal(&a, &b), Err(TooDeep));
        assert_eq!(equal(&b, &a), Err(TooDeep));
        // A key `a` doesn't have is as deep as the values.
        assert_eq!(contains(&obj(&[]), &obj(&[("b", jv("1"))])), Ok(false));
        assert_eq!(
            contains(&nest(10000, "{}"), &nest(10000, "{\"b\":1}")),
            Err(TooDeep)
        );
        assert_eq!(
            contains(&nest(9999, "{}"), &nest(9999, "{\"b\":1}")),
            Ok(false)
        );
        // jv_cmp compares the key lists a level deeper: two empty objects
        // at depth 10000 are already too deep.
        assert_eq!(
            compare(&nest(10000, "{}"), &nest(10000, "{}"), false),
            Err(TooDeep)
        );
        assert_eq!(
            compare(&nest(9999, "{}"), &nest(9999, "{}"), false),
            Ok(Ordering::Equal)
        );
        assert_eq!(equal(&nest(10000, "{}"), &nest(10000, "{}")), Ok(true));
    }
}
