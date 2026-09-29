//! iai-callgrind regression benchmarks for qj's core: the jq 1.8.1 port's
//! compiler and VM, its value printer, and the input layer (simdjson's tape
//! to jq values, jq's parser port, and the NDJSON reader).
//!
//! These benchmarks count CPU instructions (via Valgrind) rather than wall-clock
//! time, making them perfectly deterministic on CI. Any change that adds work
//! (extra allocations, deeper recursion, unnecessary materialization) shows up as
//! an instruction count increase — regardless of runner load.
//!
//! Setup (compiling the program, parsing the input) runs outside the measured
//! function, so each benchmark counts one thing: compiling, running the VM,
//! parsing, reading or printing.
//!
//! Run locally (requires valgrind):
//!   cargo bench --bench eval_regression
//!
//! On CI this runs automatically on ubuntu via checks.yml.

use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use std::hint::black_box;
use std::rc::Rc;

use qj::io::simd::SimdParser;
use qj::io::{InputReader, MemoryOpener, ReaderOptions};
use qj::jq::lang::bytecode::Bytecode;
use qj::jq::lang::execute::Jq;
use qj::jq::lang::{CompileOptions, jq_compile_args};
use qj::jq::value::print::dump_to_vec;
use qj::jq::value::{DumpOptions, Value, parse_sized};

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

/// NDJSON records for the reader benchmark: the fixture, compacted, once per line.
const NDJSON_RECORDS: usize = 20;

fn options() -> CompileOptions {
    CompileOptions::new("/usr/local/bin")
}

fn fixture_value() -> Value {
    parse_sized(FIXTURE.as_bytes()).unwrap()
}

// ---------------------------------------------------------------------------
// Setup (not measured)
// ---------------------------------------------------------------------------

/// A program's text and the options to compile it with.
fn setup_compile(program: &str) -> (&str, CompileOptions) {
    (program, options())
}

/// A compiled program, ready to run on the fixture.
fn setup_vm(program: &str) -> (Jq, Value) {
    let bc = jq_compile_args(program.as_bytes(), &options()).unwrap();
    (Jq::new(bc), fixture_value())
}

/// A simdjson parser that has already parsed the fixture once (its buffers
/// are allocated, as for every record but the first), and the fixture
/// followed by simdjson's padding, so it's parsed in place.
fn setup_simd() -> (SimdParser, Vec<u8>) {
    let mut buf = FIXTURE.as_bytes().to_vec();
    buf.resize(FIXTURE.len() + qj::simdjson::padding(), 0);
    let mut parser = SimdParser::new();
    parser.parse(&buf, 0, FIXTURE.len()).unwrap();
    (parser, buf)
}

/// A reader over `NDJSON_RECORDS` lines of the compacted fixture.
fn setup_reader() -> InputReader {
    let line = qj::jq::value::dump_string(&fixture_value(), &DumpOptions::compact());
    let data = format!("{line}\n").repeat(NDJSON_RECORDS);
    let mut files = MemoryOpener::new();
    files.add("records.ndjson", data);
    InputReader::with_opener(
        vec!["records.ndjson".into()],
        ReaderOptions::default(),
        Box::new(files),
    )
}

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

// `jq_compile_args`: parsing, binding the builtins the program uses
// (`builtin.jq` is bound lazily), and code generation.
#[library_benchmark]
#[bench::identity(setup_compile("."))]
#[bench::select_construct(setup_compile(
    r#".items[] | select(.price > 10) | {sku, total: (.price * .qty)}"#
))]
#[bench::jq_defined_builtins(setup_compile(
    r#"with_entries(.value |= tostring) | to_entries | map(.key) | join(",")"#
))]
fn compile((program, opts): (&str, CompileOptions)) -> Rc<Bytecode> {
    black_box(jq_compile_args(program.as_bytes(), &opts).unwrap())
}

// `jq_start` + `jq_next` until the program is done, on the fixture.
#[library_benchmark]
#[bench::identity(setup_vm("."))]
#[bench::field(setup_vm(".name"))]
#[bench::pipe_length(setup_vm(".items | length"))]
#[bench::iterate_field(setup_vm(".items[].sku"))]
#[bench::select_construct(setup_vm(r#".items[] | select(.price > 10) | {sku, price}"#))]
#[bench::if_then_else(setup_vm("if .active then .name else .id end"))]
#[bench::sort_by(setup_vm(
    "[.items[] | {sku, total: (.price * .qty)}] | sort_by(.total) | reverse"
))]
#[bench::reduce(setup_vm("reduce .items[] as $i (0; . + $i.price * $i.qty)"))]
#[bench::update(setup_vm(".items[].price |= . * 2"))]
#[bench::paths(setup_vm("[paths]"))]
#[bench::to_entries(setup_vm("to_entries"))]
#[bench::with_entries(setup_vm("with_entries(.value |= tostring)"))]
#[bench::tostream(setup_vm("[tostream]"))]
fn vm((mut jq, input): (Jq, Value)) -> (Jq, Vec<Value>) {
    jq.start(input, 0);
    let out: Vec<Value> = jq.by_ref().map(Result::unwrap).collect();
    black_box((jq, out))
}

// One document to a jq value with simdjson: the tape parse (FFI) and the
// value built from the tape.
#[library_benchmark]
#[bench::fixture(setup_simd())]
fn simd_parse((mut parser, buf): (SimdParser, Vec<u8>)) -> (SimdParser, Value) {
    let value = parser.parse(&buf, 0, FIXTURE.len()).unwrap();
    black_box((parser, value))
}

// The same document with jq's parser port (the fallback for anything
// simdjson rejects).
#[library_benchmark]
fn jq_parse() -> Value {
    black_box(parse_sized(black_box(FIXTURE.as_bytes())).unwrap())
}

// The input layer on NDJSON: jq's input loop with the simdjson fast path.
#[library_benchmark]
#[bench::records(setup_reader())]
fn read_ndjson(mut reader: InputReader) -> (InputReader, Vec<Value>) {
    let mut values = Vec::with_capacity(NDJSON_RECORDS);
    while let Some(value) = reader.next() {
        values.push(value.unwrap());
    }
    black_box((reader, values))
}

// The printer (`jv_dump_term`), compact and pretty.
#[library_benchmark]
#[bench::compact(fixture_value(), DumpOptions::compact())]
#[bench::pretty(fixture_value(), DumpOptions::pretty())]
fn dump(value: Value, opts: DumpOptions) -> (Value, Vec<u8>) {
    let mut out = Vec::new();
    dump_to_vec(&value, &opts, &mut out);
    black_box((value, out))
}

// ---------------------------------------------------------------------------
// Groups & main
// ---------------------------------------------------------------------------

library_benchmark_group!(
    name = compile_group;
    benchmarks = compile
);

library_benchmark_group!(
    name = vm_group;
    benchmarks = vm
);

library_benchmark_group!(
    name = input_group;
    benchmarks = simd_parse, jq_parse, read_ndjson
);

library_benchmark_group!(
    name = output_group;
    benchmarks = dump
);

main!(
    library_benchmark_groups = compile_group,
    vm_group,
    input_group,
    output_group
);
