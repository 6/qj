"""Random jq programs, JSON inputs and command lines for differential fuzzing.

Used by fuzz.py (the runner). Everything is driven by a `random.Random`, so a
case is reproducible from its seed. Programs are built as small ASTs (`N`) so
the minimizer can replace subexpressions; the printer adds parentheses only
where jq's grammar needs them.

The generator aims at runtime semantics, not syntax: most programs compile and
run, and they are weighted toward the places where bugs hide (paths and
assignment, errors and try/catch, generators and early exits, number literals,
strings and regex, formats, dates, and the command line).
"""

import json
import random

# ---------------------------------------------------------------------------
# AST and printer
# ---------------------------------------------------------------------------

# Precedence levels, tightest first. A child in a slot that needs level L is
# parenthesized when its own level is looser than L.
A = 0  # atom: can take postfix (.a, [0], ?)
T = 1  # term: -x, try, number/format literals (no postfix without parens)
E = 2  # binary operators
C = 3  # comma
P = 4  # pipe
Q = 5  # anything: def, as-binding, label (extend to the right)


class N:
    """An AST node: `parts` is a list of literal strings and (need, N) slots.

    `kind` is "x" for expressions the minimizer may replace, "p" for patterns
    and other structure it must leave alone.
    """

    __slots__ = ("lvl", "parts", "kind")

    def __init__(self, lvl, parts, kind="x"):
        self.lvl = lvl
        self.parts = parts
        self.kind = kind

    def slots(self):
        return [p for p in self.parts if not isinstance(p, str)]


def render(node, need=Q):
    out = []
    for p in node.parts:
        if isinstance(p, str):
            out.append(p)
        else:
            s = render(p[1], p[0])
            # Keep tokens apart: `- -1`, `. .a` style joins.
            if out and s and out[-1] and out[-1][-1] == "-" and s[0] == "-":
                s = " " + s
            out.append(s)
    s = "".join(out)
    if node.lvl > need:
        return "(" + s + ")"
    return s


def text_level(text):
    """The loosest operator at the top level of a program fragment: leaves
    written as text (`"now | type"`) must be parenthesized like the nodes
    they stand for."""
    depth = 0
    lvl = A
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        if c == '"':
            i += 1
            while i < n and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
        elif c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif depth == 0:
            rest = text[i:]
            if c == "|" and not rest.startswith("|="):
                lvl = max(lvl, P)
            elif c == ",":
                lvl = max(lvl, C)
            elif c == ";":
                lvl = Q  # def f: ...; rest
            elif rest.startswith((" as ", "def ", "label ")):
                lvl = Q
            elif rest[:1] in "+-*/%<>=!" or rest.startswith((" and ", " or ", "//")):
                if not (c == "-" and i == 0):
                    lvl = max(lvl, E)
        i += 1
    return lvl


def lf(text, lvl=A):
    return N(max(lvl, text_level(text)), [text])


DOT = None  # set below


def pipe(a, b):
    return N(P, [(C, a), " | ", (Q, b)])


def comma(a, b):
    return N(C, [(C, a), ", ", (E, b)])


def binop(a, op, b):
    return N(E, [(T, a), " " + op + " ", (T, b)])


def arr(q):
    return N(A, ["[", (Q, q), "]"])


def call(name, *args):
    if not args:
        return lf(name)
    parts = [name + "("]
    for i, a in enumerate(args):
        if i:
            parts.append("; ")
        parts.append((Q, a))
    parts.append(")")
    return N(A, parts)


def suffix(t, text_parts):
    """Postfix: `t` followed by `.a`, `[q]`, `?` ... (text_parts may hold slots)."""
    # `.` followed by `.a` must print as `.a`, not `..a`.
    if isinstance(t, N) and t.parts == ["."] and text_parts and isinstance(text_parts[0], str):
        first = text_parts[0]
        if first.startswith(".") and first != ".":
            return N(A, list(text_parts))
        if first.startswith("["):
            return N(A, ["."] + list(text_parts))
    return N(A, [(A, t)] + list(text_parts))


def neg(t):
    return N(T, ["-", (A, t)])


def jstr(s):
    """A jq string literal for the Python string s."""
    return json.dumps(s, ensure_ascii=False)


# ---------------------------------------------------------------------------
# Value pools
# ---------------------------------------------------------------------------

KEYS = ["a", "b", "c", "a", "b", "", "é", "a b", "1", "A", "aa", "__loc__", "if", "x"]

# Number literals as they appear in JSON input and in programs.
NUM_LITS = [
    "0", "-0", "1", "-1", "2", "3", "5", "10", "-3", "0.5", "1.5", "-2.5", "1.0", "1.000",
    "100", "1e2", "1E2", "1e+2", "1e-2", "0.1", "0.2", "0.30000000000000004", "3.14159",
    "1e17", "1e300", "1e308", "1.7976931348623157e308", "1e309", "1e1000", "-1e1000",
    "5e-324", "1e-400", "9007199254740992", "9007199254740993", "100000000000000000001",
    "123456789012345678901234567890", "0.00001", "1.5e-7", "2.0e0", "0e0", "-0.0", "0.0",
    "1e0", "12345678901234567", "4.0", "1.25e3", "-1.5e-3", "1e-7", "1000000",
    "99999999999999999999", "0.1e1", "10.50", "-100000000000000000001", "1.23456789012345678",
    "2", "7", "42", "-7", "1e-5", "3.0", "255", "256", "65536", "4294967296", "-2147483649",
]

# Program-only number literal spellings.
PROG_NUM_LITS = NUM_LITS + [".5", "1.", "00", "007", "1e1000", "-0"]

# Strings as JSON text (bytes may be invalid UTF-8 on purpose).
JSON_STRS = [
    b'""', b'"a"', b'"b"', b'"abc"', b'"A"', b'"a,b"', b'"a b c"', b'"1"', b'"1.5"', b'"-1"',
    b'"1e3"', b'"nan"', b'"\xc3\xa9"', b'"\\u00e9"', b'"\xf0\x9f\x98\x80"', b'"\\ud83d\\ude00"',
    b'"\\u0000"', b'"\\u001f"', b'"\x7f"', b'"\\u007f"',
    b'"\xe2\x80\xa8"', b'"\\u2028"', b'"\\n"', b'"\\t"', b'"\\""', b'"\\\\"', b'"\\/"',
    b'"<&>\'"', b'"2015-03-05T23:51:47Z"', b'"10:15"', b'"  pad  "', b'"aaa"', b'"abcabc"',
    b'"a\\nb"', b'"true"', b'"null"', b'"[1,2]"', b'"{\\"a\\":1}"', b'"ab12cd"', b'"x_y-z"',
    b'"\xc3\x80\xc3\x89\xc3\x8e"', b'"\xc3\x9f"', b'"\xc4\xb0"', b'"\\ufeff"', b'"\\uffff"',
    b'"\\udbff\\udfff"', b'"\xff"', b'"a\xc3"', b'"\xe2\x82"', b'"\xed\xa0\x80"', b'"\xc0\xaf"',
    b'"\xf4\x90\x80\x80"', b'"\xf8\x88\x80\x80\x80"', b'"a\\u0000b"', b'"12"', b'" 1"',
    b'"0x10"', b'"1.0"', b'"-0"', b'"1e1000"', b'"Infinity"', b'"a.b"', b'"a\\\\b"',
    b'"\xe4\xb8\xad\xe6\x96\x87"', b'"1,2,3"', b'"foo bar"', b'"FOO"', b'"a1b2"',
    b'"aGVsbG8="', b'"aGk"', b'"!!"', b'"=="', b'"MFRGG==="', b'"%41%zz"',
    b'"1425599621"', b'"2015-03-05"', b'"\\u00001"',
]

KEY_JSON = [jstr(k).encode() for k in KEYS] + [b'"\\u0061"', b'"\xff"']
RARE_STRS = [b'"\\ud800"', b'"\\udc00x"', b'"\\ud800\\u0041"', b'"\\udfff\\ud800"']


def json_text(v):
    """Python value -> JSON text (for --argjson etc.)."""
    return json.dumps(v, ensure_ascii=False)


# ---------------------------------------------------------------------------
# JSON inputs
# ---------------------------------------------------------------------------

# A JSON value tree: ("n", bytes) number, ("s", bytes) string, ("k", bytes)
# keyword, ("a", [values]) array, ("o", [(keybytes, value)]) object,
# ("raw", bytes) arbitrary bytes.


def gen_value(r, depth):
    k = r.random()
    if depth <= 0 or k < 0.35:
        return gen_scalar(r)
    if k < 0.65:
        return ("a", [gen_value(r, depth - 1) for _ in range(r.choice([0, 1, 2, 2, 3, 3, 4, 5]))])
    n = r.choice([0, 1, 1, 2, 2, 3, 3, 4])
    pairs = []
    for _ in range(n):
        key = r.choice(KEY_JSON) if r.random() < 0.9 else r.choice(JSON_STRS)
        pairs.append((key, gen_value(r, depth - 1)))
    if pairs and r.random() < 0.08:
        pairs.append((pairs[0][0], gen_value(r, depth - 1)))  # duplicate key
    return ("o", pairs)


def gen_number_text(r):
    k = r.random()
    if k < 0.55:
        return r.choice(NUM_LITS)
    if k < 0.75:
        return str(r.randint(-20, 100))
    if k < 0.85:
        return repr(round(r.uniform(-1000, 1000), r.randint(0, 6)))
    if k < 0.92:
        mant = r.choice(["1", "2.5", "9.99", "-1", "7", "1.0"])
        return mant + r.choice(["e", "E"]) + r.choice(["", "+", "-"]) + str(r.randint(0, 400))
    return str(r.randint(-(10 ** 25), 10 ** 25))


def gen_scalar(r):
    k = r.random()
    if k < 0.4:
        return ("n", gen_number_text(r).encode())
    if k < 0.8:
        return ("s", r.choice(JSON_STRS) if r.random() > 0.01 else r.choice(RARE_STRS))
    return ("k", r.choice([b"null", b"true", b"false", b"null", b"true"]))


def ser(v, style, r, indent=0):
    t = v[0]
    if t in ("n", "s", "k", "raw"):
        return v[1]
    sp = style == "spaced"
    nl = style == "pretty"
    if t == "a":
        if not v[1]:
            return b"[]" if style != "weird" else b"[ ]"
        items = [ser(x, style, r, indent + 1) for x in v[1]]
        if nl:
            pad = b"\n" + b"  " * (indent + 1)
            return b"[" + pad + (b"," + pad).join(items) + b"\n" + b"  " * indent + b"]"
        if style == "weird":
            ws = lambda: r.choice([b"", b" ", b"\t", b"\n", b"\r\n", b"  "])
            return b"[" + ws() + (ws() + b"," + ws()).join(items) + ws() + b"]"
        return b"[" + (b", " if sp else b",").join(items) + b"]"
    if t == "o":
        if not v[1]:
            return b"{}"
        items = []
        for key, val in v[1]:
            vs = ser(val, style, r, indent + 1)
            if style == "weird":
                ws = lambda: r.choice([b"", b" ", b"\t", b"\n"])
                items.append(ws() + key + ws() + b":" + ws() + vs + ws())
            else:
                items.append(key + (b": " if (sp or nl) else b":") + vs)
        if nl:
            pad = b"\n" + b"  " * (indent + 1)
            return b"{" + pad + (b"," + pad).join(items) + b"\n" + b"  " * indent + b"}"
        return b"{" + (b", " if sp else b",").join(items) + b"}"
    raise ValueError(t)


MALFORMED_TAILS = [
    b"{", b"[", b"[1,", b'{"a":', b'{"a"', b'{"a" 1}', b"[1,]", b"[1 2]", b"tru", b"nul",
    b"1.2.3", b"01", b"-", b"+1", b"1e", b"1e+", b'"\\x"', b'"\\u12"', b'"abc', b"]", b"}",
    b"foo", b"nan", b"NaN", b"-nan", b"infinity", b"'a'", b'{"a":1,}', b"[,1]", b"{,}",
    b'{"a":1 "b":2}', b":", b",", b"1 2 ]", b"\x00", b"\xef\xbb\xbf1", b'"\t"', b'"a\nb"',
    b"[1,[2,[3", b"true false", b"{1:2}", b'{"a":1}}', b"[]]", b"\x1e1", b"1\x1e",
    b"0x10", b".5", b"5.", b"-.5", b"1e1000", b"--1", b"[-]", b'"\\ud800\\u0041"',
    b"\"\xff\"", b"/* c */ 1", b"# c\n1", b"[\"a\" : 1]", b"{\"a\":nan}", b"[nan]",
    b"[1e1000]", b"[-0]", b"123456789012345678901234567890e-10",
]


def gen_input_bytes(r):
    """Stdin/file content: one or more JSON texts, sometimes malformed."""
    style = r.choice(["compact", "compact", "spaced", "pretty", "weird"])
    k = r.random()
    if k < 0.55:
        docs = [gen_value(r, r.randint(0, 4))]
    elif k < 0.85:
        docs = [gen_value(r, r.randint(0, 3)) for _ in range(r.randint(2, 5))]
    else:
        docs = [gen_value(r, r.randint(0, 2)) for _ in range(r.randint(0, 3))]
        docs.insert(r.randint(0, len(docs)), ("raw", r.choice(MALFORMED_TAILS)))
    return {"docs": docs, "style": style, "sep": r.choice([b"\n", b"\n", b" ", b"", b"\r\n", b"\t", b"\n\n"]),
            "trail": r.choice([b"\n", b"\n", b"", b" ", b"\n\n"]), "seed": r.randrange(1 << 30)}


def input_to_bytes(inp):
    r = random.Random(inp["seed"])
    parts = []
    for d in inp["docs"]:
        b = ser(d, inp["style"], r)
        parts.append(b)
    out = inp["sep"].join(parts)
    if parts:
        out += inp["trail"]
    return out


def raw_text_input(r):
    """Content for -R: lines of text, some JSON-looking."""
    lines = []
    for _ in range(r.randint(0, 5)):
        lines.append(r.choice([
            b"a", b"", b"abc", b"1", b"1.0", b'"q"', b"a,b,c", b"x y", b"\xc3\xa9", b"\xff",
            b"{\"a\":1}", b"  sp  ", b"\t", b"a\rb", b"null", b"2015-03-05T23:51:47Z",
        ]))
    sep = r.choice([b"\n", b"\n", b"\r\n"])
    return sep.join(lines) + r.choice([b"\n", b"", b"\n"])


# ---------------------------------------------------------------------------
# Programs
# ---------------------------------------------------------------------------

REGEXES = [
    "a", "b", "", "a+", "a*", "x*", ".", "^", "$", "^a", "c$", "[a-c]", "[^a]", "\\d+",
    "\\d", "\\s+", "\\w+", "(a)(b)?", "(?<x>a)", "(?<x>a)|(?<y>b)", "(?<n>\\d+)", "a|b",
    "(a|b)+", "(?i)A", "A", "é", "\\b", "a{2}", "a{1,}", "(?=a)", "(?!a)", "(a)\\1",
    "[[:alpha:]]+", "\\p{L}", "[é-ü]", ",", ", *", "\\.", "(?<a>.)(?<b>.)", "(.)",
    "(", "[", "*", "a{", "\\", "(?<n>", "(?P<x>a)", "\\u00e9", "😀", "\\n", "(?x) a b",
    "(?<x>)", "[a-", "a??", "(a*)*", "(?m).", "^$", "\\Z", "\\A", "\\K",
]
FLAGS = ["g", "i", "x", "n", "s", "l", "p", "gi", "ix", "gn", "", "gx", "q", "gg", "ng", "gs"]
SHORT_STRS = ["", "a", "b", ",", "a,b", "é", " ", "ab", "x", "abc", "\n", "\u0000", "1", "aa",
              "😀", "\\", "\"", "A", "-", "a b", "é,ü"]
FMTS = ["%Y-%m-%dT%H:%M:%SZ", "%Y-%m-%d", "%H:%M:%S", "%A, %B %d, %Y", "%s", "%j", "%e", "%c",
        "%Z", "%z", "%H:%M", "%%", "%G-W%V-%u", "", "%", "%a %b %e %H:%M:%S %Z %Y", "%D",
        "%T", "%y", "%I %p", "%U %W", "%C", "%n%t", "%k", "%Ex", "%Oy", "%Q", "x%Yx",
        "%d/%m/%Y", "%Y", "%m", "%S", "%u %w"]
DATE_STRS = ["2015-03-05T23:51:47Z", "1970-01-01T00:00:00Z", "2015-03-05", "10:15:00",
             "Thursday, March 05, 2015", "2015-03-05T23:51:47.123Z", "1425599621", "", "x",
             "2038-01-19T03:14:08Z", "1900-02-28T00:00:00Z", "2000-02-29T12:00:00Z",
             "9999-12-31T23:59:59Z", "0000-01-01T00:00:00Z"]
TYPES = ["null", "boolean", "number", "string", "array", "object"]
FORMATS = ["@text", "@json", "@html", "@uri", "@csv", "@tsv", "@sh", "@base64", "@base64d",
           "@base32", "@base32d", "@uri", "@urid"]

MATH0 = ["floor", "sqrt", "ceil", "round", "fabs", "trunc", "log", "log2", "log10", "exp",
         "exp2", "exp10", "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh",
         "asinh", "acosh", "atanh", "cbrt", "significand", "logb", "gamma", "lgamma", "tgamma",
         "expm1", "log1p", "rint", "nearbyint", "frexp", "modf", "j0", "j1", "y0", "y1",
         "erf", "erfc", "abs", "toboolean"]
MATH2 = ["pow", "atan2", "fmin", "fmax", "fmod", "ldexp", "scalb", "scalbln", "nextafter",
         "nexttoward", "copysign", "drem", "fdim", "hypot", "remainder", "jn", "yn"]
MATH3 = ["fma"]
FILTERS0 = ["arrays", "objects", "iterables", "booleans", "numbers", "normals", "finites",
            "strings", "nulls", "values", "scalars"]
PLAIN0 = ["length", "utf8bytelength", "not", "keys", "keys_unsorted", "to_entries",
          "from_entries", "add", "any", "all", "flatten", "floor", "sqrt", "min", "max",
          "unique", "reverse", "sort", "tostring", "tojson", "fromjson", "tonumber", "type",
          "infinite", "nan", "isinfinite", "isnan", "isnormal", "explode", "implode",
          "ascii_downcase", "ascii_upcase", "ltrim", "rtrim", "trim", "recurse", "env",
          "transpose", "first", "last", "combinations", "tostream", "paths", "todate",
          "fromdate", "todateiso8601", "fromdateiso8601", "mktime", "gmtime", "localtime",
          "halt", "halt_error", "input", "inputs", "debug", "stderr", "input_filename",
          "input_line_number", "$__loc__", "have_decnum", "have_literal_numbers", "abs",
          "error", "empty", "get_prog_origin", "values", "toboolean", "significand",
          "gamma", "frexp", "modf", "logb", "trunc", "keys", "length", "tojson", "add",
          "to_entries", "tostring", "type", "min", "max", "unique", "sort", "not"]
# Not defined in jq 1.8.1 (or not with arity 0): compile errors.
BOGUS0 = ["leaf_paths", "isvalid", "dateiso8601", "splits", "ascii", "toarray", "getpath",
          "ltrimstr", "to_number", "join", "map", "select", "del", "path", "has", "in",
          "date", "dateadd", "recurse_down", "finites(1)", "error(1;2)", "range"]


class Gen:
    def __init__(self, r, vars_=(), named_args=False):
        self.r = r
        self.vars = list(vars_)       # bound $names (without $)
        self.funcs = []               # (name, params) user defs; params like ["f", "$x"]
        self.closures = []            # closure params in scope (arity 0)
        self.labels = []
        self.named_args = named_args

    # -- helpers ----------------------------------------------------------

    def pick(self, options):
        total = sum(w for w, _ in options)
        x = self.r.random() * total
        for w, f in options:
            x -= w
            if x < 0:
                return f()
        return options[-1][1]()

    def ch(self, xs):
        return self.r.choice(xs)

    def chance(self, p):
        return self.r.random() < p

    # -- leaves -----------------------------------------------------------

    def num(self):
        k = self.r.random()
        if k < 0.6:
            s = self.ch(["0", "1", "2", "3", "-1", "0.5", "1.5", "10", "-2", "5", "100", "2.5"])
        elif k < 0.9:
            s = self.ch(PROG_NUM_LITS)
        else:
            s = self.ch(["nan", "infinite", "-infinite", "null", "\"1\"", "[]", "true"])
        if s.startswith("-"):
            return neg(lf(s[1:], T)) if s[1:] not in ("infinite",) else neg(lf(s[1:]))
        return lf(s, A if s in ("nan", "infinite", "null", "true", "[]") or s.startswith('"') else T)

    def small_int(self):
        s = self.ch(["0", "1", "2", "3", "1", "2", "5", "-1", "10", "0.5", "1.5", "-2"])
        if s.startswith("-"):
            return neg(lf(s[1:], T))
        return lf(s, T)

    def strlit(self):
        k = self.r.random()
        if k < 0.7:
            return lf(jstr(self.ch(SHORT_STRS)))
        if k < 0.85:
            return lf(jstr(self.ch(KEYS)))
        return lf(self.ch(['"\\u00e9"', '"\\ud83d\\ude00"', '"a\\tb"', '"\\u0000"', '"\\/"',
                           '"\\u2028"', '"\\"q\\""', '"\\\\"', '"\\u007f"']))

    def keylit(self):
        return lf(jstr(self.ch(KEYS)))

    def literal(self):
        k = self.r.random()
        if k < 0.45:
            return self.num()
        if k < 0.75:
            return self.strlit()
        if k < 0.9:
            return lf(self.ch(["null", "true", "false"]))
        return lf(self.ch(["[]", "{}", "[1,2,3]", '{"a":1}', '[null]', '[{"a":1},{"a":2}]',
                           '["a","b"]', "[[1,2],[3]]", '{"a":{"b":2}}', "[3,1,2]"]))

    def var(self):
        if self.vars and self.chance(0.9):
            return lf("$" + self.ch(self.vars))
        if self.chance(0.9):
            return lf(self.ch(["$__loc__", "$ENV", "$ARGS", "$ENV.PAGER", "$ARGS.named",
                               "$ARGS.positional", "$ENV.TZ"]))
        return lf(self.ch(["$undefined_var", "$__prog_args"]))

    def leaf(self):
        return self.pick([
            (6, lambda: lf(".")),
            (5, lambda: self.field()),
            (4, self.literal),
            (1.5, self.var),
            (1, lambda: lf("..", T)),
            (1, lambda: lf("empty")),
            (0.5, lambda: lf("$__loc__")),
            (1, lambda: lf(self.ch(["length", "keys", "type", "tostring", "tojson", "add",
                                    "not", "-.", "..|numbers", "first", "sort"]), A)),
            (0.8, lambda: self.closure_ref()),
        ])

    def closure_ref(self):
        if self.closures:
            return lf(self.ch(self.closures))
        return lf(".")

    def field(self):
        k = self.r.random()
        if k < 0.35:
            s = "." + self.ch(["a", "b", "c", "a", "x", "if", "A", "aa"])
        elif k < 0.5:
            s = "." + self.ch(['"a"', '"a b"', '"é"', '""', '"1"'])
        elif k < 0.7:
            s = ".[" + self.ch(["0", "1", "-1", "2", "-2", "1.5", "0.5", "-0", "nan", "1e300",
                                "-1e300", "null", "\"a\"", "3"]) + "]"
        elif k < 0.8:
            s = ".[]"
        elif k < 0.9:
            lo = self.ch(["", "0", "1", "-1", "2", "1.5", "null", "-2", "10", "-10", "0.5"])
            hi = self.ch(["", "2", "-1", "3", "1.7", "null", "100", "0", "-10", "1e300"])
            if lo == "" and hi == "":
                lo = "1"
            s = ".[" + lo + ":" + hi + "]"
        else:
            s = self.ch([".a.b", ".a[0]", ".[0].a", ".a[]", ".[][]?", ".a.b.c", ".[-1:]",
                         ".a[1:]", '.["a"].b', ".[1:][0]", ".a.[0]"])
        if self.chance(0.2):
            s += "?"
        return lf(s)

    # -- structure ----------------------------------------------------------

    def q(self, d):
        """Query: pipes, commas, bindings, defs, labels."""
        if d <= 0:
            return self.e(0)
        return self.pick([
            (9, lambda: self.e(d)),
            (6, lambda: pipe(self.e(d - 1), self.q(d - 1))),
            (3, lambda: comma(self.q(d - 1), self.e(d - 1))),
            (2.5, lambda: self.as_binding(d)),
            (2, lambda: self.funcdef(d)),
            (1.2, lambda: self.label(d)),
        ])

    def e(self, d):
        if d <= 0:
            return self.t(0)
        return self.pick([
            (12, lambda: self.t(d)),
            (3, lambda: binop(self.t(d - 1), self.ch(["+", "-", "*", "/", "%"]), self.t(d - 1))),
            (2, lambda: binop(self.t(d - 1), self.ch(["==", "!=", "<", "<=", ">", ">="]),
                              self.t(d - 1))),
            (1, lambda: binop(self.t(d - 1), self.ch(["and", "or"]), self.t(d - 1))),
            (2, lambda: binop(self.t(d - 1), "//", self.t(d - 1))),
            (4, lambda: self.assign(d)),
        ])

    def t(self, d):
        if d <= 0:
            return self.leaf()
        return self.pick([
            (5, self.leaf),
            (4, lambda: self.postfix(d)),
            (3, lambda: arr(self.q(d - 1))),
            (3, lambda: self.obj(d)),
            (2.5, lambda: self.string(d)),
            (18, lambda: self.builtin(d)),
            (3, lambda: self.idiom(d)),
            (2, lambda: self.format(d)),
            (1, lambda: neg(self.t(d - 1))),
            (3, lambda: self.if_(d)),
            (4, lambda: self.try_(d)),
            (2.5, lambda: self.reduce(d)),
            (2.5, lambda: self.foreach(d)),
            (1, self.break_),
            (1.5, lambda: self.user_call(d)),
            (1, lambda: suffix(self.t(d - 1), ["?"])),
        ])

    def postfix(self, d):
        base = self.t(d - 1)
        k = self.r.random()
        if k < 0.3:
            sfx = ["." + self.ch(["a", "b", "c"])]
        elif k < 0.5:
            sfx = ["[", (Q, self.q(d - 1)), "]"]
        elif k < 0.65:
            sfx = ["[]"]
        elif k < 0.8:
            sfx = ["[", (Q, self.small_int()), ":", (Q, self.small_int()), "]"]
        elif k < 0.9:
            sfx = ["[" + self.ch(["0", "-1", "1", "\"a\""]) + "]"]
        else:
            sfx = ["." + self.ch(['"a"', '"b"']), ]
        if self.chance(0.25):
            sfx.append("?")
        return suffix(base, sfx)

    def obj(self, d):
        pairs = []
        for _ in range(self.ch([0, 1, 1, 2, 2, 3])):
            k = self.r.random()
            if k < 0.35:
                pairs += [self.ch(["a", "b", "c", "if", "and", "x"]) + ": ", (T, self.e(d - 1))]
            elif k < 0.5:
                pairs += [jstr(self.ch(KEYS)) + ": ", (T, self.e(d - 1))]
            elif k < 0.6:
                pairs += [self.ch(["a", "b", "c"])]
            elif k < 0.7 and self.vars:
                pairs += ["$" + self.ch(self.vars)]
            elif k < 0.75:
                pairs += ["$__loc__"]
            elif k < 0.88:
                pairs += ["(", (Q, self.q(d - 1)), "): ", (T, self.e(d - 1))]
            elif k < 0.95:
                pairs += ['"\\(', (Q, self.e(d - 1)), ')": ', (T, self.e(d - 1))]
            else:
                pairs += [self.ch(["@base64", "@text"]) + ' "k": ', (T, self.e(d - 1))]
            pairs.append(", ")
        if pairs:
            pairs.pop()
        return N(A, ["{"] + pairs + ["}"])

    def string(self, d):
        parts = ['"']
        for _ in range(self.r.randint(0, 3)):
            if self.chance(0.5):
                parts.append(self.ch(["x", "a b", "\\t", "\\\"", "é", "-", "\\u00e9", "\\n", ":"]))
            else:
                parts += ["\\(", (Q, self.q(d - 1)), ")"]
        parts.append('"')
        if self.chance(0.25):
            parts.insert(0, self.ch(FORMATS) + " ")
            return N(T, parts)
        return N(A, parts)

    def format(self, d):
        f = lf(self.ch(FORMATS), T)
        if self.chance(0.5):
            return f
        return pipe(self.t(d - 1), f)

    def if_(self, d):
        parts = ["if ", (Q, self.cond(d - 1)), " then ", (Q, self.q(d - 1))]
        for _ in range(self.ch([0, 0, 1])):
            parts += [" elif ", (Q, self.cond(d - 1)), " then ", (Q, self.q(d - 1))]
        if self.chance(0.75):
            parts += [" else ", (Q, self.q(d - 1))]
        parts.append(" end")
        return N(A, parts)

    def try_(self, d):
        body = self.pick([
            (3, lambda: self.t(d - 1)),
            (2, lambda: self.errorish(d - 1)),
            (1, lambda: self.gen(d - 1)),
        ])
        parts = ["try ", (A, body)]
        if self.chance(0.7):
            handler = self.pick([
                (4, lambda: lf(".")),
                (2, lambda: self.t(d - 1)),
                (1, lambda: call("error")),
                (1, lambda: call("error", self.t(d - 1))),
                (1, lambda: lf("empty")),
                (0.5, self.break_),
                (1, lambda: lf(self.ch(["length", "type", "tostring", "ascii_upcase", "[.]",
                                        "{e: .}", ".a", "-.", "tojson"]))),
            ])
            parts += [" catch ", (A, handler)]
        return N(T, parts)

    def errorish(self, d):
        return self.pick([
            (2, lambda: call("error", self.t(d))),
            (1, lambda: call("error")),
            (1, lambda: comma(comma(self.small_int(), call("error", self.strlit())), self.small_int())),
            (1, lambda: lf(self.ch(['.a.b.c', '.[0]', 'tonumber', '.[] | .a', 'keys',
                                    '1 / 0', '{} | .[0]', '"x" - 1', 'implode', 'fromjson',
                                    'error(null)', 'error({})', 'error([1])', 'null | error',
                                    '"\\(1,2)" | tonumber', '[] | first(.[])', 'ltrimstr(1)',
                                    'splits(1)', 'test("(")', 'error("\\u0000")', '.[] as [$a] | $a',
                                    '{a:1} | has(0)', '[1] | has("a")', '"abc" | .[0]',
                                    'input', 'error(error)', 'nan | tostring | tonumber']), E)),
            (1, lambda: self.t(d)),
        ])

    def pattern(self, d, names):
        k = self.r.random()
        if d <= 0 or k < 0.45:
            n = self.ch(["a", "b", "c", "x", "y"])
            names.append(n)
            return N(A, ["$" + n], "p")
        if k < 0.7:
            parts = ["["]
            for i in range(self.ch([1, 1, 2, 2, 3])):
                if i:
                    parts.append(", ")
                parts.append((A, self.pattern(d - 1, names)))
            parts.append("]")
            return N(A, parts, "p")
        parts = ["{"]
        for i in range(self.ch([1, 1, 2, 3])):
            if i:
                parts.append(", ")
            kind = self.r.random()
            if kind < 0.3:
                n = self.ch(["a", "b", "c"])
                names.append(n)
                parts.append("$" + n)
            elif kind < 0.55:
                parts += [self.ch(["a", "b", "c", "x"]) + ": ", (A, self.pattern(d - 1, names))]
            elif kind < 0.7:
                parts += [jstr(self.ch(KEYS)) + ": ", (A, self.pattern(d - 1, names))]
            elif kind < 0.85:
                n = self.ch(["a", "b"])
                names.append(n)
                parts += ["$" + n + ": ", (A, self.pattern(d - 1, names))]
            else:
                parts += ["(", (Q, self.e(0)), "): ", (A, self.pattern(d - 1, names))]
        parts.append("}")
        return N(A, parts, "p")

    def patterns(self, names):
        pats = [self.pattern(2, names)]
        if self.chance(0.2):
            for _ in range(self.ch([1, 1, 2])):
                pats.append(self.pattern(2, names))
        parts = []
        for i, p in enumerate(pats):
            if i:
                parts.append(" ?// ")
            parts.append((A, p))
        return N(A, parts, "p")

    def with_vars(self, names, f):
        saved = list(self.vars)
        self.vars += [n for n in names if n not in self.vars]
        try:
            return f()
        finally:
            self.vars = saved

    def as_binding(self, d):
        names = []
        src = self.pick([(3, lambda: self.e(d - 1)), (2, lambda: self.gen(d - 1))])
        pats = self.patterns(names)
        body = self.with_vars(names, lambda: self.q(d - 1))
        return N(Q, [(E, src), " as ", (A, pats), " | ", (Q, body)])

    def reduce(self, d):
        names = []
        src = self.gen(d - 1)
        pats = self.patterns(names) if self.chance(0.4) else self.simple_pat(names)
        init = self.pick([(3, lambda: self.literal()), (2, lambda: self.q(d - 1)),
                          (2, lambda: lf(self.ch(["0", "[]", "{}", "null", ".", '""'])))])
        upd = self.with_vars(names, lambda: self.pick([
            (3, lambda: self.q(d - 1)),
            (3, lambda: binop(lf("."), self.ch(["+", "*", "-"]), lf("$" + names[0]))),
            (2, lambda: N(E, [".[", (Q, self.key_of(names)), "] = ", (T, lf("$" + names[-1]))])),
            (1, lambda: N(E, [".[", (Q, self.key_of(names)), "] += ", (T, self.t(d - 1))])),
            (1, lambda: binop(lf("."), "+", arr(lf("$" + names[0])))),
        ]))
        return N(A, ["reduce ", (E, src), " as ", (A, pats), " (", (Q, init), "; ", (Q, upd), ")"])

    def key_of(self, names):
        return self.ch([lf("$" + names[0]), lf('"k"'), lf("$" + names[0] + "|tostring"),
                        lf("$" + names[0] + "|tojson")])

    def simple_pat(self, names):
        n = self.ch(["x", "item", "v"])
        names.append(n)
        return N(A, ["$" + n], "p")

    def foreach(self, d):
        names = []
        src = self.gen(d - 1)
        pats = self.patterns(names) if self.chance(0.3) else self.simple_pat(names)
        init = self.pick([(3, lambda: self.literal()), (2, lambda: self.q(d - 1)),
                          (2, lambda: lf(self.ch(["0", "[]", "null", ".", "{}"])))])
        upd = self.with_vars(names, lambda: self.pick([
            (3, lambda: self.q(d - 1)),
            (3, lambda: binop(lf("."), "+", lf("$" + names[0]))),
            (1, lambda: lf("empty")),
            (1, lambda: comma(lf("."), lf("$" + names[0]))),
            (1, lambda: N(A, ["if $" + names[0] + " == ", (T, self.literal()), " then error(\"stop\") else . end"])),
        ]))
        parts = ["foreach ", (E, src), " as ", (A, pats), " (", (Q, init), "; ", (Q, upd)]
        if self.chance(0.6):
            ext = self.with_vars(names, lambda: self.pick([
                (3, lambda: self.q(d - 1)),
                (2, lambda: arr(comma(lf("$" + names[0]), lf(".")))),
                (1, lambda: lf("empty")),
                (1, lambda: comma(lf("."), lf("."))),
            ]))
            parts += ["; ", (Q, ext)]
        parts.append(")")
        return N(A, parts)

    def label(self, d):
        name = self.ch(["out", "f", "l", "done"])
        saved = list(self.labels)
        self.labels.append(name)
        try:
            body = self.pick([
                (3, lambda: self.q(d - 1)),
                (3, lambda: pipe(self.gen(d - 1), N(A, ["if ", (Q, self.cond(d - 1)),
                                                        " then ., break $" + name + " else . end"]))),
                (2, lambda: comma(self.q(d - 1), lf("break $" + name))),
                (1, lambda: N(A, ["foreach ", (E, self.gen(d - 1)),
                                  " as $item (0; . + 1; if . > 2 then ., break $" + name
                                  + " else $item end)"])),
            ])
        finally:
            self.labels = saved
        return N(Q, ["label $" + name + " | ", (Q, body)])

    def break_(self):
        if self.labels and self.chance(0.95):
            return lf("break $" + self.ch(self.labels))
        if self.chance(0.85):
            # A label right here: `label $x | ..., break $x`.
            n = self.ch(["out", "x"])
            return N(Q, ["label $" + n + " | ", (Q, comma(lf(self.ch(["1", ".", ".a?", "2"])), lf("break $" + n)))])
        return lf("break $" + self.ch(["nolabel", "out"]))

    def funcdef(self, d):
        name = self.ch(["f", "g", "h", "f", "rec", "map", "length", "not", "select", "_f",
                        "empty", "add", "first", "range", "error", "tostring"])
        nparams = self.ch([0, 0, 1, 1, 1, 2, 2, 3])
        params = []
        for i in range(nparams):
            params.append(self.ch(["$p", "q", "$r", "s", "f"]) + str(i))
        saved = (list(self.vars), list(self.closures), list(self.funcs))
        for p in params:
            if p.startswith("$"):
                self.vars.append(p[1:])
                self.closures.append(p[1:])  # $x params are also callable as x
            else:
                self.closures.append(p)
        body = self.pick([
            (4, lambda: self.q(d - 1)),
            # Bounded recursion.
            (2, lambda: N(A, ["if type != \"number\" or isnan or . >= 3 or . <= -5 then . else (. + 1 | " + name
                              + ("(" + "; ".join(p if not p.startswith("$") else p for p in params) + ")"
                                 if params else "") + ") end"])),
            (1, lambda: comma(self.closure_or_dot(), self.closure_or_dot())),
            (1, lambda: pipe(self.closure_or_dot(), self.q(d - 1))),
        ])
        self.vars, self.closures = saved[0], saved[1]
        self.funcs = saved[2] + [(name, params)]
        rest = self.q(d - 1)
        if self.chance(0.7):
            rest = pipe(self.user_call_named(name, params, d), rest) if self.chance(0.6) else \
                comma(self.user_call_named(name, params, d), rest)
        self.funcs = saved[2]
        head = "def " + name + ("(" + "; ".join(params) + ")" if params else "") + ": "
        return N(Q, [head, (Q, body), "; ", (Q, rest)])

    def closure_or_dot(self):
        if self.closures and self.chance(0.8):
            return lf(self.ch(self.closures))
        return lf(".")

    def user_call_named(self, name, params, d):
        args = []
        for p in params:
            if p.startswith("$"):
                args.append(self.pick([(3, self.literal), (2, lambda: self.t(d - 1)),
                                       (1, lambda: comma(self.small_int(), self.small_int()))]))
            else:
                args.append(self.pick([(3, lambda: self.t(d - 1)), (1, lambda: self.gen(d - 1)),
                                       (1, lambda: self.upd(d - 1))]))
        return call(name, *args)

    def user_call(self, d):
        if self.funcs:
            name, params = self.ch(self.funcs)
            return self.user_call_named(name, params, d)
        if self.closures:
            return lf(self.ch(self.closures))
        return self.builtin(d)

    # -- semantic sub-generators ------------------------------------------

    def cond(self, d):
        if d <= 0:
            return self.pick([
                (3, lambda: lf(self.ch([". == null", "type == \"number\"", ". > 1", "length > 1",
                                        "not", ". == \"a\"", "true", "false", "null", ".",
                                        ".a", "has(\"a\")?", "isnan", "type == \"string\"",
                                        ". < 2", ". != 0", "length == 0", "(. | numbers) > 0"]), E)),
                (1, lambda: binop(lf("."), self.ch(["==", "<", ">", "!="]), self.literal())),
            ])
        return self.pick([
            (4, lambda: self.cond(0)),
            (3, lambda: binop(self.t(d - 1), self.ch(["==", "!=", "<", "<=", ">", ">="]),
                              self.t(d - 1))),
            (2, lambda: binop(self.cond(d - 1), self.ch(["and", "or"]), self.cond(d - 1))),
            (1, lambda: pipe(self.cond(d - 1), lf("not"))),
            (1, lambda: call("isempty", self.gen(d - 1))),
            (1, lambda: call("has", self.key())),
            (1, lambda: call("test", self.regex())),
            (1, lambda: call(self.ch(["startswith", "endswith", "contains", "inside"]),
                             self.strlit() if self.chance(0.7) else self.literal())),
            (1, lambda: call(self.ch(["any", "all"]), self.cond(d - 1))),
            (1, lambda: call(self.ch(["any", "all"]), self.gen(d - 1), self.cond(d - 1))),
            (1, lambda: call("IN", self.gen(d - 1))),
            (1, lambda: call("in", self.literal())),
            (0.5, lambda: self.q(d - 1)),
        ])

    def key(self):
        return self.pick([(3, self.keylit), (2, self.small_int), (1, self.literal)])

    def gen(self, d):
        """Something that usually yields several outputs (or none)."""
        if d <= 0:
            return lf(self.ch([".[]", ".[]?", "range(3)", "(1,2,3)", "empty", "..", "(.a,.b)",
                               "keys[]", "to_entries[]", "paths", "(1, error(\"x\"), 3)",
                               "range(0; 10; 3)", "(\"a\",\"b\")", "(null, false, 0)",
                               ".[]?|.[]?", "splits(\",\")?", "tostream", "(1,2)",
                               "range(5;0;-2)", "inputs", "recurse", "getpath([\"a\"],[\"b\"])"]))
        return self.pick([
            (4, lambda: self.gen(0)),
            (2, lambda: call("range", self.small_int())),
            (1, lambda: call("range", self.small_int(), self.small_int())),
            (1, lambda: call("range", self.small_int(), self.small_int(), self.small_int())),
            (2, lambda: comma(comma(self.t(d - 1), self.t(d - 1)), self.t(d - 1))),
            (2, lambda: call("limit", self.small_int(), self.gen(d - 1))),
            (1, lambda: call("skip", self.small_int(), self.gen(d - 1))),
            (1, lambda: call("first", self.gen(d - 1))),
            (1, lambda: call("last", self.gen(d - 1))),
            (1, lambda: call("nth", self.small_int(), self.gen(d - 1))),
            (1, lambda: call("limit", self.small_int(), call("repeat", self.upd(d - 1)))),
            (1, lambda: call("limit", self.small_int(), call("while", self.cond(d - 1), self.upd(d - 1)))),
            (1, lambda: self.until()),
            (1, lambda: call("limit", lf("6", T), call("recurse", self.upd(d - 1), self.cond(d - 1)))),
            (1, lambda: call("recurse", lf(".[]?"))),
            (2, lambda: pipe(lf(".[]"), call("select", self.cond(d - 1)))),
            (1, lambda: call(self.ch(["scan", "splits"]), self.regex())),
            (1, lambda: call("match", self.regex(), lf('"g"'))),
            (1, lambda: call("paths", self.cond(d - 1))),
            (1, lambda: pipe(lf(".[]?"), self.upd(d - 1))),
            (1, lambda: lf(self.ch(["input", "inputs", "$__loc__", "env|keys[]|select(startswith(\"T\"))"]))),
            (1, lambda: self.errorish(d - 1)),
            (1, lambda: call("getpath", self.patharr())),
            (1, lambda: self.q(d - 1)),
        ])

    def upd(self, d):
        """An update function (of `.`)."""
        if d <= 0:
            return lf(self.ch([". + 1", ". * 2", ". - 1", "tostring", "tojson", "length", "[.]",
                               "{a: .}", "empty", "null", "1", "\"x\"", "error", "(., .)",
                               ". // 0", "ascii_downcase", ".[1:]", ".a", "not", "-.", "keys",
                               "add", ". + \"x\"", ". / 2", "floor", ". % 2", "[.[]?]", "..",
                               "tonumber", "@base64", "\"\\(.)!\"", "abs", "first(.[]?)",
                               "try error catch .", ".a |= 1", "del(.a)", "select(. != null)",
                               ". + [1]", "{} + .", "type", "..|numbers", "(1, 2)", "input",
                               "if . then 1 end", ". == 1", "ltrimstr(\"a\")", "$__loc__",
                               "[.] | add", "getpath([\"a\"])", "tostream", "@json",
                               ".[0]?", "{(.|tostring): 1}", "reverse", "sort", "- .",
                               "infinite", "nan", "splits(\"a\")"]), E)
        return self.pick([
            (3, lambda: self.upd(0)),
            (2, lambda: self.e(d - 1)),
            (1, lambda: pipe(self.upd(d - 1), self.upd(d - 1))),
            (1, lambda: comma(self.upd(d - 1), self.upd(d - 1))),
            (1, lambda: self.if_(d)),
            (1, lambda: call("select", self.cond(d - 1))),
            (1, lambda: self.try_(d)),
        ])

    def path(self, d):
        """A path expression (for path(), del(), paths, assignment LHS, pick())."""
        if d <= 0:
            return self.pick([
                (6, self.field),
                (1, lambda: lf(".")),
                (1, lambda: lf("..", T)),
                (0.5, lambda: lf(self.ch(["empty", "error", "1", "$__loc__", ".a + 1", "tostring",
                                          "[.a]", "{}", "null", "input", "\"a\"", "$ENV",
                                          "first(.a, .b)", "getpath([\"a\"])", "getpath([0, \"a\"])",
                                          "paths", ".. | numbers", ".[] | select(. == 2)",
                                          "to_entries", "select(.a)", "recurse(.[]?; . != 3)",
                                          ".[length - 1]?", ".[.a]?", "last(.[])", "limit(1; .[])",
                                          "nth(1; .[])", ".a // .b", "if .a then .a else .b end",
                                          "try .a catch .b", "(.a, .b)", ".a as $x | .b",
                                          "def f: .a; f", "label $f | .a, break $f", "getpath([])",
                                          "first(empty)", ".[1:3][0]", ".[-1:][0]?", "values",
                                          "objects", "arrays", "scalars", "splits(\"a\")"]), E)),
            ])
        return self.pick([
            (4, lambda: self.path(0)),
            (2, lambda: pipe(self.path(d - 1), self.path(d - 1))),
            (1.5, lambda: comma(self.path(d - 1), self.path(d - 1))),
            (1.5, lambda: pipe(self.path(d - 1), call("select", self.cond(d - 1)))),
            (1, lambda: N(A, ["if ", (Q, self.cond(d - 1)), " then ", (Q, self.path(d - 1)),
                              " else ", (Q, self.path(d - 1)), " end"])),
            (1, lambda: binop(self.path(d - 1), "//", self.path(d - 1))),
            (1, lambda: call(self.ch(["first", "last"]), self.path(d - 1))),
            (1, lambda: call("limit", self.small_int(), self.path(d - 1))),
            (1, lambda: call("getpath", self.patharr())),
            (1, lambda: pipe(lf(".."), lf(self.ch(["numbers", "strings", "select(type == \"object\")",
                                                   "arrays", "nulls", "scalars"])))),
            (1, lambda: call("recurse", lf(self.ch([".[]?", ".[]?; . != null", ".a?; . != null", ".[0]?; . != null"])))),
            (1, lambda: suffix(self.path(d - 1), [self.ch([".a", "[0]", "[]", "[1:]", "?", "[-1]"])])),
            (0.5, lambda: self.try_(d)),
            (0.3, lambda: self.reduce(d)),
            (0.5, lambda: self.foreach(d)),
        ])

    def patharr(self):
        return lf(self.ch([
            '[]', '["a"]', '["a","b"]', '[0]', '[-1]', '["a",0]', '[0,"a"]', '[1,2]',
            '[{"start":1,"end":2}]', '[{"start":null,"end":-1}]', '[null]', '[1.5]', '[true]',
            '["a",null]', '[[0]]', '[{}]', '["é"]', '[""]', '["a",{"start":0}]', '[-5]',
            '["b",-1]', '[3]', '["a","a","a"]', '[{"start":1}]', '[0,0]', '["x",1]',
            '[{"start":-1,"end":null}]', '[1e10]', '["a", {"start": 0, "end": 1}, 0]',
        ]))

    def patharrs(self, d):
        return self.pick([
            (3, lambda: lf(self.ch(['[["a"]]', '[[0],[1]]', '[[1],[0]]', '[["a","b"],["a"]]', '[[]]',
                                    '[[-1]]', '[[{"start":0,"end":1}]]', '[["a"],["b"],["a"]]',
                                    '[[0,"a"],[0]]', '[]', '[[5]]', '[[-1],[0]]', '[["x"]]',
                                    '[[{"start":1}],[0]]', '[1]', '["a"]', '[[null]]']))),
            (1, lambda: arr(call("paths"))),
            (1, lambda: arr(call("paths", self.cond(0)))),
            (1, lambda: arr(call("path", self.path(d - 1)))),
        ])

    def regex(self):
        return lf(jstr(self.ch(REGEXES)))

    def flags(self):
        if self.chance(0.1):
            return lf("null")
        return lf(jstr(self.ch(FLAGS)))

    def builtin(self, d):
        a = lambda: self.q(d - 1)
        tt = lambda: self.t(d - 1)
        return self.pick([
            (8, lambda: lf(self.ch(PLAIN0), A)),
            (0.3, lambda: lf(self.ch(BOGUS0), A)),
            (1.5, lambda: lf(self.ch(FILTERS0))),
            (2, lambda: lf(self.ch(MATH0))),
            (1, lambda: call(self.ch(MATH2), self.num(), self.num())),
            (1, lambda: call(self.ch(MATH2), lf("."), self.num())),
            (0.3, lambda: call("fma", self.num(), self.num(), self.num())),
            (0.4, lambda: pipe(lf(self.ch(["2.5", "0.5", "-1.5", "10", "1", "3"]), T), lf("lgamma_r"))),
            (2, lambda: call("map", self.upd(d - 1))),
            (1, lambda: call("map_values", self.upd(d - 1))),
            (1.5, lambda: call("select", self.cond(d - 1))),
            (1, lambda: call("has", self.key())),
            (0.5, lambda: call("in", a())),
            (0.5, lambda: call("inside", self.literal())),
            (1, lambda: call("contains", self.literal() if self.chance(0.7) else a())),
            (1.5, lambda: call("path", self.path(d - 1))),
            (1.5, lambda: arr(call("paths"))),
            (1, lambda: arr(call("paths", self.cond(d - 1)))),
            (2, lambda: call("del", self.path(d - 1))),
            (1.5, lambda: call("getpath", self.patharr())),
            (1.5, lambda: call("setpath", self.patharr(), self.t(d - 1))),
            (1.5, lambda: call("delpaths", self.patharrs(d))),
            (1, lambda: call("pick", self.path(d - 1))),
            (1, lambda: call("to_entries")),
            (1.5, lambda: call("with_entries", self.entry_upd(d))),
            (0.5, lambda: pipe(lf("to_entries"), lf("from_entries"))),
            (1, lambda: call("from_entries")),
            (1.5, lambda: call(self.ch(["sort_by", "group_by", "unique_by", "min_by", "max_by"]),
                               self.keyfn(d))),
            (1, lambda: call("add", self.gen(d - 1))),
            (1, lambda: call(self.ch(["any", "all"]), self.gen(d - 1), self.cond(d - 1))),
            (1, lambda: call(self.ch(["any", "all"]), self.cond(d - 1))),
            (1, lambda: call("flatten", self.small_int())),
            (1, lambda: call("range", self.small_int())),
            (1, lambda: call("range", self.small_int(), self.small_int())),
            (0.5, lambda: call("range", self.small_int(), self.small_int(), self.small_int())),
            (1.5, lambda: arr(call("limit", self.small_int(), self.gen(d - 1)))),
            (1, lambda: arr(call("skip", self.small_int(), self.gen(d - 1)))),
            (1, lambda: call("first", self.gen(d - 1))),
            (1, lambda: call("last", self.gen(d - 1))),
            (0.5, lambda: call("nth", self.small_int())),
            (1, lambda: call("nth", self.small_int(), self.gen(d - 1))),
            (1, lambda: arr(call("limit", self.small_int(), call("repeat", self.upd(d - 1))))),
            (1, lambda: arr(call("limit", lf("5", T), call("while", self.cond(d - 1), self.upd(d - 1))))),
            (1, self.until),
            (1, lambda: call("isempty", self.gen(d - 1))),
            (1.5, lambda: call("error", tt())),
            (0.4, lambda: call("halt_error", self.small_int())),
            (1, lambda: arr(call("limit", lf("8", T), call("recurse", self.upd(d - 1), self.cond(d - 1))))),
            (1, lambda: arr(call("recurse", lf(".[]?")))),
            (1, lambda: call("walk", self.upd(d - 1))),
            (1, lambda: call("fromstream", self.gen(d - 1) if self.chance(0.4) else lf("tostream"))),
            (0.5, lambda: arr(pipe(self.small_int(), call("truncate_stream", lf("[[0],1],[[1,0],2],[[1,0]],[[1]]"))))),
            (0.5, lambda: arr(call("truncate_stream", pipe(self.small_int(), lf("tostream"))))),
            (1, lambda: call("combinations", self.small_int())),
            (1, lambda: call("bsearch", self.literal())),
            (1, lambda: call(self.ch(["indices", "index", "rindex"]), self.literal())),
            (1, lambda: call(self.ch(["ltrimstr", "rtrimstr", "trimstr", "startswith", "endswith"]),
                             self.strlit() if self.chance(0.8) else self.literal())),
            (1, lambda: call("split", self.strlit())),
            (1, lambda: call("split", self.regex(), self.flags())),
            (1, lambda: call("join", self.strlit() if self.chance(0.8) else self.literal())),
            (2, lambda: call(self.ch(["test", "match", "capture"]), self.regex())),
            (2, lambda: call(self.ch(["test", "match", "capture", "scan", "splits"]), self.regex(), self.flags())),
            (1, lambda: arr(call(self.ch(["scan", "splits"]), self.regex()))),
            (1.5, lambda: call(self.ch(["sub", "gsub"]), self.regex(), self.repl(d))),
            (1, lambda: call(self.ch(["sub", "gsub"]), self.regex(), self.repl(d), self.flags())),
            (1, lambda: call(self.ch(["strftime", "strflocaltime", "strptime"]), lf(jstr(self.ch(FMTS))))),
            (1, lambda: pipe(lf(jstr(self.ch(DATE_STRS))), call(self.ch(["strptime", "fromdate", "fromdateiso8601"]) if self.chance(0.5) else "strptime", *([] if self.chance(0.5) else [lf(jstr(self.ch(FMTS)))])))),
            (1, lambda: pipe(self.num(), lf(self.ch(["todate", "gmtime", "localtime", "gmtime|mktime", "gmtime|todate", "strftime(\"%c\")", "localtime|mktime", "gmtime|strftime(\"%A %j %U\")", "todateiso8601", "dateadd(\"seconds\"; 1)", "date"])))),
            (0.3, lambda: lf("now | type")),
            (1, lambda: call("IN", self.gen(d - 1))),
            (0.5, lambda: call("IN", self.gen(d - 1), self.gen(d - 1))),
            (1, lambda: call("INDEX", self.keyfn(d))),
            (0.5, lambda: call("INDEX", self.gen(d - 1), self.keyfn(d))),
            (0.3, lambda: call("JOIN", lf('{"1":"x","a":"y"}'), self.keyfn(d))),
            (0.3, lambda: arr(call("JOIN", lf('{"1":"x","a":"y"}'), lf(".[]"), self.keyfn(d)))),
            (0.3, lambda: call("getpath", self.patharr())),
            (0.5, lambda: call("splits", self.regex())),
            (1, lambda: call("tojson")),
            (1, lambda: pipe(lf("tojson"), lf("fromjson"))),
            (1, lambda: call("format", lf(jstr(self.ch(["text", "json", "csv", "tsv", "html", "uri",
                                                     "sh", "base64", "base64d", "base32", "base32d",
                                                     "nope"]))))),
            (0.5, lambda: call("debug", self.t(d - 1))),
            (0.5, lambda: call("ltrimstr", self.t(d - 1))),
            (0.5, lambda: call("splits", self.strlit(), self.flags())),
            (0.3, lambda: call("getpath", arr(self.gen(d - 1)))),
            (0.3, lambda: call("error", call("error", tt()))),
            (0.3, lambda: call("tojson", tt())),
            (0.3, lambda: call(self.ch(["min_by", "max_by"]), lf(".[0]?"))),
            (0.3, lambda: call("add", lf(".[]?"))),
            (0.3, lambda: call("have_literal_numbers")),
            (0.2, lambda: call("toarray")),
            (0.2, lambda: call("abs")),
            (0.2, lambda: call("getpath", lf('["a"]'), lf("1"))),  # wrong arity
            (0.3, lambda: call(self.ch(["ascii", "implode", "explode"]))),
            (0.3, lambda: call("splits", lf("null"))),
            (0.2, lambda: call("input_line_number")),
            (0.3, lambda: call("$__loc__")),
            (0.3, lambda: pipe(lf("$__loc__"), self.t(d - 1))),
            (0.3, lambda: call("error", lf("$__loc__"))),
        ])

    UNTILS = [
        ("length > 3 or type != \"array\"", ". + [1]"),
        (". >= 5 or type != \"number\" or isnan", ". + 1"),
        ("type != \"array\" or length == 0", ".[1:]"),
        ("type != \"string\" or length < 2", ".[1:]"),
        ("type != \"object\" or length == 0", "del(keys_unsorted[0] as $k | .[$k])"),
        (". == null or type == \"boolean\"", "if type == \"array\" then .[0] elif type == \"object\" then .a else null end"),
        ("true", "error"),
        ("type != \"number\" or . > 100 or . < -100 or isnan", ". * 2 - 1"),
        ("length > 2", ". + \"x\""),
    ]

    def until(self):
        c, u = self.ch(self.UNTILS)
        return call("until", lf(c, E), lf(u, E))

    def keyfn(self, d):
        return self.pick([
            (3, lambda: lf(self.ch([".", ".a", ".b", "length", "type", "-.", ".[0]", "tostring",
                                    ".a, .b", "(.a | tostring)", "[.a, .b]", "not", "null",
                                    ".[]?", "empty", "keys", "abs", "1", "{a}", ".a // 0",
                                    "error", "tojson", ". % 2", "floor", "isnan"]), E)),
            (1, lambda: self.e(d - 1)),
        ])

    def entry_upd(self, d):
        return self.pick([
            (3, lambda: lf(self.ch([".value += 1", ".key |= ascii_upcase", "select(.value)",
                                    ".value |= tostring", "{key: .value, value: .key}",
                                    ".key |= tostring", "select(.key != \"a\")", ".value = null",
                                    ".key = 1", "empty", "(., .)", ".key += \"x\"", "{k: .key, v: .value}",
                                    "{name: .key, value}", ".value |= [.]", ".key |= \"\\(.)\"",
                                    "select(.value | type == \"number\")", "{key: null}",
                                    "{key: true, value: 1}", ".key |= length"]), E)),
            (1, lambda: self.upd(d - 1)),
        ])

    def repl(self, d):
        return self.pick([
            (3, lambda: lf(jstr(self.ch(["x", "", "\\\\", "$1", "-", "é", "\\n"])))),
            (2, lambda: lf(self.ch(['"<\\(.x)>"', '"\\(.a)"', '"[\\(.)]"', '"\\(.x // "-")"',
                                    '"\\(.captures)"', '"\\(.n)!"', '"\\(.)"', '(.x | ascii_upcase)',
                                    '"\\(.a)\\(.b)"', '("1", "2")', 'empty', '1', 'error("r")']))),
            (1, lambda: self.string(d - 1)),
        ])

    def assign(self, d):
        op = self.ch(["=", "|=", "+=", "-=", "*=", "/=", "%=", "//=", "|=", "|=", "=", "+="])
        lhs = self.path(d - 1)
        if op == "|=":
            rhs = self.upd(d - 1)
        else:
            rhs = self.pick([(3, lambda: self.t(d - 1)), (1, lambda: self.gen(0)),
                             (2, self.literal)])
        return N(E, [(T, lhs), " " + op + " ", (T, rhs)])

    def idiom(self, d):
        """Common jq idioms with random holes."""
        x = self.r.random()
        s = lambda: self.q(d - 1)
        if x < 0.08:
            return N(A, ["[.[] | ", (Q, self.upd(d - 1)), "]"])
        if x < 0.14:
            return N(A, ["reduce .[] as [$k, $v] ({}; .[$k|tostring] = $v)"])
        if x < 0.2:
            return pipe(lf("to_entries"), pipe(call("map", self.entry_upd(d)), lf("from_entries")))
        if x < 0.26:
            return N(A, ["[paths(", (Q, self.cond(d - 1)), ")]"])
        if x < 0.32:
            return N(E, ["(", (Q, self.path(d - 1)), ") |= ", (T, self.upd(d - 1))])
        if x < 0.38:
            return N(A, ["[.[] | tostring]"])
        if x < 0.44:
            return N(A, ["[.. | select(", (Q, self.cond(d - 1)), ")]"])
        if x < 0.5:
            return N(A, ["[splits(", (Q, self.regex()), ")]"])
        if x < 0.56:
            return N(A, ["[match(", (Q, self.regex()), "; ", (Q, self.flags()),
                         ") | [.offset, .length, .string, (.captures | map(.name, .string, .offset))]]"])
        if x < 0.62:
            return N(A, ["[foreach .[] as $x (0; . + ($x | numbers? // 1); [$x, .])]"])
        if x < 0.66:
            return N(A, ["[limit(", (Q, self.small_int()), "; ", (Q, self.gen(d - 1)), ")]"])
        if x < 0.7:
            return N(A, ["try (", (Q, self.q(d - 1)), ") catch ({e: ., t: type})"])
        if x < 0.74:
            return N(A, ["[.[] as [$a, $b] | {a: $a, b: $b}]"])
        if x < 0.78:
            return N(A, ["[.[] as {a: $x} ?// [$x] ?// $x | $x]"])
        if x < 0.82:
            return N(A, ["(tostream | select(length == 2) | .[1]) |= ", (T, self.upd(d - 1))])
        if x < 0.85:
            return N(A, ["[tostream] | fromstream(.[])"])
        if x < 0.88:
            return N(A, ["getpath([\"a\",\"b\"]) as $x | setpath([\"c\"]; $x)"])
        if x < 0.91:
            return N(A, ["[.[] | numbers] | (add / length)"])
        if x < 0.94:
            return N(A, ["with_entries(.value |= ", (T, self.upd(d - 1)), ")"])
        if x < 0.97:
            return N(A, ["[.[]?] | sort_by(", (Q, self.keyfn(d)), ") | group_by(", (Q, self.keyfn(d)), ")"])
        return N(A, ["label $out | reduce .[]? as $i (0; if $i == ", (T, self.literal()),
                     " then ., break $out else . + 1 end)"])


def gen_program(r, depth=None, vars_=()):
    g = Gen(r, vars_)
    if depth is None:
        depth = r.choice([1, 2, 2, 3, 3, 3, 4, 4, 5])
    return g.q(depth)


# ---------------------------------------------------------------------------
# Command lines
# ---------------------------------------------------------------------------

OUTPUT_FLAGS = [
    ["-c"], ["-r"], ["-j"], ["--tab"], ["--indent", "0"],
    ["--indent", "1"], ["--indent", "3"], ["--indent", "7"], ["-a"], ["-S"],
    ["-C"], ["-M"], ["--raw-output0"], ["--seq"], ["-e"], ["-cr"], ["-rj"], ["-ac"], ["-Sc"],
    ["-Cc"], ["--ascii-output"], ["--sort-keys"], ["--compact-output"], ["--join-output"],
    ["--raw-output"], ["--color-output"], ["--monochrome-output"], ["--exit-status"],
    ["--unbuffered"], ["-er"], ["--tab", "-c"], ["-c", "--tab"], ["-S", "-r"], ["-a", "-r"],
    ["-C", "-S"], ["-e", "-c"], ["-ec"], ["-rc"], ["-cS"], ["-aj"], ["--indent", "2"],
]
BAD_FLAGS = [["--indent", "8"], ["--indent", "-1"], ["--indent", "x"], ["--foo"], ["-Z"],
             ["--arg", "x"], ["--argjson"], ["--indent"], ["-L"], ["--slurpfile", "a"]]
INPUT_FLAGS = [
    ["-n"], ["-s"], ["-R"], ["-Rs"], ["-Rn"], ["-sn"], ["--stream"],
    ["--stream-errors"], ["--stream", "-s"], ["--stream", "-n"], ["--seq"], ["-s", "-R"],
    ["--slurp"], ["--null-input"], ["--raw-input"], ["-nR"], ["--stream", "-c"], ["-n"], ["-s"],
]

COLORS = ["0;31", "1;32:0;33", "4;36:1;35:0;31:0;32:0;33:0;34:0;35:1;30", "", "x", "1;31:",
          "::::::::", "0;30:0;31:0;32:0;33:0;34:0;35:0;36:0;37:0;38", "38;5;208", "0;31;1;4;5;7",
          "1234567890123456789", ":", "0:0:0:0:0:0:0:0"]


def gen_cli(r):
    """A command line: returns a case dict (see fuzz.py for the shape)."""
    flags = []
    k = r.random()
    if k < 0.45:
        flags.append(["-c"])
    elif k < 0.6:
        pass
    else:
        flags.append(r.choice(OUTPUT_FLAGS))
    if r.random() < 0.15:
        flags.append(r.choice(OUTPUT_FLAGS))
    if r.random() < 0.25:
        flags.append(r.choice(INPUT_FLAGS))
    if r.random() < 0.01:
        flags.insert(r.randint(0, len(flags)), r.choice(BAD_FLAGS))
    vars_ = []
    named = []
    for _ in range(r.choice([0] * 12 + [1, 1, 2])):
        k = r.random()
        name = r.choice(["x", "y", "v", "a", "ENV", "__loc__", "1x", "x"])
        if k < 0.4:
            val = r.choice(["1", "abc", "", "é", "1.0", "null", "{\"a\":1}", "-1", "\\n", " "])
            flags.append(["--arg", name, val])
        elif k < 0.8:
            val = r.choice(["1", "1.0", "1e1000", "[1,2]", "{\"a\":1}", "\"s\"", "null", "1.50",
                            "100000000000000000001", "-0", "{\"a\":1,\"a\":2}", "[]", "true",
                            "\"\\u00e9\"", "[1,[2]]", "0.1", "1E2"]
                           + (["x", "", "1 2", "nan", "["] if r.random() < 0.1 else []))
            flags.append(["--argjson", name, val])
        elif k < 0.9:
            flags.append(["--slurpfile", name, r.choice(["in0.json", "missing.json", "in1.json"])])
        else:
            flags.append(["--rawfile", name, r.choice(["in0.json", "missing.json", "raw.txt"])])
        vars_.append(name)
        named.append(name)
    positional = None
    if r.random() < 0.06:
        mode = r.choice(["--args", "--jsonargs"])
        vals = ["a", "1", "{}", "b c", "", "null", "2.50", "[1]", "\"s\"", "1e1000"]
        if r.random() < 0.1:
            vals += ["[1,", "-c", "x y"]
        positional = (mode, [r.choice(vals) for _ in range(r.randint(0, 3))])
    env = {}
    if any(("-C" in f or "--color-output" in f or "-Cc" in f) for f in flags) and r.random() < 0.5:
        env["JQ_COLORS"] = r.choice(COLORS)
    if r.random() < 0.03:
        env["NO_COLOR"] = r.choice(["1", ""])
    return flags, vars_, positional, env
