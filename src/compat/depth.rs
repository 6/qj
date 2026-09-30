//! How deep jq 1.8.1's recursive value operations go, following jq's own
//! traversal — for [`crate::compat`], which raises `SIGSEGV` where that
//! depth would overflow jq's C stack.
//!
//! Only the depth jq *reaches* counts, so each walk mirrors the C exactly:
//! `jv_equal`'s shortcut for values that share an allocation, the early
//! exits at the first difference, object slot order, `jv_cmp` comparing the
//! sorted key arrays before the values, `jv_contains` searching the elements
//! of an array, and `jv_object_merge_recursive` descending only where both
//! sides are objects.
//!
//! Every walk is iterative (jq's values can be nested far deeper than qj's
//! own stack allows), and every walk takes a `cap`: it stops as soon as the
//! depth passes it, because the caller only needs to know whether jq's stack
//! would have run out.
//!
//! Depth is counted in jq's frames as the measurements in
//! [`crate::compat`] count them: 1 for the outermost call, and one more per
//! level of nesting it descends. Where a level costs jq two C frames (the
//! `jv_equal`/`jvp_array_equal` pair, say) both are in the measured bytes a
//! level, so they are one level here.

use std::cmp::Ordering;

use crate::jq::value::{Object, Str, Value};

/// One step of a traversal: a pair of values to visit at `depth`, or the
/// point at which jq's frame answers "not equal" / "not contained" /
/// nonzero and the whole traversal unwinds without visiting anything else.
enum Step<'a> {
    Pair(&'a Value, &'a Value, u64),
    Stop,
}

/// What one node of a traversal does: the pairs it visits one level down,
/// in order, and whether it answers (and so unwinds the traversal) once
/// they are done.
struct Node<'a> {
    children: Vec<(&'a Value, &'a Value)>,
    stops: bool,
}

impl<'a> Node<'a> {
    /// jq's frame answers right away, without recursing: `stops` says
    /// whether that answer unwinds the traversal.
    fn leaf(stops: bool) -> Node<'a> {
        Node {
            children: Vec::new(),
            stops,
        }
    }
}

/// Runs a traversal whose nodes are described by `node`, from the pair
/// `(a, b)`, and returns the deepest frame it reaches — at most `cap + 1`,
/// where it stops early.
fn walk<'a, F>(a: &'a Value, b: &'a Value, cap: u64, mut node: F) -> u64
where
    F: FnMut(&'a Value, &'a Value, u64, &mut u64) -> Node<'a>,
{
    let mut work = vec![Step::Pair(a, b, 1)];
    let mut deepest = 0;
    while let Some(step) = work.pop() {
        // A `Stop` is jq's frame returning its answer: nothing else is
        // visited.
        let Step::Pair(x, y, d) = step else { break };
        deepest = deepest.max(d);
        if deepest > cap {
            return deepest;
        }
        let n = node(x, y, d, &mut deepest);
        if deepest > cap {
            return deepest;
        }
        if n.stops {
            work.push(Step::Stop);
        }
        for (u, v) in n.children.into_iter().rev() {
            work.push(Step::Pair(u, v, d + 1));
        }
    }
    deepest
}

// ------------------------------------------------------------------ equal

/// The deepest `jv_equal` frame comparing `a` and `b`, capped at `cap + 1`.
pub(super) fn equal(a: &Value, b: &Value, cap: u64) -> u64 {
    walk(a, b, cap, |x, y, _d, _deepest| match (x, y) {
        (Value::Array(p), Value::Array(q)) => {
            // jv_equal's shortcut: the same allocation with the same length
            // is equal without looking inside. (jq compares kind, size and
            // pointer there, and not a slice's offset, so two different
            // slices of one array of the same length take it too.)
            if p.same_storage(q) {
                return Node::leaf(false);
            }
            // jvp_array_equal: arrays of different lengths are unequal,
            // without comparing any element.
            if p.len() != q.len() {
                return Node::leaf(true);
            }
            Node {
                children: p.iter().zip(q.iter()).collect(),
                stops: false,
            }
        }
        (Value::Object(p), Value::Object(q)) => {
            if p.ptr_eq(q) {
                return Node::leaf(false);
            }
            // jvp_object_equal walks o1's slots in order, looks each key up
            // in o2 and compares the values; a key o2 doesn't have stops it
            // there, and it compares the key *counts* only at the end — so a
            // different number of keys does not keep it from comparing the
            // values of the keys they share.
            let mut children = Vec::with_capacity(p.len());
            let mut stops = p.len() != q.len();
            for (k, v) in p.iter() {
                match q.get_str(k) {
                    Some(w) => children.push((v, w)),
                    None => {
                        stops = true;
                        break;
                    }
                }
            }
            Node { children, stops }
        }
        (Value::Null, Value::Null) => Node::leaf(false),
        (Value::Bool(p), Value::Bool(q)) => Node::leaf(p != q),
        (Value::Number(p), Value::Number(q)) => Node::leaf(!p.equal(q)),
        (Value::String(p), Value::String(q)) => Node::leaf(p != q),
        // Different kinds: not equal.
        _ => Node::leaf(true),
    })
}

// ---------------------------------------------------------------- compare

/// An object's keys in `jv_keys` order (jq sorts them with `string_cmp`,
/// which is `Str`'s order).
fn sorted_keys(o: &Object) -> Vec<&Str> {
    let mut keys: Vec<&Str> = o.keys().collect();
    keys.sort();
    keys
}

/// The deepest `jv_cmp` frame comparing `a` and `b`, capped at `cap + 1`.
pub(super) fn compare(a: &Value, b: &Value, cap: u64) -> u64 {
    walk(a, b, cap, |x, y, d, deepest| {
        if x.kind() != y.kind() {
            // Different kinds: the kinds decide, nothing is compared.
            return Node::leaf(true);
        }
        match (x, y) {
            (Value::Array(p), Value::Array(q)) => {
                // Lexical ordering: the elements up to the shorter length
                // are compared, and only then does the length decide.
                Node {
                    children: p.iter().zip(q.iter()).collect(),
                    stops: p.len() != q.len(),
                }
            }
            (Value::Object(p), Value::Object(q)) => {
                // jv_cmp compares the sorted key arrays first, with a
                // jv_cmp of its own: one frame for the array, one for the
                // strings in it.
                let kp = sorted_keys(p);
                let kq = sorted_keys(q);
                *deepest = (*deepest).max(if kp.is_empty() || kq.is_empty() {
                    d + 1
                } else {
                    d + 2
                });
                if kp != kq {
                    return Node::leaf(true);
                }
                // Then the values, in sorted key order.
                Node {
                    children: kp.iter().map(|k| (&p[*k], &q[*k])).collect(),
                    stops: false,
                }
            }
            (Value::Number(p), Value::Number(q)) => {
                // A NaN is compared as null, in a jv_cmp of its own, and
                // that answer is never 0 (null is below every number).
                if p.is_nan() || q.is_nan() {
                    *deepest = (*deepest).max(d + 1);
                    return Node::leaf(true);
                }
                Node::leaf(p.compare(q) != Ordering::Equal)
            }
            (Value::String(p), Value::String(q)) => Node::leaf(p != q),
            // null, false and true: there is only one of each.
            _ => Node::leaf(false),
        }
    })
}

// --------------------------------------------------------------- contains

/// The deepest `jv_contains` frame asking whether `a` contains `b`, capped
/// at `cap + 1`.
///
/// `jv_contains` is a search, not a comparison: a false answer makes
/// `jvp_array_contains` try the next element of `a` rather than unwind, so
/// this walk keeps jq's frames explicitly instead of using [`walk`].
pub(super) fn contains(a: &Value, b: &Value, cap: u64) -> u64 {
    enum Frame<'a> {
        /// Every key of `b` must be contained in `a`'s value for that key,
        /// in `b`'s slot order; the first one that isn't ends the frame.
        Obj(&'a Object, &'a Object, usize),
        /// Every element of `b` must be contained in some element of `a`:
        /// `b[bi]` is being tried against `a[ai]`.
        Arr(&'a [Value], &'a [Value], usize, usize),
    }
    /// A pair's own frame: `Ask` recurses, `Done` answers it right away.
    enum Step<'a> {
        Done(bool),
        Ask(Frame<'a>),
    }
    fn start<'a>(a: &'a Value, b: &'a Value) -> Step<'a> {
        if a.kind() != b.kind() {
            return Step::Done(false);
        }
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => Step::Ask(Frame::Obj(x, y, 0)),
            (Value::Array(x), Value::Array(y)) => {
                Step::Ask(Frame::Arr(x.as_slice(), y.as_slice(), 0, 0))
            }
            // A string is searched for with memmem, and anything else goes
            // to jv_equal, which does not recurse for scalars.
            _ => Step::Done(true),
        }
    }
    let mut deepest = 1;
    let mut stack: Vec<Frame<'_>> = match start(a, b) {
        Step::Done(_) => return deepest,
        Step::Ask(f) => vec![f],
    };
    // The answer to the sub-question just asked, `None` for a fresh frame.
    let mut answer: Option<bool> = None;
    loop {
        let d = stack.len() as u64;
        deepest = deepest.max(d);
        if deepest > cap {
            return deepest;
        }
        let top = stack.last_mut().expect("a frame is active");
        // The frame's next sub-question, or its own answer.
        let next: Result<(&Value, &Value), bool> = match top {
            Frame::Obj(x, y, i) => {
                if answer == Some(false) {
                    Err(false)
                } else if *i < y.len() {
                    let (xo, yo): (&Object, &Object) = (x, y);
                    let (k, bv) = yo.get_index(*i).expect("in range");
                    *i += 1;
                    match xo.get_str(k) {
                        Some(av) => Ok((av, bv)),
                        // jv_object_get gives an invalid value, which
                        // contains nothing: one frame, then false.
                        None => {
                            deepest = deepest.max(d + 1);
                            Err(false)
                        }
                    }
                } else {
                    Err(true)
                }
            }
            Frame::Arr(x, y, bi, ai) => {
                match answer {
                    // a[ai] contains b[bi]: on to the next element of b
                    Some(true) => {
                        *bi += 1;
                        *ai = 0;
                    }
                    // it doesn't: try the next element of a
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
                Step::Done(r) => {
                    deepest = deepest.max(d + 1);
                    answer = Some(r);
                }
                Step::Ask(f) => {
                    stack.push(f);
                    answer = None;
                }
            },
            Err(r) => {
                stack.pop();
                if stack.is_empty() {
                    return deepest;
                }
                answer = Some(r);
            }
        }
    }
}

// ------------------------------------------------------------------ merge

/// The deepest `jv_object_merge_recursive` frame merging `b` into `a`,
/// capped at `cap + 1`.
///
/// jq descends only where the key holds an object on both sides, and
/// nothing stops it early, so the order of the walk cannot change the
/// answer.
pub(super) fn merge(a: &Object, b: &Object, cap: u64) -> u64 {
    let mut work: Vec<(&Object, &Object, u64)> = vec![(a, b, 1)];
    let mut deepest = 0;
    while let Some((x, y, d)) = work.pop() {
        deepest = deepest.max(d);
        if deepest > cap {
            return deepest;
        }
        for (k, v) in y.iter() {
            if let (Some(Value::Object(p)), Value::Object(q)) = (x.get_str(k), v) {
                work.push((p, q, d + 1));
            }
        }
    }
    deepest
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jq::value::parse_sized;

    fn jv(text: &str) -> Value {
        parse_sized(text.as_bytes()).expect("valid JSON")
    }

    const CAP: u64 = 1_000_000;

    fn eq(a: &str, b: &str) -> u64 {
        equal(&jv(a), &jv(b), CAP)
    }

    fn cmp(a: &str, b: &str) -> u64 {
        compare(&jv(a), &jv(b), CAP)
    }

    fn has(a: &str, b: &str) -> u64 {
        contains(&jv(a), &jv(b), CAP)
    }

    fn mg(a: &str, b: &str) -> u64 {
        let (Value::Object(x), Value::Object(y)) = (jv(a), jv(b)) else {
            panic!("objects");
        };
        merge(&x, &y, CAP)
    }

    /// `n` arrays (or objects) nested around a `null`, as a program would
    /// build it — jq's JSON parser stops at 10,000 levels.
    fn nest(n: usize, obj: bool) -> Value {
        use crate::jq::value::{Array, Object, Str};
        let mut v = Value::Null;
        for _ in 0..n {
            v = if obj {
                Value::Object(Object::from_iter([(Str::from("a"), v)]))
            } else {
                Value::Array(Array::from_vec(vec![v]))
            };
        }
        v
    }

    #[test]
    fn equal_counts_the_frames_jq_uses() {
        assert_eq!(eq("1", "1"), 1);
        assert_eq!(eq("1", "2"), 1);
        assert_eq!(eq("1", "\"a\""), 1);
        assert_eq!(eq("[]", "[]"), 1);
        assert_eq!(eq("[1]", "[1]"), 2);
        assert_eq!(eq("[[1]]", "[[1]]"), 3);
        // Different lengths stop before the elements are looked at.
        assert_eq!(eq("[[1]]", "[[1],[2]]"), 1);
        // The first unequal element stops the walk, but the ones before it
        // were compared.
        assert_eq!(eq("[[[1]],2]", "[[[1]],3]"), 4);
        assert_eq!(eq("[2,[[1]]]", "[3,[[1]]]"), 2);
        assert_eq!(equal(&nest(5000, false), &nest(5000, false), CAP), 5001);
        assert_eq!(equal(&nest(5000, true), &nest(5000, true), CAP), 5001);
    }

    #[test]
    fn equal_takes_jqs_shortcut_for_a_shared_allocation() {
        use crate::jq::value::Array;
        let deep = nest(5000, false);
        // The same value on both sides: jv_equal answers from the pointer.
        assert_eq!(equal(&deep, &deep, CAP), 1);
        // A copy of the outer array is the same allocation, so it answers
        // from the pointer too.
        let Value::Array(a) = &deep else { panic!() };
        let one = Value::Array(a.clone());
        assert_eq!(equal(&deep, &one, CAP), 1);
        // A new array around the same element shares only the element: the
        // walk stops one level down.
        let inner = a.get(0).expect("one element").clone();
        let wrapped = Value::Array(Array::from_vec(vec![inner]));
        assert_eq!(equal(&deep, &wrapped, CAP), 2);
    }

    #[test]
    fn equal_compares_shared_keys_before_counting_them() {
        // jq compares the "a" values (3 frames) and only then notices that
        // the objects have different numbers of keys.
        assert_eq!(eq("{\"a\":[[1]]}", "{\"a\":[[1]],\"b\":1}"), 4);
        // A key the other object doesn't have stops it at that slot, after
        // the slots before it were compared...
        assert_eq!(eq("{\"a\":[[1]],\"z\":1}", "{\"a\":[[1]],\"b\":1}"), 4);
        // ... and before the ones after it were.
        assert_eq!(eq("{\"z\":1,\"a\":[[1]]}", "{\"a\":[[1]],\"b\":1}"), 1);
    }

    #[test]
    fn compare_counts_the_key_arrays() {
        assert_eq!(cmp("1", "1"), 1);
        assert_eq!(cmp("1", "\"a\""), 1);
        assert_eq!(cmp("[1]", "[1]"), 2);
        assert_eq!(cmp("[[1]]", "[[1]]"), 3);
        // The elements up to the shorter length are compared first.
        assert_eq!(cmp("[[1]]", "[[1],2]"), 3);
        // An object compares its sorted key arrays: one frame for the
        // array, one for the strings.
        assert_eq!(cmp("{}", "{}"), 2);
        assert_eq!(cmp("{\"a\":1}", "{\"a\":1}"), 3);
        assert_eq!(cmp("{\"a\":1}", "{\"b\":1}"), 3);
        // ... which is why an object nests one frame deeper than an array.
        assert_eq!(compare(&nest(5000, false), &nest(5000, false), CAP), 5001);
        assert_eq!(compare(&nest(5000, true), &nest(5000, true), CAP), 5002);
        // A NaN is compared as null, one frame down.
        assert_eq!(cmp("nan", "1"), 2);
        assert_eq!(cmp("[nan]", "[1]"), 3);
    }

    #[test]
    fn contains_searches_every_element_it_tries() {
        assert_eq!(has("1", "1"), 1);
        assert_eq!(has("\"abc\"", "\"b\""), 1);
        assert_eq!(has("[1]", "[1]"), 2);
        assert_eq!(has("[[1]]", "[[1]]"), 3);
        // b's element is tried against every element of a until one contains
        // it, and the attempt that fails counts as much as the one that
        // works: here `[[9]]` is searched for `[1]` before `[1]` is.
        assert_eq!(has("[[[9]],[1]]", "[[1]]"), 3);
        assert_eq!(has("[[9],[[1]]]", "[[[1]]]"), 4);
        assert_eq!(has("{\"a\":[[1]]}", "{\"a\":[[1]]}"), 4);
        // A key a doesn't have: one frame for the invalid value.
        assert_eq!(has("{\"a\":1}", "{\"b\":1}"), 2);
        assert_eq!(contains(&nest(5000, false), &nest(5000, false), CAP), 5001);
    }

    #[test]
    fn merge_descends_only_where_both_are_objects() {
        assert_eq!(mg("{}", "{}"), 1);
        assert_eq!(mg("{\"a\":1}", "{\"a\":1}"), 1);
        assert_eq!(mg("{\"a\":{}}", "{\"a\":{}}"), 2);
        // Only an object on both sides is merged.
        assert_eq!(mg("{\"a\":{\"b\":{}}}", "{\"a\":1}"), 1);
        assert_eq!(mg("{\"a\":1}", "{\"a\":{\"b\":{}}}"), 1);
        let (Value::Object(x), Value::Object(y)) = (nest(5000, true), nest(5000, true)) else {
            panic!("objects");
        };
        assert_eq!(merge(&x, &y, CAP), 5000);
    }

    /// Every walk stops as soon as it passes the cap, and says so — so a
    /// value far deeper than the cap costs no more than one just past it.
    #[test]
    fn the_cap_stops_the_walk() {
        let deep = nest(100_000, false);
        let other = nest(100_000, false);
        for got in [
            equal(&deep, &other, 10),
            compare(&deep, &other, 10),
            contains(&deep, &other, 10),
        ] {
            assert_eq!(got, 11);
        }
        let (Value::Object(x), Value::Object(y)) = (nest(100_000, true), nest(100_000, true))
        else {
            panic!("objects");
        };
        assert_eq!(merge(&x, &y, 10), 11);
    }
}
