//! Random programs and inputs for checking the optimized code (regions and frameless
//! calls, `region.rs`) against jq's instructions as they are: every construct that
//! compiles to a region (arithmetic, comparisons, `if`/`and`/`or`, object and string
//! construction, indexing, variables, `reduce`/`foreach` bodies, `select` conditions,
//! closures of natives) nested in and around everything that doesn't (generators, fork
//! points, `try`, labels, path expressions, `?//`, function definitions), and probes for
//! what they must keep: errors and their order, the order of side effects (`debug`,
//! `stderr`, `input`), labels, value identity (`path($x)` accepts only a
//! `jv_identical` value) and array storage (writes past the end of a unique view bring
//! back stale elements).
//!
//! Plain Rust with no crate dependencies: the in-process test (`opt_tests.rs`) and the
//! out-of-process one (`tests/vm_opt_diff.rs`, which includes this file by path) share
//! it. [`Gen::bounded`] programs are safe to run in process (small numbers, shallow
//! nesting); unbounded ones must run in a child process with memory and time limits.

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

    fn chance(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }
}

/// Constants and simple terms.
const LEAVES: &[&str] = &[
    ".",
    ".",
    ".",
    "0",
    "1",
    "2",
    "-1",
    "2.5",
    "1.000",
    "3",
    "\"a\"",
    "\"abc\"",
    "\"\"",
    "\"b,a\"",
    "null",
    "true",
    "false",
    "[]",
    "{}",
    "[1,2]",
    "[[1],\"a\"]",
    "{\"a\":1,\"b\":[2]}",
    "{\"a\":{\"b\":null}}",
    ".a",
    ".b",
    ".[0]",
    ".[1]",
    ".[-1]",
    ".a?",
    ".[0]?",
    ".a.b?",
    ".[\"a\"]?",
    "$__loc__",
    "length?",
    "type",
    "keys?",
    "tostring",
    "tojson",
    "not",
    "empty",
    "error",
    "(.a // 7)",
    "(input? // \"none\")",
    "nan",
    "infinite",
    "-.?",
    ".[1:]?",
    ".[:1]?",
    "(.[0:1]? // null)",
];

/// Leaves that can make big values (only unbounded).
const BIG_LEAVES: &[&str] = &[
    "100000000000000000001",
    "1e1000",
    "-1e1000",
    "9007199254740993",
    "5e-324",
];

/// Builtins applied to a value (`E | f`).
const FILTERS: &[&str] = &[
    "tostring",
    "tojson",
    "type",
    "length?",
    "keys?",
    "not",
    "debug",
    "stderr",
    "to_entries?",
    "from_entries?",
    "add?",
    "tostream",
    "[paths]",
    "[..]",
    "flatten?",
    "sort?",
    "unique?",
    "min?",
    "max?",
    "reverse?",
    "tonumber?",
    "ascii_downcase?",
    "ascii_upcase?",
    "explode?",
    "implode?",
    "@text",
    "@json",
    "@base64",
    "@html?",
    "@csv?",
    "has(\"a\")?",
    "has(0)?",
    "contains(\"a\")?",
    "ltrimstr(\"a\")?",
    "split(\",\")?",
    "join(\",\")?",
    "test(\"a\")?",
    "getpath([\"a\"])?",
    "getpath([\"a\", 0])?",
    "setpath([\"a\"]; 1)?",
    "delpaths([[\"a\"]])?",
    "del(.a)?",
    "del(.[0])?",
    "to_entries? | from_entries?",
    "with_entries(.value |= tostring)?",
    "walk(if type == \"number\" then . + 1 else . end)",
    "walk(.)",
    "map_values(. // 0)?",
    "abs?",
    "floor?",
    "sqrt?",
    "isnan?",
    "isinfinite?",
    "utf8bytelength?",
    "splits(\"a\")?",
    "halt_error?",
    "input?",
    "$__loc__",
    "env | type",
    "input_line_number",
    "[limit(2; .[]?)]",
    "first(.[]?)",
    "isempty(.[]?)",
    "any",
    "all",
    "[.[]?]",
    "[recurse] | length",
    "indices(1)?",
    "index(\"a\")?",
    "inside([1,2,3])?",
    "combinations? | tostring",
    "transpose?",
    "tojson | fromjson",
    "@base64 | @base64d",
    "getpath([]) | type",
];

const BINOPS: &[&str] = &[
    "+", "+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">=", "and", "or", "//",
];

const UPDATES: &[&str] = &["|=", "+=", "-=", "*=", "//=", "="];

const PATHS: &[&str] = &[
    ".a",
    ".[0]",
    ".a.b",
    ".[]?",
    "..",
    "(.a, .b)",
    "(if type == \"array\" then .[0] else .a end)",
    "getpath([\"a\"])",
    "(.a | select(. != null))",
    "first(.[]?)",
    "(.. | numbers)",
    "empty",
    "(.a // .b)",
    "(.[1:])",
    ".[-1]?",
];

/// A program generator (see the module docs).
pub struct Gen {
    pub r: Rng,
    bounded: bool,
    /// Variables in scope.
    vars: Vec<String>,
    /// Zero-argument functions in scope.
    funcs: Vec<String>,
    /// One-argument functions in scope.
    funcs1: Vec<String>,
    fresh: u32,
}

impl Gen {
    /// Programs safe to run in process.
    pub fn bounded(seed: u64) -> Gen {
        Gen {
            r: Rng(seed),
            bounded: true,
            vars: Vec::new(),
            funcs: Vec::new(),
            funcs1: Vec::new(),
            fresh: 0,
        }
    }

    /// Programs that may need unbounded memory or time (run them in a child process
    /// with limits, as `tests/vm_opt_diff.rs` does).
    #[allow(dead_code)]
    pub fn unbounded(seed: u64) -> Gen {
        Gen {
            bounded: false,
            ..Gen::bounded(seed)
        }
    }

    fn name(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    fn leaf(&mut self) -> String {
        if !self.vars.is_empty() && self.r.chance(3) {
            let i = self.r.below(self.vars.len());
            return self.vars[i].clone();
        }
        if !self.funcs.is_empty() && self.r.chance(6) {
            let i = self.r.below(self.funcs.len());
            return self.funcs[i].clone();
        }
        if !self.bounded && self.r.chance(12) {
            return self.r.pick(BIG_LEAVES).to_string();
        }
        self.r.pick(LEAVES).to_string()
    }

    /// `f` with `v` bound as a variable while it is generated.
    fn with_var<T>(&mut self, v: &str, f: impl FnOnce(&mut Gen) -> T) -> T {
        self.vars.push(v.to_string());
        let r = f(self);
        self.vars.pop();
        r
    }

    /// An expression (`d` levels deep at most).
    pub fn expr(&mut self, d: u32) -> String {
        if d == 0 || self.r.chance(5) {
            return self.leaf();
        }
        let d = d - 1;
        match self.r.below(46) {
            0..=7 => {
                let op = self.r.pick(BINOPS);
                format!("({} {op} {})", self.expr(d), self.expr(d))
            }
            8 => format!("(-{})", self.expr(d)),
            9 => format!("({} | not)", self.expr(d)),
            10 | 11 => format!(
                "(if {} then {} else {} end)",
                self.expr(d),
                self.expr(d),
                self.expr(d)
            ),
            12 => format!("(if {} then {} end)", self.expr(d), self.expr(d)),
            13 => format!(
                "(if {} then {} elif {} then {} else {} end)",
                self.expr(d),
                self.expr(d),
                self.expr(d),
                self.expr(d),
                self.expr(d)
            ),
            14 => match self.r.below(4) {
                0 => format!("{{a: {}}}", self.expr(d)),
                1 => format!("{{({}): {}}}", self.expr(d), self.expr(d)),
                2 => format!("{{a: {}, b: {}}}", self.expr(d), self.expr(d)),
                _ => format!("{{a, \"b\": {}}}", self.expr(d)),
            },
            15 => match self.r.below(3) {
                0 => format!("[{}]", self.expr(d)),
                1 => format!("[{}, {}]", self.expr(d), self.expr(d)),
                _ => "[]".to_string(),
            },
            16 => format!("\"x\\({})y\\({})\"", self.expr(d), self.expr(d)),
            17 => match self.r.below(4) {
                0 => format!(".[{}]", self.expr(d)),
                1 => format!("({} | .[{}])", self.expr(d), self.expr(d)),
                2 => format!("{}?", self.expr(d)),
                _ => format!(".[{}]?", self.expr(d)),
            },
            18 | 19 => {
                let v = self.name("$v");
                let src = self.expr(d);
                let body = self.with_var(&v, |g| g.expr(d));
                format!("({src} as {v} | {body})")
            }
            20 | 21 => {
                let v = self.name("$x");
                let src = self.generator(d);
                let init = self.expr(d);
                let body = self.with_var(&v, |g| g.expr(d));
                format!("reduce {src} as {v} ({init}; {body})")
            }
            22 => {
                let v = self.name("$x");
                let src = self.generator(d);
                let init = self.expr(d);
                let (upd, ext) = self.with_var(&v, |g| (g.expr(d), g.expr(d)));
                if self.r.chance(2) {
                    format!("[foreach {src} as {v} ({init}; {upd}; {ext})]")
                } else {
                    format!("[foreach {src} as {v} ({init}; {upd})]")
                }
            }
            23 | 24 => format!("[{} | select({})]", self.generator(d), self.expr(d)),
            25 => format!("select({})", self.expr(d)),
            26 => format!("(map({}))?", self.expr(d)),
            27 => {
                let op = self.r.pick(UPDATES);
                let p = self.r.pick(PATHS);
                format!("(({p}) {op} {})?", self.expr(d))
            }
            28 => format!("(.[]? |= {})", self.expr(d)),
            29 => match self.r.below(5) {
                0 => format!("first({})", self.generator(d)),
                1 => format!("[limit(2; {})]", self.generator(d)),
                2 => format!("isempty({})", self.generator(d)),
                3 => format!("any({}; {})", self.generator(d), self.expr(d)),
                _ => format!("all({}; {})", self.generator(d), self.expr(d)),
            },
            30 => format!("(try {} catch {})", self.expr(d), self.expr(d)),
            31 => {
                let l = self.name("$l");
                format!("[label {l} | {} | ., break {l}]", self.generator(d))
            }
            32 => {
                let p = self.r.pick(PATHS);
                format!("(try path({p} | {}) catch \"E\")", self.expr(d))
            }
            33 => format!("(try path({}) catch \"E\")", self.expr(d)),
            34 | 35 => {
                let f = self.name("f");
                let body = self.expr(d);
                self.funcs.push(f.clone());
                let rest = self.expr(d);
                self.funcs.pop();
                format!("(def {f}: {body}; {rest})")
            }
            36 => {
                let f = self.name("g");
                let body = self.expr(d);
                let arg = self.expr(d);
                self.funcs1.push(f.clone());
                let rest = format!("{f}({arg})");
                self.funcs1.pop();
                format!("(def {f}(h): h | {body}; {rest})")
            }
            37 => {
                let f = self.name("k");
                let v = self.name("$p");
                let body = self.with_var(&v, |g| g.expr(d));
                let arg = self.expr(d);
                format!("(def {f}({v}): {body}; {f}({arg}))")
            }
            38 => format!("({} | {})", self.expr(d), self.r.pick(FILTERS)),
            39 => format!("({} | {})", self.expr(d), self.expr(d)),
            40 => format!("({}, {})", self.expr(d), self.expr(d)),
            41 => {
                let (a, b) = (self.name("$a"), self.name("$b"));
                let body = self.with_var(&a, |g| g.with_var(&b, |g| g.expr(d)));
                match self.r.below(3) {
                    0 => format!("(. as [{a}, {b}] | {body})?"),
                    1 => format!("(. as {{a: {a}, b: {b}}} | {body})?"),
                    _ => format!("(.[]? as [{a}] ?// {a} | {b} | {body})"),
                }
            }
            42 => format!("error({})", self.expr(d)),
            43 => format!("({} // {})", self.expr(d), self.expr(d)),
            44 => format!("[{}]", self.generator(d)),
            _ => format!("({} | {} | {})", self.expr(d), self.expr(d), self.expr(d)),
        }
    }

    /// An expression that is likely to have several outputs.
    fn generator(&mut self, d: u32) -> String {
        match self.r.below(12) {
            0 => ".[]?".to_string(),
            1 => "range(3)".to_string(),
            2 => "range(1; 4)".to_string(),
            3 => self
                .r
                .pick(&["range(0; 10; 3)", "range(5; 0; -2)", "range(0; 1; 0.25)"])
                .to_string(),
            4 => format!("({}, {})", self.expr(d), self.expr(d)),
            5 => format!("(.[]? | {})", self.expr(d)),
            6 => format!("(range(3) | {})", self.expr(d)),
            7 => "(.. | numbers)".to_string(),
            // (`repeat` of something that can be empty never ends, in jq too.)
            8 => format!(
                "limit(3; repeat({}))",
                self.r.pick(&["1", ".", "(. + 1)", "[.]", "(.a // 2)"])
            ),
            9 => "limit(4; recurse(.[]?))".to_string(),
            10 => format!("(to_entries?[] | {})", self.expr(d)),
            _ => self.expr(d),
        }
    }

    /// A probe around the program `n` (see the module docs).
    fn probe(&mut self, n: &str) -> String {
        const VIEW: &str = "[., ., ., .][0:2]";
        match self.r.below(24) {
            0 => format!("[{n}]"),
            1 => format!("first({n})"),
            2 => format!("[limit(2; {n})]"),
            3 => format!("try ({n}) catch ."),
            4 => format!("[{n}] | length"),
            5 => format!("label $f | {n} | ., break $f"),
            6 => format!("({n}) | [label $q | try break $q catch .]"),
            7 => format!("reduce ({n}) as $x (null; [$x, .])"),
            8 => format!("[{n}] | .[0] as $a | .[-1] | try path($a) catch \"E\""),
            9 => format!("{VIEW} as $v | $v | ({n}) | $$$$v | .[3] = 9"),
            10 => format!("{VIEW} | reduce ({n}) as $j (.; .[3] = 9)"),
            11 => format!("({n}) as $a | ({n}) | try path($a) catch \"E\""),
            12 => format!(
                "[({n}) | if type == \"array\" then .[0:1] else . end] | map(if type == \"array\" then .[3] = 1 else . end)"
            ),
            13 => format!("try path({n}) catch \"E\""),
            14 => format!("[({n}) | debug]"),
            15 => format!("[{n}, (1 | {n})]"),
            16 => format!("[paths({n})]?"),
            17 => format!("[.[]? | {n}]"),
            18 => format!("{VIEW} | ({n}) | if type == \"array\" then .[3] = 9 else . end"),
            19 => format!("[({n}) | tojson]"),
            20 => format!("[{n}] | .[0:1] | .[3] = 1"),
            _ => n.to_string(),
        }
    }

    /// A JSON input: at most 3 levels, 3 elements per container.
    fn value(&mut self, depth: u32, out: &mut String) {
        const KEYS: &[&str] = &["a", "b", "c", "key", "value"];
        const SCALARS: &[&str] = &[
            "null", "true", "false", "0", "1", "-1", "1.5", "1.000", "2", "\"\"", "\"a\"",
            "\"b,a\"", "\"AbC\"", "10",
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
        self.vars.clear();
        self.funcs.clear();
        self.funcs1.clear();
        let depth = if self.bounded {
            3
        } else {
            4 + self.r.below(2) as u32
        };
        let e = self.expr(depth);
        let program = self.probe(&e);
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
