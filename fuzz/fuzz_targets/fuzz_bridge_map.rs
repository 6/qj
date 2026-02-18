#![no_main]
use libfuzzer_sys::fuzz_target;
use qj::simdjson::{
    dom_array_map_builtin, dom_array_map_field, dom_array_map_fields_obj, dom_field_has,
    dom_validate, minify, pad_buffer,
};

// Fuzz the array mapping functions, dom_validate, dom_field_has, and minify.
// These are complex C++ functions operating on untrusted input via FFI.
// Goal: no crashes, no UB — output correctness is not validated.
fuzz_target!(|data: &[u8]| {
    let buf = pad_buffer(data);
    let len = data.len();

    // --- dom_validate ---
    let _ = dom_validate(&buf, len);

    // --- minify ---
    let _ = minify(&buf, len);

    // --- dom_field_has ---
    for field in &["a", "b", "name", "type"] {
        let _ = dom_field_has(&buf, len, &[], field);
    }
    // With prefix
    let _ = dom_field_has(&buf, len, &["a"], "b");

    // --- dom_array_map_field ---
    let fields: &[&[&str]] = &[&["a"], &["b"], &["name"], &["a", "b"]];
    for f in fields {
        let _ = dom_array_map_field(&buf, len, &[], f, true);
        let _ = dom_array_map_field(&buf, len, &[], f, false);
    }
    // With prefix
    let _ = dom_array_map_field(&buf, len, &["items"], &["a"], true);

    // --- dom_array_map_fields_obj ---
    let keys_ab = [&b"\"a\""[..], &b"\"b\""[..]];
    let _ = dom_array_map_fields_obj(&buf, len, &[], &keys_ab, &["a", "b"], true);
    let _ = dom_array_map_fields_obj(&buf, len, &[], &keys_ab, &["a", "b"], false);
    let keys_one = [&b"\"x\""[..]];
    let _ = dom_array_map_fields_obj(&buf, len, &[], &keys_one, &["x"], true);

    // --- dom_array_map_builtin ---
    // op_code: 0=length, 1=keys, 2=type, 3=has
    for op in 0..=3i32 {
        let sorted = op == 1; // keys sorted
        let arg = if op == 3 { "a" } else { "" };
        let _ = dom_array_map_builtin(&buf, len, &[], op, sorted, arg, true);
        let _ = dom_array_map_builtin(&buf, len, &[], op, sorted, arg, false);
    }
    // keys unsorted
    let _ = dom_array_map_builtin(&buf, len, &[], 1, false, "", true);
    // With prefix
    let _ = dom_array_map_builtin(&buf, len, &["items"], 0, true, "", true);
});
