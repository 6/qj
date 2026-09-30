//! Random programs and inputs for checking natives against their definitions: each
//! program wraps native builtin calls in probes for laziness, errors, labels, value
//! identity (`path($x)` accepts only a `jv_identical` value) and array storage (writes
//! past the end of a unique view bring back stale elements).
//!
//! Plain Rust with no crate dependencies: the in-process test (`tests.rs`) and the
//! out-of-process one (`tests/native_diff.rs`, which includes this file by path) share
//! it.
//!
//! [`Gen::bounded`] programs are safe to run in process: closure arguments of nested
//! natives only use leaves whose output is no bigger than their input, so values can't
//! grow exponentially (`walk(walk([tostream]))` quickly needs gigabytes, in jq too).
//! Unbounded programs must run in a child process with memory and time limits.

/// xorshift64*: deterministic cases without a dependency.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

/// Closure leaves whose output is never bigger than their input (or is a constant).
const SAFE_LEAVES: &[&str] = &[
    ".",
    "empty",
    "error",
    "debug",
    "tostring",
    "type",
    "length?",
    "not",
    ".[]?",
    "select(type == \"number\")",
    "select(. != null)",
    "(if type == \"string\" then ascii_downcase else . end)",
    "(. as $x | $x)",
    "first(.[]?)",
    "(label $l | ., break $l)",
    "(try error catch .)",
    "null",
    "false",
    ".a?",
    ".[0]?",
    ".key?",
    "(.value |= tostring)?",
    "(.key |= ascii_upcase)?",
    "(input? // 7)",
    "stderr",
    "(.[]? |= .)",
    "(if . then 1 else empty end)",
    "limit(1; .[]?)",
    "(error(\"x\")? // 5)",
    "(if type == \"number\" then halt_error(3) else . end)",
    "(. as [$a] ?// $a | $a)",
    "$__loc__",
    "(.. |= .)",
    "(if type == \"array\" then .[0:1] else . end)",
    "(if type == \"number\" then . + 1 else . end)",
    "(if type == \"string\" then ascii_downcase end)",
    "(if \"object\" == type then del(.a) else . end)",
    "(if type == \"array\" then .[1:] else . end)",
    "(type == \"array\")",
    "(\"string\" == type)",
    "scalars",
    "numbers",
    "values",
    "nulls",
    "booleans",
    "strings",
    "arrays",
    "objects",
    "iterables",
];

/// Leaves that can make values bigger.
const GROWING_LEAVES: &[&str] = &[
    "(., .)",
    "keys?",
    "tojson",
    "[.]",
    "{a: .}",
    "(. + 1)?",
    "(1, 2)",
    "to_entries?",
    "[paths]",
    "[tostream]",
    "[.[]?]",
];

/// A program generator (see the module docs).
pub struct Gen {
    pub r: Rng,
    bounded: bool,
}

impl Gen {
    /// Programs safe to run in process.
    pub fn bounded(seed: u64) -> Gen {
        Gen {
            r: Rng(seed),
            bounded: true,
        }
    }

    /// Programs that may need unbounded memory or time (run them in a child process
    /// with limits, as `tests/native_diff.rs` does).
    #[allow(dead_code)]
    pub fn unbounded(seed: u64) -> Gen {
        Gen {
            r: Rng(seed),
            bounded: false,
        }
    }

    fn leaf(&mut self, safe: bool) -> String {
        if safe || self.r.below(3) == 0 {
            self.r.pick(SAFE_LEAVES).to_string()
        } else {
            let all = self.r.below(SAFE_LEAVES.len() + GROWING_LEAVES.len());
            if all < SAFE_LEAVES.len() {
                SAFE_LEAVES[all].to_string()
            } else {
                GROWING_LEAVES[all - SAFE_LEAVES.len()].to_string()
            }
        }
    }

    /// A closure argument. `safe`: only non-growing leaves (for nested natives).
    fn closure(&mut self, depth: u32, safe: bool) -> String {
        if depth == 0 || self.r.below(3) > 0 {
            return self.leaf(safe);
        }
        // Natives nested in a closure take only non-growing leaves when bounded.
        let inner_safe = safe || self.bounded;
        match self.r.below(10) {
            0 => format!("walk({})", self.closure(depth - 1, inner_safe)),
            1 => format!("with_entries({})?", self.closure(depth - 1, inner_safe)),
            2 => format!("[paths({})]", self.closure(depth - 1, inner_safe)),
            3 => format!(
                "({} | {})",
                self.closure(depth - 1, safe),
                self.closure(depth - 1, safe)
            ),
            4 => format!(
                "({}, {})",
                self.closure(depth - 1, safe),
                self.closure(depth - 1, safe)
            ),
            5 => format!("[{}]", self.closure(depth - 1, safe)),
            6 => format!("({} // null)", self.closure(depth - 1, safe)),
            7 => format!("try {} catch .", self.closure(depth - 1, safe)),
            8 => format!("select({})", self.closure(depth - 1, safe)),
            _ => format!("first({})", self.closure(depth - 1, safe)),
        }
    }

    /// A path expression (some aren't: those raise jq's path errors).
    fn path_expr(&mut self) -> String {
        const PATHS: &[&str] = &[
            ".a",
            ".[]?",
            "..",
            "(.. | numbers)",
            "(.. | strings)",
            ".a.b",
            ".[0]",
            ".[1:]",
            "(.a, .b)",
            "first(.[]?)",
            "empty",
            "(.[]? | select(. != null))",
            ".key?",
            ".[-1]?",
            "getpath([\"a\", 0])?",
            "paths",
            "(.a | tostring)",
            ".[\"value\"]?",
            "(.. | select(type == \"object\"))",
            "(if type == \"array\" then .[0] else .a end)",
        ];
        self.r.pick(PATHS).to_string()
    }

    /// A native call.
    fn native(&mut self, depth: u32) -> String {
        let f = self.closure(if self.bounded { 1 } else { 2 }, false);
        let chain = depth > 0;
        match self.r.below(if chain { 12 } else { 9 }) {
            // `..`, the type filters, add, flatten, first, limit, isempty, any/all, IN.
            // select, map, repeat (natives for direct closures).
            _ if self.r.below(4) == 0 => match self.r.below(5) {
                0 => format!("[.[]? | select({f})]"),
                1 => format!("select({f})"),
                2 => format!("map({f})?"),
                3 => format!("[.[]? | map({f})?]"),
                // `repeat` of something that can be empty never ends (in jq too).
                _ => format!(
                    "[limit(3; repeat({}))]",
                    self.r.pick(&[
                        "1",
                        ".",
                        "tostring",
                        "(. + 1)",
                        "error",
                        "[.]",
                        "(.a? // 1)",
                        "input",
                        "(if . then 1 else 2 end)",
                    ])
                ),
            },
            _ if self.r.below(3) == 0 => match self.r.below(20) {
                0 => "[..]".to_string(),
                1 => format!(
                    "[.. | {}]",
                    self.r.pick(&[
                        "numbers",
                        "strings",
                        "scalars",
                        "values",
                        "nulls",
                        "booleans",
                        "arrays",
                        "objects",
                        "iterables"
                    ])
                ),
                2 => "add".to_string(),
                3 => format!("add({f})"),
                4 => "flatten".to_string(),
                5 => format!(
                    "flatten({})",
                    self.r.pick(&["0", "1", "2", "-1", "1.5", "null", "\"a\""])
                ),
                6 => format!("first({f})"),
                7 => format!(
                    "[limit({}; {f})]",
                    self.r.pick(&["0", "1", "2", "-1", "1.5"])
                ),
                8 => format!("limit(1; {f})"),
                9 => format!("isempty({f})"),
                10 => "any".to_string(),
                11 => "all".to_string(),
                12 => format!("any({f})"),
                13 => format!("all({f})"),
                14 => format!("any(.[]?; {f})"),
                15 => format!("all({f}; {f})"),
                16 => format!("IN({f})"),
                17 => format!("IN(.[]?; {f})"),
                18 => ".. | numbers".to_string(),
                _ => format!("(.. | {f})"),
            },
            // The update-assignments (natives `_modify` and `_assign`) and `join`.
            _ if self.r.below(3) == 0 => {
                let p = self.path_expr();
                let g = self.closure(0, self.bounded);
                match self.r.below(8) {
                    0 => format!("({p}) |= {f}"),
                    1 => format!("({p}) = {g}"),
                    2 => format!("({p}) += 1"),
                    3 => format!("({p}) //= {g}"),
                    4 => format!("map_values({f})"),
                    5 => format!(
                        "[.[]? | tostring] | join({})",
                        self.r
                            .pick(&["\",\"", "\"\"", "null", "1", "$__loc__.file", ".[0]?"])
                    ),
                    6 => "join(\"-\")".to_string(),
                    _ => format!("({p}) |= ({p} |= {g})?"),
                }
            }
            0 => format!("walk({f})"),
            1 => "paths".to_string(),
            2 => format!("paths({f})"),
            3 => "tostream".to_string(),
            4 => "to_entries".to_string(),
            5 => "from_entries".to_string(),
            6 => format!("with_entries({f})"),
            7 => "ascii_downcase".to_string(),
            8 => "ascii_upcase".to_string(),
            9 => format!("(.[]? | {})", self.native(depth - 1)),
            10 => {
                let g = self.closure(0, self.bounded);
                format!("({g} | {})", self.native(depth - 1))
            }
            _ => {
                let g = self.closure(0, self.bounded);
                format!("with_entries({g}) | walk({f})")
            }
        }
    }

    /// A probe around a native call `n`.
    fn probe(&mut self, n: &str) -> String {
        // Reference lifetimes: `$$$$v` moves a variable out, and `reduce` moves its
        // state into the update, so a slice of fresh storage is uniquely owned there
        // unless something (like a suspended fork point of the native's definition)
        // still holds it; a write past its end then brings back stale elements.
        const VIEW: &str = "[., ., ., .][0:2]";
        match self.r.below(31) {
            22 => return format!("{VIEW} as $v | $v | {n} | $$$$v | .[3] = 9"),
            23 => return format!("{VIEW} | reduce ({n}) as $j (.; .[3] = 9)"),
            24 => {
                return format!("{VIEW} | {n} | if type == \"array\" then .[3] = 9 else . end");
            }
            25 => return format!("{VIEW} as $v | $v | [{n}] | $$$$v | .[3] = 9"),
            26 => return format!("{VIEW} as $v | $v | last({n}) | $$$$v | .[3] = 9"),
            27 => return format!("{VIEW} as $v | $v | first({n}) | $$$$v | .[3] = 9"),
            28 => {
                return format!(
                    "[{VIEW}, .] | {n} | if type == \"array\" then .[length + 1] = 0 else . end"
                );
            }
            29 => return format!("[{VIEW}, .] | reduce ({n}) as $j (.; .[0][3] = 9)"),
            30 => {
                return format!(
                    "{{a: {VIEW}, b: .}} | {n} | if type == \"array\" then .[length + 1] = 0 else . end"
                );
            }
            _ => {}
        }
        match self.r.below(22) {
            0 => format!("[{n}]"),
            1 => n.to_string(),
            2 => format!("first({n})"),
            3 => format!("[limit(2; {n})]"),
            4 => format!("try {n} catch ."),
            5 => format!("[{n}] | length"),
            6 => format!("{n} | debug"),
            7 => format!("label $f | {n} | ., break $f"),
            8 => format!("[{n}] | .[0] as $a | .[-1] | try path($a) catch \"E\""),
            9 => format!("last({n}) | if type == \"array\" then .[length + 2] = 0 else . end"),
            10 => format!("{n} | [label $q | try break $q catch .]"),
            11 => format!(
                "[{n} | if type == \"array\" then .[0:1] else . end] | map(if type == \"array\" then .[3] = 1 else . end)"
            ),
            12 => format!("reduce ({n}) as $x (null; [$x, .])"),
            13 => format!("isempty({n})"),
            14 => format!("[{n}] | unique | length"),
            15 => format!("({n}) as $x | [$x, $x == $x]"),
            16 => format!("[{n}, {n}]"),
            17 => format!("try ({n} | error) catch ."),
            18 => format!("first({n} | select(. != null))"),
            19 => format!("path({n})?"),
            20 => format!("[{n} | tojson]"),
            _ => format!("({n}) as $a | ({n}) | try path($a) catch \"E\""),
        }
    }

    /// A JSON input: at most 3 levels, 3 elements per container.
    fn value(&mut self, depth: u32, out: &mut String) {
        const KEYS: &[&str] = &[
            "a", "b", "key", "value", "name", "Name", "Key", "Value", "c", "A", "\u{e9}",
        ];
        const SCALARS: &[&str] = &[
            "null",
            "true",
            "false",
            "0",
            "1",
            "-1",
            "1.5",
            "1.000",
            "1e2",
            "\"\"",
            "\"a\"",
            "\"AbC\"",
            "\"x\u{e9}Y\"",
            "\"key\"",
            "\"value\"",
            "100000000000000000001",
        ];
        let k = if depth == 0 { 0 } else { self.r.below(5) };
        match k {
            0 | 1 => out.push_str(self.r.pick(SCALARS)),
            2 | 3 => {
                out.push('[');
                for i in 0..self.r.below(4) {
                    if i > 0 {
                        out.push(',');
                    }
                    self.value(depth - 1, out);
                }
                out.push(']');
            }
            _ => {
                out.push('{');
                for i in 0..self.r.below(4) {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    out.push_str(self.r.pick(KEYS));
                    out.push_str("\":");
                    self.value(depth - 1, out);
                }
                out.push('}');
            }
        }
    }

    /// The next case: a program and its input (one or two JSON texts).
    pub fn case(&mut self) -> (String, String) {
        let native = self.native(1);
        let program = self.probe(&native);
        let mut input = String::new();
        for i in 0..1 + self.r.below(2) {
            if i > 0 {
                input.push(' ');
            }
            self.value(3, &mut input);
        }
        (program, input)
    }
}
