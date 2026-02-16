//! Zero-copy navigation of the flat token buffer produced by simdjson bridge.
//!
//! `FlatValue<'a>` is a lightweight `Copy`-able view into the flat buffer that
//! lets the evaluator navigate objects and arrays without heap-allocating a full
//! `Value` tree. Only the subtrees actually accessed by the filter get
//! materialized via `to_value()`.

use crate::simdjson::{
    TAG_ARRAY_START, TAG_BOOL, TAG_DOUBLE, TAG_INT, TAG_NULL, TAG_OBJECT_START, TAG_STRING,
};
use crate::value::Value;

const TAG_ARRAY_END: u8 = 6;
const TAG_OBJECT_END: u8 = 8;

/// A zero-copy, `Copy`-able view into a flat token buffer.
///
/// Points to a specific value within the buffer. Navigation methods like
/// `get_field` and `array_iter` return new `FlatValue`s pointing deeper
/// into the same buffer, without any allocation.
#[derive(Clone, Copy)]
pub struct FlatValue<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> FlatValue<'a> {
    /// Create a new FlatValue pointing at position `pos` in `buf`.
    #[inline]
    pub fn new(buf: &'a [u8], pos: usize) -> Self {
        Self { buf, pos }
    }

    /// Read the type tag at the current position.
    #[inline]
    pub fn tag(&self) -> u8 {
        self.buf[self.pos]
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.tag() == TAG_NULL
    }
    #[inline]
    pub fn is_bool(&self) -> bool {
        self.tag() == TAG_BOOL
    }
    #[inline]
    pub fn is_int(&self) -> bool {
        self.tag() == TAG_INT
    }
    #[inline]
    pub fn is_double(&self) -> bool {
        self.tag() == TAG_DOUBLE
    }
    #[inline]
    pub fn is_string(&self) -> bool {
        self.tag() == TAG_STRING
    }
    #[inline]
    pub fn is_array(&self) -> bool {
        self.tag() == TAG_ARRAY_START
    }
    #[inline]
    pub fn is_object(&self) -> bool {
        self.tag() == TAG_OBJECT_START
    }

    /// Read a bool value. Returns None if not a bool.
    pub fn as_bool(&self) -> Option<bool> {
        if self.tag() != TAG_BOOL {
            return None;
        }
        Some(self.buf[self.pos + 1] != 0)
    }

    /// Read an i64 value. Returns None if not an int.
    pub fn as_int(&self) -> Option<i64> {
        if self.tag() != TAG_INT {
            return None;
        }
        let start = self.pos + 1;
        Some(i64::from_le_bytes(
            self.buf[start..start + 8].try_into().unwrap(),
        ))
    }

    /// Read a f64 value with optional raw text. Returns None if not a double.
    pub fn as_f64(&self) -> Option<(f64, Option<&'a str>)> {
        if self.tag() != TAG_DOUBLE {
            return None;
        }
        let start = self.pos + 1;
        let f = f64::from_le_bytes(self.buf[start..start + 8].try_into().unwrap());
        let raw_len =
            u32::from_le_bytes(self.buf[start + 8..start + 12].try_into().unwrap()) as usize;
        let raw = if raw_len > 0 {
            Some(std::str::from_utf8(&self.buf[start + 12..start + 12 + raw_len]).unwrap())
        } else {
            None
        };
        Some((f, raw))
    }

    /// Read a string value as a zero-copy reference into the buffer.
    pub fn as_str(&self) -> Option<&'a str> {
        if self.tag() != TAG_STRING {
            return None;
        }
        let start = self.pos + 1;
        let len = u32::from_le_bytes(self.buf[start..start + 4].try_into().unwrap()) as usize;
        Some(std::str::from_utf8(&self.buf[start + 4..start + 4 + len]).unwrap())
    }

    /// Returns true if this is an empty container or null.
    pub fn is_empty(&self) -> bool {
        self.len().is_some_and(|n| n == 0)
    }

    /// Get the element count for arrays/objects, or string byte length for strings.
    /// Returns None for scalars (null, bool, int, double).
    pub fn len(&self) -> Option<usize> {
        match self.tag() {
            TAG_ARRAY_START | TAG_OBJECT_START => {
                let start = self.pos + 1;
                Some(u32::from_le_bytes(self.buf[start..start + 4].try_into().unwrap()) as usize)
            }
            TAG_STRING => {
                let start = self.pos + 1;
                Some(u32::from_le_bytes(self.buf[start..start + 4].try_into().unwrap()) as usize)
            }
            TAG_NULL => Some(0),
            _ => None,
        }
    }

    /// Look up a field in an object by key. Returns None if not found or not an object.
    pub fn get_field(&self, key: &str) -> Option<FlatValue<'a>> {
        if self.tag() != TAG_OBJECT_START {
            return None;
        }
        let count =
            u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap()) as usize;

        let mut pos = self.pos + 5; // past tag + count
        for _ in 0..count {
            // Each entry: TAG_STRING + u32 len + key bytes + value
            debug_assert_eq!(self.buf[pos], TAG_STRING);
            let key_len =
                u32::from_le_bytes(self.buf[pos + 1..pos + 5].try_into().unwrap()) as usize;
            let entry_key = std::str::from_utf8(&self.buf[pos + 5..pos + 5 + key_len]).unwrap();

            let value_pos = pos + 5 + key_len;
            if entry_key == key {
                return Some(FlatValue::new(self.buf, value_pos));
            }
            // Skip this value to get to the next entry
            let value = FlatValue::new(self.buf, value_pos);
            pos = value_pos + value.skip_bytes();
        }
        None
    }

    /// Index into an array. Returns None if index is out of bounds or not an array.
    pub fn get_index(&self, idx: usize) -> Option<FlatValue<'a>> {
        if self.tag() != TAG_ARRAY_START {
            return None;
        }
        let count =
            u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap()) as usize;
        if idx >= count {
            return None;
        }

        let mut pos = self.pos + 5; // past tag + count
        for i in 0..count {
            if i == idx {
                return Some(FlatValue::new(self.buf, pos));
            }
            let elem = FlatValue::new(self.buf, pos);
            pos += elem.skip_bytes();
        }
        None
    }

    /// Iterate over array elements.
    pub fn array_iter(&self) -> FlatArrayIter<'a> {
        if self.tag() != TAG_ARRAY_START {
            return FlatArrayIter {
                buf: self.buf,
                pos: 0,
                remaining: 0,
            };
        }
        let count =
            u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap()) as usize;
        FlatArrayIter {
            buf: self.buf,
            pos: self.pos + 5,
            remaining: count,
        }
    }

    /// Iterate over object key-value pairs.
    pub fn object_iter(&self) -> FlatObjectIter<'a> {
        if self.tag() != TAG_OBJECT_START {
            return FlatObjectIter {
                buf: self.buf,
                pos: 0,
                remaining: 0,
            };
        }
        let count =
            u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap()) as usize;
        FlatObjectIter {
            buf: self.buf,
            pos: self.pos + 5,
            remaining: count,
        }
    }

    /// Compute the byte size of this value in the flat buffer (including tag).
    /// Used to skip over values during iteration without materializing them.
    pub fn skip_bytes(&self) -> usize {
        match self.tag() {
            TAG_NULL => 1,
            TAG_BOOL => 2,
            TAG_INT => 9, // tag + 8 bytes i64
            TAG_DOUBLE => {
                // tag + 8 bytes f64 + 4 bytes raw_len + raw_len bytes
                let raw_len =
                    u32::from_le_bytes(self.buf[self.pos + 9..self.pos + 13].try_into().unwrap())
                        as usize;
                13 + raw_len
            }
            TAG_STRING => {
                // tag + 4 bytes len + len bytes
                let len =
                    u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap())
                        as usize;
                5 + len
            }
            TAG_ARRAY_START => {
                let count =
                    u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap())
                        as usize;
                let mut size = 5; // tag + count
                let mut p = self.pos + 5;
                for _ in 0..count {
                    let elem = FlatValue::new(self.buf, p);
                    let s = elem.skip_bytes();
                    size += s;
                    p += s;
                }
                size + 1 // + TAG_ARRAY_END
            }
            TAG_OBJECT_START => {
                let count =
                    u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap())
                        as usize;
                let mut size = 5; // tag + count
                let mut p = self.pos + 5;
                for _ in 0..count {
                    // key: TAG_STRING + u32 len + bytes
                    let key_fv = FlatValue::new(self.buf, p);
                    let ks = key_fv.skip_bytes();
                    size += ks;
                    p += ks;
                    // value
                    let val_fv = FlatValue::new(self.buf, p);
                    let vs = val_fv.skip_bytes();
                    size += vs;
                    p += vs;
                }
                size + 1 // + TAG_OBJECT_END
            }
            TAG_ARRAY_END | TAG_OBJECT_END => 1,
            _ => 1, // unknown tag, shouldn't happen
        }
    }

    /// Materialize this FlatValue into a full `Value` tree.
    ///
    /// This allocates — use only when the evaluator actually needs a concrete Value.
    pub fn to_value(&self) -> Value {
        let mut pos = self.pos;
        crate::simdjson::decode_value(self.buf, &mut pos).unwrap_or(Value::Null)
    }

    /// jq type name for this value.
    pub fn type_name(&self) -> &'static str {
        match self.tag() {
            TAG_NULL => "null",
            TAG_BOOL => "boolean",
            TAG_INT | TAG_DOUBLE => "number",
            TAG_STRING => "string",
            TAG_ARRAY_START => "array",
            TAG_OBJECT_START => "object",
            _ => "null",
        }
    }

    /// jq truthiness: only null and false are falsy.
    pub fn is_truthy(&self) -> bool {
        match self.tag() {
            TAG_NULL => false,
            TAG_BOOL => self.buf[self.pos + 1] != 0,
            _ => true,
        }
    }
}

/// Iterator over array elements in a flat buffer.
pub struct FlatArrayIter<'a> {
    buf: &'a [u8],
    pos: usize,
    remaining: usize,
}

impl<'a> Iterator for FlatArrayIter<'a> {
    type Item = FlatValue<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let fv = FlatValue::new(self.buf, self.pos);
        self.pos += fv.skip_bytes();
        self.remaining -= 1;
        Some(fv)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for FlatArrayIter<'_> {}

/// Iterator over object key-value pairs in a flat buffer.
pub struct FlatObjectIter<'a> {
    buf: &'a [u8],
    pos: usize,
    remaining: usize,
}

impl<'a> Iterator for FlatObjectIter<'a> {
    type Item = (&'a str, FlatValue<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        // Key: TAG_STRING + u32 len + bytes
        debug_assert_eq!(self.buf[self.pos], TAG_STRING);
        let key_len =
            u32::from_le_bytes(self.buf[self.pos + 1..self.pos + 5].try_into().unwrap()) as usize;
        let key = std::str::from_utf8(&self.buf[self.pos + 5..self.pos + 5 + key_len]).unwrap();
        let value_pos = self.pos + 5 + key_len;

        let value = FlatValue::new(self.buf, value_pos);
        self.pos = value_pos + value.skip_bytes();
        self.remaining -= 1;

        Some((key, value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for FlatObjectIter<'_> {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Encode a Value into the flat token format (for tests without FFI).
    fn encode_to_flat(value: &Value) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_value(&mut buf, value);
        buf
    }

    fn encode_value(buf: &mut Vec<u8>, value: &Value) {
        match value {
            Value::Null => buf.push(TAG_NULL),
            Value::Bool(b) => {
                buf.push(TAG_BOOL);
                buf.push(if *b { 1 } else { 0 });
            }
            Value::Int(n) => {
                buf.push(TAG_INT);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Double(f, raw) => {
                buf.push(TAG_DOUBLE);
                buf.extend_from_slice(&f.to_le_bytes());
                match raw {
                    Some(s) => {
                        buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                        buf.extend_from_slice(s.as_bytes());
                    }
                    None => {
                        buf.extend_from_slice(&0u32.to_le_bytes());
                    }
                }
            }
            Value::String(s) => {
                buf.push(TAG_STRING);
                buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
                buf.extend_from_slice(s.as_bytes());
            }
            Value::Array(arr) => {
                buf.push(TAG_ARRAY_START);
                buf.extend_from_slice(&(arr.len() as u32).to_le_bytes());
                for elem in arr.iter() {
                    encode_value(buf, elem);
                }
                buf.push(TAG_ARRAY_END);
            }
            Value::Object(pairs) => {
                buf.push(TAG_OBJECT_START);
                buf.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
                for (key, val) in pairs.iter() {
                    buf.push(TAG_STRING);
                    buf.extend_from_slice(&(key.len() as u32).to_le_bytes());
                    buf.extend_from_slice(key.as_bytes());
                    encode_value(buf, val);
                }
                buf.push(TAG_OBJECT_END);
            }
        }
    }

    // --- Scalar navigation ---

    #[test]
    fn null_navigation() {
        let buf = encode_to_flat(&Value::Null);
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.is_null());
        assert!(!fv.is_bool());
        assert_eq!(fv.tag(), TAG_NULL);
        assert_eq!(fv.type_name(), "null");
        assert!(!fv.is_truthy());
        assert_eq!(fv.skip_bytes(), 1);
        assert_eq!(fv.to_value(), Value::Null);
    }

    #[test]
    fn bool_navigation() {
        for b in [true, false] {
            let buf = encode_to_flat(&Value::Bool(b));
            let fv = FlatValue::new(&buf, 0);
            assert!(fv.is_bool());
            assert_eq!(fv.as_bool(), Some(b));
            assert_eq!(fv.type_name(), "boolean");
            assert_eq!(fv.is_truthy(), b);
            assert_eq!(fv.skip_bytes(), 2);
            assert_eq!(fv.to_value(), Value::Bool(b));
        }
    }

    #[test]
    fn int_navigation() {
        for n in [0i64, 1, -1, i64::MAX, i64::MIN, 42] {
            let buf = encode_to_flat(&Value::Int(n));
            let fv = FlatValue::new(&buf, 0);
            assert!(fv.is_int());
            assert_eq!(fv.as_int(), Some(n));
            assert_eq!(fv.type_name(), "number");
            assert!(fv.is_truthy());
            assert_eq!(fv.skip_bytes(), 9);
            assert_eq!(fv.to_value(), Value::Int(n));
        }
    }

    #[test]
    fn double_no_raw() {
        let buf = encode_to_flat(&Value::Double(3.14, None));
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.is_double());
        let (f, raw) = fv.as_f64().unwrap();
        assert!((f - 3.14).abs() < f64::EPSILON);
        assert!(raw.is_none());
        assert_eq!(fv.skip_bytes(), 13);
        assert_eq!(fv.to_value(), Value::Double(3.14, None));
    }

    #[test]
    fn double_with_raw() {
        let buf = encode_to_flat(&Value::Double(75.80, Some("75.80".into())));
        let fv = FlatValue::new(&buf, 0);
        let (f, raw) = fv.as_f64().unwrap();
        assert!((f - 75.80).abs() < f64::EPSILON);
        assert_eq!(raw, Some("75.80"));
        assert_eq!(fv.skip_bytes(), 13 + 5); // 13 base + 5 bytes "75.80"
    }

    #[test]
    fn string_navigation() {
        let buf = encode_to_flat(&Value::String("hello".into()));
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.is_string());
        assert_eq!(fv.as_str(), Some("hello"));
        assert_eq!(fv.type_name(), "string");
        assert!(fv.is_truthy());
        assert_eq!(fv.skip_bytes(), 5 + 5); // tag + u32 + "hello"
        assert_eq!(fv.to_value(), Value::String("hello".into()));
    }

    #[test]
    fn string_zero_copy() {
        let buf = encode_to_flat(&Value::String("test".into()));
        let fv = FlatValue::new(&buf, 0);
        let s = fv.as_str().unwrap();
        // Verify the string points into the buffer (zero-copy)
        let s_ptr = s.as_ptr();
        let buf_start = buf.as_ptr();
        let buf_end = unsafe { buf_start.add(buf.len()) };
        assert!(s_ptr >= buf_start && s_ptr < buf_end);
    }

    #[test]
    fn empty_string() {
        let buf = encode_to_flat(&Value::String(String::new()));
        let fv = FlatValue::new(&buf, 0);
        assert_eq!(fv.as_str(), Some(""));
        assert_eq!(fv.skip_bytes(), 5); // tag + u32 + 0 bytes
    }

    // --- Container navigation ---

    #[test]
    fn empty_array() {
        let buf = encode_to_flat(&Value::Array(Arc::new(vec![])));
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.is_array());
        assert_eq!(fv.len(), Some(0));
        assert_eq!(fv.type_name(), "array");
        assert!(fv.is_truthy());
        assert_eq!(fv.array_iter().count(), 0);
        assert_eq!(fv.skip_bytes(), 6); // tag + u32(0) + end tag
    }

    #[test]
    fn array_get_index() {
        let arr = Value::Array(Arc::new(vec![
            Value::Int(10),
            Value::String("two".into()),
            Value::Bool(true),
        ]));
        let buf = encode_to_flat(&arr);
        let fv = FlatValue::new(&buf, 0);
        assert_eq!(fv.len(), Some(3));

        let e0 = fv.get_index(0).unwrap();
        assert_eq!(e0.as_int(), Some(10));

        let e1 = fv.get_index(1).unwrap();
        assert_eq!(e1.as_str(), Some("two"));

        let e2 = fv.get_index(2).unwrap();
        assert_eq!(e2.as_bool(), Some(true));

        assert!(fv.get_index(3).is_none());
    }

    #[test]
    fn array_iteration() {
        let arr = Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
        let buf = encode_to_flat(&arr);
        let fv = FlatValue::new(&buf, 0);

        let vals: Vec<i64> = fv.array_iter().map(|e| e.as_int().unwrap()).collect();
        assert_eq!(vals, vec![1, 2, 3]);
    }

    #[test]
    fn empty_object() {
        let buf = encode_to_flat(&Value::Object(Arc::new(vec![])));
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.is_object());
        assert_eq!(fv.len(), Some(0));
        assert_eq!(fv.type_name(), "object");
        assert_eq!(fv.object_iter().count(), 0);
        assert_eq!(fv.skip_bytes(), 6); // tag + u32(0) + end tag
    }

    #[test]
    fn object_get_field() {
        let obj = Value::Object(Arc::new(vec![
            ("name".into(), Value::String("alice".into())),
            ("age".into(), Value::Int(30)),
            ("active".into(), Value::Bool(true)),
        ]));
        let buf = encode_to_flat(&obj);
        let fv = FlatValue::new(&buf, 0);

        let name = fv.get_field("name").unwrap();
        assert_eq!(name.as_str(), Some("alice"));

        let age = fv.get_field("age").unwrap();
        assert_eq!(age.as_int(), Some(30));

        let active = fv.get_field("active").unwrap();
        assert_eq!(active.as_bool(), Some(true));

        assert!(fv.get_field("missing").is_none());
    }

    #[test]
    fn object_iteration() {
        let obj = Value::Object(Arc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
        ]));
        let buf = encode_to_flat(&obj);
        let fv = FlatValue::new(&buf, 0);

        let pairs: Vec<(&str, i64)> = fv
            .object_iter()
            .map(|(k, v)| (k, v.as_int().unwrap()))
            .collect();
        assert_eq!(pairs, vec![("a", 1), ("b", 2)]);
    }

    #[test]
    fn nested_navigation() {
        let obj = Value::Object(Arc::new(vec![(
            "a".into(),
            Value::Object(Arc::new(vec![("b".into(), Value::Int(42))])),
        )]));
        let buf = encode_to_flat(&obj);
        let fv = FlatValue::new(&buf, 0);

        let b = fv.get_field("a").unwrap().get_field("b").unwrap();
        assert_eq!(b.as_int(), Some(42));
    }

    #[test]
    fn deeply_nested_navigation() {
        let inner = Value::Object(Arc::new(vec![("c".into(), Value::String("deep".into()))]));
        let mid = Value::Object(Arc::new(vec![("b".into(), inner)]));
        let outer = Value::Object(Arc::new(vec![("a".into(), mid)]));
        let buf = encode_to_flat(&outer);
        let fv = FlatValue::new(&buf, 0);

        let c = fv
            .get_field("a")
            .unwrap()
            .get_field("b")
            .unwrap()
            .get_field("c")
            .unwrap();
        assert_eq!(c.as_str(), Some("deep"));
    }

    #[test]
    fn get_field_on_non_object() {
        let buf = encode_to_flat(&Value::Int(42));
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.get_field("x").is_none());
    }

    #[test]
    fn get_index_on_non_array() {
        let buf = encode_to_flat(&Value::String("hi".into()));
        let fv = FlatValue::new(&buf, 0);
        assert!(fv.get_index(0).is_none());
    }

    // --- to_value equivalence ---

    #[test]
    fn to_value_scalars() {
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(42),
            Value::Int(-1),
            Value::Double(3.14, None),
            Value::Double(75.80, Some("75.80".into())),
            Value::String("hello world".into()),
            Value::String(String::new()),
        ];
        for v in values {
            let buf = encode_to_flat(&v);
            let fv = FlatValue::new(&buf, 0);
            assert_eq!(fv.to_value(), v, "to_value mismatch for {:?}", v);
        }
    }

    #[test]
    fn to_value_containers() {
        let values = vec![
            Value::Array(Arc::new(vec![])),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])),
            Value::Object(Arc::new(vec![])),
            Value::Object(Arc::new(vec![
                ("x".into(), Value::Int(1)),
                ("y".into(), Value::Array(Arc::new(vec![Value::Bool(true)]))),
            ])),
        ];
        for v in values {
            let buf = encode_to_flat(&v);
            let fv = FlatValue::new(&buf, 0);
            assert_eq!(fv.to_value(), v, "to_value mismatch for {:?}", v);
        }
    }

    #[test]
    fn to_value_complex_nested() {
        let v = Value::Object(Arc::new(vec![
            ("type".into(), Value::String("PushEvent".into())),
            (
                "payload".into(),
                Value::Object(Arc::new(vec![(
                    "commits".into(),
                    Value::Array(Arc::new(vec![
                        Value::Object(Arc::new(vec![(
                            "message".into(),
                            Value::String("fix bug".into()),
                        )])),
                        Value::Object(Arc::new(vec![(
                            "message".into(),
                            Value::String("add test".into()),
                        )])),
                    ])),
                )])),
            ),
            (
                "actor".into(),
                Value::Object(Arc::new(vec![(
                    "login".into(),
                    Value::String("alice".into()),
                )])),
            ),
        ]));
        let buf = encode_to_flat(&v);
        let fv = FlatValue::new(&buf, 0);
        assert_eq!(fv.to_value(), v);
    }

    // --- skip_bytes correctness ---

    #[test]
    fn skip_bytes_all_types() {
        // Encode multiple values sequentially and verify skip_bytes advances correctly
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(42),
            Value::Double(3.14, None),
            Value::Double(1.0, Some("1.00".into())),
            Value::String("hi".into()),
            Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])),
            Value::Object(Arc::new(vec![("k".into(), Value::Bool(false))])),
        ];

        let mut buf = Vec::new();
        for v in &values {
            encode_value(&mut buf, v);
        }

        let mut pos = 0;
        for (i, v) in values.iter().enumerate() {
            let fv = FlatValue::new(&buf, pos);
            let expected = fv.to_value();
            assert_eq!(&expected, v, "value mismatch at index {i}");
            pos += fv.skip_bytes();
        }
        assert_eq!(pos, buf.len(), "should have consumed entire buffer");
    }

    // --- len ---

    #[test]
    fn len_variants() {
        // null -> 0
        let buf = encode_to_flat(&Value::Null);
        assert_eq!(FlatValue::new(&buf, 0).len(), Some(0));

        // bool -> None
        let buf = encode_to_flat(&Value::Bool(true));
        assert_eq!(FlatValue::new(&buf, 0).len(), None);

        // int -> None
        let buf = encode_to_flat(&Value::Int(1));
        assert_eq!(FlatValue::new(&buf, 0).len(), None);

        // double -> None
        let buf = encode_to_flat(&Value::Double(1.0, None));
        assert_eq!(FlatValue::new(&buf, 0).len(), None);

        // string -> byte length
        let buf = encode_to_flat(&Value::String("abc".into()));
        assert_eq!(FlatValue::new(&buf, 0).len(), Some(3));

        // array -> element count
        let buf = encode_to_flat(&Value::Array(Arc::new(vec![Value::Int(1), Value::Int(2)])));
        assert_eq!(FlatValue::new(&buf, 0).len(), Some(2));

        // object -> field count
        let buf = encode_to_flat(&Value::Object(Arc::new(vec![
            ("a".into(), Value::Int(1)),
            ("b".into(), Value::Int(2)),
            ("c".into(), Value::Int(3)),
        ])));
        assert_eq!(FlatValue::new(&buf, 0).len(), Some(3));
    }

    // --- type_name and is_truthy ---

    #[test]
    fn type_name_all() {
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::Null), 0).type_name(),
            "null"
        );
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::Bool(true)), 0).type_name(),
            "boolean"
        );
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::Int(0)), 0).type_name(),
            "number"
        );
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::Double(0.0, None)), 0).type_name(),
            "number"
        );
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::String(String::new())), 0).type_name(),
            "string"
        );
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::Array(Arc::new(vec![]))), 0).type_name(),
            "array"
        );
        assert_eq!(
            FlatValue::new(&encode_to_flat(&Value::Object(Arc::new(vec![]))), 0).type_name(),
            "object"
        );
    }

    #[test]
    fn is_truthy_semantics() {
        // false and null are falsy
        assert!(!FlatValue::new(&encode_to_flat(&Value::Null), 0).is_truthy());
        assert!(!FlatValue::new(&encode_to_flat(&Value::Bool(false)), 0).is_truthy());
        // everything else is truthy
        assert!(FlatValue::new(&encode_to_flat(&Value::Bool(true)), 0).is_truthy());
        assert!(FlatValue::new(&encode_to_flat(&Value::Int(0)), 0).is_truthy());
        assert!(FlatValue::new(&encode_to_flat(&Value::Double(0.0, None)), 0).is_truthy());
        assert!(FlatValue::new(&encode_to_flat(&Value::String(String::new())), 0).is_truthy());
        assert!(FlatValue::new(&encode_to_flat(&Value::Array(Arc::new(vec![]))), 0).is_truthy());
        assert!(FlatValue::new(&encode_to_flat(&Value::Object(Arc::new(vec![]))), 0).is_truthy());
    }

    // --- FFI round-trip ---

    #[test]
    fn ffi_round_trip() {
        use crate::simdjson::{dom_parse_to_flat_buf, pad_buffer};

        let json = br#"{"name":"alice","age":30,"scores":[10,20],"active":true,"meta":null}"#;
        let buf = pad_buffer(json);
        let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
        let fv = flat_buf.root();

        assert!(fv.is_object());
        assert_eq!(fv.len(), Some(5));

        assert_eq!(fv.get_field("name").unwrap().as_str(), Some("alice"));
        assert_eq!(fv.get_field("age").unwrap().as_int(), Some(30));
        assert!(fv.get_field("active").unwrap().as_bool().unwrap());
        assert!(fv.get_field("meta").unwrap().is_null());

        let scores = fv.get_field("scores").unwrap();
        assert!(scores.is_array());
        assert_eq!(scores.len(), Some(2));
        assert_eq!(scores.get_index(0).unwrap().as_int(), Some(10));
        assert_eq!(scores.get_index(1).unwrap().as_int(), Some(20));

        // to_value round-trip
        let expected = crate::simdjson::dom_parse_to_value(&buf, json.len()).unwrap();
        assert_eq!(fv.to_value(), expected);
    }

    #[test]
    fn ffi_nested_field_navigation() {
        use crate::simdjson::{dom_parse_to_flat_buf, pad_buffer};

        let json = br#"{"a":{"b":{"c":"deep"}}}"#;
        let buf = pad_buffer(json);
        let flat_buf = dom_parse_to_flat_buf(&buf, json.len()).unwrap();
        let fv = flat_buf.root();

        let c = fv
            .get_field("a")
            .unwrap()
            .get_field("b")
            .unwrap()
            .get_field("c")
            .unwrap();
        assert_eq!(c.as_str(), Some("deep"));
    }
}
