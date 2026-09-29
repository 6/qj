//! Tests whose expectations come from the jq 1.8.1 binary.

use super::*;

#[test]
fn value_size() {
    assert!(std::mem::size_of::<Value>() <= 24);
}
