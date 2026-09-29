//! jq arrays: port of the array parts of `jv.c`.
//!
//! Like jq's `jvp_array`, an [`Array`] is a *view* (`offset`, `len`) into
//! shared, reference-counted storage. Slicing is O(1) and shares storage
//! (while `offset < 65536`, jq's `unsigned short` limit), and writes happen in
//! place when the storage is uniquely owned and the target position is below
//! jq's `alloc_length`; otherwise the view is copied into new storage with
//! `alloc_length = new_len * 3 / 2`.
//!
//! These details are observable and deliberately reproduced:
//! * `jv_equal` treats two views of the same storage with the same length as
//!   equal without comparing elements, so `[1,2,3,4] | .[0:2] == .[2:4]` is
//!   `true` in jq 1.8.1 (see [`Array::same_storage`]).
//! * writing past the end of a uniquely owned view reuses the storage, so
//!   elements beyond the view's end reappear:
//!   `[range(4)+1] | .[0:2] | .[3] = 9` gives `[1,2,3,9]`.

use std::fmt;
use std::rc::Rc;

use super::{Error, Value};

/// jq's `ARRAY_SIZE_ROUND_UP`.
#[inline]
fn round_up(n: usize) -> usize {
    n * 3 / 2
}

/// Size of a new `jv_array()`.
const DEFAULT_ARRAY_SIZE: usize = 16;

/// jq's `jvp_array` payload.
#[derive(Clone)]
struct Storage {
    /// Elements; `items.len()` is jq's `array->length` (it can extend past
    /// the end of the views that use this storage).
    items: Vec<Value>,
    /// jq's `alloc_length`: positions below it can be written in place.
    alloc: usize,
}

impl Drop for Storage {
    fn drop(&mut self) {
        drop_values_iteratively(&mut self.items);
    }
}

/// Whether dropping `v` would free a container (and so recurse).
#[inline]
pub(crate) fn owns_container(v: &Value) -> bool {
    match v {
        Value::Array(a) => Rc::strong_count(&a.storage) == 1,
        Value::Object(o) => o.is_unique(),
        _ => false,
    }
}

/// Drops `items` without recursing into nested containers, so that deeply
/// nested values (jq accepts 10000 levels of JSON, and programs can build
/// deeper ones) cannot overflow the stack.
pub(crate) fn drop_values_iteratively(items: &mut Vec<Value>) {
    if !items.iter().any(owns_container) {
        return; // plain drop is shallow
    }
    let mut stack = std::mem::take(items);
    while let Some(mut v) = stack.pop() {
        match &mut v {
            Value::Array(a) => {
                if let Some(st) = Rc::get_mut(&mut a.storage) {
                    stack.append(&mut st.items);
                }
            }
            Value::Object(o) => o.drain_values_into(&mut stack),
            _ => {}
        }
        // `v` is now shallow (its contents, if we owned them, moved to `stack`).
    }
}

/// A jq array value (`JV_KIND_ARRAY`).
#[derive(Clone)]
pub struct Array {
    storage: Rc<Storage>,
    offset: u32,
    len: u32,
}

impl Default for Array {
    fn default() -> Array {
        Array::new()
    }
}

impl Array {
    /// `jv_array()`: an empty array (jq allocates 16 slots).
    pub fn new() -> Array {
        Array::with_capacity(DEFAULT_ARRAY_SIZE)
    }

    /// `jv_array_sized(n)`: an empty array with `alloc_length = n`.
    pub fn with_capacity(n: usize) -> Array {
        Array {
            storage: Rc::new(Storage {
                items: Vec::with_capacity(n.min(DEFAULT_ARRAY_SIZE)),
                alloc: n,
            }),
            offset: 0,
            len: 0,
        }
    }

    /// An array holding `items`, with the `alloc_length` jq would have after
    /// appending them one by one to `jv_array()` (which is how jq builds
    /// arrays from `[...]` collection and from JSON text).
    pub fn from_vec(items: Vec<Value>) -> Array {
        let len = items.len();
        let mut alloc = DEFAULT_ARRAY_SIZE;
        while len > alloc {
            alloc = round_up(alloc + 1);
        }
        Array {
            storage: Rc::new(Storage { items, alloc }),
            offset: 0,
            len: len as u32,
        }
    }

    #[inline]
    fn off(&self) -> usize {
        self.offset as usize
    }

    /// `jv_array_length`.
    #[inline]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the array is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The visible elements.
    #[inline]
    pub fn as_slice(&self) -> &[Value] {
        &self.storage.items[self.off()..self.off() + self.len()]
    }

    /// Iterates over the elements.
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, Value> {
        self.as_slice().iter()
    }

    /// `jv_array_get` for `idx >= 0`: `None` when out of range.
    #[inline]
    pub fn get(&self, idx: usize) -> Option<&Value> {
        self.as_slice().get(idx)
    }

    /// First element.
    pub fn first(&self) -> Option<&Value> {
        self.as_slice().first()
    }

    /// Last element.
    pub fn last(&self) -> Option<&Value> {
        self.as_slice().last()
    }

    /// Whether both arrays are views of the same storage with the same
    /// length. This is `jv_equal`'s identity fast path, which ignores the
    /// view offset (a jq quirk that `==`, `unique` and `group_by` expose).
    #[inline]
    pub fn same_storage(&self, other: &Array) -> bool {
        self.len == other.len && Rc::ptr_eq(&self.storage, &other.storage)
    }

    /// `jv_identical` for arrays: same storage, offset and length.
    #[inline]
    pub fn identical(&self, other: &Array) -> bool {
        self.same_storage(other) && self.offset == other.offset
    }

    fn is_unique(&mut self) -> bool {
        Rc::get_mut(&mut self.storage).is_some()
    }

    /// The number of references to the storage (`jv_get_refcnt`; views of
    /// the same storage share it).
    pub fn refcount(&self) -> usize {
        Rc::strong_count(&self.storage)
    }

    /// Port of `jvp_array_write`: returns the slot for index `i`, extending
    /// the array with nulls, writing in place when possible.
    fn write_slot(&mut self, i: usize) -> &mut Value {
        let pos = i + self.off();
        let (offset, len) = (self.off(), self.len());
        let unique = self.is_unique();
        if pos < self.storage.alloc && unique {
            // use existing array space
            let st = Rc::get_mut(&mut self.storage).expect("unique");
            if st.items.len() <= pos {
                st.items.resize(pos + 1, Value::Null);
            }
            self.len = len.max(i + 1) as u32;
            return &mut st.items[pos];
        }
        // allocate a new array
        let new_length = (i + 1).max(len);
        let alloc = round_up(new_length);
        if unique {
            // Unique: move the visible elements instead of copying them.
            let st = Rc::get_mut(&mut self.storage).expect("unique");
            st.items.truncate(offset + len);
            st.items.drain(..offset);
            st.items.resize(new_length, Value::Null);
            st.alloc = alloc;
        } else {
            let mut items = Vec::with_capacity(new_length.max(DEFAULT_ARRAY_SIZE).min(alloc));
            items.extend_from_slice(&self.storage.items[offset..offset + len]);
            items.resize(new_length, Value::Null);
            self.storage = Rc::new(Storage { items, alloc });
        }
        self.offset = 0;
        self.len = new_length as u32;
        &mut Rc::get_mut(&mut self.storage).expect("unique").items[i]
    }

    /// `jv_array_set`: negative indices count from the end; errors
    /// `Out of bounds negative array index` and `Array index too large`.
    pub fn set(&mut self, idx: i64, val: Value) -> Result<(), Error> {
        let mut idx = idx;
        if idx < 0 {
            idx += self.len as i64;
        }
        if idx < 0 {
            return Err(Error::msg("Out of bounds negative array index"));
        }
        if idx > (i32::MAX >> 2) as i64 - self.offset as i64 {
            return Err(Error::msg("Array index too large"));
        }
        *self.write_slot(idx as usize) = val;
        Ok(())
    }

    /// `jv_array_append`.
    pub fn push(&mut self, val: Value) {
        let i = self.len();
        *self.write_slot(i) = val;
    }

    /// `jv_array_concat`: appends every element of `other`.
    pub fn extend_from_array(&mut self, other: &Array) {
        for v in other.iter() {
            self.push(v.clone());
        }
    }

    /// Appends every element produced by `iter`.
    pub fn extend<I: IntoIterator<Item = Value>>(&mut self, iter: I) {
        for v in iter {
            self.push(v);
        }
    }

    /// Mutable access to the visible elements, unsharing the storage first
    /// if needed (the equivalent of `jv_array_set` on each element).
    pub fn as_mut_slice(&mut self) -> &mut [Value] {
        if self.len == 0 {
            return &mut [];
        }
        if !self.is_unique() {
            // jq copies the view (jvp_array_write's "allocate a new array").
            let items = self.as_slice().to_vec();
            self.storage = Rc::new(Storage {
                items,
                alloc: round_up(self.len()),
            });
            self.offset = 0;
        }
        let (o, l) = (self.off(), self.len());
        &mut Rc::get_mut(&mut self.storage).expect("unique").items[o..o + l]
    }

    /// Mutable access to one element (unsharing like [`Array::as_mut_slice`]).
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Value> {
        if idx >= self.len() {
            return None;
        }
        self.as_mut_slice().get_mut(idx)
    }

    /// `jv_array_slice`: `start`/`end` are clamped (negative values count
    /// from the end). Shares storage unless the view offset would reach
    /// 65536, or the slice is empty (a fresh `jv_array()`).
    pub fn slice(&self, start: i64, end: i64) -> Array {
        let len = self.len as i64;
        let (start, end) = super::string::clamp_slice_params(len, start, end);
        let (start, end) = (start as usize, end as usize);
        if start == end {
            return Array::new();
        }
        if self.off() + start >= 1 << 16 {
            let mut r = Array::with_capacity(end - start);
            for v in &self.as_slice()[start..end] {
                r.push(v.clone());
            }
            return r;
        }
        Array {
            storage: self.storage.clone(),
            offset: (self.off() + start) as u32,
            len: (end - start) as u32,
        }
    }

    /// Consuming variant of [`Array::slice`].
    pub fn into_slice(self, start: i64, end: i64) -> Array {
        self.slice(start, end)
    }

    /// `jv_array_indexes`: the start positions where `b` occurs as a
    /// contiguous run in `self`, comparing with `jv_equal`.
    pub fn indexes(&self, b: &Array) -> Array {
        let mut res = Array::new();
        let a = self.as_slice();
        let mut idx: i64 = -1;
        for ai in 0..a.len() {
            for (bi, belem) in b.iter().enumerate() {
                let eq = match a.get(ai + bi) {
                    Some(x) => x.equal(belem),
                    None => false,
                };
                if !eq {
                    idx = -1;
                } else if bi == 0 && idx == -1 {
                    idx = ai as i64;
                }
            }
            if idx > -1 {
                res.push(Value::from(idx as f64));
            }
            idx = -1;
        }
        res
    }

    /// Converts into a `Vec`, moving the elements out when the storage is
    /// uniquely owned.
    pub fn into_vec(mut self) -> Vec<Value> {
        let (o, l) = (self.off(), self.len());
        match Rc::get_mut(&mut self.storage) {
            Some(st) => {
                let mut v = std::mem::take(&mut st.items);
                v.truncate(o + l);
                v.drain(..o);
                v
            }
            None => self.as_slice().to_vec(),
        }
    }
}

impl<'a> IntoIterator for &'a Array {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for Array {
    type Item = Value;
    type IntoIter = std::vec::IntoIter<Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_vec().into_iter()
    }
}

impl FromIterator<Value> for Array {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Array {
        Array::from_vec(iter.into_iter().collect())
    }
}

impl From<Vec<Value>> for Array {
    fn from(v: Vec<Value>) -> Array {
        Array::from_vec(v)
    }
}

impl fmt::Debug for Array {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(xs: &[f64]) -> Array {
        xs.iter().map(|&x| Value::from(x)).collect()
    }

    fn nums(a: &Array) -> Vec<Option<f64>> {
        a.iter()
            .map(|v| match v {
                Value::Number(n) => Some(n.value()),
                Value::Null => None,
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn set_errors() {
        let mut a = arr(&[1.0]);
        assert_eq!(
            a.set(-2, Value::Null).unwrap_err().to_string(),
            "Out of bounds negative array index"
        );
        assert_eq!(
            a.set(536870912, Value::Null).unwrap_err().to_string(),
            "Array index too large"
        );
        a.set(-1, Value::from(5.0)).unwrap();
        a.set(3, Value::from(7.0)).unwrap();
        assert_eq!(nums(&a), [Some(5.0), None, None, Some(7.0)]);
    }

    #[test]
    fn slices_share_storage() {
        let a = arr(&[1.0, 2.0, 3.0, 4.0]);
        let x = a.slice(0, 2);
        let y = a.slice(2, 4);
        assert!(x.same_storage(&y));
        assert!(!x.identical(&y));
        assert_eq!(nums(&a.slice(-3, -1)), [Some(2.0), Some(3.0)]);
        assert_eq!(nums(&a.slice(3, 1)), Vec::<Option<f64>>::new());
    }

    #[test]
    fn stale_elements_reappear_like_jq() {
        // `jq -nc '[range(4)+1] | .[0:2] | .[3] = 9'` => [1,2,3,9]
        let a = arr(&[1.0, 2.0, 3.0, 4.0]);
        let mut v = a.slice(0, 2);
        drop(a);
        v.set(3, Value::from(9.0)).unwrap();
        assert_eq!(nums(&v), [Some(1.0), Some(2.0), Some(3.0), Some(9.0)]);
        // Appending just overwrites the next stale slot:
        // `[range(4)+1] | .[0:2] | . + [9]` => [1,2,9]
        let a = arr(&[1.0, 2.0, 3.0, 4.0]);
        let mut v = a.slice(0, 2);
        drop(a);
        v.push(Value::from(9.0));
        assert_eq!(nums(&v), [Some(1.0), Some(2.0), Some(9.0)]);
        // A shared view is copied instead.
        let a = arr(&[1.0, 2.0, 3.0, 4.0]);
        let mut v = a.slice(0, 2);
        v.set(3, Value::from(9.0)).unwrap();
        assert_eq!(nums(&v), [Some(1.0), Some(2.0), None, Some(9.0)]);
    }

    #[test]
    fn alloc_growth_matches_jq() {
        // jv_array() + appends: 16 -> 25 -> 39 -> 60 ...
        let mut a = Array::new();
        for i in 0..20 {
            a.push(Value::from(i as f64));
        }
        assert_eq!(a.storage.alloc, 25);
        assert_eq!(Array::from_vec(vec![Value::Null; 20]).storage.alloc, 25);
        assert_eq!(Array::from_vec(vec![Value::Null; 26]).storage.alloc, 39);
        assert_eq!(Array::from_vec(vec![Value::Null; 16]).storage.alloc, 16);
    }

    #[test]
    fn indexes() {
        // `jq -nc '[1,2,1,2] | .[[1,2]]'` => [0,2]
        let a = arr(&[1.0, 2.0, 1.0, 2.0]);
        assert_eq!(nums(&a.indexes(&arr(&[1.0, 2.0]))), [Some(0.0), Some(2.0)]);
        assert_eq!(nums(&a.indexes(&arr(&[]))), Vec::<Option<f64>>::new());
        assert_eq!(
            nums(&a.indexes(&arr(&[2.0, 3.0]))),
            Vec::<Option<f64>>::new()
        );
    }
}
