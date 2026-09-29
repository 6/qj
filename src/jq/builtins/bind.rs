//! Port of builtin.c's `builtins_bind`: binding jq's builtins (C functions, the
//! bytecoded ones, and `builtin.jq`) into a program.
//!
//! jq parses `builtin.jq` on every compile, builds a binder for every builtin, and
//! lets `block_bind_referenced` keep those the program references (transitively).
//! The result here is identical, but much cheaper:
//!
//! * each `builtin.jq` definition's name, arity, source span and free calls
//!   (`name/arity` pairs left unbound in its body) are precomputed in
//!   [`table`] (generated from `builtin.jq` and checked by a test);
//! * a compile first simulates `block_bind_referenced` on those signatures to find
//!   which binders would bind something, then parses (once per process), lowers and
//!   binds only those.
//!
//! A binder binds something exactly when an unbound call with its name and arity is
//! in the body, and a kept binder adds its own free calls to the body, so the
//! simulation keeps exactly the binders jq keeps, and binding just those produces the
//! same blocks (unkept binders bind nothing in jq).
//!
//! `builtin.jq` is vendored verbatim from jq 1.8.1 (MIT, see `LICENSE-jq`) as
//! `src/jq/builtins/builtin.jq`, behind a license header that is stripped here.

mod table;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::OnceLock;

use super::{CFunction, function_list};
use crate::jq::lang::ast::{FuncDef, ProgramBody};
use crate::jq::lang::bytecode::OP_IS_CALL_PSEUDO;
use crate::jq::lang::bytecode::Opcode::*;
use crate::jq::lang::compile::{Block, Compiler, LocFileId};
use crate::jq::lang::locfile::LocFile;
use crate::jq::lang::lower::Lowerer;
use crate::jq::lang::parser::{NoHooks, parse_library};
use crate::jq::value::Value;

/// `src/jq/builtins/builtin.jq` as vendored (with its license header).
const BUILTIN_JQ_FILE: &str = include_str!("builtin.jq");

/// The license header of `builtin.jq` (three comment lines).
const HEADER_LINES: usize = 3;

/// jq's `builtin.jq`, byte for byte (`jq_builtins[]`).
pub fn builtin_jq() -> &'static str {
    let mut rest = BUILTIN_JQ_FILE;
    for _ in 0..HEADER_LINES {
        let nl = rest.find('\n').expect("builtin.jq header");
        debug_assert!(rest.starts_with('#'));
        rest = &rest[nl + 1..];
    }
    rest
}

/// A `builtin.jq` definition, as recorded in [`table::JQ_DEFS`].
struct JqDef {
    name: &'static str,
    arity: i32,
    /// Byte range of `def ... ;` in [`builtin_jq`].
    span: (usize, usize),
    /// Calls its body leaves unbound (name, arity), deduplicated.
    free: &'static [(&'static str, i32)],
}

/// One binder of jq's builtins block, in block order.
#[derive(Clone, Copy, Debug)]
enum Binder {
    /// `function_list[i]` (a `CLOSURE_CREATE_C`).
    C(usize),
    /// A bytecoded builtin: `empty`, `not`, `path/1`, `last/1`, `range/2`.
    Bytecoded(Bytecoded),
    /// `builtin.jq`'s definition `i`.
    Jq(usize),
    /// `builtins/0` (`gen_builtin_list`).
    List,
}

#[derive(Clone, Copy, Debug)]
enum Bytecoded {
    Empty,
    Not,
    Path,
    Last,
    Range,
}

/// `bind_bytecoded_builtins`' definitions, in order.
const BYTECODED: [(Bytecoded, &str, i32); 5] = [
    (Bytecoded::Empty, "empty", 0),
    (Bytecoded::Not, "not", 0),
    (Bytecoded::Path, "path", 1),
    (Bytecoded::Last, "last", 1),
    (Bytecoded::Range, "range", 2),
];

/// Everything about the builtins that doesn't depend on the program.
struct BuiltinLib {
    /// `function_list[]`.
    cfunctions: Vec<CFunction>,
    /// The binders in jq's block order, with their name and arity.
    binders: Vec<(Binder, &'static str, i32)>,
}

/// Shared by all threads (plain data: strings and fn pointers).
static LIB: OnceLock<BuiltinLib> = OnceLock::new();

fn lib() -> &'static BuiltinLib {
    LIB.get_or_init(|| {
        let cfunctions = function_list();
        // gen_cbinding prepends each C function, so the block lists them in reverse;
        // then bind_bytecoded_builtins' definitions, builtin.jq's, and `builtins`.
        let mut binders: Vec<(Binder, &'static str, i32)> =
            Vec::with_capacity(cfunctions.len() + BYTECODED.len() + table::JQ_DEFS.len() + 1);
        for (i, cf) in cfunctions.iter().enumerate().rev() {
            binders.push((Binder::C(i), cf.name, cf.nargs as i32 - 1));
        }
        for (b, name, arity) in BYTECODED {
            binders.push((Binder::Bytecoded(b), name, arity));
        }
        for (i, d) in table::JQ_DEFS.iter().enumerate() {
            binders.push((Binder::Jq(i), d.name, d.arity));
        }
        binders.push((Binder::List, "builtins", 0));
        BuiltinLib {
            cfunctions,
            binders,
        }
    })
}

/// The free calls of binder `i`.
fn binder_free(b: Binder) -> &'static [(&'static str, i32)] {
    match b {
        Binder::Jq(i) => table::JQ_DEFS[i].free,
        // C functions bind nothing; the bytecoded ones only call their own params.
        _ => &[],
    }
}

/// Parsed `builtin.jq` definitions (parsed on first use, then shared).
static PARSED: OnceLock<Vec<OnceLock<FuncDef>>> = OnceLock::new();

/// `builtin.jq`'s definition `i`, parsed from its span (padded with spaces, so its
/// locations are those of the whole file).
fn jq_def(i: usize) -> &'static FuncDef {
    let parsed =
        PARSED.get_or_init(|| (0..table::JQ_DEFS.len()).map(|_| OnceLock::new()).collect());
    parsed[i].get_or_init(|| {
        let (start, end) = table::JQ_DEFS[i].span;
        let mut src = vec![b' '; start];
        src.extend_from_slice(&builtin_jq().as_bytes()[start..end]);
        let program = parse_library(&src, &mut NoHooks).expect("builtin.jq definition parses");
        let ProgramBody::Library(mut defs) = program.body else {
            unreachable!("parse_library returns a library")
        };
        assert_eq!(defs.len(), 1, "builtin.jq span {start}..{end}");
        defs.pop().unwrap()
    })
}

/// The `builtins/0` list, in jq's order (`gen_builtin_list`:
/// `block_list_funcs(builtins, 1)` plus `builtins/0`).
pub fn builtin_list() -> &'static [String] {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        let mut seen = HashSet::new();
        let mut list = Vec::new();
        for (b, name, arity) in &lib().binders {
            if matches!(b, Binder::List) || name.starts_with('_') {
                continue;
            }
            let key = format!("{name}/{arity}");
            if seen.insert(key.clone()) {
                list.push(key);
            }
        }
        list.push("builtins/0".to_string());
        list
    })
}

/// Port of `builtins_bind`: binds the builtins `program` references into it (by
/// `block_bind_referenced`, which prepends the kept definitions in block order).
pub fn builtins_bind(c: &mut Compiler, program: Block) -> Block {
    let lib = lib();

    // Which binders would block_bind_referenced keep? It walks them from the last
    // to the first; a binder binds (and is kept) when the body has an unbound call
    // with its name and arity, and a kept binder's body joins the body.
    // (Arities are kept as bit sets; no builtin takes 64 arguments.)
    let mut calls = Vec::new();
    c.unbound_calls(program, &mut calls);
    let mut unbound: HashMap<&str, u64> = HashMap::new();
    let arity_bit = |a: i32| if (0..64).contains(&a) { 1u64 << a } else { 0 };
    for (name, arity) in &calls {
        *unbound.entry(name).or_default() |= arity_bit(*arity);
    }
    let mut keep = vec![false; lib.binders.len()];
    for (i, &(binder, name, arity)) in lib.binders.iter().enumerate().rev() {
        let bit = arity_bit(arity);
        if let Some(m) = unbound.get_mut(name)
            && *m & bit != 0
        {
            *m &= !bit;
            keep[i] = true;
            for &(n, a) in binder_free(binder) {
                *unbound.entry(n).or_default() |= arity_bit(a);
            }
        }
    }

    // Build the kept binders, in block order.
    let mut builtin_lf: Option<LocFileId> = None;
    let mut binders = Block::NOOP;
    for (i, &(binder, _, _)) in lib.binders.iter().enumerate() {
        if !keep[i] {
            continue;
        }
        let b = match binder {
            Binder::C(ci) => c.gen_cfunction(lib.cfunctions[ci]),
            Binder::Bytecoded(bc) => gen_bytecoded(c, bc),
            Binder::Jq(di) => {
                let lf = *builtin_lf.get_or_insert_with(|| {
                    c.add_locfile(Rc::new(LocFile::new("<builtin>", builtin_jq().as_bytes())))
                });
                Lowerer::new(c, lf).lower_funcdef(jq_def(di))
            }
            Binder::List => {
                let list: Value = builtin_list()
                    .iter()
                    .map(|s| Value::from(s.as_str()))
                    .collect();
                let k = c.gen_const(list);
                c.gen_function("builtins", Block::NOOP, k)
            }
        };
        binders = c.block_join(binders, b);
    }
    c.block_bind_referenced(binders, program, OP_IS_CALL_PSEUDO)
}

/// builtin.c `bind_bytecoded_builtins`' definitions.
fn gen_bytecoded(c: &mut Compiler, which: Bytecoded) -> Block {
    match which {
        Bytecoded::Empty => {
            let body = c.gen_op_simple(BACKTRACK);
            c.gen_function("empty", Block::NOOP, body)
        }
        Bytecoded::Not => {
            let f = c.gen_const(Value::Bool(false));
            let t = c.gen_const(Value::Bool(true));
            let body = c.gen_condbranch(f, t);
            c.gen_function("not", Block::NOOP, body)
        }
        Bytecoded::Path => {
            let begin = c.gen_op_simple(PATH_BEGIN);
            let arg = c.gen_call("arg", Block::NOOP);
            let end = c.gen_op_simple(PATH_END);
            let body = c.block3(begin, arg, end);
            let param = c.gen_param("arg");
            c.gen_function("path", param, body)
        }
        Bytecoded::Last => {
            let body = gen_last_1(c);
            let param = c.gen_param("arg");
            c.gen_function("last", param, body)
        }
        Bytecoded::Range => {
            // Note that we can now define `range` as a jq-coded function
            let rangevar = c.gen_op_var_fresh(STOREV, "rangevar");
            let rangestart = c.gen_op_var_fresh(STOREV, "rangestart");
            let dup = c.gen_op_simple(DUP);
            let start = c.gen_call("start", Block::NOOP);
            let end = c.gen_call("end", Block::NOOP);
            let dup2 = c.gen_op_simple(DUP);
            let load = c.gen_op_bound(LOADV, rangestart);
            let range_op = c.gen_op_bound(RANGE, rangevar);
            // Reset rangevar for every value generated by "end"
            let body = c.blocks(&[dup, start, rangestart, end, dup2, load, rangevar, range_op]);
            let p1 = c.gen_param("start");
            let p2 = c.gen_param("end");
            let params = c.block_join(p1, p2);
            c.gen_function("range", params, body)
        }
    }
}

/// builtin.c `gen_last_1`: `last(g)` without boxing, yielding nothing when `g` is
/// empty.
fn gen_last_1(c: &mut Compiler) -> Block {
    let last_var = c.gen_op_var_fresh(STOREV, "last");
    let is_empty_var = c.gen_op_var_fresh(STOREV, "is_empty");
    let dup1 = c.gen_op_simple(DUP);
    let null = c.gen_const(Value::Null);
    let dup2 = c.gen_op_simple(DUP);
    let t = c.gen_const(Value::Bool(true));
    let init = c.blocks(&[dup1, null, last_var, dup2, t, is_empty_var]);
    let arg = c.gen_call("arg", Block::NOOP);
    let dup3 = c.gen_op_simple(DUP);
    let store_last = c.gen_op_bound(STOREV, last_var);
    let f = c.gen_const(Value::Bool(false));
    let store_empty = c.gen_op_bound(STOREV, is_empty_var);
    let backtrack = c.gen_op_simple(BACKTRACK);
    let call_arg = c.blocks(&[arg, dup3, store_last, f, store_empty, backtrack]);
    let if_empty = c.gen_op_simple(BACKTRACK);
    let fork = c.gen_op_target(FORK, call_arg);
    let load_empty = c.gen_op_bound(LOADVN, is_empty_var);
    let jump_f = c.gen_op_target(JUMP_F, if_empty);
    let load_last = c.gen_op_bound(LOADVN, last_var);
    let tail = c.blocks(&[load_empty, jump_f, if_empty, load_last]);
    c.blocks(&[init, fork, call_arg, tail])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jq::lang::lower::CompileHooks;
    use std::fmt::Write as _;

    #[test]
    fn builtin_jq_is_jqs_text() {
        let jq = include_str!("../../../tests/jq_lang/builtin.jq");
        assert_eq!(builtin_jq(), jq);
    }

    /// Parses all of builtin.jq the way jq does (with the compiler's hooks).
    fn full_parse() -> Vec<FuncDef> {
        let text = builtin_jq();
        let mut c = Compiler::new();
        let lf = c.add_locfile(Rc::new(LocFile::new("<builtin>", text.as_bytes())));
        let mut hooks = CompileHooks { c: &mut c, lf };
        let program = parse_library(text.as_bytes(), &mut hooks).expect("builtin.jq parses");
        assert!(program.module.is_none() && program.imports.is_empty());
        let ProgramBody::Library(defs) = program.body else {
            unreachable!()
        };
        defs
    }

    /// The generated `table.rs` for the current builtin.jq.
    fn render_table() -> String {
        let text = builtin_jq();
        let mut c = Compiler::new();
        let lf = c.add_locfile(Rc::new(LocFile::new("<builtin>", text.as_bytes())));
        let mut out = String::from(
            "//! Generated from `builtin.jq` by `QJ_BLESS=1 cargo test --lib builtin_table`;\n\
             //! do not edit. For each definition, in order: its name, arity, byte range in\n\
             //! `builtin_jq()`, and the calls its body leaves unbound (deduplicated).\n\n\
             use super::JqDef;\n\n\
             #[rustfmt::skip]\n\
             pub(super) static JQ_DEFS: &[JqDef] = &[\n",
        );
        for d in full_parse() {
            let block = Lowerer::new(&mut c, lf).lower_funcdef(&d);
            let mut calls = Vec::new();
            c.unbound_calls(block, &mut calls);
            let mut seen = HashSet::new();
            let free: Vec<String> = calls
                .into_iter()
                .filter(|(n, a)| seen.insert((n.to_string(), *a)))
                .map(|(n, a)| format!("({n:?}, {a})"))
                .collect();
            let _ = writeln!(
                out,
                "    JqDef {{ name: {:?}, arity: {}, span: ({}, {}), free: &[{}] }},",
                d.name,
                d.params.len(),
                d.loc.start,
                d.loc.end,
                free.join(", ")
            );
        }
        out.push_str("];\n");
        out
    }

    #[test]
    fn builtin_table_is_current() {
        let rendered = render_table();
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/jq/builtins/bind/table.rs");
        if std::env::var("QJ_BLESS").is_ok_and(|v| v == "1") {
            std::fs::write(path, &rendered).unwrap();
            return;
        }
        let current = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            current == rendered,
            "src/jq/builtins/bind/table.rs is stale: run QJ_BLESS=1 cargo test --lib builtin_table"
        );
    }

    #[test]
    fn parsed_spans_match_full_parse() {
        let full = full_parse();
        assert_eq!(full.len(), table::JQ_DEFS.len());
        for (i, d) in full.iter().enumerate() {
            assert_eq!(jq_def(i), d, "definition {i} ({})", d.name);
        }
    }

    #[test]
    fn builtin_list_shape() {
        let list = builtin_list();
        assert_eq!(list.last().unwrap(), "builtins/0");
        assert!(list.iter().any(|s| s == "map/1"));
        assert!(list.iter().all(|s| !s.starts_with('_')));
    }
}
