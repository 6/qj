//! `--debug-trace` output: port of the tracing parts of execute.c's `jq_next`.
//!
//! Each executed instruction prints `dump_operation` (e.g. `0005 CALL_BUILTIN _plus`),
//! a tab, then either the instruction's stack inputs separated by ` | ` or `\t<backtracking>`.
//! With `JQ_DEBUG_TRACE_DETAIL` (`--debug-trace=all`) the rest of the stack follows,
//! separated by ` || `. `LOADV`/`LOADVN` print `V<n> = <value>` and `STOREV` prints
//! `V<n> = <value> (<refcount>)`. Values are dumped with `JV_PRINT_REFCOUNT`, which
//! appends ` (<refcount>)` to strings and non-empty arrays and objects.

use std::io::Write;

use super::Jq;
use super::program::Program;
use super::{JQ_DEBUG_TRACE_DETAIL, stack::StackPtr};
use crate::jq::lang::bytecode::{dump_operation, opcode_describe};
use crate::jq::value::Value;
use crate::jq::value::print::write_json_string;

/// jv_print.c `MAX_PRINT_DEPTH`.
const MAX_PRINT_DEPTH: usize = 256;

/// `jv_get_refcnt(x)`, as the dump of a copy prints it (`jv_get_refcnt(x) - 1` of the
/// copy): the number of references held by the VM and the program.
fn refcount(v: &Value) -> Option<usize> {
    Some(v.refcount())
}

/// `jv_dump_term(C, x, flags, indent, F, S)` with `flags` either `JV_PRINT_REFCOUNT`
/// (`with_refcount`) or 0, and no colors or indentation.
pub(super) fn dump_term(v: &Value, with_refcount: bool, indent: usize, out: &mut Vec<u8>) {
    let put_refcnt = |v: &Value, out: &mut Vec<u8>| {
        if with_refcount && let Some(n) = refcount(v) {
            // `jv_get_refcnt(x) - 1` of a copy: the count before the copy.
            let _ = write!(out, " ({n})");
        }
    };
    if indent > MAX_PRINT_DEPTH {
        out.extend_from_slice(b"<skipped: too deep>");
        return;
    }
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(false) => out.extend_from_slice(b"false"),
        Value::Bool(true) => out.extend_from_slice(b"true"),
        Value::Number(n) => n.write_json(out),
        Value::String(s) => {
            write_json_string(s.as_str(), false, out);
            put_refcnt(v, out);
        }
        Value::Array(a) => {
            if a.is_empty() {
                out.extend_from_slice(b"[]");
                return;
            }
            out.push(b'[');
            for (i, elem) in a.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                dump_term(elem, with_refcount, indent + 1, out);
            }
            out.push(b']');
            put_refcnt(v, out);
        }
        Value::Object(o) => {
            if o.is_empty() {
                out.extend_from_slice(b"{}");
                return;
            }
            out.push(b'{');
            for (i, (k, val)) in o.iter().enumerate() {
                if i != 0 {
                    out.push(b',');
                }
                write_json_string(k.as_str(), false, out);
                out.push(b':');
                dump_term(val, with_refcount, indent + 1, out);
            }
            out.push(b'}');
            put_refcnt(v, out);
        }
    }
}

impl Jq {
    /// Writes trace text where jq's `printf` would (stdout by default).
    pub(super) fn trace_write(&mut self, s: &[u8]) {
        match &mut self.trace_out {
            Some(w) => {
                let _ = w.write_all(s);
            }
            None => {
                let _ = std::io::stdout().write_all(s);
            }
        }
    }

    /// The per-instruction trace line.
    pub(super) fn trace_instruction(&mut self, prog: &Program, pc: usize, backtracking: bool) {
        let func = self.stk.frame(self.curr_frame).func;
        let f = &prog.funcs[func as usize];
        let mut out = dump_operation(&f.bc, pc - f.base as usize).into_bytes();
        out.push(b'\t');
        if !backtracking {
            let opdesc = opcode_describe(prog.code[pc]);
            let mut stack_in = opdesc.stack_in;
            if stack_in == -1 {
                stack_in = prog.code[pc + 1] as i32;
            }
            let mut param: StackPtr = self.stk_top;
            for i in 0..stack_in {
                if i != 0 {
                    out.extend_from_slice(b" | ");
                    param = self.next_or_zero(param);
                }
                if param == 0 {
                    break;
                }
                dump_term(self.stk.value(param), true, 0, &mut out);
            }
            if self.debug_trace & JQ_DEBUG_TRACE_DETAIL != 0 {
                loop {
                    param = self.next_or_zero(param);
                    if param == 0 {
                        break;
                    }
                    out.extend_from_slice(b" || ");
                    dump_term(self.stk.value(param), true, 0, &mut out);
                }
            }
        } else {
            out.extend_from_slice(b"\t<backtracking>");
        }
        out.push(b'\n');
        self.trace_write(&out);
    }

    fn next_or_zero(&self, p: StackPtr) -> StackPtr {
        if p == 0 { 0 } else { self.stk.next(p) }
    }

    /// `LOADV`/`LOADVN`: `V<n> = <value with refcounts>`.
    pub(super) fn trace_var_refcount(&mut self, v: u16, var: usize) {
        let mut out = format!("V{v} = ").into_bytes();
        dump_term(&self.stk.locals[var], true, 0, &mut out);
        out.push(b'\n');
        self.trace_write(&out);
    }

    /// `STOREV`/`STORE_GLOBAL`: `V<n> = <value> (<refcount>)`.
    pub(super) fn trace_store(&mut self, v: u16, val: &Value) {
        let mut out = format!("V{v} = ").into_bytes();
        dump_term(val, false, 0, &mut out);
        // jv_get_refcnt: 1 for values without a refcount.
        let n = refcount(val).unwrap_or(1);
        let _ = writeln!(out, " ({n})");
        self.trace_write(&out);
    }
}
