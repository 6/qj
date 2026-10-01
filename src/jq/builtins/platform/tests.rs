//! Tests for the platform-backed builtins, called through `function_list()` the way the
//! VM calls them. Every expectation comes from the jq 1.8.1 binary: `cases.jsonl` is
//! written by `gen_cases.py`, and the regex and libm corpora in
//! `src/jq/platform/testdata/` were recorded by Track X.

use super::*;
use crate::jq::builtins::function_list;
use crate::jq::builtins::testing::TestHost;
use crate::jq::value::parse_sized;
use std::collections::BTreeSet;
use std::process::{Command, Output};
use std::sync::OnceLock;

const ISO: &str = "%Y-%m-%dT%H:%M:%SZ";

fn json(text: &str) -> Value {
    parse_sized(text.as_bytes()).unwrap_or_else(|e| panic!("bad JSON {text}: {e}"))
}

/// The C builtin `name` taking `nargs - 1` arguments, from jq's `function_list`.
fn cfunction(name: &str, nargs: usize) -> CFunction {
    static LIST: OnceLock<Vec<CFunction>> = OnceLock::new();
    LIST.get_or_init(function_list)
        .iter()
        .find(|c| c.name == name && c.nargs == nargs)
        .copied()
        .unwrap_or_else(|| panic!("{name}/{} is not in function_list", nargs - 1))
}

/// `input | name(args...)`.
fn call(name: &str, input: Value, mut args: Vec<Value>) -> CResult {
    let f = cfunction(name, args.len() + 1).f;
    f(&mut TestHost::default(), input, &mut args)
}

/// The output as `tojson` prints it, or the error message value, as JSON.
fn outcome(r: CResult) -> Result<String, String> {
    r.map(|v| v.to_json()).map_err(|e| e.value().to_json())
}

fn ok(name: &str, input: &str, args: &[&str]) -> String {
    let r = call(name, json(input), args.iter().map(|a| json(a)).collect());
    r.unwrap_or_else(|e| panic!("{input} | {name}{args:?}: unexpected error {e}"))
        .to_json()
}

fn err(name: &str, input: &str, args: &[&str]) -> String {
    let r = call(name, json(input), args.iter().map(|a| json(a)).collect());
    match r {
        Ok(v) => panic!("{input} | {name}{args:?}: expected an error, got {v}"),
        Err(e) => e.as_str().expect("string message").to_owned(),
    }
}

// ---------------------------------------------------------------------------------
// cases.jsonl
// ---------------------------------------------------------------------------------

#[derive(Debug)]
enum Expect {
    /// The output, as `tojson` prints it.
    Ok(String),
    /// The error message value, as JSON.
    Err(String),
    /// jq's assertion line before it died of SIGABRT.
    Abort(String),
}

#[derive(Debug)]
struct Case {
    line: usize,
    f: String,
    input: Value,
    args: Vec<Value>,
    tz: Option<String>,
    only: Option<String>,
    expect: Expect,
}

impl Case {
    /// Whether the expectation holds on this platform (`only` in gen_cases.py).
    fn applies(&self) -> bool {
        // musl's strftime has no %k or %l, so jq built on it fails these.
        if cfg!(target_env = "musl")
            && self.f.starts_with("strf")
            && self.args.iter().any(|a| {
                a.as_str()
                    .is_some_and(|s| s.contains("%k") || s.contains("%l"))
            })
        {
            return false;
        }
        match self.only.as_deref() {
            None => true,
            Some("macos") => cfg!(target_os = "macos"),
            Some("aarch64") => cfg!(target_arch = "aarch64"),
            Some("macos-aarch64") => cfg!(all(target_os = "macos", target_arch = "aarch64")),
            Some(other) => panic!("line {}: unknown platform {other}", self.line),
        }
    }

    fn run(&self) -> Result<String, String> {
        outcome(call(&self.f, self.input.clone(), self.args.clone()))
    }

    /// `None` if the builtin does what jq does, else a description of the difference.
    fn check(&self) -> Option<String> {
        let want = match &self.expect {
            Expect::Ok(out) => Ok(out.clone()),
            Expect::Err(msg) => Err(msg.clone()),
            Expect::Abort(_) => panic!("line {}: run aborting cases in a child", self.line),
        };
        let got = self.run();
        (got != want).then(|| {
            format!(
                "line {}: {} | {}{:?}\n  jq:  {want:?}\n  got: {got:?}",
                self.line, self.input, self.f, self.args
            )
        })
    }
}

fn cases() -> Vec<Case> {
    include_str!("cases.jsonl")
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(i, l)| {
            let row = json(l);
            let row = row.as_object().expect("a row is an object");
            let text = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_owned);
            let expect = if let Some(out) = text("ok") {
                Expect::Ok(out)
            } else if let Some(msg) = row.get("err") {
                Expect::Err(msg.to_json())
            } else {
                Expect::Abort(text("abort").expect("ok, err or abort"))
            };
            Case {
                line: i + 1,
                f: text("f").expect("f"),
                input: row.get("input").expect("input").clone(),
                args: row
                    .get("args")
                    .and_then(Value::as_array)
                    .expect("args")
                    .iter()
                    .cloned()
                    .collect(),
                tz: text("tz"),
                only: text("only"),
                expect,
            }
        })
        .collect()
}

fn assert_no_failures(failures: Vec<String>, n: usize) {
    assert!(
        failures.is_empty(),
        "{} of {n} cases differ from jq:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The cases that hold in any time zone and locale (gen_cases.py checks that).
#[test]
fn cases_match_jq() {
    let cases = cases();
    let run: Vec<&Case> = cases
        .iter()
        .filter(|c| c.tz.is_none() && !matches!(c.expect, Expect::Abort(_)) && c.applies())
        .collect();
    assert!(run.len() > 250, "only {} cases", run.len());
    let failures: Vec<String> = run.iter().filter_map(|c| c.check()).collect();
    assert_no_failures(failures, run.len());
}

/// Env var naming the case group a child process runs.
const CHILD_ENV: &str = "QJ_TEST_B2_CHILD";

/// Re-run this test binary for one `#[ignore]`d test, with extra environment.
fn run_child(test: &str, env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--exact",
        &format!("jq::builtins::platform::tests::{test}"),
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// Whether the tz database has `zone` (TZ=UTC works without it).
fn zone_available(zone: &str) -> bool {
    zone == "UTC"
        || ["/usr/share/zoneinfo", "/var/db/timezone/zoneinfo"]
            .iter()
            .any(|dir| std::path::Path::new(dir).join(zone).exists())
}

/// The cases for a given `TZ` (with `LC_ALL=C`), each group in a child process: the
/// time zone and locale are process-wide, and other tests change `TZ` while they run.
#[test]
fn cases_in_time_zones_match_jq() {
    let cases = cases();
    let zones: BTreeSet<&str> = cases.iter().filter_map(|c| c.tz.as_deref()).collect();
    assert!(zones.len() >= 3);
    for tz in zones {
        if !zone_available(tz) {
            eprintln!("skipping TZ={tz}: tz database not installed");
            continue;
        }
        let out = run_child(
            "time_zone_cases_child",
            &[(CHILD_ENV, tz), ("TZ", tz), ("LC_ALL", "C")],
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "TZ={tz}: child failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
#[ignore = "run by cases_in_time_zones_match_jq"]
fn time_zone_cases_child() {
    let Ok(tz) = std::env::var(CHILD_ENV) else {
        return;
    };
    let cases = cases();
    let run: Vec<&Case> = cases
        .iter()
        .filter(|c| c.tz.as_deref() == Some(tz.as_str()) && c.applies())
        .collect();
    assert!(!run.is_empty(), "no cases for TZ={tz}");
    let failures: Vec<String> = run.iter().filter_map(|c| c.check()).collect();
    assert_no_failures(failures, run.len());
}

/// jq 1.8.1 aborts on these (a failed `assert()`); so does the port. Each runs in a
/// child process, which must die of SIGABRT after printing jq's assertion line.
#[cfg(unix)]
#[test]
fn aborting_cases_abort_like_jq() {
    use std::os::unix::process::ExitStatusExt;
    let cases = cases();
    let aborting: Vec<&Case> = cases
        .iter()
        .filter(|c| matches!(c.expect, Expect::Abort(_)) && c.applies())
        .collect();
    assert!(!aborting.is_empty());
    for case in aborting {
        let Expect::Abort(assertion) = &case.expect else {
            unreachable!()
        };
        let line = case.line.to_string();
        let tz = case.tz.as_deref().unwrap_or("UTC");
        let out = run_child(
            "aborting_case_child",
            &[(CHILD_ENV, &line), ("TZ", tz), ("LC_ALL", "C")],
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.signal(),
            Some(libc::SIGABRT),
            "line {line}: expected SIGABRT, got {:?}\nstdout: {}\nstderr: {stderr}",
            out.status,
            String::from_utf8_lossy(&out.stdout)
        );
        // The assertion text is the platform's (macOS here; glibc words it differently).
        if cfg!(target_os = "macos") {
            assert!(
                stderr.lines().any(|l| l == assertion),
                "line {line}: stderr lacks {assertion:?}:\n{stderr}"
            );
        } else {
            assert!(!stderr.trim().is_empty(), "line {line}: no assertion text");
        }
    }
}

#[test]
#[ignore = "run by aborting_cases_abort_like_jq"]
fn aborting_case_child() {
    let Ok(line) = std::env::var(CHILD_ENV) else {
        return;
    };
    let line: usize = line.parse().unwrap();
    let cases = cases();
    let case = cases.iter().find(|c| c.line == line).expect("case line");
    let got = case.run();
    panic!("line {line} returned instead of aborting: {got:?}");
}

// ---------------------------------------------------------------------------------
// _match_impl
// ---------------------------------------------------------------------------------

/// Every `_match_impl` call in Track X's corpus (the regex cases of `onig.test` and
/// `manonig.test`, plus edge cases), as jq values.
#[test]
fn regex_corpus_matches_jq() {
    let corpus = include_str!("../../platform/testdata/regex_corpus.jsonl");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in corpus.lines().filter(|l| !l.trim().is_empty()) {
        let row = json(line);
        let row = row.as_object().unwrap();
        let field = |k: &str| row.get(k).cloned();
        let args = vec![
            field("re").unwrap(),
            field("flags").unwrap(),
            field("test").unwrap(),
        ];
        let want = match (field("out"), field("err")) {
            (Some(out), _) => Ok(out.as_str().unwrap().to_owned()),
            (_, Some(err)) => Err(err.to_json()),
            _ => panic!("corpus row without a result: {line}"),
        };
        n += 1;
        let got = outcome(call("_match_impl", field("input").unwrap(), args));
        if got != want {
            failures.push(format!("{line}\n  got: {got:?}"));
        }
    }
    assert!(n > 500, "corpus too small: {n}");
    assert_no_failures(failures, n);
}

#[test]
fn match_object_keys_are_not_shared() {
    // jq allocates every key of every match and capture object anew. Keys become
    // values (`keys`, `paths`), and `--debug-trace` prints their refcounts:
    // `"a" | match("a") | keys_unsorted` shows `["offset" (1),...]`.
    let out = call(
        "_match_impl",
        json(r#""aa""#),
        vec![json(r#""(?<n>a)""#), json(r#""g""#), json("false")],
    )
    .unwrap();
    let Value::Array(matches) = out else {
        panic!("not an array: {}", out.to_json())
    };
    for m in matches.iter() {
        let obj = m.as_object().unwrap();
        for (k, v) in obj.iter() {
            assert_eq!(k.refcount(), 1, "match key {}", k.as_str());
            if let Value::Array(caps) = v {
                for c in caps.iter() {
                    for (ck, _) in c.as_object().unwrap().iter() {
                        assert_eq!(ck.refcount(), 1, "capture key {}", ck.as_str());
                    }
                }
            }
        }
    }
}

#[test]
fn match_object_key_orders() {
    // A match is offset, length, string, captures. A non-empty capture of a non-empty
    // match is offset, length, string, name; any other capture is offset, string,
    // length, name.
    assert_eq!(
        ok(
            "_match_impl",
            r#""abc""#,
            &[r#""(?<n>a)(x?)(y)?""#, "null", "false"]
        ),
        concat!(
            r#"[{"offset":0,"length":1,"string":"a","captures":["#,
            r#"{"offset":0,"length":1,"string":"a","name":"n"},"#,
            r#"{"offset":1,"string":"","length":0,"name":null},"#,
            r#"{"offset":-1,"string":null,"length":0,"name":null}]}]"#
        )
    );
    assert_eq!(
        ok(
            "_match_impl",
            r#""abc""#,
            &[r#""(?<n>b?)""#, "null", "false"]
        ),
        r#"[{"offset":0,"length":0,"string":"","captures":[{"offset":0,"string":"","length":0,"name":"n"}]}]"#
    );
}

#[test]
fn match_checks_types_in_jqs_order() {
    let m = |input: &str, args: &[&str]| err("_match_impl", input, args);
    assert_eq!(
        m("1", &["2", "3", "true"]),
        "number (1) cannot be matched, as it is not a string"
    );
    assert_eq!(
        m(r#""a""#, &["2", "3", "true"]),
        "number (2) is not a string"
    );
    assert_eq!(
        m(r#""a""#, &[r#""(""#, "3", "true"]),
        "number (3) is not a string"
    );
    assert_eq!(
        m(r#""a""#, &[r#""(""#, r#""gq""#, "true"]),
        "gq is not a valid modifier string"
    );
    assert_eq!(
        m(r#""a""#, &[r#""(""#, "null", "true"]),
        "Regex failure: end pattern with unmatched parenthesis"
    );
    // jv_dump_string_trunc keeps 11 bytes without splitting a character.
    assert_eq!(
        m(r#"{"é":"😀😀😀😀"}"#, &[r#""a""#, "null", "false"]),
        r#"object ({"é":"😀...) cannot be matched, as it is not a string"#
    );
}

#[test]
fn only_true_selects_test_mode() {
    let t = |mode: &str| ok("_match_impl", r#""abc""#, &[r#""b""#, "null", mode]);
    assert_eq!(t("true"), "true");
    let matches = r#"[{"offset":1,"length":1,"string":"b","captures":[]}]"#;
    for mode in ["false", "null", "1", r#""true""#, "[true]"] {
        assert_eq!(t(mode), matches, "{mode}");
    }
    assert_eq!(
        ok("_match_impl", r#""abc""#, &[r#""x""#, r#""g""#, "true"]),
        "false"
    );
}

// ---------------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------------

/// jq.test's "Check day-of-week and day of year computations (should trip an assert if
/// this fails)", through the builtins:
/// `last(range(365 * 67)|("1970-03-01T01:02:03Z"|strptime(ISO)|mktime) + (86400 * .)|strftime(ISO)|strptime(ISO))`.
#[test]
fn day_of_week_and_year_round_trip() {
    let iso = Value::from(ISO);
    let start = call(
        "strptime",
        Value::from("1970-03-01T01:02:03Z"),
        vec![iso.clone()],
    )
    .unwrap();
    let start = call("mktime", start, vec![]).unwrap().as_f64().unwrap();
    let mut last = Value::Null;
    for day in 0..365 * 67 {
        let t = Value::number(start + 86400.0 * day as f64);
        let s = call("strftime", t, vec![iso.clone()]).unwrap();
        last = call("strptime", s, vec![iso.clone()]).unwrap();
    }
    assert_eq!(last.to_json(), "[2037,1,11,1,2,3,3,41]");
}

#[test]
fn fromdate_and_todate_compose() {
    // builtin.jq: fromdate is strptime(ISO) | mktime; todate is strftime(ISO).
    let parsed = call(
        "strptime",
        Value::from("2015-03-05T23:51:47Z"),
        vec![Value::from(ISO)],
    )
    .unwrap();
    let t = call("mktime", parsed, vec![]).unwrap();
    assert_eq!(t.to_json(), "1425599507");
    let s = call("strftime", t.clone(), vec![Value::from(ISO)]).unwrap();
    assert_eq!(s.to_json(), r#""2015-03-05T23:51:47Z""#);
    let tm = call("gmtime", t, vec![]).unwrap();
    assert_eq!(call("mktime", tm, vec![]).unwrap().to_json(), "1425599507");
}

#[test]
fn strptime_appends_the_unparsed_rest() {
    let parsed = call(
        "strptime",
        Value::from("2015-03-05T23:51:47Z \n x"),
        vec![Value::from(ISO)],
    )
    .unwrap();
    assert_eq!(parsed.to_json(), r#"[2015,2,5,23,51,47,4,63," \n x"]"#);
    // mktime and strftime read only the first 8 elements.
    assert_eq!(
        call("mktime", parsed.clone(), vec![]).unwrap().to_json(),
        "1425599507"
    );
    assert_eq!(
        call("strftime", parsed, vec![Value::from(ISO)])
            .unwrap()
            .to_json(),
        r#""2015-03-05T23:51:47Z""#
    );
}

#[test]
fn gmtime_keeps_fractional_seconds() {
    assert_eq!(
        ok("gmtime", "1425599507.5", &[]),
        "[2015,2,5,23,51,47.5,4,63]"
    );
    assert_eq!(ok("gmtime", "-0.25", &[]), "[1970,0,1,0,0,0.75,4,0]");
    // Literal inputs are read as doubles.
    assert_eq!(ok("gmtime", "1E+2", &[]), "[1970,0,1,0,1,40,4,0]");
}

#[test]
fn now_is_the_current_time() {
    let now = call("now", Value::from("ignored"), vec![]).unwrap();
    let got = now.as_f64().expect("a number");
    let expected = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    assert!(
        (got - expected).abs() < 5.0,
        "now = {got}, expected about {expected}"
    );
    assert!(!now.as_number().unwrap().is_literal());
}

// ---------------------------------------------------------------------------------
// libm
// ---------------------------------------------------------------------------------

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// The doubles in a libm builtin's result: one number, or `DA`'s two.
fn doubles(v: &Value) -> Vec<f64> {
    match v {
        Value::Number(n) => vec![n.value()],
        Value::Array(a) => a.iter().map(|x| x.as_f64().unwrap()).collect(),
        other => panic!("not a libm result: {other}"),
    }
}

/// Each `libm.h` entry gets its own wrapper, which calls that entry's function.
#[test]
fn every_libm_builtin_calls_its_own_function() {
    let list = libm_functions();
    assert_eq!(list.len(), math::table().len());
    let xs = [0.5, 2.0, -1.25, 3.0, 1e-3, 100.0, -7.5];
    for (c, entry) in list.iter().zip(math::table()) {
        assert_eq!(c.name, entry.name);
        assert_eq!(c.nargs, entry.arity + 1, "{}", entry.name);
        // Missing from this C library (see `math::table`'s tests): jq's `_NO`
        // stub, which `missing_libm_function_is_reported_before_type_checks`
        // covers.
        if entry.func.is_none() {
            continue;
        }
        for (i, &x) in xs.iter().enumerate() {
            let args: Vec<f64> =
                [x, xs[(i + 1) % xs.len()], xs[(i + 2) % xs.len()]][..entry.arity].to_vec();
            let want = match entry.apply(x, &args).unwrap() {
                LibmOutput::Number(n) => vec![n],
                LibmOutput::Pair(p) => p.to_vec(),
            };
            let (input, mut arg_values) = if entry.arity == 0 {
                (Value::number(x), vec![])
            } else {
                (
                    Value::Null,
                    args.iter().map(|&a| Value::number(a)).collect(),
                )
            };
            let got = (c.f)(&mut TestHost::default(), input, &mut arg_values).unwrap();
            let got = doubles(&got);
            assert!(
                got.len() == want.len() && got.iter().zip(&want).all(|(g, w)| same(*g, *w)),
                "{}/{} at {x}: got {got:?}, want {want:?}",
                entry.name,
                entry.arity
            );
        }
    }
}

/// jq's libm output for the whole table, through the builtins (macOS arm64 libm).
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn libm_corpus_matches_jq_on_macos_arm64() {
    fn parse(s: &str) -> Option<f64> {
        match s {
            "nan" => Some(f64::NAN),
            "?" => None, // uninitialized in jq
            hex => Some(f64::from_bits(u64::from_str_radix(hex, 16).unwrap())),
        }
    }
    let corpus = include_str!("../../platform/testdata/libm_corpus.txt");
    let mut failures = Vec::new();
    let mut n = 0;
    for line in corpus
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut parts = line.split(' ');
        let (sig, args, want) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        let (name, arity) = sig.split_once('/').unwrap();
        let arity: usize = arity.parse().unwrap();
        let args: Vec<f64> = args.split(',').map(|a| parse(a).unwrap()).collect();
        let want: Vec<Option<f64>> = want.split(',').map(parse).collect();
        let (input, arg_values) = if arity == 0 {
            (Value::number(args[0]), vec![])
        } else {
            (
                Value::Null,
                args.iter().map(|&a| Value::number(a)).collect(),
            )
        };
        let got = doubles(&call(name, input, arg_values).unwrap());
        n += 1;
        let matches = got.len() == want.len()
            && got
                .iter()
                .zip(&want)
                .all(|(g, w)| w.is_none_or(|w| same(*g, w)));
        if !matches {
            failures.push(format!("{sig}{args:?}: jq {want:?}, got {got:?}"));
        }
    }
    assert!(n > 4000, "corpus too small: {n}");
    assert_no_failures(failures, n);
}

#[test]
fn libm_type_errors_in_argument_order() {
    assert_eq!(
        err("floor", r#""a""#, &[]),
        r#"string ("a") number required"#
    );
    assert_eq!(
        err("frexp", "[1,2,3,4,5,6,7,8,9]", &[]),
        "array ([1,2,3,4,5,...) number required"
    );
    // A two- or three-argument builtin ignores its input.
    assert_eq!(ok("pow", r#""x""#, &["2", "10"]), "1024");
    assert_eq!(
        err("pow", "0", &[r#""a""#, r#""b""#]),
        r#"string ("a") number required"#
    );
    assert_eq!(err("pow", "0", &["1", "{}"]), "object ({}) number required");
    assert_eq!(
        err("fma", "0", &["1", "null", r#""c""#]),
        "null (null) number required"
    );
    assert_eq!(
        err("fma", "0", &["1", "1", r#""c""#]),
        r#"string ("c") number required"#
    );
}

#[test]
fn libm_results_are_numbers_and_pairs() {
    assert_eq!(ok("sqrt", "1E+2", &[]), "10");
    assert_eq!(ok("sqrt", "-1", &[]), "null");
    assert_eq!(ok("exp", "1000", &[]), "1.7976931348623157e+308");
    assert_eq!(ok("fma", "null", &["2", "3", "4"]), "10");
    assert_eq!(ok("frexp", "8", &[]), "[0.5,4]");
    assert_eq!(ok("modf", "-3.5", &[]), "[-0.5,-3]");
    assert_eq!(ok("lgamma_r", "1", &[]), "[0,1]");
    let r = call("floor", json("1.000"), vec![]).unwrap();
    assert!(
        !r.as_number().unwrap().is_literal(),
        "jv_number, not a literal"
    );
}

/// builtin.c's `_NO` variants, for functions missing at build time (none are on macOS
/// or glibc): the error comes before any type check.
#[test]
fn missing_libm_function_is_reported_before_type_checks() {
    let missing = |name, arity| LibmEntry {
        name,
        arity,
        func: None,
    };
    let e = call_libm(&missing("gamma", 0), Value::from("not a number"), &[]).unwrap_err();
    assert_eq!(e.as_str(), Some("Error: gamma/0 not found at build time"));
    let e = call_libm(
        &missing("drem", 2),
        Value::Null,
        &[Value::from("a"), Value::empty_object()],
    )
    .unwrap_err();
    assert_eq!(e.as_str(), Some("Error: drem/2 not found at build time"));
    let e = call_libm(
        &missing("fma", 3),
        Value::Null,
        &[Value::Null, Value::Null, Value::Null],
    )
    .unwrap_err();
    assert_eq!(e.as_str(), Some("Error: fma/3 not found at build time"));
    let e = call_libm(&missing("frexp", 0), Value::from(1.0), &[]).unwrap_err();
    assert_eq!(e.as_str(), Some("Error: frexp/0 not found at build time"));
}
