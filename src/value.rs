use std::rc::Rc;

/// JSON value representation.
///
/// Uses `Int(i64)` for integers (not `f64` like jq) to preserve precision
/// on large IDs. `Object` uses `Vec<(String, Value)>` to preserve key
/// insertion order (matching jq behavior).
///
/// Array and Object use `Rc<Vec<...>>` so that cloning during filter
/// evaluation is O(1) reference-count bump instead of deep copy.
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
    Array(Rc<Vec<Value>>),
    Object(Rc<Vec<(String, Value)>>),
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
        assert_eq!(Value::Array(Rc::new(vec![])).type_name(), "array");
        assert_eq!(Value::Object(Rc::new(vec![])).type_name(), "object");
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(0).is_truthy());
        assert!(Value::Double(0.0, None).is_truthy());
        assert!(Value::String("".into()).is_truthy());
        assert!(Value::Array(Rc::new(vec![])).is_truthy());
        assert!(Value::Object(Rc::new(vec![])).is_truthy());
    }
}
