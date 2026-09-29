//! jq's `sort_items` calls the C library's `qsort` with a comparator built
//! on `jv_cmp`. When `jv_cmp` is a consistent order, every correct sort
//! gives the same result (jq breaks ties by index), so the port sorts with
//! Rust. But `jv_cmp` is *not* consistent in two situations, and then jq's
//! output depends on the platform's `qsort` algorithm:
//!
//! * NaN: `jv_cmp(nan, nan) < 0` whichever way round;
//! * literals that differ as decimals but round to the same double, mixed
//!   with native numbers (literal vs literal compares decimals, anything
//!   else compares doubles, so equality is not transitive).
//!
//! For those inputs this module calls the platform `qsort` itself, exactly
//! like jq, so the result matches the jq binary of the same platform
//! (verified against macOS's libc; glibc builds match a Linux jq).

use std::cell::Cell;
use std::cmp::Ordering;
use std::ffi::{c_int, c_void};

use super::Value;

thread_local! {
    /// The comparator of the sort in progress (qsort has no context
    /// argument that is portable across libcs).
    static CMP: Cell<Option<*const dyn Fn(usize, usize) -> Ordering>> = const { Cell::new(None) };
}

extern "C" fn compare_indices(a: *const c_void, b: *const c_void) -> c_int {
    // SAFETY: qsort passes pointers into the index slice we gave it.
    let (ia, ib) = unsafe { (*(a as *const usize), *(b as *const usize)) };
    let f = CMP.with(|c| c.get()).expect("comparator set during qsort");
    // SAFETY: the pointer is set by `platform_qsort` for the duration of
    // the qsort call, and the closure outlives that call.
    match unsafe { (*f)(ia, ib) } {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// Sorts `idx` with the C library's `qsort` and the comparator `cmp`
/// (which receives the index values, like jq's `sort_cmp` receives
/// entries).
pub(crate) fn platform_qsort(idx: &mut [usize], cmp: &dyn Fn(usize, usize) -> Ordering) {
    if idx.len() < 2 {
        return;
    }
    // Erase the lifetime to store the comparator in the thread-local; it is
    // only used while this function runs.
    let ptr: *const (dyn Fn(usize, usize) -> Ordering + '_) = cmp;
    // SAFETY: transmuting only the trait-object lifetime; the value is
    // cleared (restored) before `cmp` goes out of scope.
    let ptr: *const (dyn Fn(usize, usize) -> Ordering + 'static) =
        unsafe { std::mem::transmute(ptr) };
    let prev = CMP.with(|c| c.replace(Some(ptr)));
    // SAFETY: `idx` is a valid, properly aligned slice of usize and the
    // comparator only reads elements of it.
    unsafe {
        libc::qsort(
            idx.as_mut_ptr() as *mut c_void,
            idx.len(),
            std::mem::size_of::<usize>(),
            Some(compare_indices),
        );
    }
    CMP.with(|c| c.set(prev));
}

/// Whether sorting these keys with `jv_cmp` needs the platform `qsort` to
/// reproduce jq (see the module docs).
pub(crate) fn needs_platform_qsort(keys: &[Value]) -> bool {
    let (mut native, mut lossy_literal) = (false, false);
    // Iterative walk: keys can be deeply nested.
    let mut work: Vec<&Value> = keys.iter().rev().collect();
    while let Some(v) = work.pop() {
        match v {
            Value::Number(n) => {
                if n.is_nan() {
                    return true;
                } else if n.is_literal() {
                    lossy_literal |= n.is_lossy_literal();
                } else {
                    native = true;
                }
                if native && lossy_literal {
                    return true;
                }
            }
            Value::Array(a) => work.extend(a.iter().rev()),
            Value::Object(o) => work.extend(o.values().rev()),
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_qsort_sorts() {
        let keys = [5, 3, 9, 1, 3, 7, 2, 8, 6, 0, 4];
        let mut idx: Vec<usize> = (0..keys.len()).collect();
        platform_qsort(&mut idx, &|a, b| keys[a].cmp(&keys[b]).then(a.cmp(&b)));
        let sorted: Vec<i32> = idx.iter().map(|&i| keys[i]).collect();
        assert_eq!(sorted, [0, 1, 2, 3, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(idx[3], 1); // stable via the index tie-break
    }
}
