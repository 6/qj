//! jq objects: port of the object parts of `jv.c`.
//!
//! jq's objects are hash tables whose slots are kept in insertion order:
//! setting an existing key keeps its position, a new key goes last, and a
//! deleted key leaves a tombstone, so re-adding it puts it last. An
//! insertion-ordered map with order-preserving removal reproduces exactly
//! that iteration order (`{a:1,b:2} | del(.a) | .a = 3` is `{"b":2,"a":3}`).
//!
//! Mutation unshares first (`jvp_object_unshare`), in place when unique.
//!
//! The entries live in one vector, in iteration order. Most objects are
//! small and are searched linearly; an object with more than
//! `LINEAR_MAX` keys gets a hash index over its entries once it has been
//! searched a few times (`INDEX_AFTER`), so objects that are only built
//! and printed (or read once) never pay for one. Keys cache their hash
//! (`Str::key_hash`), so indexing an object whose keys are shared with
//! other objects (as parsed keys are) hashes nothing.

use std::cell::{Cell, OnceCell};
use std::fmt;
use std::rc::Rc;

use hashbrown::HashTable;

use super::string::hash_key;
use super::{Str, Value};

/// Objects with at most this many keys are always searched linearly.
const LINEAR_MAX: usize = 8;

/// Searches of a bigger object after which it gets an index.
const INDEX_AFTER: u32 = 2;

/// The object payload. Its `Drop` makes dropping deeply nested values
/// iterative (see `array::drop_values_iteratively`).
#[derive(Clone, Default)]
struct Map {
    /// Keys are unique.
    entries: Vec<(Str, Value)>,
    /// Positions in `entries`, by key hash, once built.
    index: OnceCell<HashTable<u32>>,
    /// Unindexed searches so far (only counted for objects bigger than
    /// [`LINEAR_MAX`]).
    searches: Cell<u32>,
}

impl Map {
    fn with_capacity(n: usize) -> Map {
        Map::from_entries(Vec::with_capacity(n))
    }

    fn from_entries(entries: Vec<(Str, Value)>) -> Map {
        Map {
            entries,
            index: OnceCell::new(),
            searches: Cell::new(0),
        }
    }

    #[inline]
    fn scan(&self, key: &[u8]) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k.as_bytes() == key)
    }

    #[inline]
    fn probe(&self, index: &HashTable<u32>, hash: u64, key: &[u8]) -> Option<usize> {
        index
            .find(hash, |&i| self.entries[i as usize].0.as_bytes() == key)
            .map(|&i| i as usize)
    }

    /// The position of `key`; `hash` computes its hash when needed.
    #[inline]
    fn find_with(&self, key: &[u8], hash: impl FnOnce() -> u64) -> Option<usize> {
        if self.entries.len() <= LINEAR_MAX {
            return self.scan(key);
        }
        if let Some(index) = self.index.get() {
            return self.probe(index, hash(), key);
        }
        let n = self.searches.get() + 1;
        self.searches.set(n);
        if n < INDEX_AFTER {
            return self.scan(key);
        }
        let index = self.index.get_or_init(|| self.build_index());
        self.probe(index, hash(), key)
    }

    #[inline]
    fn find(&self, key: &str) -> Option<usize> {
        self.find_with(key.as_bytes(), || hash_key(key.as_bytes()))
    }

    #[inline]
    fn find_str(&self, key: &Str) -> Option<usize> {
        // An identical key is the same key (a common case: parsed and
        // program keys are shared).
        if self.entries.len() <= LINEAR_MAX
            && let Some(i) = self.entries.iter().position(|(k, _)| k.ptr_eq(key))
        {
            return Some(i);
        }
        self.find_with(key.as_bytes(), || key.key_hash())
    }

    fn build_index(&self) -> HashTable<u32> {
        let entries = &self.entries;
        let mut index = HashTable::with_capacity(entries.len());
        for (i, (k, _)) in entries.iter().enumerate() {
            index.insert_unique(k.key_hash(), i as u32, |&j| {
                entries[j as usize].0.key_hash()
            });
        }
        index
    }

    /// Appends an entry whose key isn't in the map.
    #[inline]
    fn push_new(&mut self, key: Str, value: Value) {
        let i = self.entries.len();
        self.entries.push((key, value));
        if let Some(index) = self.index.get_mut() {
            let entries = &self.entries;
            index.insert_unique(entries[i].0.key_hash(), i as u32, |&j| {
                entries[j as usize].0.key_hash()
            });
        }
    }

    /// `jvp_object_write`: an existing key keeps its position (and the old
    /// key string, as in jq); a new key goes last.
    fn insert(&mut self, key: Str, value: Value) {
        match self.find_str(&key) {
            Some(i) => self.entries[i].1 = value,
            None => self.push_new(key, value),
        }
    }

    /// Removes a key, keeping the order of the others.
    fn remove(&mut self, key: &str) -> Option<Value> {
        let i = self.find(key)?;
        let (k, v) = self.entries.remove(i);
        if let Some(index) = self.index.get_mut() {
            if let Ok(e) = index.find_entry(k.key_hash(), |&j| j as usize == i) {
                e.remove();
            }
            for j in index.iter_mut() {
                if *j as usize > i {
                    *j -= 1;
                }
            }
        }
        Some(v)
    }

    fn retain(&mut self, mut keep: impl FnMut(&Str, &Value) -> bool) {
        let before = self.entries.len();
        self.entries.retain(|(k, v)| keep(k, v));
        if self.entries.len() != before {
            self.index.take();
            self.searches.set(0);
        }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        // Drop the entries here so that the nesting is counted; past the
        // recursion budget, move the values out and drop them iteratively.
        if !super::array::drop_nested(|| self.entries.clear()) {
            let mut values: Vec<Value> = self.entries.drain(..).map(|(_, v)| v).collect();
            super::array::drop_values_iteratively(&mut values);
        }
    }
}

/// A jq object value (`JV_KIND_OBJECT`).
#[derive(Clone, Default)]
pub struct Object(Rc<Map>);

/// Iterator over an object's entries, in jq's order.
#[derive(Clone)]
pub struct Iter<'a>(std::slice::Iter<'a, (Str, Value)>);

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a Str, &'a Value);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(k, v)| (k, v))
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl DoubleEndedIterator for Iter<'_> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|(k, v)| (k, v))
    }
}

impl ExactSizeIterator for Iter<'_> {}

impl Object {
    /// `jv_object()`.
    pub fn new() -> Object {
        Object::default()
    }

    /// An empty object with room for `n` keys.
    pub fn with_capacity(n: usize) -> Object {
        Object(Rc::new(Map::with_capacity(n)))
    }

    /// An object with these entries, in this order. The keys must be
    /// distinct (as after `jv_object_set` of each).
    pub(crate) fn from_unique_entries(entries: Vec<(Str, Value)>) -> Object {
        debug_assert!(
            entries
                .iter()
                .enumerate()
                .all(|(i, (k, _))| entries[..i].iter().all(|(k2, _)| k2 != k)),
            "duplicate keys"
        );
        Object(Rc::new(Map::from_entries(entries)))
    }

    /// Moves the values out (if uniquely owned) for iterative dropping.
    pub(crate) fn drain_values_into(&mut self, out: &mut Vec<Value>) {
        if let Some(m) = Rc::get_mut(&mut self.0) {
            out.extend(m.entries.drain(..).map(|(_, v)| v));
            m.index.take();
        }
    }

    /// `jv_object_length`.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.entries.len()
    }

    /// Whether the object has no keys.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.entries.is_empty()
    }

    /// `jv_object_get`: `None` when the key is absent.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.find(key).map(|i| &self.0.entries[i].1)
    }

    /// [`Object::get`] with a string value as the key (uses its cached hash).
    #[inline]
    pub fn get_str(&self, key: &Str) -> Option<&Value> {
        self.0.find_str(key).map(|i| &self.0.entries[i].1)
    }

    /// `jv_object_has`.
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.find(key).is_some()
    }

    /// The entry at iteration position `i` (for index-based iteration like
    /// jq's `jv_object_iter`).
    #[inline]
    pub fn get_index(&self, i: usize) -> Option<(&Str, &Value)> {
        self.0.entries.get(i).map(|(k, v)| (k, v))
    }

    /// Iterates over `(key, value)` pairs in jq's order (insertion order).
    #[inline]
    pub fn iter(&self) -> Iter<'_> {
        Iter(self.0.entries.iter())
    }

    /// Iterates over the keys in insertion order (`keys_unsorted`).
    #[inline]
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &Str> + ExactSizeIterator {
        self.0.entries.iter().map(|(k, _)| k)
    }

    /// Iterates over the values in insertion order.
    #[inline]
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> + ExactSizeIterator {
        self.0.entries.iter().map(|(_, v)| v)
    }

    /// Mutable access to the map, copying it first if shared.
    #[inline]
    fn make_mut(&mut self) -> &mut Map {
        Rc::make_mut(&mut self.0)
    }

    /// `jv_object_set`: an existing key keeps its position.
    pub fn insert(&mut self, key: Str, value: Value) {
        self.make_mut().insert(key, value);
    }

    /// `jv_object_delete`: removes a key, keeping the order of the others.
    /// Like jq, this unshares the object even when the key is absent.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.make_mut().remove(key)
    }

    /// Removes every key for which `keep` returns false, preserving order
    /// (equivalent to deleting them one at a time, in linear time).
    pub fn retain(&mut self, keep: impl FnMut(&Str, &Value) -> bool) {
        self.make_mut().retain(keep);
    }

    /// Mutable access to a value (unsharing the object first).
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        let i = self.0.find(key)?;
        Some(&mut self.make_mut().entries[i].1)
    }

    /// Mutable iteration over the values (unsharing the object first).
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Value> {
        self.make_mut().entries.iter_mut().map(|(_, v)| v)
    }

    /// `jv_object_merge`: sets every key of `other` in `self`.
    pub fn merge(&mut self, other: &Object) {
        for (k, v) in other.iter() {
            self.insert(k.clone(), v.clone());
        }
    }

    /// `jv_object_merge_recursive` (object `*`): nested objects present on
    /// both sides are merged recursively, anything else is replaced.
    pub fn merge_recursive(&mut self, other: &Object) {
        // jq recurses once per level of nesting shared by both sides; this
        // keeps the recursion in an explicit stack (values can be 10000
        // levels deep) with the same order of updates.
        struct Frame<'a> {
            target: Object,
            other: &'a Object,
            i: usize,
            /// The key this frame's result is stored under in its parent.
            key: Option<Str>,
        }
        let mut stack = vec![Frame {
            target: std::mem::take(self),
            other,
            i: 0,
            key: None,
        }];
        loop {
            let top = stack.last_mut().expect("a frame is active");
            if let Some((k, v)) = top.other.get_index(top.i) {
                top.i += 1;
                match (top.target.get_str(k), v) {
                    (Some(Value::Object(a)), Value::Object(b)) => {
                        // Like jq, `a` stays referenced by the target
                        // meanwhile, so the nested object is copied rather
                        // than updated in place.
                        let child = a.clone();
                        stack.push(Frame {
                            target: child,
                            other: b,
                            i: 0,
                            key: Some(k.clone()),
                        });
                    }
                    _ => top.target.insert(k.clone(), v.clone()),
                }
            } else {
                let done = stack.pop().expect("a frame is active");
                match stack.last_mut() {
                    None => {
                        *self = done.target;
                        return;
                    }
                    Some(parent) => parent.target.insert(
                        done.key.expect("nested frames have a key"),
                        Value::Object(done.target),
                    ),
                }
            }
        }
    }

    /// Pointer identity (`jv_identical`, `jv_equal`'s fast path).
    #[inline]
    pub fn ptr_eq(&self, other: &Object) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// Whether this is the only reference to the map.
    pub fn is_unique(&self) -> bool {
        Rc::strong_count(&self.0) == 1 && Rc::weak_count(&self.0) == 0
    }

    /// The number of references to the map (`jv_get_refcnt`).
    pub fn refcount(&self) -> usize {
        Rc::strong_count(&self.0)
    }
}

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a Str, &'a Value);
    type IntoIter = Iter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FromIterator<(Str, Value)> for Object {
    fn from_iter<I: IntoIterator<Item = (Str, Value)>>(iter: I) -> Object {
        let mut m = Map::default();
        for (k, v) in iter {
            m.insert(k, v);
        }
        Object(Rc::new(m))
    }
}

impl fmt::Debug for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(o: &Object) -> Vec<&str> {
        o.keys().map(|k| k.as_str()).collect()
    }

    #[test]
    fn order_and_duplicates_like_jq() {
        // Sizes on both sides of LINEAR_MAX, looked up often enough to be
        // indexed.
        for n in [3usize, 8, 9, 40, 300] {
            let mut o = Object::new();
            for i in 0..n {
                o.insert(Str::from(format!("k{i}").as_str()), Value::from(i));
            }
            for _ in 0..3 {
                assert_eq!(o.get("k0").and_then(Value::as_f64), Some(0.0));
                assert!(o.get("nope").is_none());
            }
            // Setting an existing key keeps its position.
            o.insert(Str::from("k0"), Value::from("x"));
            assert_eq!(keys(&o)[0], "k0");
            assert_eq!(o.len(), n);
            // Removing keeps the order; re-adding goes last.
            assert!(o.remove("k1").is_some());
            assert!(o.remove("k1").is_none());
            assert_eq!(o.len(), n - 1);
            o.insert(Str::from("k1"), Value::Null);
            assert_eq!(*keys(&o).last().unwrap(), "k1");
            for i in 0..n {
                let k = format!("k{i}");
                assert!(o.contains_key(&k), "{k} of {n}");
                assert_eq!(o.get_str(&Str::from(k.as_str())).is_some(), true);
            }
            let pos = |k: &str| keys(&o).iter().position(|x| *x == k).unwrap();
            if n > 2 {
                assert_eq!(pos("k2"), 1);
            }
            // retain keeps the order of the survivors.
            o.retain(|k, _| k.as_str() != "k2");
            assert!(!o.contains_key("k2"));
            assert_eq!(o.len(), n - 1);
            if n > 3 {
                assert_eq!(keys(&o)[1], "k3");
            }
        }
    }

    #[test]
    fn copies_on_write() {
        let mut a: Object = (0..20)
            .map(|i| (Str::from(format!("{i}").as_str()), Value::from(i)))
            .collect();
        for _ in 0..3 {
            assert!(a.contains_key("5"));
        }
        let b = a.clone();
        a.insert(Str::from("new"), Value::Null);
        assert!(a.contains_key("new"));
        assert!(!b.contains_key("new"));
        assert_eq!(b.len(), 20);
        assert_eq!(a.len(), 21);
    }
}
