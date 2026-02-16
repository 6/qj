//! iai-callgrind regression benchmarks for qj's core eval paths.
//!
//! These benchmarks count CPU instructions (via Valgrind) rather than wall-clock
//! time, making them perfectly deterministic on CI. Any change that adds work
//! (extra allocations, deeper recursion, unnecessary materialization) shows up as
//! an instruction count increase — regardless of runner load.
//!
//! Run locally (requires valgrind):
//!   cargo bench --bench eval_regression
//!
//! On CI this runs automatically on ubuntu via checks.yml.

use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use std::hint::black_box;

use qj::filter::{self, Env, Filter};
use qj::flat_eval;
use qj::flat_value::FlatValue;
use qj::simdjson::{self, FlatBuffer};
use qj::value::Value;

/// Small but representative JSON fixture (~600 bytes). Contains nested objects,
/// arrays, strings, numbers, booleans, and null — enough to exercise real code
/// paths while staying fast under Valgrind.
const FIXTURE: &str = r#"{
  "id": 42,
  "name": "Alice",
  "active": true,
  "score": 98.6,
  "address": {
    "city": "Portland",
    "state": "OR",
    "zip": "97201"
  },
  "tags": ["admin", "user", "beta"],
  "items": [
    {"sku": "A1", "price": 10, "qty": 2},
    {"sku": "B2", "price": 25, "qty": 1},
    {"sku": "C3", "price": 5, "qty": 10},
    {"sku": "D4", "price": 50, "qty": 3},
    {"sku": "E5", "price": 15, "qty": 7}
  ],
  "metadata": null,
  "enabled": false
}"#;

/// Parse the fixture into a padded buffer + FlatBuffer.
fn parse_fixture() -> FlatBuffer {
    let padded = simdjson::pad_buffer(FIXTURE.as_bytes());
    simdjson::dom_parse_to_flat_buf_tape(&padded, FIXTURE.len()).unwrap()
}

/// Parse the fixture into a serde_json Value (for standard eval benchmarks).
fn parse_fixture_value() -> Value {
    let flat = parse_fixture();
    flat.root().to_value()
}

/// Parse a filter string.
fn parse(expr: &str) -> Filter {
    filter::parse(expr).unwrap()
}

/// Collect all outputs from eval_flat into a Vec.
fn run_flat(filter: &Filter, flat: FlatValue<'_>) -> Vec<Value> {
    let env = Env::empty();
    let mut out = Vec::new();
    flat_eval::eval_flat(filter, flat, &env, &mut |v| out.push(v));
    out
}

/// Collect all outputs from eval_filter into a Vec.
fn run_standard(filter: &Filter, input: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    filter::eval::eval_filter(filter, input, &mut |v| out.push(v));
    out
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

#[library_benchmark]
fn parse_flat_buf_tape() -> FlatBuffer {
    let padded = simdjson::pad_buffer(black_box(FIXTURE.as_bytes()));
    black_box(simdjson::dom_parse_to_flat_buf_tape(&padded, FIXTURE.len()).unwrap())
}

#[library_benchmark]
fn eval_flat_identity() -> Vec<Value> {
    let flat = parse_fixture();
    let f = parse(".");
    black_box(run_flat(&f, flat.root()))
}

#[library_benchmark]
fn eval_flat_field() -> Vec<Value> {
    let flat = parse_fixture();
    let f = parse(".name");
    black_box(run_flat(&f, flat.root()))
}

#[library_benchmark]
fn eval_flat_pipe_length() -> Vec<Value> {
    let flat = parse_fixture();
    let f = parse(".items | length");
    black_box(run_flat(&f, flat.root()))
}

#[library_benchmark]
fn eval_flat_iterate_field() -> Vec<Value> {
    let flat = parse_fixture();
    let f = parse(".items[].sku");
    black_box(run_flat(&f, flat.root()))
}

#[library_benchmark]
fn eval_flat_select() -> Vec<Value> {
    let flat = parse_fixture();
    let f = parse(r#".items[] | select(.price > 10) | {sku, price}"#);
    black_box(run_flat(&f, flat.root()))
}

#[library_benchmark]
fn eval_flat_if_then_else() -> Vec<Value> {
    let flat = parse_fixture();
    let f = parse(r#"if .active then .name else .id end"#);
    black_box(run_flat(&f, flat.root()))
}

#[library_benchmark]
fn eval_standard_identity() -> Vec<Value> {
    let input = parse_fixture_value();
    let f = parse(".");
    black_box(run_standard(&f, &input))
}

#[library_benchmark]
fn eval_standard_complex() -> Vec<Value> {
    let input = parse_fixture_value();
    let f = parse("[.items[] | {sku, total: (.price * .qty)}] | sort_by(.total) | reverse");
    black_box(run_standard(&f, &input))
}

#[library_benchmark]
fn parse_filter_complex() -> Filter {
    black_box(
        filter::parse(black_box(
            r#".items[] | select(.price > 10) | {sku, total: (.price * .qty)}"#,
        ))
        .unwrap(),
    )
}

// ---------------------------------------------------------------------------
// Groups & main
// ---------------------------------------------------------------------------

library_benchmark_group!(
    name = parse_group;
    benchmarks = parse_flat_buf_tape, parse_filter_complex
);

library_benchmark_group!(
    name = flat_eval_group;
    benchmarks =
        eval_flat_identity,
        eval_flat_field,
        eval_flat_pipe_length,
        eval_flat_iterate_field,
        eval_flat_select,
        eval_flat_if_then_else
);

library_benchmark_group!(
    name = standard_eval_group;
    benchmarks =
        eval_standard_identity,
        eval_standard_complex
);

main!(
    library_benchmark_groups = parse_group,
    flat_eval_group,
    standard_eval_group
);
