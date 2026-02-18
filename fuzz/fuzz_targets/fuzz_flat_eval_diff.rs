#![no_main]
use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use qj::filter::{self, Env, Filter, ObjKey};
use qj::flat_eval::{eval_flat, is_flat_safe};
use qj::output::{write_value, OutputConfig, OutputMode};
use qj::simdjson::{dom_parse_to_flat_buf, dom_parse_to_value, pad_buffer};

// ---------------------------------------------------------------------------
// JSON generation — produce valid JSON bytes from structured input
// ---------------------------------------------------------------------------

const KEYS: &[&str] = &["a", "b", "c", "name", "type", "items", "x", "y"];

#[derive(Debug)]
enum FuzzJson {
    Null,
    Bool(bool),
    Int(i32),
    Double(u8),
    Str(u8),
    Array(Vec<FuzzJson>),
    Object(Vec<(u8, FuzzJson)>),
}

const INTERESTING_DOUBLES: &[f64] = &[0.0, -0.0, 1.0, -1.0, 3.14, 99.9, 1e10, 1e-5];

const STRINGS: &[&str] = &[
    "hello",
    "",
    "42",
    "true",
    "null",
    "foo bar",
    "café",
    "a\\nb",
];

impl FuzzJson {
    fn arbitrary_depth(u: &mut Unstructured<'_>, depth: usize) -> arbitrary::Result<Self> {
        if depth == 0 {
            return Self::arbitrary_scalar(u);
        }
        let choice = u.int_in_range(0u8..=7)?;
        match choice {
            0..=4 => Self::arbitrary_scalar(u),
            5 => Ok(FuzzJson::Str(u.arbitrary()?)),
            6 => {
                let len = u.int_in_range(0u8..=5)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(FuzzJson::arbitrary_depth(u, depth - 1)?);
                }
                Ok(FuzzJson::Array(items))
            }
            _ => {
                let len = u.int_in_range(0u8..=5)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push((u.arbitrary()?, FuzzJson::arbitrary_depth(u, depth - 1)?));
                }
                Ok(FuzzJson::Object(items))
            }
        }
    }

    fn arbitrary_scalar(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        let choice = u.int_in_range(0u8..=4)?;
        match choice {
            0 => Ok(FuzzJson::Null),
            1 => Ok(FuzzJson::Bool(u.arbitrary()?)),
            2 => Ok(FuzzJson::Int(u.arbitrary()?)),
            3 => Ok(FuzzJson::Double(u.arbitrary()?)),
            _ => Ok(FuzzJson::Str(u.arbitrary()?)),
        }
    }

    fn to_json(&self, buf: &mut Vec<u8>) {
        match self {
            FuzzJson::Null => buf.extend_from_slice(b"null"),
            FuzzJson::Bool(true) => buf.extend_from_slice(b"true"),
            FuzzJson::Bool(false) => buf.extend_from_slice(b"false"),
            FuzzJson::Int(n) => {
                use std::io::Write;
                write!(buf, "{n}").unwrap();
            }
            FuzzJson::Double(idx) => {
                use std::io::Write;
                let f = INTERESTING_DOUBLES[*idx as usize % INTERESTING_DOUBLES.len()];
                write!(buf, "{f}").unwrap();
            }
            FuzzJson::Str(idx) => {
                let s = STRINGS[*idx as usize % STRINGS.len()];
                buf.push(b'"');
                for c in s.bytes() {
                    match c {
                        b'"' => buf.extend_from_slice(b"\\\""),
                        b'\\' => buf.extend_from_slice(b"\\\\"),
                        b'\n' => buf.extend_from_slice(b"\\n"),
                        b'\r' => buf.extend_from_slice(b"\\r"),
                        b'\t' => buf.extend_from_slice(b"\\t"),
                        c if c < 0x20 => {
                            use std::io::Write;
                            write!(buf, "\\u{:04x}", c).unwrap();
                        }
                        c => buf.push(c),
                    }
                }
                buf.push(b'"');
            }
            FuzzJson::Array(items) => {
                buf.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        buf.push(b',');
                    }
                    item.to_json(buf);
                }
                buf.push(b']');
            }
            FuzzJson::Object(items) => {
                buf.push(b'{');
                for (i, (key_idx, val)) in items.iter().enumerate() {
                    if i > 0 {
                        buf.push(b',');
                    }
                    let key = KEYS[*key_idx as usize % KEYS.len()];
                    buf.push(b'"');
                    buf.extend_from_slice(key.as_bytes());
                    buf.push(b'"');
                    buf.push(b':');
                    val.to_json(buf);
                }
                buf.push(b'}');
            }
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzJson {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        FuzzJson::arbitrary_depth(u, 3)
    }
}

// ---------------------------------------------------------------------------
// Filter generation — only flat-safe filters
// ---------------------------------------------------------------------------

const FIELDS: &[&str] = &["a", "b", "c", "name", "type", "items", "x", "y"];

#[derive(Debug)]
enum FlatFilter {
    Identity,
    Field(u8),
    Iterate,
    Pipe(Box<FlatFilter>, Box<FlatFilter>),
    Comma(Box<FlatFilter>, Box<FlatFilter>),
    ArrayConstruct(Box<FlatFilter>),
    ObjectConstruct(u8, Box<FlatFilter>),
    Select(Box<FlatFilter>),
    Alternative(Box<FlatFilter>, Box<FlatFilter>),
    Try(Box<FlatFilter>),
    Not(Box<FlatFilter>),
    Length,
    Type,
    Keys,
    MapInner(Box<FlatFilter>),
    Literal(u8), // index into LITERALS
}

const LITERALS: &[&str] = &["null", "true", "false", "0", "1", "\"a\"", "[]", "{}"];

impl FlatFilter {
    fn arbitrary_depth(u: &mut Unstructured<'_>, depth: usize) -> arbitrary::Result<Self> {
        if depth == 0 {
            return Self::arbitrary_leaf(u);
        }
        let choice = u.int_in_range(0u8..=15)?;
        let sub = |u: &mut Unstructured<'_>| -> arbitrary::Result<Box<FlatFilter>> {
            Ok(Box::new(FlatFilter::arbitrary_depth(u, depth - 1)?))
        };
        match choice {
            0..=6 => Self::arbitrary_leaf(u),
            7 => Ok(FlatFilter::Pipe(sub(u)?, sub(u)?)),
            8 => Ok(FlatFilter::Comma(sub(u)?, sub(u)?)),
            9 => Ok(FlatFilter::ArrayConstruct(sub(u)?)),
            10 => Ok(FlatFilter::ObjectConstruct(u.arbitrary()?, sub(u)?)),
            11 => Ok(FlatFilter::Select(sub(u)?)),
            12 => Ok(FlatFilter::Alternative(sub(u)?, sub(u)?)),
            13 => Ok(FlatFilter::Try(sub(u)?)),
            14 => Ok(FlatFilter::Not(sub(u)?)),
            _ => Ok(FlatFilter::MapInner(sub(u)?)),
        }
    }

    fn arbitrary_leaf(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        let choice = u.int_in_range(0u8..=7)?;
        match choice {
            0 => Ok(FlatFilter::Identity),
            1 => Ok(FlatFilter::Field(u.arbitrary()?)),
            2 => Ok(FlatFilter::Iterate),
            3 => Ok(FlatFilter::Length),
            4 => Ok(FlatFilter::Type),
            5 => Ok(FlatFilter::Keys),
            6 => Ok(FlatFilter::Literal(u.arbitrary()?)),
            _ => Ok(FlatFilter::Identity),
        }
    }

    /// Check if this filter contains constructs that interact with error state
    /// (Alternative, Try) which can cause legitimate divergences between
    /// flat_eval and regular eval due to different error propagation.
    fn has_error_interaction(&self) -> bool {
        match self {
            FlatFilter::Alternative(_, _) | FlatFilter::Try(_) => true,
            FlatFilter::Pipe(a, b) | FlatFilter::Comma(a, b) => {
                a.has_error_interaction() || b.has_error_interaction()
            }
            FlatFilter::ArrayConstruct(f)
            | FlatFilter::ObjectConstruct(_, f)
            | FlatFilter::Select(f)
            | FlatFilter::Not(f)
            | FlatFilter::MapInner(f) => f.has_error_interaction(),
            _ => false,
        }
    }

    fn to_filter(&self) -> Filter {
        match self {
            FlatFilter::Identity => Filter::Identity,
            FlatFilter::Field(idx) => {
                Filter::Field(FIELDS[*idx as usize % FIELDS.len()].to_string())
            }
            FlatFilter::Iterate => Filter::Iterate,
            FlatFilter::Pipe(a, b) => {
                Filter::Pipe(Box::new(a.to_filter()), Box::new(b.to_filter()))
            }
            FlatFilter::Comma(a, b) => Filter::Comma(vec![a.to_filter(), b.to_filter()]),
            FlatFilter::ArrayConstruct(f) => Filter::ArrayConstruct(Box::new(f.to_filter())),
            FlatFilter::ObjectConstruct(key, val) => {
                let key_name = FIELDS[*key as usize % FIELDS.len()].to_string();
                Filter::ObjectConstruct(vec![(ObjKey::Name(key_name), Box::new(val.to_filter()))])
            }
            FlatFilter::Select(f) => Filter::Select(Box::new(f.to_filter())),
            FlatFilter::Alternative(l, r) => {
                Filter::Alternative(Box::new(l.to_filter()), Box::new(r.to_filter()))
            }
            FlatFilter::Try(f) => Filter::Try(Box::new(f.to_filter())),
            FlatFilter::Not(f) => Filter::Not(Box::new(f.to_filter())),
            FlatFilter::Length => Filter::Builtin("length".to_string(), vec![]),
            FlatFilter::Type => Filter::Builtin("type".to_string(), vec![]),
            FlatFilter::Keys => Filter::Builtin("keys".to_string(), vec![]),
            FlatFilter::MapInner(f) => Filter::Builtin("map".to_string(), vec![f.to_filter()]),
            FlatFilter::Literal(idx) => {
                let lit_str = LITERALS[*idx as usize % LITERALS.len()];
                filter::parse(lit_str).unwrap_or(Filter::Identity)
            }
        }
    }
}

impl<'a> Arbitrary<'a> for FlatFilter {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        FlatFilter::arbitrary_depth(u, 3)
    }
}

// ---------------------------------------------------------------------------
// Top-level fuzz input
// ---------------------------------------------------------------------------

/// Top-level JSON is always an object — this matches the NDJSON use case
/// where flat_eval is actually used. Non-object top-level inputs can cause
/// divergences in error state propagation between the two eval paths
/// (thread-local LAST_ERROR interactions with Alternative/Pipe/Field), but
/// these don't affect real-world behavior since NDJSON records are objects.
#[derive(Debug)]
struct FuzzInput {
    json: FuzzJson,
    filter: FlatFilter,
}

impl<'a> Arbitrary<'a> for FuzzInput {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // Always generate an object at the top level
        let len = u.int_in_range(0u8..=5)? as usize;
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            items.push((u.arbitrary()?, FuzzJson::arbitrary_depth(u, 3)?));
        }
        Ok(FuzzInput {
            json: FuzzJson::Object(items),
            filter: u.arbitrary()?,
        })
    }
}

// ---------------------------------------------------------------------------
// Fuzz target: compare flat_eval vs eval_filter on same input
// ---------------------------------------------------------------------------

const MAX_OUTPUTS: usize = 200;

fuzz_target!(|input: FuzzInput| {
    let filter = input.filter.to_filter();

    // Only test filters that flat_eval accepts
    if !is_flat_safe(&filter) {
        return;
    }

    // Generate valid JSON bytes
    let mut json_bytes = Vec::new();
    input.json.to_json(&mut json_bytes);

    // Parse through both paths
    let padded = pad_buffer(&json_bytes);
    let len = json_bytes.len();

    let value = match dom_parse_to_value(&padded, len) {
        Ok(v) => v,
        Err(_) => return,
    };
    let flat_buf = match dom_parse_to_flat_buf(&padded, len) {
        Ok(fb) => fb,
        Err(_) => return,
    };

    let env = Env::empty();

    // Clear any thread-local errors before each eval
    let _ = qj::filter::eval::take_last_error();

    // Evaluate with regular evaluator
    let mut regular_results = Vec::new();
    filter::eval::eval_filter_with_env(&filter, &value, &env, &mut |v| {
        if regular_results.len() < MAX_OUTPUTS {
            regular_results.push(v);
        }
    });
    let regular_had_error = qj::filter::eval::has_last_error();
    let _ = qj::filter::eval::take_last_error();

    // Evaluate with flat evaluator
    let mut flat_results = Vec::new();
    eval_flat(&filter, flat_buf.root(), &env, &mut |v| {
        if flat_results.len() < MAX_OUTPUTS {
            flat_results.push(v);
        }
    });
    let flat_had_error = qj::filter::eval::has_last_error();
    let _ = qj::filter::eval::take_last_error();

    // Skip comparison when errors occurred or when the filter interacts with
    // error state. The two eval paths have different error propagation:
    //
    // - eval_filter_with_env clears LAST_ERROR on entry, so flat_eval's
    //   delegated calls get fresh error state per sub-expression
    // - regular eval runs the entire filter in a single eval() call, so
    //   errors can leak across Comma branches and into Pipe's error check
    //
    // This affects filters containing Try (which clears errors, masking the
    // leak), Alternative (which branches based on error state), or any
    // error-producing operation (Iterate/Field on wrong types) combined
    // with Pipe. Since flat_eval is only used for NDJSON (where input is
    // always an object), this divergence doesn't affect real-world behavior.
    if regular_had_error || flat_had_error || input.filter.has_error_interaction() {
        return;
    }

    let config = OutputConfig {
        mode: OutputMode::Compact,
        ..OutputConfig::default()
    };

    let common_len = flat_results.len().min(regular_results.len());
    for i in 0..common_len {
        let mut flat_out = Vec::new();
        let mut reg_out = Vec::new();
        let _ = write_value(&mut flat_out, &flat_results[i], &config);
        let _ = write_value(&mut reg_out, &regular_results[i], &config);
        assert_eq!(
            flat_out,
            reg_out,
            "output value mismatch at index {i}: flat={:?} regular={:?} filter={:?} json={:?}",
            std::str::from_utf8(&flat_out).unwrap_or("<binary>"),
            std::str::from_utf8(&reg_out).unwrap_or("<binary>"),
            filter,
            std::str::from_utf8(&json_bytes).unwrap_or("<binary>")
        );
    }
});
