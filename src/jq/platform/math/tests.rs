//! Tests for the libm table. Expected values come from the jq 1.8.1 binary.

use super::*;

/// The libm names in `jq -n 'builtins'` (jq 1.8.1 on macOS; glibc has the same set),
/// in `libm.h` order.
const JQ_LIBM_BUILTINS: [&str; 61] = [
    "acos/0",
    "acosh/0",
    "asin/0",
    "asinh/0",
    "atan/0",
    "atan2/2",
    "atanh/0",
    "cbrt/0",
    "cos/0",
    "cosh/0",
    "exp/0",
    "exp2/0",
    "floor/0",
    "hypot/2",
    "j0/0",
    "j1/0",
    "log/0",
    "log10/0",
    "log2/0",
    "pow/2",
    "remainder/2",
    "sin/0",
    "sinh/0",
    "sqrt/0",
    "tan/0",
    "tanh/0",
    "tgamma/0",
    "y0/0",
    "y1/0",
    "jn/2",
    "yn/2",
    "ceil/0",
    "copysign/2",
    "drem/2",
    "erf/0",
    "erfc/0",
    "exp10/0",
    "expm1/0",
    "fabs/0",
    "fdim/2",
    "fma/3",
    "fmax/2",
    "fmin/2",
    "fmod/2",
    "gamma/0",
    "lgamma/0",
    "log1p/0",
    "logb/0",
    "nearbyint/0",
    "nextafter/2",
    "nexttoward/2",
    "rint/0",
    "round/0",
    "scalb/2",
    "scalbln/2",
    "significand/0",
    "trunc/0",
    "ldexp/2",
    "modf/0",
    "frexp/0",
    "lgamma_r/0",
];

#[cfg(any(target_vendor = "apple", libm_bessel))]
#[test]
fn table_is_jqs_builtin_set_in_libm_h_order() {
    let names: Vec<String> = table()
        .iter()
        .map(|e| format!("{}/{}", e.name, e.arity))
        .collect();
    assert_eq!(names, JQ_LIBM_BUILTINS);
    // What the C library lacks, so jq's configure leaves out (its `_NO` stubs):
    // nothing on macOS and glibc.
    let lacking: &[&str] = if cfg!(any(target_os = "freebsd", target_os = "netbsd")) {
        &["exp10"]
    } else if cfg!(target_env = "musl") {
        &["gamma"]
    } else if cfg!(windows) {
        &["drem", "exp10", "gamma", "scalb", "significand", "lgamma_r"]
    } else {
        &[]
    };
    let missing: Vec<&str> = table()
        .iter()
        .filter(|e| e.func.is_none())
        .map(|e| e.name)
        .collect();
    assert_eq!(missing, lacking);
}

#[test]
fn shapes_match_arities() {
    for e in table() {
        let expected = match e.func {
            Some(LibmFn::DD(_)) | Some(LibmFn::DA(_)) => 0,
            Some(LibmFn::DDD(_)) => 2,
            Some(LibmFn::DDDD(_)) => 3,
            None => continue,
        };
        assert_eq!(e.arity, expected, "{}", e.name);
    }
}

#[test]
fn not_found_message() {
    let missing = LibmEntry {
        name: "gamma",
        arity: 0,
        func: None,
    };
    let err = Error::Msg("Error: gamma/0 not found at build time".into());
    assert_eq!(missing.not_found(), err);
    assert_eq!(missing.apply(1.0, &[]), Err(err));
    let missing2 = LibmEntry {
        name: "drem",
        arity: 2,
        func: None,
    };
    assert_eq!(
        missing2.not_found().message(),
        "Error: drem/2 not found at build time"
    );
}

fn call(name: &str, arity: usize, input: f64, args: &[f64]) -> LibmOutput {
    lookup(name, arity)
        .unwrap_or_else(|| panic!("{name}/{arity} missing"))
        .apply(input, args)
        .unwrap()
}

fn num(name: &str, x: f64) -> f64 {
    match call(name, 0, x, &[]) {
        LibmOutput::Number(n) => n,
        other => panic!("{other:?}"),
    }
}

fn num2(name: &str, a: f64, b: f64) -> f64 {
    // The input of a two-argument builtin is ignored.
    match call(name, 2, f64::NAN, &[a, b]) {
        LibmOutput::Number(n) => n,
        other => panic!("{other:?}"),
    }
}

fn pair(name: &str, x: f64) -> [f64; 2] {
    match call(name, 0, x, &[]) {
        LibmOutput::Pair(p) => p,
        other => panic!("{other:?}"),
    }
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

/// Functions IEEE 754 / C99 specify exactly, so any libm agrees (jq 1.8.1 output).
#[test]
fn exact_functions() {
    assert_eq!(num("floor", -2.5), -3.0);
    assert_eq!(num("ceil", -2.5), -2.0);
    assert_eq!(num("round", 2.5), 3.0);
    assert_eq!(num("round", -2.5), -3.0);
    assert_eq!(num("rint", 2.5), 2.0);
    assert_eq!(num("rint", 3.5), 4.0);
    assert_eq!(num("nearbyint", 2.5), 2.0);
    assert_eq!(num("trunc", -3.7), -3.0);
    assert_eq!(num("fabs", -0.5), 0.5);
    assert_eq!(num("sqrt", 2.0), std::f64::consts::SQRT_2);
    assert_eq!(num("logb", 8.0), 3.0);
    assert_eq!(num("logb", 0.0), f64::NEG_INFINITY);
    assert!(same(num("trunc", -0.5), -0.0));
    assert_eq!(pair("frexp", 8.0), [0.5, 4.0]);
    assert_eq!(pair("frexp", 0.0), [0.0, 0.0]);
    assert_eq!(pair("frexp", 5e-324), [0.5, -1073.0]);
    assert_eq!(pair("modf", -3.5), [-0.5, -3.0]);
    assert_eq!(pair("modf", f64::INFINITY), [0.0, f64::INFINITY]);
    assert_eq!(num2("fmod", 5.0, 3.0), 2.0);
    assert_eq!(num2("fmod", -5.0, 3.0), -2.0);
    assert_eq!(num2("remainder", 5.0, 3.0), -1.0);
    assert_eq!(num2("copysign", 1.0, -0.0), -1.0);
    assert_eq!(num2("fmin", f64::NAN, 1.0), 1.0);
    assert_eq!(num2("fmax", 2.0, 0.5), 2.0);
    assert_eq!(num2("fdim", 1.0, 2.0), 0.0);
    assert_eq!(num2("nextafter", 0.0, 1.0), 5e-324);
    assert_eq!(num2("nextafter", 0.5, 2.0), 0.5000000000000001);
    assert_eq!(num2("nexttoward", 0.5, 2.0), 0.5000000000000001);
    assert_eq!(num2("nexttoward", 1.0, 1.0), 1.0);
    assert_eq!(num2("ldexp", 0.5, 2.0), 2.0);
    assert_eq!(num2("scalbln", 0.5, 2.0), 2.0);
    // The second argument is truncated to an integer.
    assert_eq!(num2("ldexp", 1.0, 2.9), 4.0);
    assert_eq!(num2("scalbln", 1.0, -1.9), 0.5);
    assert_eq!(
        call("fma", 3, f64::NAN, &[2.0, 3.0, 4.0]),
        LibmOutput::Number(10.0)
    );
    // fma rounds once: 0.1 * 10 - 1 is not 0.
    assert_eq!(
        call("fma", 3, f64::NAN, &[0.1, 10.0, -1.0]),
        LibmOutput::Number(5.551115123125783e-17)
    );
}

/// macOS specifics (jq 1.8.1 output on macOS arm64).
#[cfg(target_vendor = "apple")]
#[test]
fn apple_renames() {
    // gamma is tgamma: gamma(0.5) = sqrt(pi).
    assert_eq!(num("gamma", 0.5), 1.772453850905516);
    assert_eq!(num("gamma", 0.5), num("tgamma", 0.5));
    assert_eq!(num("exp10", 0.5), 3.1622776601683795);
    assert_eq!(num2("drem", 5.0, 3.0), -1.0);
    assert_eq!(num2("drem", 0.5, 2.0), 0.5);
    // significand is 2*frexp(x).
    assert_eq!(num("significand", 0.5), 1.0);
    assert_eq!(num("significand", 12.0), 1.5);
    assert_eq!(num("significand", -3.0), -1.5);
    assert_eq!(num("significand", 0.0), 0.0);
}

/// C's double-to-int conversion for `jn`, `yn`, `ldexp` and `scalbln` saturates on
/// aarch64 and maps NaN to 0 (jq 1.8.1 on macOS arm64).
#[cfg(target_arch = "aarch64")]
#[test]
fn integer_arguments_saturate_on_aarch64() {
    assert_eq!(num2("ldexp", 1.0, 1e10), f64::INFINITY);
    assert_eq!(num2("ldexp", 1.0, -1e10), 0.0);
    assert_eq!(num2("ldexp", 1.0, f64::NAN), 1.0);
    assert_eq!(num2("scalbln", 1.0, 1e19), f64::INFINITY);
    // jn(NaN; x) is jn(0; x) = j0(x).
    assert_eq!(num2("jn", f64::NAN, 1.0), num("j0", 1.0));
}

/// Every libm builtin on a grid of inputs, against jq 1.8.1 on macOS arm64
/// (`testdata/libm_corpus.txt`, bit patterns).
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn corpus_matches_jq_on_macos_arm64() {
    fn parse(s: &str) -> Option<f64> {
        match s {
            "nan" => Some(f64::NAN),
            "?" => None, // unspecified in jq (uninitialized)
            hex => Some(f64::from_bits(u64::from_str_radix(hex, 16).unwrap())),
        }
    }
    let corpus = include_str!("../testdata/libm_corpus.txt");
    let mut failures = Vec::new();
    let mut seen = std::collections::HashSet::new();
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
        seen.insert(sig.to_owned());
        let entry = lookup(name, arity).unwrap_or_else(|| panic!("{sig} missing"));
        let got = match entry.apply(args[0], if arity == 0 { &[] } else { &args }) {
            Ok(LibmOutput::Number(x)) => vec![x],
            Ok(LibmOutput::Pair(p)) => p.to_vec(),
            Err(e) => panic!("{sig}: {e}"),
        };
        n += 1;
        let ok = got.len() == want.len()
            && got
                .iter()
                .zip(&want)
                .all(|(g, w)| w.is_none_or(|w| same(*g, w)));
        if !ok {
            failures.push(format!("{sig}{args:?}: jq {want:?}, got {got:?}"));
        }
    }
    assert_eq!(seen.len(), 61, "every libm builtin is covered");
    assert!(n > 4000, "corpus too small: {n}");
    assert!(
        failures.is_empty(),
        "{} of {n} differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
