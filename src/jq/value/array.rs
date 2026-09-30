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

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::fmt;
use std::ptr::{self, NonNull};

use super::{Error, Value};

/// jq's `ARRAY_SIZE_ROUND_UP`.
#[inline]
fn round_up(n: usize) -> usize {
    n * 3 / 2
}

/// Size of a new `jv_array()`.
const DEFAULT_ARRAY_SIZE: usize = 16;

/// jq's `alloc_length` after appending `len` elements to `jv_array()`.
fn appended_alloc(len: usize) -> usize {
    let mut alloc = DEFAULT_ARRAY_SIZE;
    while len > alloc {
        alloc = round_up(alloc + 1);
    }
    alloc
}

/// The header of an array's storage, which its elements follow in the same
/// allocation (like jq's `jvp_array`).
#[repr(C)]
struct Header {
    strong: Cell<usize>,
    /// Initialized elements: jq's `array->length` (it can extend past the
    /// end of the views that use this storage).
    len: usize,
    /// Element slots allocated. Only `alloc` is jq's: the slots grow as
    /// elements are added, which nothing can observe.
    cap: usize,
    /// jq's `alloc_length`: positions below it can be written in place.
    alloc: usize,
}

/// jq's `jvp_array` payload: reference-counted, one allocation.
struct Storage(NonNull<Header>);

impl Storage {
    fn layout(cap: usize) -> Layout {
        let size = std::mem::size_of::<Value>()
            .checked_mul(cap)
            .and_then(|n| n.checked_add(std::mem::size_of::<Header>()))
            .expect("array size overflow");
        let align = std::mem::align_of::<Header>().max(std::mem::align_of::<Value>());
        Layout::from_size_align(size, align).expect("array layout")
    }

    /// Empty storage with room for `cap` elements and jq's `alloc_length`.
    fn new(cap: usize, alloc: usize) -> Storage {
        let layout = Storage::layout(cap);
        // SAFETY: the layout has a non-zero size (the header).
        let p = unsafe { alloc::alloc(layout) }.cast::<Header>();
        let Some(p) = NonNull::new(p) else {
            alloc::handle_alloc_error(layout)
        };
        // SAFETY: `p` is a fresh allocation for a header and `cap` values.
        unsafe {
            p.as_ptr().write(Header {
                strong: Cell::new(1),
                len: 0,
                cap,
                alloc,
            })
        };
        Storage(p)
    }

    /// Storage holding `items` (moved in).
    fn from_vec(mut items: Vec<Value>, alloc: usize) -> Storage {
        let n = items.len();
        let mut st = Storage::new(n, alloc);
        // SAFETY: the storage has room for `n` values; the vector's are
        // moved (copied, then forgotten by setting its length to 0).
        unsafe {
            ptr::copy_nonoverlapping(items.as_ptr(), st.items_ptr(), n);
            items.set_len(0);
            st.header_mut().len = n;
        }
        st
    }

    #[inline]
    fn header(&self) -> &Header {
        // SAFETY: the pointer is a live allocation holding a header.
        unsafe { self.0.as_ref() }
    }

    /// The header, for changes: only while the storage is unique (or being
    /// freed), so that no view sees them happen.
    #[inline]
    unsafe fn header_mut(&mut self) -> &mut Header {
        // SAFETY: the caller has the only reference to the storage.
        unsafe { self.0.as_mut() }
    }

    #[inline]
    fn items_ptr(&self) -> *mut Value {
        // SAFETY: the elements follow the header in the allocation (the
        // layout's alignment suits both).
        unsafe { self.0.as_ptr().add(1).cast::<Value>() }
    }

    #[inline]
    fn items(&self) -> &[Value] {
        // SAFETY: the first `len` slots are initialized.
        unsafe { std::slice::from_raw_parts(self.items_ptr(), self.header().len) }
    }

    #[inline]
    fn len(&self) -> usize {
        self.header().len
    }

    #[inline]
    fn alloc(&self) -> usize {
        self.header().alloc
    }

    #[inline]
    fn strong(&self) -> usize {
        self.header().strong.get()
    }

    #[inline]
    fn is_unique(&self) -> bool {
        self.strong() == 1
    }

    #[inline]
    fn ptr_eq(&self, other: &Storage) -> bool {
        self.0 == other.0
    }

    // ---- changes, on unique storage only ----

    /// The elements, mutably.
    #[inline]
    fn items_mut(&mut self) -> &mut [Value] {
        debug_assert!(self.is_unique());
        // SAFETY: unique, and the first `len` slots are initialized.
        unsafe { std::slice::from_raw_parts_mut(self.items_ptr(), self.header().len) }
    }

    fn set_alloc(&mut self, alloc: usize) {
        debug_assert!(self.is_unique());
        // SAFETY: unique.
        unsafe { self.header_mut().alloc = alloc };
    }

    /// Makes room for `total` elements (growing by doubling).
    fn reserve(&mut self, total: usize) {
        debug_assert!(self.is_unique());
        let cap = self.header().cap;
        if total <= cap {
            return;
        }
        let new_cap = total.max(cap.saturating_mul(2)).max(4);
        let new_layout = Storage::layout(new_cap);
        // SAFETY: the allocation was made with `layout(cap)`; the header
        // and the initialized elements move with it (values are movable).
        let p = unsafe {
            alloc::realloc(
                self.0.as_ptr().cast(),
                Storage::layout(cap),
                new_layout.size(),
            )
        }
        .cast::<Header>();
        let Some(p) = NonNull::new(p) else {
            alloc::handle_alloc_error(new_layout)
        };
        self.0 = p;
        // SAFETY: unique.
        unsafe { self.header_mut().cap = new_cap };
    }

    /// Grows to `n` elements with nulls, or truncates to `n`.
    fn resize_null(&mut self, n: usize) {
        let len = self.len();
        if n <= len {
            self.truncate(n);
            return;
        }
        self.reserve(n);
        let p = self.items_ptr();
        for i in len..n {
            // SAFETY: slot `i` is allocated and uninitialized.
            unsafe { p.add(i).write(Value::Null) };
        }
        // SAFETY: unique; the slots up to `n` are initialized now.
        unsafe { self.header_mut().len = n };
    }

    /// Appends clones of `items`.
    fn extend_cloned(&mut self, items: &[Value]) {
        self.reserve(self.len() + items.len());
        let p = self.items_ptr();
        for v in items {
            let len = self.len();
            // SAFETY: slot `len` is allocated; the length covers it only
            // once it's written (a panicking clone leaves it out).
            unsafe {
                p.add(len).write(v.clone());
                self.header_mut().len = len + 1;
            }
        }
    }

    /// Drops the elements from `n` on.
    fn truncate(&mut self, n: usize) {
        debug_assert!(self.is_unique());
        let len = self.len();
        if n >= len {
            return;
        }
        // SAFETY: unique; the length drops first, so the elements are
        // never seen after they're dropped.
        unsafe {
            self.header_mut().len = n;
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                self.items_ptr().add(n),
                len - n,
            ));
        }
    }

    /// Drops the first `n` elements, moving the rest down.
    fn remove_prefix(&mut self, n: usize) {
        debug_assert!(self.is_unique());
        let len = self.len();
        let n = n.min(len);
        if n == 0 {
            return;
        }
        let p = self.items_ptr();
        // SAFETY: unique; the first `n` elements are dropped, then the rest
        // (initialized, `len - n` of them) move down.
        unsafe {
            self.header_mut().len = 0;
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(p, n));
            ptr::copy(p.add(n), p, len - n);
            self.header_mut().len = len - n;
        }
    }

    /// Moves every element out, onto the end of `out`.
    fn drain_into(&mut self, out: &mut Vec<Value>) {
        debug_assert!(self.is_unique());
        let len = self.len();
        out.reserve(len);
        // SAFETY: unique; the elements are moved (copied, then forgotten
        // by setting the length to 0) into reserved room.
        unsafe {
            ptr::copy_nonoverlapping(self.items_ptr(), out.as_mut_ptr().add(out.len()), len);
            out.set_len(out.len() + len);
            self.header_mut().len = 0;
        }
    }
}

impl Clone for Storage {
    #[inline]
    fn clone(&self) -> Storage {
        let s = &self.header().strong;
        s.set(s.get() + 1);
        Storage(self.0)
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        let strong = self.header().strong.get();
        if strong > 1 {
            self.header().strong.set(strong - 1);
            return;
        }
        // The last reference: drop the elements here, counting the nesting
        // (or iteratively past the budget), then free the allocation.
        if self.len() > 0 && !drop_nested(|| self.truncate(0)) {
            let mut items = Vec::new();
            self.drain_into(&mut items);
            drop_values_iteratively(&mut items);
        }
        let cap = self.header().cap;
        // SAFETY: allocated with `layout(cap)`; nothing refers to it now.
        unsafe { alloc::dealloc(self.0.as_ptr().cast(), Storage::layout(cap)) };
    }
}

thread_local! {
    /// How many container drops are in progress on this thread.
    static DROP_DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Container drops that may recurse natively before switching to
/// [`drop_values_iteratively`]. Ordinary data never gets close, so drops
/// stay as cheap as the default drop glue.
const MAX_DROP_RECURSION: u32 = 256;

/// Runs `drop_contents` one nesting level deeper, or returns false (without
/// running it) when the recursion budget is used up, in which case the
/// caller must drop iteratively.
#[inline]
pub(crate) fn drop_nested(drop_contents: impl FnOnce()) -> bool {
    let depth = DROP_DEPTH.with(Cell::get);
    if depth >= MAX_DROP_RECURSION {
        return false;
    }
    DROP_DEPTH.with(|d| d.set(depth + 1));
    drop_contents();
    DROP_DEPTH.with(|d| d.set(depth));
    true
}

/// Whether dropping `v` would free a container (and so recurse).
#[inline]
pub(crate) fn owns_container(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.storage.is_unique(),
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
    // QJ_JQ_COMPAT=1: jq's jv_free recurses, so a value this deep overflows
    // its C stack and the process dies of SIGSEGV. Nothing has been freed
    // yet, which is where jq dies too. `drop_nested` refuses at the storage
    // of the container MAX_DROP_RECURSION + 1 levels down, so that many
    // jv_free frames are already committed above `items`.
    crate::compat::freeing_iteratively(items, u64::from(MAX_DROP_RECURSION) + 1);
    let mut stack = std::mem::take(items);
    while let Some(mut v) = stack.pop() {
        match &mut v {
            Value::Array(a) => {
                if a.storage.is_unique() {
                    a.storage.drain_into(&mut stack);
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
    storage: Storage,
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

    /// `jv_array_sized(n)`: an empty array with `alloc_length = n`. (Only
    /// that number is jq's: the elements' memory grows as they're added,
    /// which nothing can observe.)
    pub fn with_capacity(n: usize) -> Array {
        Array {
            storage: Storage::new(0, n),
            offset: 0,
            len: 0,
        }
    }

    /// An array holding `items`, with the `alloc_length` jq would have after
    /// appending them one by one to `jv_array()` (which is how jq builds
    /// arrays from `[...]` collection and from JSON text).
    pub fn from_vec(items: Vec<Value>) -> Array {
        let len = items.len();
        Array {
            storage: Storage::from_vec(items, appended_alloc(len)),
            offset: 0,
            len: len as u32,
        }
    }

    /// [`Array::from_vec`] of the elements `items` yields (moved), without
    /// collecting them first.
    pub fn from_exact<I: ExactSizeIterator<Item = Value>>(items: I) -> Array {
        let n = items.len();
        let mut storage = Storage::new(n, appended_alloc(n));
        let p = storage.items_ptr();
        for v in items.take(n) {
            let len = storage.len();
            // SAFETY: the storage is fresh (unique) with room for `n`
            // values, and at most `n` are written; the length covers each
            // once it's written.
            unsafe {
                p.add(len).write(v);
                storage.header_mut().len = len + 1;
            }
        }
        let len = storage.len();
        if len != n {
            storage.set_alloc(appended_alloc(len));
        }
        Array {
            storage,
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
        &self.storage.items()[self.off()..self.off() + self.len()]
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
        self.len == other.len && self.storage.ptr_eq(&other.storage)
    }

    /// `jv_identical` for arrays: same storage, offset and length.
    #[inline]
    pub fn identical(&self, other: &Array) -> bool {
        self.same_storage(other) && self.offset == other.offset
    }

    fn is_unique(&self) -> bool {
        self.storage.is_unique()
    }

    /// The number of references to the storage (`jv_get_refcnt`; views of
    /// the same storage share it).
    pub fn refcount(&self) -> usize {
        self.storage.strong()
    }

    /// Port of `jvp_array_write`: returns the slot for index `i`, extending
    /// the array with nulls, writing in place when possible.
    fn write_slot(&mut self, i: usize) -> &mut Value {
        let pos = i + self.off();
        let (offset, len) = (self.off(), self.len());
        let unique = self.is_unique();
        if pos < self.storage.alloc() && unique {
            // use existing array space
            if self.storage.len() <= pos {
                self.storage.resize_null(pos + 1);
            }
            self.len = len.max(i + 1) as u32;
            return &mut self.storage.items_mut()[pos];
        }
        // allocate a new array
        let new_length = (i + 1).max(len);
        let alloc = round_up(new_length);
        if unique {
            // Unique: move the visible elements instead of copying them.
            self.storage.truncate(offset + len);
            self.storage.remove_prefix(offset);
            self.storage.resize_null(new_length);
            self.storage.set_alloc(alloc);
        } else {
            let mut st = Storage::new(new_length, alloc);
            st.extend_cloned(&self.as_slice()[..len]);
            st.resize_null(new_length);
            self.storage = st;
        }
        self.offset = 0;
        self.len = new_length as u32;
        &mut self.storage.items_mut()[i]
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
            let mut st = Storage::new(self.len(), round_up(self.len()));
            st.extend_cloned(self.as_slice());
            self.storage = st;
            self.offset = 0;
        }
        let (o, l) = (self.off(), self.len());
        &mut self.storage.items_mut()[o..o + l]
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
        if !self.is_unique() {
            return self.as_slice().to_vec();
        }
        self.storage.truncate(o + l);
        self.storage.remove_prefix(o);
        let mut v = Vec::new();
        self.storage.drain_into(&mut v);
        v
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
        assert_eq!(a.storage.alloc(), 25);
        assert_eq!(Array::from_vec(vec![Value::Null; 20]).storage.alloc(), 25);
        assert_eq!(Array::from_vec(vec![Value::Null; 26]).storage.alloc(), 39);
        assert_eq!(Array::from_vec(vec![Value::Null; 16]).storage.alloc(), 16);
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

    #[test]
    fn deep_drops_stay_off_the_stack() {
        use crate::jq::value::{Object, Str};
        // Much deeper than jq's parser allows, dropped on a small thread.
        let t = std::thread::Builder::new()
            .stack_size(512 << 10)
            .spawn(|| {
                for shape in 0..3 {
                    let mut v = Value::from(1.0);
                    for i in 0..100_000 {
                        let wrap_object = match shape {
                            0 => false,
                            1 => true,
                            _ => i % 2 == 0,
                        };
                        v = if wrap_object {
                            let mut o = Object::new();
                            o.insert(Str::from("a"), v);
                            o.insert(Str::from("b"), Value::from(vec![Value::Null]));
                            Value::Object(o)
                        } else {
                            Value::from(vec![v, Value::from("x")])
                        };
                    }
                    drop(v);
                    // The recursion budget is restored after every drop.
                    assert_eq!(DROP_DEPTH.with(Cell::get), 0);
                }
                // Shared subtrees are only released once.
                let leaf = Value::from(vec![Value::from(2.0)]);
                let a = Value::from(vec![leaf.clone(), leaf.clone()]);
                drop(a);
                assert_eq!(leaf.refcount(), 1);
            })
            .unwrap();
        t.join().unwrap();
    }
}
