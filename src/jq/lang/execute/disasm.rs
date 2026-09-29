//! Test support: rebuilds a program's [`Bytecode`] from `jq --debug-dump-disasm` output,
//! so the VM can run jq's own compiled code (independently of our compiler) and be
//! compared with jq instruction by instruction.
//!
//! The disassembly is almost lossless. What it doesn't show is recovered as follows:
//! `nlocals` is one more than the highest local index referenced; `CALL_BUILTIN`'s
//! arity comes from `function_list()` by name; a `CALL_JQ` closure is a subfunction
//! (`ARG_NEWCLOSURE`) when that frame's subfunction at the index has the printed name,
//! else a parameter. `STORE_GLOBAL` (whose variable isn't printed) is unsupported.

use std::collections::HashMap;
use std::rc::{Rc, Weak};

use crate::jq::builtins::{CFunction, function_list};
use crate::jq::lang::bytecode::{
    ARG_NEWCLOSURE, Bytecode, DebugInfo, OP_HAS_BRANCH, OP_HAS_CONSTANT, OP_HAS_VARIABLE, Opcode,
    SymbolTable,
};
use crate::jq::value::parse_sized;

/// One function of the disassembly.
#[derive(Default, Debug)]
struct DisFunc {
    name: Option<String>,
    params: Vec<String>,
    /// `(pc, opname, argument text)`.
    instrs: Vec<(usize, String, String)>,
    subs: Vec<DisFunc>,
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn parse_func(lines: &[&str], i: &mut usize, indent: usize, name: Option<String>) -> DisFunc {
    let mut f = DisFunc {
        name,
        ..DisFunc::default()
    };
    if *i < lines.len() && indent_of(lines[*i]) == indent {
        let l = &lines[*i][indent..];
        if let Some(rest) = l.strip_prefix("[params: ") {
            let rest = rest.strip_suffix(']').expect("params line");
            f.params = rest.split(", ").map(str::to_string).collect();
            *i += 1;
        }
    }
    while *i < lines.len()
        && indent_of(lines[*i]) == indent
        && lines[*i][indent..].starts_with(|c: char| c.is_ascii_digit())
    {
        let l = &lines[*i][indent..];
        let (pc, rest) = l.split_once(' ').expect("pc");
        let (op, arg) = match rest.split_once(' ') {
            Some((op, arg)) => (op, arg),
            None => (rest, ""),
        };
        f.instrs
            .push((pc.parse().expect("pc"), op.to_string(), arg.to_string()));
        *i += 1;
    }
    while *i < lines.len() && indent_of(lines[*i]) == indent {
        let header = lines[*i][indent..]
            .strip_suffix(':')
            .expect("subfunction header");
        let (name, _idx) = header.rsplit_once(':').expect("name:idx");
        *i += 1;
        let sub = parse_func(lines, i, indent + 2, Some(name.to_string()));
        f.subs.push(sub);
    }
    f
}

/// `$name:idx` or `$name:idx^level` (also `name:idx^level` for closures).
fn parse_ref(s: &str) -> (String, u16, u16) {
    let (body, level) = match s.rsplit_once('^') {
        Some((b, l)) if l.chars().all(|c| c.is_ascii_digit()) => (b, l.parse().unwrap()),
        _ => (s, 0),
    };
    let (name, idx) = body.rsplit_once(':').expect("name:idx");
    (name.to_string(), idx.parse().expect("idx"), level)
}

struct Builder {
    cfuncs: Vec<CFunction>,
    cfunc_index: HashMap<&'static str, u16>,
    all: Vec<CFunction>,
    /// Locals per function id (DFS order): name by index.
    locals: Vec<Vec<String>>,
    overrides: HashMap<&'static str, crate::jq::builtins::CFn>,
}

/// Assigns DFS ids and collects the locals each function's frame needs.
fn collect_locals(f: &DisFunc, chain: &mut Vec<usize>, next_id: &mut usize, b: &mut Builder) {
    let id = *next_id;
    *next_id += 1;
    b.locals.push(Vec::new());
    chain.push(id);
    for (_, op, arg) in &f.instrs {
        let opc = opcode_by_name(op);
        let d = opc.describe();
        if d.flags & OP_HAS_VARIABLE != 0 && d.flags & OP_HAS_CONSTANT == 0 {
            let (name, idx, level) = parse_ref(arg.trim_start_matches('$'));
            let target = chain[chain.len() - 1 - level as usize];
            let l = &mut b.locals[target];
            if l.len() <= idx as usize {
                l.resize(idx as usize + 1, String::new());
            }
            l[idx as usize] = name;
        }
    }
    for s in &f.subs {
        collect_locals(s, chain, next_id, b);
    }
    chain.pop();
}

fn opcode_by_name(name: &str) -> Opcode {
    crate::jq::lang::bytecode::ALL_OPCODES
        .iter()
        .copied()
        .find(|o| o.name() == name)
        .unwrap_or_else(|| panic!("unknown opcode {name}"))
}

fn build(
    f: &DisFunc,
    chain: &mut Vec<*const DisFunc>,
    parent: Weak<Bytecode>,
    next_id: &mut usize,
    globals: &Rc<SymbolTable>,
    b: &Builder,
) -> Rc<Bytecode> {
    let id = *next_id;
    *next_id += 1;
    chain.push(f as *const DisFunc);
    let mut code = Vec::new();
    let mut constants = Vec::new();
    for (pc, op, arg) in &f.instrs {
        assert_eq!(*pc, code.len(), "pc mismatch at {op} {arg}");
        let opc = opcode_by_name(op);
        let d = opc.describe();
        code.push(opc as u16);
        match opc {
            Opcode::CALL_JQ | Opcode::TAIL_CALL_JQ => {
                let refs: Vec<&str> = arg.split(' ').collect();
                code.push((refs.len() - 1) as u16);
                for r in refs {
                    let (name, idx, level) = parse_ref(r);
                    // SAFETY: the chain holds pointers to live ancestors of `f`.
                    let target = unsafe { &*chain[chain.len() - 1 - level as usize] };
                    let is_sub = target
                        .subs
                        .get(idx as usize)
                        .is_some_and(|s| s.name.as_deref() == Some(name.as_str()));
                    if !is_sub {
                        assert_eq!(
                            target.params.get(idx as usize).map(String::as_str),
                            Some(name.as_str()),
                            "closure {r}"
                        );
                    }
                    code.push(level);
                    code.push(if is_sub { idx | ARG_NEWCLOSURE } else { idx });
                }
            }
            Opcode::CALL_BUILTIN => {
                let cf = b.all.iter().find(|c| c.name == arg).expect("builtin");
                code.push(cf.nargs as u16);
                code.push(b.cfunc_index[cf.name]);
            }
            _ if d.flags & OP_HAS_BRANCH != 0 => {
                let target: usize = arg.parse().expect("branch target");
                code.push((target - (pc + 2)) as u16);
            }
            Opcode::STORE_GLOBAL => panic!("STORE_GLOBAL is not supported by the loader"),
            _ if d.flags & OP_HAS_CONSTANT != 0 => {
                let v = parse_sized(arg.as_bytes()).expect("constant");
                code.push(constants.len() as u16);
                constants.push(v);
            }
            _ if d.flags & OP_HAS_VARIABLE != 0 => {
                let (_, idx, level) = parse_ref(arg.trim_start_matches('$'));
                code.push(level);
                code.push(idx);
            }
            _ => assert_eq!(d.length, 1, "unexpected immediate for {op}"),
        }
    }
    let locals = b.locals[id].clone();
    let bc = Rc::new_cyclic(|me: &Weak<Bytecode>| {
        let subfunctions = f
            .subs
            .iter()
            .map(|s| build(s, chain, me.clone(), next_id, globals, b))
            .collect();
        Bytecode {
            code,
            nlocals: locals.len(),
            nclosures: f.params.len(),
            constants,
            globals: globals.clone(),
            subfunctions,
            parent,
            debuginfo: DebugInfo {
                name: f.name.clone(),
                params: f.params.clone(),
                locals,
            },
        }
    });
    chain.pop();
    bc
}

/// Rebuilds the program printed by `jq --debug-dump-disasm` (the text before the blank
/// line). `overrides` replaces C builtins by name (for tests that need a builtin
/// whose port hasn't landed).
pub(super) fn load(
    text: &str,
    overrides: &[(&'static str, crate::jq::builtins::CFn)],
) -> Rc<Bytecode> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    let mut i = 0;
    let top = parse_func(&lines, &mut i, 0, None);
    assert_eq!(i, lines.len(), "unparsed disassembly at line {i}");

    let mut b = Builder {
        cfuncs: Vec::new(),
        cfunc_index: HashMap::new(),
        all: function_list(),
        locals: Vec::new(),
        overrides: overrides.iter().copied().collect(),
    };
    let mut next_id = 0;
    collect_locals(&top, &mut Vec::new(), &mut next_id, &mut b);
    // The symbol table, in order of first use.
    fn collect_cfuncs(f: &DisFunc, b: &mut Builder) {
        for (_, op, arg) in &f.instrs {
            if op == "CALL_BUILTIN" && !b.cfunc_index.contains_key(arg.as_str()) {
                let mut cf = *b
                    .all
                    .iter()
                    .find(|c| c.name == arg)
                    .unwrap_or_else(|| panic!("unknown builtin {arg}"));
                if let Some(f) = b.overrides.get(cf.name) {
                    cf.f = *f;
                }
                b.cfunc_index.insert(cf.name, b.cfuncs.len() as u16);
                b.cfuncs.push(cf);
            }
        }
        for s in &f.subs {
            collect_cfuncs(s, b);
        }
    }
    collect_cfuncs(&top, &mut b);
    let globals = Rc::new(SymbolTable {
        cfunctions: b.cfuncs.clone(),
    });
    let mut next_id = 0;
    build(
        &top,
        &mut Vec::new(),
        Weak::new(),
        &mut next_id,
        &globals,
        &b,
    )
}
