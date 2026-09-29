//! Track B1's fixture suite: every case in `../testdata/b1_*.json` calls one C builtin
//! through [`function_list`] and compares with what the jq 1.8.1 binary printed for
//! `try [0, NAME($a; $b)] catch [1, .]`. Regenerate the fixtures with
//! `python3 src/jq/builtins/testdata/b1_gen.py`.

use crate::jq::builtins::testing::TestHost;
use crate::jq::builtins::{CFunction, function_list};
use crate::jq::value::{Object, Value, parse_sized};

/// `. * 1` applied to every number: literals become native doubles (the generator's
/// `nat`).
fn native(v: Value) -> Value {
    match v {
        Value::Number(n) => Value::number(n.value() * 1.0),
        Value::Array(a) => a.into_iter().map(native).collect(),
        Value::Object(o) => {
            let o: Object = o
                .iter()
                .map(|(k, v)| (k.clone(), native(v.clone())))
                .collect();
            Value::Object(o)
        }
        other => other,
    }
}

fn field<'a>(case: &'a Value, name: &str) -> Option<&'a Value> {
    case.as_object().and_then(|o| o.get(name))
}

fn text(case: &Value, name: &str) -> String {
    match field(case, name) {
        Some(Value::String(s)) => s.as_str().to_owned(),
        other => panic!("case field {name} is not a string: {other:?}"),
    }
}

fn parse(json: &str) -> Value {
    parse_sized(json.as_bytes()).unwrap_or_else(|e| panic!("bad fixture JSON {json:?}: {e}"))
}

/// Runs every case of a fixture file; panics listing the mismatches.
fn run_fixture(name: &str, json: &str) {
    let fixture = parse(json);
    let cases = match field(&fixture, "cases") {
        Some(Value::Array(a)) => a.clone(),
        _ => panic!("{name}: no cases"),
    };
    let list = function_list();
    let mut failures = Vec::new();
    for case in cases.iter() {
        let f = text(case, "f");
        let input_text = text(case, "in");
        let arg_texts: Vec<String> = match field(case, "args") {
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| v.as_str().expect("arg text").to_owned())
                .collect(),
            _ => panic!("{name}: case without args"),
        };
        let is_native = matches!(field(case, "native"), Some(Value::Bool(true)));
        let want = text(case, "out");

        let mut input = parse(&input_text);
        if is_native {
            input = native(input);
        }
        // An argument `.` is the input itself (the same value, as jq passes it).
        let mut args: Vec<Value> = arg_texts
            .iter()
            .map(|t| match t.as_str() {
                "." => input.clone(),
                t if is_native => native(parse(t)),
                t => parse(t),
            })
            .collect();
        let cf: &CFunction = list
            .iter()
            .find(|c| c.name == f && c.nargs == args.len() + 1)
            .unwrap_or_else(|| panic!("no builtin {f}/{}", args.len()));
        let mut host = TestHost::default();
        let got = match (cf.f)(&mut host, input, &mut args) {
            Ok(v) => Value::from(vec![Value::number(0.0), v]),
            Err(e) => Value::from(vec![Value::number(1.0), e.into_value()]),
        }
        .to_json();
        if got != want {
            failures.push(format!(
                "{f}: in={input_text} args={arg_texts:?}{}\n   jq: {want}\n  got: {got}",
                if is_native { " (native)" } else { "" }
            ));
        }
    }
    if !failures.is_empty() {
        let n = failures.len();
        failures.truncate(40);
        panic!(
            "{name}: {n} of {} cases differ from jq 1.8.1:\n{}",
            cases.len(),
            failures.join("\n")
        );
    }
}

#[test]
fn fixture_binops() {
    run_fixture("b1_binops.json", include_str!("../testdata/b1_binops.json"));
}

#[test]
fn fixture_general() {
    run_fixture(
        "b1_general.json",
        include_str!("../testdata/b1_general.json"),
    );
}

#[test]
fn fixture_strings() {
    run_fixture(
        "b1_strings.json",
        include_str!("../testdata/b1_strings.json"),
    );
}

#[test]
fn fixture_format() {
    run_fixture("b1_format.json", include_str!("../testdata/b1_format.json"));
}

#[test]
fn fixture_random() {
    run_fixture("b1_random.json", include_str!("../testdata/b1_random.json"));
}

#[test]
fn fixture_matrix() {
    run_fixture("b1_matrix.json", include_str!("../testdata/b1_matrix.json"));
}

#[test]
fn type_names_are_shared_but_copied_before_a_change() {
    use crate::jq::builtins::binops::f_plus;
    use crate::jq::builtins::general::f_type;
    let mut host = TestHost::default();
    let t = f_type(&mut host, parse("1"), &mut []).unwrap();
    // `type + "!"` must not append to the cached "number".
    let mut args = [t, parse("\"!\"")];
    let r = f_plus(&mut host, Value::Null, &mut args).unwrap();
    assert_eq!(r.to_json(), "\"number!\"");
    let again = f_type(&mut host, parse("2"), &mut []).unwrap();
    assert_eq!(again.to_json(), "\"number\"");
}

// ---------------------------------------------------------------- host-dependent builtins
//
// These builtins talk to the interpreter through `Host`, so they are exercised with a
// scripted `TestHost` instead of the fixtures. Expected values and messages come from
// jq 1.8.1 (commands in the comments).

mod host {
    use std::collections::VecDeque;

    use crate::jq::builtins::general::*;
    use crate::jq::builtins::testing::TestHost;
    use crate::jq::builtins::{CResult, Host};
    use crate::jq::value::{Error, Value, parse_sized};

    fn v(json: &str) -> Value {
        parse_sized(json.as_bytes()).unwrap()
    }

    fn show(r: CResult) -> String {
        match r {
            Ok(v) => v.to_json(),
            Err(e) => format!("ERR {}", e.value().to_json()),
        }
    }

    #[test]
    fn input_pulls_from_the_host() {
        let mut host = TestHost {
            inputs: VecDeque::from([Ok(v("1")), Err(Error::msg("bad input")), Ok(v("[2]"))]),
            ..TestHost::default()
        };
        assert_eq!(show(f_input(&mut host, v("null"), &mut [])), "1");
        assert_eq!(
            show(f_input(&mut host, v("null"), &mut [])),
            r#"ERR "bad input""#
        );
        assert_eq!(show(f_input(&mut host, v("null"), &mut [])), "[2]");
        // `jq -n 'try input catch .'` => "break" (no more input)
        assert_eq!(
            show(f_input(&mut host, v("null"), &mut [])),
            r#"ERR "break""#
        );
    }

    #[test]
    fn debug_and_stderr_pass_the_input_through() {
        let mut host = TestHost::default();
        assert_eq!(
            show(f_debug(&mut host, v("{\"a\":1}"), &mut [])),
            "{\"a\":1}"
        );
        assert_eq!(show(f_stderr(&mut host, v("\"x\""), &mut [])), "\"x\"");
        assert_eq!(host.debugged.len(), 1);
        assert_eq!(host.debugged[0].to_json(), "{\"a\":1}");
        assert_eq!(host.stderred.len(), 1);
        assert_eq!(host.stderred[0].to_json(), "\"x\"");
    }

    #[test]
    fn halt_and_halt_error() {
        let mut host = TestHost::default();
        assert_eq!(show(f_halt(&mut host, v("1"), &mut [])), "true");
        assert!(matches!(host.halted, Some((None, None))));

        let mut host = TestHost::default();
        let mut args = [v("3")];
        assert_eq!(
            show(f_halt_error(&mut host, v("\"bye\""), &mut args)),
            "true"
        );
        let (code, msg) = host.halted.take().expect("halted");
        assert_eq!(code.unwrap().to_json(), "3");
        assert_eq!(msg.unwrap().to_json(), "\"bye\"");

        // `jq -n '"abc" | halt_error("a")'`: the error names the input, not the code.
        let mut host = TestHost::default();
        let mut args = [v("\"a\"")];
        assert_eq!(
            show(f_halt_error(&mut host, v("\"abc\""), &mut args)),
            r#"ERR "string (\"abc\") halt_error/1: number required""#
        );
        assert!(host.halted.is_none());
        // `jq -n '{"a":[1,2,3]} | halt_error(null)'`
        let mut args = [v("null")];
        assert_eq!(
            show(f_halt_error(&mut host, v("{\"a\":[1,2,3]}"), &mut args)),
            r#"ERR "object ({\"a\":[1,2,3]}) halt_error/1: number required""#
        );
    }

    #[test]
    fn origins_search_list_and_input_position() {
        let mut host = TestHost {
            lib_dirs: v("[\"~/.jq\",\"$ORIGIN/../lib/jq\"]"),
            prog_origin: v("\"/prog\""),
            jq_origin: v("\"/usr/bin\""),
            line: v("7"),
            ..TestHost::default()
        };
        assert_eq!(
            show(f_get_search_list(&mut host, v("null"), &mut [])),
            "[\"~/.jq\",\"$ORIGIN/../lib/jq\"]"
        );
        assert_eq!(
            show(f_get_prog_origin(&mut host, v("1"), &mut [])),
            "\"/prog\""
        );
        assert_eq!(
            show(f_get_jq_origin(&mut host, v("1"), &mut [])),
            "\"/usr/bin\""
        );
        assert_eq!(show(f_current_line(&mut host, v("1"), &mut [])), "7");
        // `jq -n 'input_filename'` => null
        assert_eq!(show(f_current_filename(&mut host, v("1"), &mut [])), "null");
        host.filename = Some(v("\"data.json\""));
        assert_eq!(
            show(f_current_filename(&mut host, v("1"), &mut [])),
            "\"data.json\""
        );
    }

    #[test]
    fn modulemeta_checks_its_input() {
        let mut host = TestHost::default();
        // TestHost's module_meta answers null for any name.
        assert_eq!(show(f_modulemeta(&mut host, v("\"m\""), &mut [])), "null");
        // `jq -n '1 | modulemeta'`
        assert_eq!(
            show(f_modulemeta(&mut host, v("1"), &mut [])),
            r#"ERR "modulemeta input module name must be a string""#
        );
    }

    #[test]
    fn env_reads_the_process_environment() {
        let mut host = TestHost::default();
        let env = f_env(&mut host, v("null"), &mut []).unwrap();
        let obj = env.as_object().expect("object");
        let mut n = 0;
        for (name, value) in std::env::vars_os() {
            if name.as_encoded_bytes().first() == Some(&b'=') {
                continue;
            }
            n += 1;
            let key = name.to_string_lossy();
            let got = obj.get(&key).unwrap_or_else(|| panic!("env lacks {key}"));
            assert_eq!(got.as_str(), Some(&*value.to_string_lossy()), "{key}");
        }
        assert!(n > 0);
        assert!(obj.len() <= n + 1);
    }

    /// A host that tracks paths like the VM: `path` is the tracked path and `current`
    /// the value at it (`jq->path`, `jq->value_at_path`).
    struct PathHost {
        inner: TestHost,
        path: Value,
        current: Value,
    }

    impl Host for PathHost {
        fn next_input(&mut self) -> Option<CResult> {
            self.inner.next_input()
        }
        fn debug(&mut self, v: &Value) {
            self.inner.debug(v)
        }
        fn stderr(&mut self, v: &Value) {
            self.inner.stderr(v)
        }
        fn halt(&mut self, exit_code: Option<Value>, error_message: Option<Value>) {
            self.inner.halt(exit_code, error_message)
        }
        fn lib_dirs(&self) -> Value {
            self.inner.lib_dirs()
        }
        fn prog_origin(&self) -> Value {
            self.inner.prog_origin()
        }
        fn jq_origin(&self) -> Value {
            self.inner.jq_origin()
        }
        fn module_meta(&mut self, name: &Value) -> CResult {
            self.inner.module_meta(name)
        }
        fn current_filename(&self) -> Option<Value> {
            self.inner.current_filename()
        }
        fn current_line(&self) -> Value {
            self.inner.current_line()
        }
        fn path_append(&mut self, v: Value, p: Value, value_at_path: CResult) -> CResult {
            let value_at_path = value_at_path?;
            if !v.identical(&self.current) {
                return Ok(value_at_path);
            }
            let (Value::Array(path), Value::Array(p)) = (&mut self.path, p) else {
                panic!("paths are arrays");
            };
            path.extend(p);
            self.current = value_at_path.clone();
            Ok(value_at_path)
        }
    }

    #[test]
    fn getpath_extends_the_tracked_path() {
        // `jq -c 'path(getpath(["a",1]))' <<< '{"a":[1,2]}'` => ["a",1]
        let root = v("{\"a\":[1,2]}");
        let mut host = PathHost {
            inner: TestHost::default(),
            path: v("[]"),
            current: root.clone(),
        };
        let mut args = [v("[\"a\",1]")];
        assert_eq!(show(f_getpath(&mut host, root.clone(), &mut args)), "2");
        assert_eq!(host.path.to_json(), "[\"a\",1]");
        // A value that isn't the one at the current path doesn't extend it.
        let mut args = [v("[\"a\"]")];
        assert_eq!(show(f_getpath(&mut host, root, &mut args)), "[1,2]");
        assert_eq!(host.path.to_json(), "[\"a\",1]");
        // Errors pass through untouched:
        // `jq -c 'try getpath(["a","b"]) catch .' <<< '{"a":1}'`
        let mut args = [v("[\"a\",\"b\"]")];
        let cur = host.current.clone();
        assert_eq!(
            show(f_getpath(&mut host, v("{\"a\":1}"), &mut args)),
            r#"ERR "Cannot index number with string \"b\"""#
        );
        assert!(host.current.identical(&cur));
    }
}
