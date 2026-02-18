#![no_main]
use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use qj::filter::{self, ArithOp, BoolOp, CmpOp, Filter, ObjKey, Pattern, StringPart};
use qj::output::{write_value, OutputConfig, OutputMode};
use qj::value::Value;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Lookup tables
// ---------------------------------------------------------------------------

const INTERESTING_DOUBLES: &[f64] = &[
    0.0,
    -0.0,
    0.5,
    1.5,
    3.14,
    -1.0,
    99.9,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    f64::MIN,
    f64::MAX,
    f64::EPSILON,
    1e308,
    5e-324,
];

const STRINGS: &[&str] = &["", "a", "hello", "null", "true", "42", "foo bar", "\n\t"];

const KEYS: &[&str] = &["a", "b", "c", "d"];

const FIELDS: &[&str] = &["a", "b", "c", "d"];

const NULLARY_BUILTINS: &[&str] = &[
    "length",
    "keys",
    "keys_unsorted",
    "values",
    "type",
    "not",
    "empty",
    "reverse",
    "sort",
    "flatten",
    "unique",
    "first",
    "last",
    "min",
    "max",
    "add",
    "to_entries",
    "from_entries",
    "ascii_downcase",
    "ascii_upcase",
    "tostring",
    "tonumber",
    "tojson",
    "fromjson",
    "explode",
    "implode",
    "floor",
    "ceil",
    "round",
    "abs",
    "utf8bytelength",
    "isnan",
    "isinfinite",
    "isnormal",
    "paths",
    "leaf_paths",
    "any",
    "all",
    "transpose",
    "env",
    "path(.[0])",
    "getpath([\"a\"])",
    "indices(\"a\")",
    "inside([1,2,3])",
    "ltrimstr(\"a\")",
    "rtrimstr(\"a\")",
    "startswith(\"a\")",
    "endswith(\"a\")",
    "contains(\"a\")",
    "split(\",\")",
    "join(\",\")",
    "map(. + 1)",
    "map(type)",
    "select(. > 0)",
    "select(. != null)",
    "sort_by(.)",
    "group_by(.)",
    "unique_by(.)",
    "flatten(1)",
    "limit(2; .[])",
    "range(5)",
    "range(1; 5)",
    "has(\"a\")",
    "@json",
    "@text",
    "@html",
    "@uri",
    "@csv",
    "@tsv",
    "@sh",
    "@base64",
    "@base64d",
];

// ---------------------------------------------------------------------------
// Fuzz value — maps to qj::value::Value
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum FuzzValue {
    Null,
    Bool(bool),
    SmallInt(i8),
    BigInt(i64),
    Double(u8),
    Str(u8),
    Array(Vec<FuzzValue>),
    Object(Vec<(u8, FuzzValue)>),
}

impl FuzzValue {
    fn arbitrary_depth(u: &mut Unstructured<'_>, depth: usize) -> arbitrary::Result<Self> {
        if depth == 0 {
            let choice = u.int_in_range(0u8..=5)?;
            return match choice {
                0 => Ok(FuzzValue::Null),
                1 => Ok(FuzzValue::Bool(u.arbitrary()?)),
                2 => Ok(FuzzValue::SmallInt(u.arbitrary()?)),
                3 => Ok(FuzzValue::BigInt(u.arbitrary()?)),
                4 => Ok(FuzzValue::Double(u.arbitrary()?)),
                _ => Ok(FuzzValue::Str(u.arbitrary()?)),
            };
        }
        let choice = u.int_in_range(0u8..=7)?;
        match choice {
            0 => Ok(FuzzValue::Null),
            1 => Ok(FuzzValue::Bool(u.arbitrary()?)),
            2 => Ok(FuzzValue::SmallInt(u.arbitrary()?)),
            3 => Ok(FuzzValue::BigInt(u.arbitrary()?)),
            4 => Ok(FuzzValue::Double(u.arbitrary()?)),
            5 => Ok(FuzzValue::Str(u.arbitrary()?)),
            6 => {
                let len = u.int_in_range(0u8..=4)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(FuzzValue::arbitrary_depth(u, depth - 1)?);
                }
                Ok(FuzzValue::Array(items))
            }
            _ => {
                let len = u.int_in_range(0u8..=4)? as usize;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push((u.arbitrary()?, FuzzValue::arbitrary_depth(u, depth - 1)?));
                }
                Ok(FuzzValue::Object(items))
            }
        }
    }

    fn to_value(&self) -> Value {
        match self {
            FuzzValue::Null => Value::Null,
            FuzzValue::Bool(b) => Value::Bool(*b),
            FuzzValue::SmallInt(n) => Value::Int(*n as i64),
            FuzzValue::BigInt(n) => Value::Int(*n),
            FuzzValue::Double(idx) => {
                let f = INTERESTING_DOUBLES[*idx as usize % INTERESTING_DOUBLES.len()];
                Value::Double(f, None)
            }
            FuzzValue::Str(idx) => {
                Value::String(STRINGS[*idx as usize % STRINGS.len()].to_string())
            }
            FuzzValue::Array(items) => {
                Value::Array(Arc::new(items.iter().map(|v| v.to_value()).collect()))
            }
            FuzzValue::Object(items) => Value::Object(Arc::new(
                items
                    .iter()
                    .map(|(k, v)| {
                        let key = KEYS[*k as usize % KEYS.len()].to_string();
                        (key, v.to_value())
                    })
                    .collect(),
            )),
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzValue {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        FuzzValue::arbitrary_depth(u, 3)
    }
}

// ---------------------------------------------------------------------------
// Fuzz filter — maps to qj::filter::Filter
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum FuzzFilter {
    // Leaf variants
    Identity,
    Field(u8),
    Iterate,
    Literal(FuzzValue),
    Builtin(u8),
    Var,
    // Recursive variants
    Pipe(Box<FuzzFilter>, Box<FuzzFilter>),
    Comma(Box<FuzzFilter>, Box<FuzzFilter>),
    Arith(Box<FuzzFilter>, u8, Box<FuzzFilter>),
    Compare(Box<FuzzFilter>, u8, Box<FuzzFilter>),
    BoolOp(Box<FuzzFilter>, u8, Box<FuzzFilter>),
    Not(Box<FuzzFilter>),
    Neg(Box<FuzzFilter>),
    Try(Box<FuzzFilter>),
    Alternative(Box<FuzzFilter>, Box<FuzzFilter>),
    IfThenElse(Box<FuzzFilter>, Box<FuzzFilter>, Box<FuzzFilter>),
    ArrayConstruct(Box<FuzzFilter>),
    ObjectConstruct(u8, Box<FuzzFilter>),
    Select(Box<FuzzFilter>),
    Bind(Box<FuzzFilter>, Box<FuzzFilter>),
    Reduce(Box<FuzzFilter>, Box<FuzzFilter>, Box<FuzzFilter>),
    StringInterp(Box<FuzzFilter>),
    Index(Box<FuzzFilter>),
}

const LEAF_COUNT: u8 = 6;
const RECURSIVE_COUNT: u8 = 17;

impl FuzzFilter {
    fn arbitrary_depth(u: &mut Unstructured<'_>, depth: usize) -> arbitrary::Result<Self> {
        if depth == 0 {
            return Self::arbitrary_leaf(u);
        }

        let choice = u.int_in_range(0u8..=(LEAF_COUNT + RECURSIVE_COUNT - 1))?;

        if choice < LEAF_COUNT {
            return Self::arbitrary_leaf_by_index(u, choice);
        }

        let sub = |u: &mut Unstructured<'_>| -> arbitrary::Result<Box<FuzzFilter>> {
            Ok(Box::new(FuzzFilter::arbitrary_depth(u, depth - 1)?))
        };

        match choice - LEAF_COUNT {
            0 => Ok(FuzzFilter::Pipe(sub(u)?, sub(u)?)),
            1 => Ok(FuzzFilter::Comma(sub(u)?, sub(u)?)),
            2 => Ok(FuzzFilter::Arith(sub(u)?, u.arbitrary()?, sub(u)?)),
            3 => Ok(FuzzFilter::Compare(sub(u)?, u.arbitrary()?, sub(u)?)),
            4 => Ok(FuzzFilter::BoolOp(sub(u)?, u.arbitrary()?, sub(u)?)),
            5 => Ok(FuzzFilter::Not(sub(u)?)),
            6 => Ok(FuzzFilter::Neg(sub(u)?)),
            7 => Ok(FuzzFilter::Try(sub(u)?)),
            8 => Ok(FuzzFilter::Alternative(sub(u)?, sub(u)?)),
            9 => Ok(FuzzFilter::IfThenElse(sub(u)?, sub(u)?, sub(u)?)),
            10 => Ok(FuzzFilter::ArrayConstruct(sub(u)?)),
            11 => Ok(FuzzFilter::ObjectConstruct(u.arbitrary()?, sub(u)?)),
            12 => Ok(FuzzFilter::Select(sub(u)?)),
            13 => Ok(FuzzFilter::Bind(sub(u)?, sub(u)?)),
            14 => Ok(FuzzFilter::Reduce(sub(u)?, sub(u)?, sub(u)?)),
            15 => Ok(FuzzFilter::StringInterp(sub(u)?)),
            _ => Ok(FuzzFilter::Index(sub(u)?)),
        }
    }

    fn arbitrary_leaf(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        let choice = u.int_in_range(0u8..=(LEAF_COUNT - 1))?;
        Self::arbitrary_leaf_by_index(u, choice)
    }

    fn arbitrary_leaf_by_index(u: &mut Unstructured<'_>, idx: u8) -> arbitrary::Result<Self> {
        match idx {
            0 => Ok(FuzzFilter::Identity),
            1 => Ok(FuzzFilter::Field(u.arbitrary()?)),
            2 => Ok(FuzzFilter::Iterate),
            3 => Ok(FuzzFilter::Literal(u.arbitrary()?)),
            4 => Ok(FuzzFilter::Builtin(u.arbitrary()?)),
            _ => Ok(FuzzFilter::Var),
        }
    }

    fn to_filter(&self) -> Filter {
        match self {
            FuzzFilter::Identity => Filter::Identity,
            FuzzFilter::Field(idx) => {
                Filter::Field(FIELDS[*idx as usize % FIELDS.len()].to_string())
            }
            FuzzFilter::Iterate => Filter::Iterate,
            FuzzFilter::Literal(v) => Filter::Literal(v.to_value()),
            FuzzFilter::Builtin(idx) => {
                let name = NULLARY_BUILTINS[*idx as usize % NULLARY_BUILTINS.len()];
                // Parse the builtin expression to get a proper Filter AST.
                // This handles both simple names and complex expressions like
                // "map(. + 1)" or "select(. > 0)".
                filter::parse(name).unwrap_or(Filter::Identity)
            }
            FuzzFilter::Var => Filter::Var("x".to_string()),
            FuzzFilter::Pipe(a, b) => {
                Filter::Pipe(Box::new(a.to_filter()), Box::new(b.to_filter()))
            }
            FuzzFilter::Comma(a, b) => Filter::Comma(vec![a.to_filter(), b.to_filter()]),
            FuzzFilter::Arith(a, op, b) => {
                let ops = [
                    ArithOp::Add,
                    ArithOp::Sub,
                    ArithOp::Mul,
                    ArithOp::Div,
                    ArithOp::Mod,
                ];
                Filter::Arith(
                    Box::new(a.to_filter()),
                    ops[*op as usize % ops.len()],
                    Box::new(b.to_filter()),
                )
            }
            FuzzFilter::Compare(a, op, b) => {
                let ops = [CmpOp::Eq, CmpOp::Ne, CmpOp::Lt, CmpOp::Le, CmpOp::Gt, CmpOp::Ge];
                Filter::Compare(
                    Box::new(a.to_filter()),
                    ops[*op as usize % ops.len()],
                    Box::new(b.to_filter()),
                )
            }
            FuzzFilter::BoolOp(a, op, b) => {
                let ops = [BoolOp::And, BoolOp::Or];
                Filter::BoolOp(
                    Box::new(a.to_filter()),
                    ops[*op as usize % ops.len()],
                    Box::new(b.to_filter()),
                )
            }
            FuzzFilter::Not(f) => Filter::Not(Box::new(f.to_filter())),
            FuzzFilter::Neg(f) => Filter::Neg(Box::new(f.to_filter())),
            FuzzFilter::Try(f) => Filter::Try(Box::new(f.to_filter())),
            FuzzFilter::Alternative(a, b) => {
                Filter::Alternative(Box::new(a.to_filter()), Box::new(b.to_filter()))
            }
            FuzzFilter::IfThenElse(cond, t, e) => Filter::IfThenElse(
                Box::new(cond.to_filter()),
                Box::new(t.to_filter()),
                Some(Box::new(e.to_filter())),
            ),
            FuzzFilter::ArrayConstruct(f) => Filter::ArrayConstruct(Box::new(f.to_filter())),
            FuzzFilter::ObjectConstruct(key, val) => {
                let key_name = KEYS[*key as usize % KEYS.len()].to_string();
                Filter::ObjectConstruct(vec![(ObjKey::Name(key_name), Box::new(val.to_filter()))])
            }
            FuzzFilter::Select(f) => Filter::Select(Box::new(f.to_filter())),
            FuzzFilter::Bind(bind, body) => Filter::Bind(
                Box::new(bind.to_filter()),
                Pattern::Var("x".to_string()),
                Box::new(body.to_filter()),
            ),
            FuzzFilter::Reduce(_source, init, update) => Filter::Reduce(
                Box::new(Filter::Iterate), // reduce .[] as $x (init; update)
                Pattern::Var("x".to_string()),
                Box::new(init.to_filter()),
                Box::new(update.to_filter()),
            ),
            FuzzFilter::StringInterp(f) => Filter::StringInterp(vec![
                StringPart::Lit("val=".to_string()),
                StringPart::Expr(f.to_filter()),
            ]),
            FuzzFilter::Index(f) => Filter::Index(Box::new(f.to_filter())),
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzFilter {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        FuzzFilter::arbitrary_depth(u, 3)
    }
}

// ---------------------------------------------------------------------------
// Top-level fuzz input
// ---------------------------------------------------------------------------

#[derive(Debug, Arbitrary)]
struct FuzzInput {
    value: FuzzValue,
    filter: FuzzFilter,
}

// ---------------------------------------------------------------------------
// Fuzz target: construct Value + Filter directly, evaluate, format output.
// Every iteration reaches the evaluator — no parsing overhead or rejections.
// ---------------------------------------------------------------------------

/// Hard limit on eval outputs to prevent combinatorial explosion
/// (e.g., deeply nested Bind(Iterate, Iterate) on arrays/objects).
const MAX_OUTPUTS: usize = 500;

fuzz_target!(|input: FuzzInput| {
    let value = input.value.to_value();
    let filter = input.filter.to_filter();

    let config = OutputConfig {
        mode: OutputMode::Compact,
        ..OutputConfig::default()
    };

    // Evaluate, collecting up to MAX_OUTPUTS then stopping.
    // Format each output value to exercise the eval → output pipeline.
    let mut count = 0;
    filter::eval::eval_filter(&filter, &value, &mut |v: Value| {
        count += 1;
        if count > MAX_OUTPUTS {
            return;
        }
        let mut out = Vec::new();
        let _ = write_value(&mut out, &v, &config);
    });
});
