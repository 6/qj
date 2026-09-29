//! `to_entries`, `from_entries` and `with_entries(f)`.
//!
//! ```jq
//! def to_entries: [keys_unsorted[] as $k | {key: $k, value: .[$k]}];
//! def from_entries: map({(.key // .Key // .name // .Name): (if has("value") then .value else .Value end)}) | add | .//={};
//! def with_entries(f): to_entries | map(f) | from_entries;
//! ```

use super::{cannot_iterate, empty_array, empty_object, string};
use crate::jq::builtins::binops::binop_plus;
use crate::jq::lang::execute::Jq;
use crate::jq::lang::execute::native::{Closure, ConstRef, ConstView, Pool, Stop};
use crate::jq::value::{Error, Object, Str, Value, dump_string_trunc};

/// to_entries' constants: its collect's `[]`, then `"key"` and `"value"`.
pub(super) fn to_entries_consts(pool: &Pool<'_>) -> Option<Vec<ConstRef>> {
    Some(vec![
        empty_array(pool, 0)?,
        string(pool, 0, "key")?,
        string(pool, 0, "value")?,
    ])
}

/// from_entries' constants: the `{}` of `.//={}` (its `$tmp`), then the keys it
/// looks up.
pub(super) fn from_entries_consts(pool: &Pool<'_>) -> Option<Vec<ConstRef>> {
    let mut c = vec![empty_object(pool, 0)?];
    for k in ["key", "Key", "name", "Name", "value", "Value"] {
        c.push(ConstRef::Own(Value::from(k)));
    }
    Some(c)
}

fn as_str(v: &Value) -> &Str {
    match v {
        Value::String(s) => s,
        _ => unreachable!("string constant"),
    }
}

/// `to_entries`. `c`: [`to_entries_consts`].
pub(super) fn to_entries(input: Value, c: ConstView<'_>) -> Result<Value, Stop> {
    // `keys_unsorted` (its error), then one `{key: $k, value: .[$k]}` per key, collected
    // into the definition's `[]`.
    let Value::Array(mut out) = c.get(0).clone() else {
        unreachable!()
    };
    let (key, value) = (as_str(c.get(1)), as_str(c.get(2)));
    match &input {
        Value::Object(o) => {
            for (k, v) in o.iter() {
                let mut e = Object::with_capacity(2);
                e.insert(key.clone(), Value::String(k.clone()));
                e.insert(value.clone(), v.clone());
                out.push(Value::Object(e));
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                let mut e = Object::with_capacity(2);
                e.insert(key.clone(), Value::from(i));
                e.insert(value.clone(), v.clone());
                out.push(Value::Object(e));
            }
        }
        _ => return Err(Error::type_error(&input, "has no keys").into()),
    }
    Ok(Value::Array(out))
}

/// `from_entries`. `c`: [`from_entries_consts`].
pub(super) fn from_entries(vm: &mut Jq, input: Value, c: ConstView<'_>) -> Result<Value, Stop> {
    let entries: &[Value] = match &input {
        Value::Array(a) => a.as_slice(),
        Value::Object(_) => &[],
        _ => return Err(cannot_iterate(&input)),
    };
    let mut acc = Value::Null;
    let mut entry = |e: &Value| -> Result<(), Stop> {
        // `{(K): V}`: K, then V, then the key check (INSERT), each entry in turn; `add`
        // then merges them (it can't fail on objects).
        let mut k = e.get(c.get(1))?;
        for name in 2..5 {
            if k.is_truthy() {
                break;
            }
            k = e.get(c.get(name))?;
        }
        let v = if e.has(c.get(5))? {
            e.get(c.get(5))?
        } else {
            e.get(c.get(6))?
        };
        let Value::String(k) = k else {
            return Err(Error::msg(format!(
                "Cannot use {} ({}) as object key",
                k.kind_name(),
                dump_string_trunc(&k, 15)
            ))
            .into());
        };
        let mut o = Object::with_capacity(1);
        o.insert(k, v);
        acc = binop_plus(std::mem::take(&mut acc), Value::Object(o))?;
        Ok(())
    };
    match &input {
        Value::Object(o) => {
            for e in o.values() {
                entry(e)?;
            }
        }
        _ => {
            for e in entries {
                entry(e)?;
            }
        }
    }
    // `.//={}` is `_modify(.; . // $tmp)`: one label, and `$tmp` itself for `null`.
    vm.gen_labels(1);
    Ok(if acc.is_truthy() {
        acc
    } else {
        c.get(0).clone()
    })
}

/// `with_entries(f)`. `c`: [`to_entries_consts`], map's `[]`, [`from_entries_consts`].
pub(super) fn with_entries(
    vm: &mut Jq,
    input: Value,
    f: Closure,
    c: ConstView<'_>,
) -> Result<Value, Stop> {
    let Value::Array(entries) = to_entries(input, c)? else {
        unreachable!()
    };
    // `map(f)`: every output of `f` on each entry, collected into map's `[]`.
    let Value::Array(mut out) = c.get(3).clone() else {
        unreachable!()
    };
    for e in entries.iter() {
        vm.sub_each(f, e.clone(), |_, v| {
            out.push(v);
            Ok(())
        })?;
    }
    drop(entries);
    from_entries(vm, Value::Array(out), c.from(4))
}
