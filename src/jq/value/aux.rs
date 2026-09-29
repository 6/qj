//! Port of `jv_aux.c`: indexing, paths, keys, comparison and sorting.

use std::cmp::Ordering;

use super::string::double_to_int;
use super::{Array, Error, Object, Str, Value};

/// `INT_MIN`/`INT_MAX` clamping followed by C's `(int)` truncation.
fn clamp_index(d: f64) -> i64 {
    let d = d.clamp(i32::MIN as f64, i32::MAX as f64);
    d as i32 as i64
}

/// Port of `parse_slice`: turns a `{"start": s, "end": e}` key into clamped
/// `(start, end)` indices for a sequence of length `len` (array elements or
/// string codepoints). The start is rounded down and the end up.
fn parse_slice(len: usize, slice: &Object) -> Result<(usize, usize), Error> {
    // jv_object_get gives an invalid value for a missing key; only null
    // is replaced by a default.
    let start_jv = match slice.get("start") {
        Some(Value::Null) => Some(Value::number(0.0)),
        other => other.cloned(),
    };
    let end_jv = match slice.get("end") {
        Some(Value::Null) => Some(Value::number(len as f64)),
        other => other.cloned(),
    };
    let (dstart, dend) = match (&start_jv, &end_jv) {
        (Some(Value::Number(s)), Some(Value::Number(e))) => (s.value(), e.value()),
        _ => {
            return Err(Error::msg("Array/string slice indices must be integers"));
        }
    };
    let lenf = len as f64;
    let mut dstart = dstart;
    let mut dend = dend;
    if dstart.is_nan() {
        dstart = 0.0;
    }
    if dstart < 0.0 {
        dstart += lenf;
    }
    if dstart < 0.0 {
        dstart = 0.0;
    }
    if dstart > lenf {
        dstart = lenf;
    }
    let start: i64 = if dstart > i32::MAX as f64 {
        i32::MAX as i64
    } else {
        dstart as i32 as i64 // Rounds down
    };

    if dend.is_nan() {
        dend = lenf;
    }
    if dend < 0.0 {
        dend += lenf;
    }
    if dend < 0.0 {
        dend = start as f64;
    }
    let mut end: i64 = if dend > i32::MAX as f64 {
        i32::MAX as i64
    } else {
        dend as i32 as i64
    };
    let len = len as i64;
    if end > len {
        end = len;
    }
    if end < len && (end as f64) < dend {
        end += 1; // We round start down but round end up
    }
    if end < start {
        end = start;
    }
    debug_assert!(0 <= start && start <= end && end <= len);
    Ok((start as usize, end as usize))
}

impl Value {
    /// `jv_get` (`.[k]`): objects by string, arrays by number (truncated,
    /// negative from the end, out of range is `null`), slices by
    /// `{"start","end"}` objects, `array[array]` gives the indices of a
    /// sub-array, and `null[...]` is `null`.
    pub fn get(&self, k: &Value) -> Result<Value, Error> {
        match (self, k) {
            (Value::Object(o), Value::String(key)) => {
                Ok(o.get(key.as_str()).cloned().unwrap_or(Value::Null))
            }
            (Value::Array(a), Value::Number(n)) => {
                if n.is_nan() {
                    return Ok(Value::Null);
                }
                let mut idx = clamp_index(n.value());
                if idx < 0 {
                    idx += a.len() as i64;
                }
                if idx < 0 {
                    return Ok(Value::Null);
                }
                Ok(a.get(idx as usize).cloned().unwrap_or(Value::Null))
            }
            (Value::Array(a), Value::Object(slice)) => {
                let (start, end) = parse_slice(a.len(), slice)?;
                Ok(Value::Array(a.slice(start as i64, end as i64)))
            }
            (Value::String(s), Value::Object(slice)) => {
                let (start, end) = parse_slice(s.codepoint_len(), slice)?;
                Ok(Value::String(s.slice(start as i64, end as i64)))
            }
            (Value::Array(a), Value::Array(b)) => Ok(Value::Array(a.indexes(b))),
            (Value::Null, Value::String(_) | Value::Number(_) | Value::Object(_)) => {
                Ok(Value::Null)
            }
            _ => {
                // If k is a short string it's probably from a jq .foo
                // expression or similar.
                if let Value::String(key) = k
                    && key.len() < 30
                {
                    return Err(Error::msg(format!(
                        "Cannot index {} with string \"{}\"",
                        self.kind_name(),
                        key.as_c_str()
                    )));
                }
                Err(Error::msg(format!(
                    "Cannot index {} with {}",
                    self.kind_name(),
                    k.kind_name()
                )))
            }
        }
    }

    /// `jv_set` (`.[k] = v` on a single level). Updates in place when the
    /// container is uniquely owned; `null` becomes an object or array as
    /// needed.
    pub fn set(self, k: &Value, v: Value) -> Result<Value, Error> {
        let t = self;
        let isnull = t.is_null();
        match k {
            Value::String(key) if isnull || matches!(t, Value::Object(_)) => {
                let mut o = match t {
                    Value::Object(o) => o,
                    _ => Object::new(),
                };
                o.insert(key.clone(), v);
                Ok(Value::Object(o))
            }
            Value::Number(n) if isnull || matches!(t, Value::Array(_)) => {
                if n.is_nan() {
                    return Err(Error::msg("Cannot set array element at NaN index"));
                }
                let mut a = match t {
                    Value::Array(a) => a,
                    _ => Array::new(),
                };
                a.set(clamp_index(n.value()), v)?;
                Ok(Value::Array(a))
            }
            Value::Object(slice) if isnull || matches!(t, Value::Array(_)) => {
                let mut t = match t {
                    Value::Array(a) => a,
                    _ => Array::new(),
                };
                let (start, end) = parse_slice(t.len(), slice)?;
                let v = match v {
                    Value::Array(v) => v,
                    _ => {
                        return Err(Error::msg(
                            "A slice of an array can only be assigned another array",
                        ));
                    }
                };
                let array_len = t.len();
                let slice_len = end - start;
                let insert_len = v.len();
                if slice_len < insert_len {
                    // array is growing
                    let shift = insert_len - slice_len;
                    let mut i = array_len as i64 - 1;
                    while i >= end as i64 {
                        let x = t.get(i as usize).cloned().unwrap_or_default();
                        t.set(i + shift as i64, x)?;
                        i -= 1;
                    }
                } else if slice_len > insert_len {
                    // array is shrinking
                    let shift = slice_len - insert_len;
                    for i in end..array_len {
                        let x = t.get(i).cloned().unwrap_or_default();
                        t.set((i - shift) as i64, x)?;
                    }
                    t = t.into_slice(0, (array_len - shift) as i64);
                }
                for (i, x) in v.iter().enumerate() {
                    t.set((start + i) as i64, x.clone())?;
                }
                Ok(Value::Array(t))
            }
            Value::Object(_) if matches!(t, Value::String(_)) => {
                Err(Error::msg("Cannot update string slices"))
            }
            _ => Err(Error::msg(format!(
                "Cannot update field at {} index of {}",
                k.kind_name(),
                t.kind_name()
            ))),
        }
    }

    /// `jv_has` (`has(k)`).
    pub fn has(&self, k: &Value) -> Result<bool, Error> {
        match (self, k) {
            (Value::Null, _) => Ok(false),
            (Value::Object(o), Value::String(key)) => Ok(o.contains_key(key.as_str())),
            (Value::Array(a), Value::Number(n)) => {
                if n.is_nan() {
                    return Ok(false);
                }
                let idx = double_to_int(n.value());
                Ok(idx >= 0 && (idx as usize) < a.len())
            }
            _ => Err(Error::msg(format!(
                "Cannot check whether {} has a {} key",
                self.kind_name(),
                k.kind_name()
            ))),
        }
    }

    /// `jv_getpath` (`getpath(p)`): follows `path` with [`Value::get`];
    /// errors from `get` propagate.
    pub fn getpath(&self, path: &Value) -> Result<Value, Error> {
        let path = match path {
            Value::Array(p) => p,
            _ => return Err(Error::msg("Path must be specified as an array")),
        };
        let mut cur = self.clone();
        for k in path.iter() {
            cur = cur.get(k)?;
        }
        Ok(cur)
    }

    /// `jv_setpath` (`setpath(p; v)`), taking care (like jq) to drop the
    /// container's reference to the child before recursing so that nested
    /// updates happen in place.
    pub fn setpath(self, path: &Value, value: Value) -> Result<Value, Error> {
        let path = match path {
            Value::Array(p) => p,
            _ => return Err(Error::msg("Path must be specified as an array")),
        };
        setpath_rec(self, path.as_slice(), value)
    }

    /// `jv_delpaths` (`delpaths(ps)`): sorts the paths, then deletes them,
    /// deepest-and-last first as jq does.
    pub fn delpaths(self, paths: &Value) -> Result<Value, Error> {
        let paths = match paths {
            Value::Array(p) => p,
            _ => return Err(Error::msg("Paths must be specified as an array")),
        };
        let paths = sort(paths, paths);
        for p in paths.iter() {
            if !matches!(p, Value::Array(_)) {
                return Err(Error::msg(format!(
                    "Path must be specified as array, not {}",
                    p.kind_name()
                )));
            }
        }
        if paths.is_empty() {
            // nothing is being deleted
            return Ok(self);
        }
        if paths
            .get(0)
            .and_then(Value::as_array)
            .is_some_and(Array::is_empty)
        {
            // everything is being deleted
            return Ok(Value::Null);
        }
        delpaths_sorted(self, paths.as_slice(), 0)
    }

    /// `jv_keys` (`keys`): sorted object keys, or `[0..n)` for arrays.
    /// Other kinds give builtin.c's `"<kind> (<value>) has no keys"` error.
    pub fn keys(&self) -> Result<Value, Error> {
        match self {
            Value::Object(o) => {
                if o.is_empty() {
                    return Ok(Value::empty_array());
                }
                let mut keys: Vec<&Str> = o.keys().collect();
                keys.sort();
                let mut answer = Array::with_capacity(keys.len());
                for k in keys {
                    answer.push(Value::String(k.clone()));
                }
                Ok(Value::Array(answer))
            }
            Value::Array(a) => {
                let mut answer = Array::new();
                for i in 0..a.len() {
                    answer.push(Value::from(i));
                }
                Ok(Value::Array(answer))
            }
            _ => Err(Error::type_error(self, "has no keys")),
        }
    }

    /// `jv_keys_unsorted` (`keys_unsorted`): object keys in insertion order.
    pub fn keys_unsorted(&self) -> Result<Value, Error> {
        match self {
            Value::Object(o) => {
                let mut answer = Array::with_capacity(o.len());
                for k in o.keys() {
                    answer.push(Value::String(k.clone()));
                }
                Ok(Value::Array(answer))
            }
            _ => self.keys(),
        }
    }

    /// `jv_cmp`: jq's total order across kinds (null < false < true <
    /// numbers < strings < arrays < objects). Arrays compare
    /// lexicographically; objects compare their sorted key lists first, then
    /// the values key by key. A NaN compares below every number — including
    /// another NaN, so `nan < nan` (this order is not antisymmetric; use
    /// [`sort`] rather than a std sort with it).
    pub fn compare(&self, other: &Value) -> Ordering {
        cmp_impl(self, other, false)
    }
}

/// jv_cmp; with `total`, NaN == NaN (a consistent order for sorting).
fn cmp_impl(a: &Value, b: &Value, total: bool) -> Ordering {
    let (ka, kb) = (a.kind(), b.kind());
    if ka != kb {
        return ka.cmp(&kb);
    }
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
        (Value::Array(x), Value::Array(y)) => {
            // Lexical ordering of arrays
            for (xa, xb) in x.iter().zip(y.iter()) {
                let r = cmp_impl(xa, xb, total);
                if r != Ordering::Equal {
                    return r;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            let mut kx: Vec<&Str> = x.keys().collect();
            let mut ky: Vec<&Str> = y.keys().collect();
            kx.sort();
            ky.sort();
            // jv_cmp of the two key arrays (strings: bytewise, then length).
            let r = kx.cmp(&ky);
            if r != Ordering::Equal {
                return r;
            }
            for k in kx {
                let r = cmp_impl(&x[k], &y[k], total);
                if r != Ordering::Equal {
                    return r;
                }
            }
            Ordering::Equal
        }
        // null, false, true: there's only one of each of these values
        _ => Ordering::Equal,
    }
}

impl std::ops::Index<&Str> for Object {
    type Output = Value;
    fn index(&self, k: &Str) -> &Value {
        self.get(k.as_str()).expect("key present")
    }
}

fn setpath_rec(root: Value, path: &[Value], value: Value) -> Result<Value, Error> {
    let Some((pathcurr, pathrest)) = path.split_first() else {
        return Ok(value);
    };
    if matches!(pathcurr, Value::Object(_)) {
        // Assignment to slice -- dunno yet how to avoid the extra copy
        let sub = root.get(pathcurr)?;
        let newsub = setpath_rec(sub, pathrest, value)?;
        return root.set(pathcurr, newsub);
    }
    let subroot = root.get(pathcurr)?;
    // To avoid the extra copy we drop the reference from `root` by setting
    // that to null first.
    let root = root.set(pathcurr, Value::Null)?;
    let newsub = setpath_rec(subroot, pathrest, value)?;
    root.set(pathcurr, newsub)
}

/// Port of `jv_dels`: deletes the (sorted) keys from `t`.
fn dels(t: Value, keys: Vec<Value>) -> Result<Value, Error> {
    if t.is_null() || keys.is_empty() {
        return Ok(t);
    }
    match t {
        Value::Array(a) => {
            let mut neg_keys: Vec<f64> = Vec::new();
            let mut nonneg_keys: Vec<f64> = Vec::new();
            let mut starts: Vec<usize> = Vec::new();
            let mut ends: Vec<usize> = Vec::new();
            for key in &keys {
                match key {
                    Value::Number(n) => {
                        if n.value() < 0.0 {
                            neg_keys.push(n.value());
                        } else {
                            nonneg_keys.push(n.value());
                        }
                    }
                    Value::Object(slice) => {
                        let (start, end) = parse_slice(a.len(), slice)?;
                        starts.push(start);
                        ends.push(end);
                    }
                    _ => {
                        return Err(Error::msg(format!(
                            "Cannot delete {} element of array",
                            key.kind_name()
                        )));
                    }
                }
            }
            let len = a.len() as i64;
            let mut new_array = Array::new();
            let mut neg_idx = 0usize;
            let mut nonneg_idx = 0usize;
            for i in 0..len {
                let mut del = false;
                while neg_idx < neg_keys.len() {
                    let delidx = len + double_to_int(neg_keys[neg_idx]);
                    if i == delidx {
                        del = true;
                    }
                    if i < delidx {
                        break;
                    }
                    neg_idx += 1;
                }
                while nonneg_idx < nonneg_keys.len() {
                    let delidx = double_to_int(nonneg_keys[nonneg_idx]);
                    if i == delidx {
                        del = true;
                    }
                    if i < delidx {
                        break;
                    }
                    nonneg_idx += 1;
                }
                if !del {
                    for (s, e) in starts.iter().zip(ends.iter()) {
                        if (*s as i64) <= i && i < *e as i64 {
                            del = true;
                            break;
                        }
                    }
                }
                if !del {
                    new_array.push(a.get(i as usize).cloned().unwrap_or_default());
                }
            }
            Ok(Value::Array(new_array))
        }
        Value::Object(mut o) => {
            for k in &keys {
                match k {
                    Value::String(key) => {
                        o.remove(key.as_str());
                    }
                    _ => {
                        return Err(Error::msg(format!(
                            "Cannot delete {} field of object",
                            k.kind_name()
                        )));
                    }
                }
            }
            Ok(Value::Object(o))
        }
        _ => Err(Error::msg(format!(
            "Cannot delete fields from {}",
            t.kind_name()
        ))),
    }
}

/// Port of `delpaths_sorted`: `paths` is sorted and every path is longer
/// than `start`.
fn delpaths_sorted(object: Value, paths: &[Value], start: usize) -> Result<Value, Error> {
    let mut object = object;
    let mut delkeys: Vec<Value> = Vec::new();
    let path_at = |i: usize| -> &Array { paths[i].as_array().expect("paths are arrays") };
    let mut i = 0;
    while i < paths.len() {
        let mut j = i;
        debug_assert!(path_at(i).len() > start);
        let delkey = path_at(i).len() == start + 1;
        let key = path_at(i).get(start).cloned().unwrap_or_default();
        while j < paths.len() && path_at(j).get(start).is_some_and(|k| key.equal(k)) {
            j += 1;
        }
        // Deviation: a NaN key is unequal to itself, so jq 1.8.1 never
        // advances here and loops forever (`[1] | delpaths([[nan]])` hangs).
        // Treat such a key as a group of one instead.
        if j == i {
            j = i + 1;
        }
        // if i <= entry < j, then entry starts with key
        if delkey {
            // deleting this entire key, we don't care about any more specific deletions
            delkeys.push(key);
        } else {
            // deleting certain sub-parts of this key
            let subobject = object.get(&key)?;
            if !subobject.is_null() {
                let newsubobject = delpaths_sorted(subobject, &paths[i..j], start + 1)?;
                object = object.set(&key, newsubobject)?;
            }
        }
        i = j;
    }
    dels(object, delkeys)
}

/// jq's `sort_items`: indices of `keys` in sorted order, stable.
fn sort_items(keys: &Array) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..keys.len()).collect();
    let k = keys.as_slice();
    // jq's sort_cmp: jv_cmp, then the index (which makes the sort stable).
    let sort_cmp = |a: usize, b: usize| cmp_impl(&k[a], &k[b], false).then(a.cmp(&b));
    if super::qsort::needs_platform_qsort(k) {
        // jv_cmp is not a consistent order here, so the result depends on
        // the sorting algorithm: use the C library's qsort, as jq does.
        super::qsort::platform_qsort(&mut idx, &sort_cmp);
    } else {
        // A strict total order: every correct sort gives jq's result.
        idx.sort_by(|&a, &b| sort_cmp(a, b));
    }
    idx
}

/// `jv_sort(objects, keys)`: `objects` reordered by `keys` (same length),
/// stably. (`sort` is `jv_sort(x, x)`; `sort_by(f)` uses `[f]` keys.)
///
/// jq sorts with `qsort` and `jv_cmp`, whose NaN handling is not a
/// consistent order; here NaNs compare equal to each other, which gives the
/// stable result every `qsort` produces on small inputs.
pub fn sort(objects: &Array, keys: &Array) -> Array {
    debug_assert_eq!(objects.len(), keys.len());
    let order = sort_items(keys);
    let mut ret = Array::new();
    for i in order {
        ret.push(objects.get(i).cloned().unwrap_or_default());
    }
    ret
}

/// `jv_group(objects, keys)`: sorts like [`sort`], then groups runs of
/// `jv_equal` keys (`group_by`).
pub fn group(objects: &Array, keys: &Array) -> Array {
    debug_assert_eq!(objects.len(), keys.len());
    let order = sort_items(keys);
    let mut ret = Array::new();
    let mut iter = order.into_iter();
    if let Some(first) = iter.next() {
        let mut curr_key = &keys.as_slice()[first];
        let mut group = Array::new();
        group.push(objects.get(first).cloned().unwrap_or_default());
        for i in iter {
            let k = &keys.as_slice()[i];
            if !curr_key.equal(k) {
                curr_key = k;
                ret.push(Value::Array(std::mem::take(&mut group)));
            }
            group.push(objects.get(i).cloned().unwrap_or_default());
        }
        ret.push(Value::Array(group));
    }
    ret
}

/// `jv_unique(objects, keys)`: sorts like [`sort`], keeping the first
/// element of each run of `jv_equal` keys (`unique`, `unique_by`).
pub fn unique(objects: &Array, keys: &Array) -> Array {
    debug_assert_eq!(objects.len(), keys.len());
    let order = sort_items(keys);
    let mut ret = Array::new();
    let mut curr_key: Option<&Value> = None;
    for i in order {
        let k = &keys.as_slice()[i];
        if curr_key.is_some_and(|c| c.equal(k)) {
            continue;
        }
        curr_key = Some(k);
        ret.push(objects.get(i).cloned().unwrap_or_default());
    }
    ret
}
