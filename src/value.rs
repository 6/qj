use std::sync::Arc;

/// JSON value representation.
///
/// Uses `Int(i64)` for integers (not `f64` like jq) to preserve precision
/// on large IDs. `Object` uses `Vec<(String, Value)>` to preserve key
/// insertion order (matching jq behavior).
///
/// Array and Object use `Arc<Vec<...>>` so that cloning during filter
/// evaluation is O(1) reference-count bump instead of deep copy.
/// Arc (vs Rc) enables sharing filter literals across rayon threads
/// in the NDJSON parallel path with negligible overhead.
#[derive(Debug, Clone)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    /// f64 value + optional raw JSON text for literal preservation.
    /// `Some("75.80")` preserves the original formatting from JSON input.
    /// `None` for computed values (arithmetic, filter literals).
    Double(f64, Option<Box<str>>),
    String(String),
    Array(Arc<Vec<Value>>),
    Object(Arc<Vec<(String, Value)>>),
}

/// PartialEq ignores the raw-text field on Double — two Doubles with the
/// same f64 are equal regardless of original formatting.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Double(a, _), Value::Double(b, _)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Object(a), Value::Object(b)) => a == b,
            _ => false,
        }
    }
}

impl Value {
    /// Returns the jq type name string.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Int(_) | Value::Double(..) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// Returns true if the value is "truthy" in jq semantics.
    /// Only `false` and `null` are falsy.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Null | Value::Bool(false))
    }

    /// Short description of this value for error messages (matches jq truncation).
    pub fn short_desc(&self) -> String {
        match self {
            Value::Null => "null".to_string(),
            Value::Bool(b) => format!("{b}"),
            Value::Int(n) => format!("{n}"),
            Value::Double(f, _) => format!("{f}"),
            Value::String(s) => {
                if s.len() > 10 {
                    // Truncate at ~10 bytes, aligned to char boundaries (matches jq)
                    let mut end = 10;
                    while end > 0 && !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("\"{}...", &s[..end])
                } else {
                    format!("\"{s}\"")
                }
            }
            Value::Array(_) | Value::Object(_) => {
                let mut buf = Vec::new();
                crate::output::write_compact(&mut buf, self, false).unwrap();
                let s = String::from_utf8(buf).unwrap_or_default();
                if s.len() > 13 {
                    // jq truncates at ~11 chars + "..." for objects/arrays > 13 chars
                    let mut end = 11;
                    while end > 0 && !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...", &s[..end])
                } else {
                    s
                }
            }
        }
    }
}

impl From<serde_json::Value> for Value {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Double(n.as_f64().unwrap_or(0.0), None)
                }
            }
            serde_json::Value::String(s) => Value::String(s),
            serde_json::Value::Array(a) => {
                Value::Array(Arc::new(a.into_iter().map(Value::from).collect()))
            }
            serde_json::Value::Object(o) => Value::Object(Arc::new(
                o.into_iter().map(|(k, v)| (k, Value::from(v))).collect(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "boolean");
        assert_eq!(Value::Int(42).type_name(), "number");
        assert_eq!(Value::Double(3.14, None).type_name(), "number");
        assert_eq!(Value::String("hi".into()).type_name(), "string");
        assert_eq!(Value::Array(Arc::new(vec![])).type_name(), "array");
        assert_eq!(Value::Object(Arc::new(vec![])).type_name(), "object");
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(0).is_truthy());
        assert!(Value::Double(0.0, None).is_truthy());
        assert!(Value::String("".into()).is_truthy());
        assert!(Value::Array(Arc::new(vec![])).is_truthy());
        assert!(Value::Object(Arc::new(vec![])).is_truthy());
    }
}
