//! `paths`, `paths(f)` and `tostream`.
//!
//! ```jq
//! def recurse: recurse(.[]?);          # def recurse(f): def r: ., (f | r); r;
//! def paths: path(recurse)|select(length > 0);
//! def paths(node_filter): path(recurse|select(node_filter))|select(length > 0);
//! def tostream:
//!   path(def r: (.[]?|r), .; r) as $p |
//!   getpath($p) |
//!   reduce path(.[]?) as $q ([$p, .]; [$p+$q]);
//! ```
//!
//! The paths these produce are `jq->path` itself, the array the VM builds while
//! tracking a path expression, and its storage is observable (a path yielded while
//! its view is shorter than the storage brings the stale keys back when written past
//! its end). So the natives perform the VM's operations on that array in the same
//! order: `path_append` when `.[]?` enters a child, and, at every fork point that
//! backtracking restores, a slice to the length the fork point saved, which is a
//! fresh `[]` when that length is 0. The fork points are those of the definitions:
//! `r`'s `,` (one per node, restored when its second branch runs) and `.[]?` (one per
//! container with more than one child, restored for each next child).

use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstView, Outcome, PathState, Resume, Sub};
use crate::jq::value::{Array, Value};

/// A container being iterated by `.[]?`: child `i` of `n` is being visited.
struct Level {
    node: Value,
    i: usize,
    n: usize,
}

/// How many children `.[]?` gives `v`.
fn children(v: &Value) -> usize {
    match v {
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
        _ => 0,
    }
}

/// `.[]?`'s `i`th output on `v`: the path component (`EACH`'s key) and the child.
fn child(v: &Value, i: usize) -> (Value, Value) {
    match v {
        Value::Array(a) => (Value::number(i as f64), a.as_slice()[i].clone()),
        Value::Object(o) => {
            let (k, c) = o.get_index(i).expect("child");
            (Value::String(k.clone()), c.clone())
        }
        _ => unreachable!("child of a scalar"),
    }
}

/// `jq->path = jv_array_slice(jq->path, 0, len)`, as `stack_restore` does.
fn restore(p: &mut Array, len: usize) {
    *p = std::mem::take(p).into_slice(0, len as i64);
}

/// The traversal state shared by the three: `jq->path` and the containers on it.
///
/// Containers are held as long as jq's fork points hold them, since that decides
/// whether a caller can update them in place: `.[]?`'s fork point holds its container
/// until it enters the last child, and in `tostream`'s `r` the fork point of `,` holds
/// each node until its second branch (after its children), so `hold_last` is set there.
struct Walker {
    p: Array,
    stack: Vec<Level>,
    hold_last: bool,
}

impl Walker {
    /// `path(...)` starts with a fresh `[]` (`PATH_BEGIN`).
    fn new(hold_last: bool) -> Walker {
        Walker {
            p: Array::new(),
            stack: Vec::new(),
            hold_last,
        }
    }

    /// `.[]?` enters `node`'s first child (which must exist): its fork point (if there
    /// are more children) and `path_append`. Returns the child.
    fn enter(&mut self, node: Value) -> Value {
        let n = children(&node);
        let (k, c) = child(&node, 0);
        let node = if n == 1 && !self.hold_last {
            Value::Null
        } else {
            node
        };
        self.stack.push(Level { node, i: 0, n });
        self.p.push(k);
        c
    }

    /// Backtracking from a node without (more) children to the next `.[]?` fork point:
    /// restores its length and enters the next child. `None` when no container has
    /// children left.
    fn next_sibling(&mut self) -> Option<Value> {
        let hold_last = self.hold_last;
        loop {
            let top = self.stack.last_mut()?;
            if top.i + 1 < top.n {
                top.i += 1;
                let (k, c) = child(&top.node, top.i);
                if top.i + 1 == top.n && !hold_last {
                    top.node = Value::Null;
                }
                let d = self.stack.len() - 1;
                restore(&mut self.p, d);
                self.p.push(k);
                return Some(c);
            }
            self.stack.pop();
        }
    }

    /// The path of the current node, as `PATH_END` yields it.
    fn path(&self) -> Value {
        Value::Array(self.p.clone())
    }
}

// ---- paths ------------------------------------------------------------------------

/// `paths`: the root's path `[]` is dropped by `select(length > 0)`, and the fork point
/// of the root's `,` restores length 0 (a fresh `[]`) before `.[]?` runs on it.
pub(super) fn paths0(input: Value) -> Outcome {
    let mut w = Walker::new(false);
    restore(&mut w.p, 0);
    if children(&input) == 0 {
        return Outcome::Empty;
    }
    let node = w.enter(input);
    let v = w.path();
    Outcome::Yield(v, Box::new(Paths0 { w, node }))
}

struct Paths0 {
    w: Walker,
    /// The node whose path was yielded last.
    node: Value,
}

impl Resume for Paths0 {
    fn resume(mut self: Box<Self>, _vm: &mut Jq) -> Outcome {
        // `r`'s fork point for this node restores its length (a no-op: it isn't the
        // root), then `.[]?` enters its first child, or backtracks to the next sibling.
        let node = std::mem::take(&mut self.node);
        let next = if children(&node) > 0 {
            Some(self.w.enter(node))
        } else {
            drop(node);
            self.w.next_sibling()
        };
        match next {
            Some(n) => {
                self.node = n;
                let v = self.w.path();
                Outcome::Yield(v, self)
            }
            None => Outcome::Empty,
        }
    }
}

// ---- paths(f) -----------------------------------------------------------------------

/// `paths(node_filter)`: `select(node_filter)` runs on every node (the root included)
/// inside the path expression, so `node_filter` sees `jq->path` as the node's path,
/// `jq->value_at_path` as the node, and `jq->subexp_nest` 1 (select's condition is a
/// subexpression): those registers are swapped in whenever it runs. Each truthy output
/// yields the path, except the root's (dropped by `select(length > 0)`). The sub-run's
/// base fork point is saved with the node's path length, so it is the fork point of
/// `r`'s `,`, restored once `node_filter` is exhausted.
pub(super) fn paths1(vm: &mut Jq, input: Value, f: Closure) -> Outcome {
    let g = Box::new(Paths1 {
        w: Walker::new(false),
        node: input,
        f,
        sub: None,
    });
    g.filter(vm, true)
}

struct Paths1 {
    w: Walker,
    /// The node `node_filter` runs on.
    node: Value,
    f: Closure,
    /// `node_filter`'s suspended run on `node`.
    sub: Option<Sub>,
}

impl Paths1 {
    /// Swaps the path registers in for `node_filter`, returning the outer ones.
    fn enter_filter(&mut self, vm: &mut Jq) -> PathState {
        vm.swap_path_state(PathState {
            path: Value::Array(std::mem::take(&mut self.w.p)),
            value_at_path: self.node.clone(),
            subexp_nest: 1,
        })
    }

    /// Swaps the outer registers back, keeping `jq->path` (fork points restored in
    /// `node_filter` may have replaced it).
    fn leave_filter(&mut self, vm: &mut Jq, outer: PathState) {
        match vm.swap_path_state(outer).path {
            Value::Array(a) => self.w.p = a,
            _ => unreachable!("jq->path in a path expression"),
        }
    }

    /// Runs `node_filter` (from the start on `node`, or resuming it) until it yields a
    /// truthy value for a non-root node, then moves on through the traversal.
    fn filter(mut self: Box<Self>, vm: &mut Jq, mut start: bool) -> Outcome {
        loop {
            let outer = self.enter_filter(vm);
            let r = if start {
                vm.sub_start(self.f, self.node.clone())
            } else {
                vm.sub_next(self.sub.take().expect("suspended node_filter"))
            };
            self.leave_filter(vm, outer);
            match r {
                Err(s) => return s.into(),
                Ok(Some((sub, c))) => {
                    self.sub = Some(sub);
                    start = false;
                    if c.is_truthy() && !self.w.stack.is_empty() {
                        let v = self.w.path();
                        return Outcome::Yield(v, self);
                    }
                }
                Ok(None) => {
                    // `.[]?` on the node: its first child, or the next sibling.
                    let node = std::mem::take(&mut self.node);
                    let next = if children(&node) > 0 {
                        Some(self.w.enter(node))
                    } else {
                        drop(node);
                        self.w.next_sibling()
                    };
                    match next {
                        Some(n) => {
                            self.node = n;
                            start = true;
                        }
                        None => return Outcome::Empty,
                    }
                }
            }
        }
    }
}

impl Resume for Paths1 {
    fn resume(self: Box<Self>, vm: &mut Jq) -> Outcome {
        self.filter(vm, false)
    }

    fn unwind(mut self: Box<Self>, vm: &mut Jq) {
        if let Some(sub) = self.sub.take() {
            let outer = self.enter_filter(vm);
            vm.sub_unwind(sub);
            self.leave_filter(vm, outer);
        }
    }
}

// ---- tostream -----------------------------------------------------------------------

/// `tostream`: `r` visits nodes in post-order. A node's own path is yielded when the
/// fork point of its `,` is restored (to its length) after its children; a sibling is
/// entered after the `.[]?` fork point restores the parent's length. Each path `$p`
/// gives one event: `[$p, v]` for a leaf (the reduce's initial value) or `[$p + [k]]`
/// for a container whose last child is `k`, each collected into the definition's `[]`.
pub(super) fn tostream(input: Value, c: ConstView<'_>) -> Outcome {
    let mut g = Box::new(Tostream {
        w: Walker::new(true),
        leaf: c.get(0).clone(),
        closing: c.get(1).clone(),
    });
    let v = g.descend(input);
    Outcome::Yield(v, g)
}

struct Tostream {
    w: Walker,
    /// `[$p, .]`'s `[]`.
    leaf: Value,
    /// `[$p+$q]`'s `[]`.
    closing: Value,
}

impl Tostream {
    /// Enters first children down to a node without children, and yields its event
    /// (after its `,`'s fork point restores its length).
    fn descend(&mut self, mut node: Value) -> Value {
        while children(&node) > 0 {
            node = self.w.enter(node);
        }
        let d = self.w.stack.len();
        restore(&mut self.w.p, d);
        self.event(node)
    }

    /// The event of `node`, at the current path.
    fn event(&mut self, node: Value) -> Value {
        let p = self.w.path();
        let n = children(&node);
        let ev = if n == 0 {
            // `[$p, .]`, getpath($p) being the node itself.
            let Value::Array(mut a) = self.leaf.clone() else {
                unreachable!()
            };
            a.push(p);
            a.push(node.clone());
            a
        } else {
            // `[$p+$q]` for the last child's `$q` (the earlier ones are overwritten).
            let (k, _) = child(&node, n - 1);
            let Value::Array(mut q) = p else {
                unreachable!()
            };
            q.push(k);
            let Value::Array(mut a) = self.closing.clone() else {
                unreachable!()
            };
            a.push(Value::Array(q));
            a
        };
        Value::Array(ev)
    }
}

impl Resume for Tostream {
    fn resume(mut self: Box<Self>, _vm: &mut Jq) -> Outcome {
        // The node's parent: restore its length (`.[]?`'s fork point for the next
        // child, or the parent's `,` for the parent itself).
        let d = self.w.stack.len();
        if d == 0 {
            return Outcome::Empty;
        }
        restore(&mut self.w.p, d - 1);
        let top = self.w.stack.last_mut().expect("parent");
        let v = if top.i + 1 < top.n {
            top.i += 1;
            let (k, c) = child(&top.node, top.i);
            self.w.p.push(k);
            self.descend(c)
        } else {
            let parent = self.w.stack.pop().expect("parent").node;
            self.event(parent)
        };
        Outcome::Yield(v, self)
    }
}
