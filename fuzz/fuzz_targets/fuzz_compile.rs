//! jq's front end on arbitrary programs: the lexer, the parser (bison's
//! tables), lowering, the linker, binding the builtins and the compiler
//! (`jq_compile_args`, as the CLI compiles a program), then what qj does
//! with a compiled program before it runs: loading it into the VM
//! (`Jq::new`, which compiles the VM's regions) and disassembling it
//! (`--debug-dump-disasm`); and the tape evaluator's own front end
//! (`TapeProgram::new`). Compile errors are fine; a panic or a crash isn't.
//! Nothing runs: a program can loop forever.
//!
//! Imports don't reach beyond the current directory: no library path, no
//! `$HOME`, origins that don't exist, and programs that could name a search
//! path (`search` metadata, or an escape that could spell it) are skipped.
//! An `import` then only looks for `./<name>.jq` (or `.json`) and fails
//! with "module not found".
//!
//! A program's constant expressions are folded as it compiles, as in jq,
//! so `"ab" * 1e9` builds a 2 GB string right there; programs that could
//! fold to more than 64 MB are skipped (`could_fold_huge`), and the
//! disassembly of large constants too.
//!
//! `cargo +nightly fuzz run fuzz_compile -s none -- -dict=fuzz/dict/jq.dict -max_total_time=120 -rss_limit_mb=2048`
//! (seed it with `bash fuzz/seed_corpus.sh`: jq's test programs).

#![no_main]

use libfuzzer_sys::fuzz_target;
use qj::io::tape_eval::TapeProgram;
use qj::jq::lang::bytecode::Bytecode;
use qj::jq::lang::execute::Jq;
use qj::jq::lang::linker::JqAttrs;
use qj::jq::lang::{CompileOptions, jq_compile_args};
use qj::jq::value::{Object, Str, Value};

/// Where the linker resolves `$ORIGIN` and relative search paths.
const NOWHERE: &str = "/nonexistent/qj-fuzz-compile";

fn options() -> CompileOptions {
    // As the CLI binds them (`program_arguments`): `$ARGS`, plus a named
    // argument and `$ENV`.
    let mut named = Object::new();
    named.insert(Str::from("x"), Value::from("x"));
    let mut args = Object::new();
    args.insert(Str::from("positional"), Value::from(vec![Value::from(1.0)]));
    args.insert(Str::from("named"), Value::Object(named.clone()));
    let mut vars = named;
    vars.insert(Str::from("ARGS"), Value::Object(args));
    let mut attrs = JqAttrs::new(NOWHERE);
    attrs.lib_dirs = Value::from(Vec::<Value>::new());
    attrs.prog_origin = Value::from(NOWHERE);
    attrs.home = None;
    CompileOptions {
        args: vars,
        env: Some(Value::empty_object()),
        attrs,
    }
}

fn contains(text: &[u8], needle: &[u8]) -> bool {
    text.windows(needle.len()).any(|w| w == needle)
}

/// Whether folding the program's constants could build a string of more
/// than 64 MB: `string * number` repeats the string, and jq folds it (and
/// the arithmetic giving the count) as it compiles, up to its 2 GB limit
/// (the process's peak RSS, which libFuzzer limits, keeps every such peak).
/// A bound: three times the program's length (its strings: an invalid
/// UTF-8 byte becomes a 3-byte U+FFFD) times every number in it, each taken
/// as at least 2 and as its inverse if larger (division), which bounds what
/// `+`, `-`, `*`, `/` and `%` can make of them.
fn could_fold_huge(program: &[u8]) -> bool {
    if !program.contains(&b'"') || !program.contains(&b'*') {
        return false;
    }
    let mut bound = 3.0 * program.len() as f64;
    let mut i = 0;
    while i < program.len() {
        let start = i;
        while i < program.len() && (program[i].is_ascii_digit() || program[i] == b'.') {
            i += 1;
        }
        if i > start && i < program.len() && matches!(program[i], b'e' | b'E') {
            i += 1;
            if i < program.len() && matches!(program[i], b'+' | b'-') {
                i += 1;
            }
            while i < program.len() && program[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i == start {
            i += 1;
            continue;
        }
        let x: f64 = std::str::from_utf8(&program[start..i])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(f64::INFINITY)
            .abs();
        bound *= x.max(1.0 / x).max(2.0);
    }
    bound > (64 << 20) as f64
}

/// About how many bytes the program's constants print as, up to `limit`.
/// jq folds constant expressions as it compiles, so `"abc" * 333333333` is
/// a 1 GB string before anything runs (in jq too); its disassembly would
/// take as much again.
fn constants_size(bc: &Bytecode, limit: usize) -> usize {
    fn value(v: &Value, total: &mut usize, limit: usize) {
        if *total > limit {
            return;
        }
        *total += 8;
        match v {
            Value::String(s) => *total += s.len(),
            Value::Array(a) => a.iter().for_each(|v| value(v, total, limit)),
            Value::Object(o) => o.iter().for_each(|(k, v)| {
                *total += k.len();
                value(v, total, limit);
            }),
            _ => {}
        }
    }
    let mut total = 0;
    let mut stack = vec![bc];
    while let Some(bc) = stack.pop() {
        bc.constants
            .iter()
            .for_each(|v| value(v, &mut total, limit));
        stack.extend(bc.subfunctions.iter().map(|f| &**f));
    }
    total
}

fuzz_target!(|data: &[u8]| {
    // jq reads the program as a C string (the CLI's never holds a NUL).
    let program = match data.iter().position(|&b| b == 0) {
        Some(nul) => &data[..nul],
        None => data,
    };
    if (contains(program, b"import") || contains(program, b"include"))
        && (contains(program, b"search") || program.contains(&b'\\'))
    {
        return;
    }
    if could_fold_huge(program) {
        return;
    }
    if let Ok(bc) = jq_compile_args(program, &options()) {
        let small = constants_size(&bc, 1 << 20) <= 1 << 20;
        let jq = Jq::new(bc);
        if small {
            let _ = jq.dump_disassembly(0);
        }
    }
    let _ = TapeProgram::new(program);
});
