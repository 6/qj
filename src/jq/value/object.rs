//! jq objects: port of the object parts of `jv.c`.
//!
//! jq's objects are hash tables whose slots are kept in insertion order:
//! setting an existing key keeps its position, a new key goes last, and a
//! deleted key leaves a tombstone, so re-adding it puts it last. An
//! insertion-ordered map with order-preserving removal reproduces exactly
//! that iteration order (`{a:1,b:2} | del(.a) | .a = 3` is `{"b":2,"a":3}`).
//!
//! Mutation unshares first (`jvp_object_unshare`), in place when unique.

use std::fmt;
use std::rc::Rc;

use indexmap::IndexMap;

use super::{Str, Value};

type Hasher = foldhash::fast::RandomState;

/// The object payload. A newtype so that dropping deeply nested values is
/// iterative (see `array::drop_values_iteratively`).
#[derive(Clone, Default)]
struct Map(IndexMap<Str, Value, Hasher>);

impl std::ops::Deref for Map {
    type Target = IndexMap<Str, Value, Hasher>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Map {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        if !self.0.values().any(super::array::owns_container) {
            return; // plain drop is shallow
        }
        let mut values: Vec<Value> = self.0.drain(..).map(|(_, v)| v).collect();
        super::array::drop_values_iteratively(&mut values);
    }
}

/// A jq object value (`JV_KIND_OBJECT`).
#[derive(Clone, Default)]
pub struct Object(Rc<Map>);

impl Object {
    /// `jv_object()`.
    pub fn new() -> Object {
        Object::default()
    }

    /// An empty object with room for `n` keys.
    pub fn with_capacity(n: usize) -> Object {
        Object(Rc::new(Map(IndexMap::with_capacity_and_hasher(
            n,
            Hasher::default(),
        ))))
    }

    /// Moves the values out (if uniquely owned) for iterative dropping.
    pub(crate) fn drain_values_into(&mut self, out: &mut Vec<Value>) {
        if let Some(m) = Rc::get_mut(&mut self.0) {
            out.extend(m.0.drain(..).map(|(_, v)| v));
        }
    }

    /// `jv_object_length`.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the object has no keys.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `jv_object_get`: `None` when the key is absent.
    #[inline]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// `jv_object_has`.
    #[inline]
    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    /// The entry at iteration position `i` (for index-based iteration like
    /// jq's `jv_object_iter`).
    #[inline]
    pub fn get_index(&self, i: usize) -> Option<(&Str, &Value)> {
        self.0.get_index(i)
    }

    /// Iterates over `(key, value)` pairs in jq's order (insertion order).
    #[inline]
    pub fn iter(&self) -> indexmap::map::Iter<'_, Str, Value> {
        self.0.iter()
    }

    /// Iterates over the keys in insertion order (`keys_unsorted`).
    #[inline]
    pub fn keys(&self) -> indexmap::map::Keys<'_, Str, Value> {
        self.0.keys()
    }

    /// Iterates over the values in insertion order.
    #[inline]
    pub fn values(&self) -> indexmap::map::Values<'_, Str, Value> {
        self.0.values()
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
        self.make_mut().shift_remove(key)
    }

    /// Removes every key for which `keep` returns false, preserving order
    /// (equivalent to deleting them one at a time, in linear time).
    pub fn retain(&mut self, mut keep: impl FnMut(&Str, &Value) -> bool) {
        self.make_mut().retain(|k, v| keep(k, v));
    }

    /// Mutable access to a value (unsharing the object first).
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        if !self.0.contains_key(key) {
            return None;
        }
        self.make_mut().get_mut(key)
    }

    /// Mutable iteration over the values (unsharing the object first).
    pub fn values_mut(&mut self) -> indexmap::map::ValuesMut<'_, Str, Value> {
        self.make_mut().values_mut()
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
        for (k, v) in other.iter() {
            let merged = match (self.get(k), v) {
                (Some(Value::Object(a)), Value::Object(b)) => {
                    // Like jq, `a` stays referenced by `self` meanwhile, so the
                    // nested object is copied rather than updated in place.
                    let mut a = a.clone();
                    a.merge_recursive(b);
                    Value::Object(a)
                }
                _ => v.clone(),
            };
            self.insert(k.clone(), merged);
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
    type IntoIter = indexmap::map::Iter<'a, Str, Value>;
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
