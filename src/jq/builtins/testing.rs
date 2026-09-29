//! A [`Host`] for unit tests: scripted inputs, recorded callbacks and halts.
//!
//! Shared by Tracks B1 and B2; extend it additively.

use std::collections::VecDeque;

use super::{CResult, Host};
use crate::jq::value::Value;

/// A scripted, recording [`Host`].
#[derive(Default)]
pub struct TestHost {
    /// Values returned by successive `input` calls; `None` once empty.
    pub inputs: VecDeque<CResult>,
    /// Values passed to `debug`.
    pub debugged: Vec<Value>,
    /// Values passed to `stderr`.
    pub stderred: Vec<Value>,
    /// The last `halt(exit_code, error_message)`.
    pub halted: Option<(Option<Value>, Option<Value>)>,
    /// `input_filename` (`None` is jq's invalid).
    pub filename: Option<Value>,
    /// `input_line_number`.
    pub line: Value,
    /// `get_search_list`, `get_prog_origin`, `get_jq_origin`.
    pub lib_dirs: Value,
    pub prog_origin: Value,
    pub jq_origin: Value,
}

impl Host for TestHost {
    fn next_input(&mut self) -> Option<CResult> {
        self.inputs.pop_front()
    }
    fn debug(&mut self, v: &Value) {
        self.debugged.push(v.clone());
    }
    fn stderr(&mut self, v: &Value) {
        self.stderred.push(v.clone());
    }
    fn halt(&mut self, exit_code: Option<Value>, error_message: Option<Value>) {
        self.halted = Some((exit_code, error_message));
    }
    fn lib_dirs(&self) -> Value {
        self.lib_dirs.clone()
    }
    fn prog_origin(&self) -> Value {
        self.prog_origin.clone()
    }
    fn jq_origin(&self) -> Value {
        self.jq_origin.clone()
    }
    fn module_meta(&mut self, _name: &Value) -> CResult {
        Ok(Value::Null)
    }
    fn current_filename(&self) -> Option<Value> {
        self.filename.clone()
    }
    fn current_line(&self) -> Value {
        self.line.clone()
    }
}
