#!/usr/bin/env python3
"""Regenerate Track B1's builtin fixtures from the jq 1.8.1 binary.

    python3 src/jq/builtins/testdata/b1_gen.py            # uses `jq` on PATH
    JQ=/path/to/jq python3 src/jq/builtins/testdata/b1_gen.py

Each case calls one C builtin directly, as `$in | NAME($a; $b)` with the input and
arguments given as JSON text (so numbers are literals exactly as the port's parser
reads them), and records jq's compact output of `try [0, NAME(...)] catch [1, .]`.
With `"native": true` every number in the input and arguments is first turned into a
native double (`. * 1`), which is how arithmetic results reach builtins.
`src/jq/builtins/general/tests.rs` replays the cases through `function_list()`.
"""

import json
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
JQ = os.environ.get("JQ", "jq")

MAX_FILE_BYTES = 600_000

NAT = (
    "def nat: if type == \"number\" then . * 1 elif type == \"array\" then map(nat)"
    " elif type == \"object\" then map_values(nat) else . end;"
)


def run(args, inp=b""):
    env = {k: v for k, v in os.environ.items() if k not in ("JQ_COLORS", "NO_COLOR")}
    p = subprocess.run([JQ] + args, input=inp, capture_output=True, env=env)
    return p.stdout, p.stderr, p.returncode


class Cases:
    def __init__(self):
        self.cases = []
        self.seen = set()

    def add(self, f, inp, *args, native=False):
        key = (f, inp, args, native)
        if key in self.seen:
            return
        self.seen.add(key)
        self.cases.append({"f": f, "in": inp, "args": list(args), "native": native})

    def run(self):
        """Fills in "out" for every case, one jq process per (function, arity)."""
        groups = {}
        for c in self.cases:
            groups.setdefault((c["f"], len(c["args"])), []).append(c)
        for (f, nargs), cases in groups.items():
            params = ["$a", "$b"][:nargs]
            call = f + ("(" + "; ".join(params) + ")" if nargs else "")
            prog = (
                NAT
                + ".[] | (if .[0] then .[1:] | nat else .[1:] end) as [$in, $a, $b]"
                + f" | $in | try [0, {call}] catch [1, .]"
            )
            doc = "[" + ",".join(
                "[" + ",".join(["true" if c["native"] else "false", c["in"]] + c["args"]) + "]"
                for c in cases
            ) + "]"
            out, err, rc = run(["-c", prog], doc.encode())
            lines = out.decode().split("\n")
            if rc != 0 or lines[-1] != "" or len(lines) - 1 != len(cases):
                raise SystemExit(f"{f}/{nargs}: rc={rc} {len(lines) - 1} outputs for "
                                 f"{len(cases)} cases\n{err.decode()}")
            for c, line in zip(cases, lines):
                c["out"] = line

    def write(self, name):
        self.run()
        ver = run(["--version"])[0].decode().strip()
        lines = []
        for c in self.cases:
            d = {"f": c["f"], "in": c["in"], "args": c["args"]}
            if c["native"]:
                d["native"] = True
            d["out"] = c["out"]
            lines.append(json.dumps(d, ensure_ascii=True, separators=(",", ":")))
        total = sum(len(line) + 2 for line in lines)
        if total > MAX_FILE_BYTES:
            raise SystemExit(f"{name}: fixture too large ({total} bytes)")
        with open(os.path.join(HERE, name), "w") as fh:
            fh.write('{"jq": %s, "cases": [\n' % json.dumps(ver))
            fh.write(",\n".join(lines))
            fh.write("\n]}\n")
        print(f"wrote {len(lines)} cases ({total} bytes) to {name}", file=sys.stderr)


def has_number(text):
    """Whether a JSON text contains a number or `nan` (so a native variant differs)."""
    outside_strings = re.sub(r'"(?:[^"\\]|\\.)*"', '""', text)
    return re.search(r"[0-9]|nan", outside_strings) is not None


def js(s):
    """A JSON string literal (ASCII-escaped) for the Python string s."""
    return json.dumps(s, ensure_ascii=True)


# ------------------------------------------------------------------- values

NUMBERS = [
    "0", "-0", "1", "-1", "2", "3", "0.5", "1.5", "-1.5", "3.7", "-3.7", "1.000", "-1.50",
    "1e2", "1E-2", "0.1", "1.5e300", "1e1000", "-1e1000", "1e-400", "1e-320",
    "100000000000000000001", "9007199254740993", "2147483648", "-2147483649",
    "4294967296", "123456789012", "nan",
]
STRINGS = [
    '""', '"a"', '"abc"', '"ABC"', '"a,b,c"', '"h\\u00e9llo w\\u00f6rld"', '"\\u0000"',
    '"a\\u0000b"', '" \\t\\n x \\u00a0\\u2028 "', '"\\ud83d\\ude00"', '"1"', '"1.5"', '"-0"',
    '"1e2"', '"nan"', '"true"', '"false"', '"null"', '"[1,2]"', '"{\\"a\\":1}"',
    '"abcdefghijklmnopqrstuvwxyz"', '"a\\"b\'c<d>&e"', '"%41%"', '"x y\\\\z"',
]
ARRAYS = [
    "[]", "[1]", "[1,2,3]", "[3,1,2]", "[1,\"a\",null]", "[null,true,false,0,\"a\",[],{}]",
    "[[1,2],[1],[0,5]]", "[1,[2,[3]]]", "[{\"a\":1},{\"a\":0}]", "[nan,1,nan,0]",
    "[1.000,1,1.0]", "[\"b\",\"a\",\"c\",\"a\"]", "[1,1,2,2,1]",
    "[10,9,8,7,6,5,4,3,2,1,0]", "[0.5,-1,1e1000,-1e1000]",
    "[100000000000000000001,100000000000000000000]",
]
OBJECTS = [
    "{}", "{\"a\":1}", "{\"b\":2,\"a\":1}", "{\"a\":{\"b\":{\"c\":1}}}", "{\"a\":[1,2]}",
    "{\"a\":null}", "{\"\\u00e9\":1,\"a\":2,\"A\":3,\"\":4}", "{\"a\":1,\"b\":{\"c\":2}}",
    "{\"key\":\"a long value here\",\"k2\":[1,2,3]}",
]
SCALARS = ["null", "true", "false"]
VALUES = SCALARS + NUMBERS + STRINGS + ARRAYS + OBJECTS

# A smaller set for the binop cross product.
BINOP_VALUES = [
    "null", "true", "false", "0", "-0", "1", "-1", "0.5", "1.000", "1e1000", "nan",
    "100000000000000000001", '""', '"a"', '"a,b"', "[]", "[1,2,1]", "{}",
    "{\"a\":1,\"b\":{\"c\":2}}", "{\"a\":{\"d\":3},\"b\":{\"e\":4}}",
]


def gen_binops():
    c = Cases()
    ops = ["_plus", "_minus", "_multiply", "_divide", "_mod", "_equal", "_notequal",
           "_less", "_lesseq", "_greater", "_greatereq"]
    for op in ops:
        for a in BINOP_VALUES:
            for b in BINOP_VALUES:
                c.add(op, "null", a, b)
    # Native operands: exact literal comparisons become double comparisons.
    big = ["100000000000000000001", "100000000000000000000", "1.000", "1", "-0", "0"]
    for op in ops:
        for a in big:
            for b in big:
                c.add(op, "null", a, b, native=True)
    extra = [
        ("_plus", '"x"', '"\\u0000y"'), ("_plus", "[1,2,3]", "[]"), ("_plus", "{\"a\":1}", "{}"),
        ("_plus", "{\"a\":1,\"b\":2}", "{\"b\":3,\"a\":4,\"c\":5}"),
        ("_plus", "1e308", "1e308"), ("_plus", "-1e1000", "1e1000"),
        ("_plus", '"abcdefghijklmnop"', "{\"key\":[1,2,3]}"),
        ("_minus", "[1,[1],{\"a\":[1]},nan]", "[[1],{\"a\":[1]},nan]"),
        ("_minus", "[1.0,1,1.000]", "[1]"),
        ("_multiply", '"ab"', "2.9"), ("_multiply", '"ab"', "-0.5"), ("_multiply", '"ab"', "0.9999"),
        ("_multiply", '"ab"', "1e300"), ("_multiply", '""', "1e300"), ("_multiply", '"ab"', "-1e1000"),
        ("_multiply", "3", '"xy"'), ("_multiply", '"\\u00e9"', "3"),
        ("_multiply", "{\"a\":{\"b\":1,\"c\":{\"d\":1}}}", "{\"a\":{\"c\":{\"e\":2},\"f\":3}}"),
        ("_multiply", "{\"a\":{\"b\":1}}", "{\"a\":1}"), ("_multiply", "{\"a\":1}", "{\"a\":{\"b\":1}}"),
        ("_divide", "1", "3"), ("_divide", "-1", "0"), ("_divide", "0", "-0.0"), ("_divide", "1e300", "1e-300"),
        ("_divide", '"a,,b,"', '","'), ("_divide", '"abc"', '"abc"'), ("_divide", '"xaax"', '"aa"'),
        ("_divide", '"a\\u00e9b"', '""'), ("_divide", '""', '""'), ("_divide", '"a\\u0000b"', '"\\u0000"'),
        ("_mod", "5.9", "3.9"), ("_mod", "-5", "3"), ("_mod", "5", "-3"), ("_mod", "1e19", "7"),
        ("_mod", "-9223372036854775808", "-1"), ("_mod", "9223372036854775807", "2"),
        ("_mod", "-1e1000", "3"), ("_mod", "5", "1e1000"), ("_mod", "5", "0.99"), ("_mod", "5", "-0.5"),
        ("_mod", "1e30", "1e1000"), ("_mod", "2147483648", "2147483647"),
        ("_equal", "{\"a\":1,\"b\":2}", "{\"b\":2,\"a\":1}"), ("_equal", "[1,[2]]", "[1.0,[2.00]]"),
        ("_less", "[1,2]", "[1,2,0]"), ("_less", "{\"a\":2}", "{\"b\":1}"), ("_less", "{\"a\":1,\"b\":1}", "{\"a\":2}"),
        ("_less", "{\"a\":1,\"b\":2}", "{\"a\":1,\"b\":1}"), ("_greater", '"b"', '"ab"'), ("_less", '"a"', '"a\\u0000"'),
        ("_less", '"\\u00e9"', '"z"'), ("_less", "[nan]", "[nan]"), ("_greater", "[nan]", "[nan]"),
    ]
    for op, a, b in extra:
        c.add(op, "null", a, b)
    # The input is ignored.
    c.add("_plus", "{\"x\":1}", "1", "2")
    for v in VALUES:
        c.add("_negate", v)
        if has_number(v):
            c.add("_negate", v, native=True)
    c.write("b1_binops.json")


def gen_general():
    c = Cases()
    unary = ["tojson", "tostring", "tonumber", "toboolean", "fromjson", "keys", "keys_unsorted",
             "length", "utf8bytelength", "type", "isinfinite", "isnan", "isnormal", "sort",
             "unique", "min", "max", "error"]
    for f in unary:
        for v in VALUES:
            c.add(f, v)
            if has_number(v) and f in ("tojson", "tostring", "tonumber", "length", "isinfinite",
                                       "isnan", "isnormal", "sort", "unique", "min", "max",
                                       "error"):
                c.add(f, v, native=True)
    for f in ["infinite", "nan", "have_decnum", "have_literal_numbers"]:
        c.add(f, "null")
        c.add(f, "[1]")
    # tojson/tostring corner cases.
    deep = "[" * 300 + "]" * 300
    for f in ["tojson", "tostring"]:
        for v in [deep, "{\"a\":" + "[" * 257 + "]" * 257 + "}",
                  '"\\u007f\\u0080\\u001f\\b\\f\\/"', "[1e-400,-0.0,0.00,1.10]"]:
            c.add(f, v)
    # tonumber
    for s in ["0", "-0", "1.000", "1e2", "1E+2", ".5", "5.", "+1", "-.5", "1e1000", "1e-1000",
              "Infinity", "-Infinity", "inf", "-inf", "infinity", "INF", "nan", "NaN", "-nan",
              "sNaN", "NaN0", "NaN1", "nan123", " 1", "1 ", "", "-", "+", "0x10", "1_000", "1e",
              "1e+", "1.2.3", "\u0661", "1\u00002", "\u00001", "00012", "-00",
              "123456789012345678901234567890", "0.00000001", "1e-7", "1.5E+300", "1e999999999",
              "1e1000000000", "0e10", "-0.0e-5", "1.0e+01", "true", "null", "[1]", "0.1e1"]:
        c.add("tonumber", js(s))
    # toboolean
    for s in ["true", "false", "True", "TRUE", " true", "true ", "true\u0000", "false\u0000x",
              "", "1", "0", "yes", "null", "tru", "falsey"]:
        c.add("toboolean", js(s))
    # fromjson
    for s in ["1", "1.000", " 1 ", "1 2", "", " ", "nan", "NaN", "-nan", "[1,2", "{\"a\":1}",
              "{\"a\":1,\"a\":2}", "\"\\ud800\"", "\"\\ud83d\\ude00\"", "[1,]", "{", "tru", "true",
              "null", "\"a\\u0000b\"", "1e1000", "100000000000000000001", "[nan]", "-", "01", "1.",
              ".5", "+1", "[1,2]\n[3]", "\u00a01", "{\"a\":nan}", "infinity", "Infinity",
              "-Infinity", "'a'", "[1,2,3]]", "\"\\x\"", "\"\t\"", "[1,2]  ", "{\"a\":1}x",
              "\"\\u00e9\"", "\ufeff1", "[" * 3, "]", "{\"a\"}", "{1:2}", "\"abc", "1 // c",
              "\"\\u12\"", "1e", "-0", "0.10", "[1e2,1E-2]"]:
        c.add("fromjson", js(s))
    # keys / keys_unsorted / length / utf8bytelength
    for v in ["{\"b\":1,\"a\":2,\"c\":3}", "{\"b\":1,\"B\":2,\"\\u00e9\":3,\"e\":4,\"aa\":5,\"a\":6}",
              "[5,4,3]", "[[],{}]"]:
        for f in ["keys", "keys_unsorted", "length"]:
            c.add(f, v)
    # sort / unique / min / max
    for v in ["[3,\"a\",null,true,false,[1],{\"a\":1},0.5,\"B\",[],{}]",
              "[{\"b\":1},{\"a\":2},{\"a\":1,\"b\":0},{\"a\":1}]",
              "[[1,2],[1],[1,1],[0,9],[]]", "[1.000,1,1.0,0.5,\"1\"]",
              "[nan,1,nan,-1e1000,1e1000]", "[\"b\",\"a\",\"ab\",\"\",\"B\",\"\\u00e9\"]",
              "[-0,0,-0.0]", "[{\"a\":1,\"b\":2},{\"b\":2,\"a\":1}]",
              "[100000000000000000001,100000000000000000000,100000000000000000002]",
              "[null]", "[[nan],[nan]]"]:
        for f in ["sort", "unique", "min", "max"]:
            c.add(f, v)
            c.add(f, v, native=True)
    # _sort_by_impl & co.
    pairs = [
        ("[1,2,3]", "[[3],[1],[2]]"), ("[\"a\",\"b\",\"c\",\"d\"]", "[[1],[0],[1],[0]]"),
        ("[1,2]", "[[1]]"), ("[1,2]", "1"), ("1", "[1]"), ("{}", "{}"), ("[]", "[]"),
        ("[3,1,2]", "[3,1,2]"), ("[\"x\",\"y\",\"z\"]", "[[nan],[nan],[1]]"),
        ("[\"a\",\"b\",\"c\"]", "[null,null,null]"), ("[1,2,3,4]", "[[0,1],[0],[0,1],[0]]"),
        ("[\"p\",\"q\",\"r\"]", "[1.000,1,1.0]"), ("[1,2,3]", "[2,1,2]"), ("null", "null"),
        ("[\"a\",\"b\"]", "[{\"k\":1},{\"k\":0}]"), ("[1,2,3]", "[\"a\",\"b\"]"),
    ]
    for f in ["_sort_by_impl", "_group_by_impl", "_unique_by_impl", "_min_by_impl",
              "_max_by_impl"]:
        for a, b in pairs:
            c.add(f, a, b)
    # has
    for obj in ["{\"a\":1,\"b\":null}", "{}", "null", "[1,2,3]", "[]", "\"abc\"", "1", "true"]:
        for k in ["\"a\"", "\"b\"", "\"c\"", "\"\"", "0", "1", "2", "3", "-1", "0.5", "2.9",
                  "nan", "1e10", "-0", "null", "true", "[0]", "{}"]:
            c.add("has", obj, k)
    # contains
    pairs = [
        ("\"foobar\"", "\"bar\""), ("\"foobar\"", "\"baz\""), ("\"foobar\"", "\"\""),
        ("\"a\\u0000b\"", "\"b\""), ("\"a\\u0000b\"", "\"\\u0000\""), ("\"\"", "\"\""),
        ("[1,2,[3,4]]", "[[3]]"), ("[1,2,[3,4]]", "[[5]]"), ("[\"foobar\",\"baz\"]", "[\"bar\",\"ba\"]"),
        ("[]", "[]"), ("[1]", "[]"), ("[]", "[1]"),
        ("{\"a\":1,\"b\":{\"c\":\"xyz\"}}", "{\"b\":{\"c\":\"y\"}}"), ("{\"a\":1}", "{\"a\":1,\"b\":2}"),
        ("{\"a\":[1,2]}", "{\"a\":[]}"), ("{}", "{}"), ("1", "1"), ("1", "1.0"), ("1", "2"),
        ("nan", "nan"), ("null", "null"), ("true", "true"), ("false", "false"),
        ("true", "false"), ("false", "true"), ("1", "\"1\""), ("\"a\"", "[\"a\"]"),
        ("[1]", "1"), ("{\"a\":1}", "[]"), ("null", "false"), ("[nan]", "[nan]"),
        ("{\"a\":true}", "{\"a\":false}"), ("[true]", "[false]"),
        ("\"abcdefghijklmnop\"", "{\"key\":[1,2,3]}"),
    ]
    for a, b in pairs:
        c.add("contains", a, b)
    # getpath / setpath / delpaths
    roots = ["null", "{\"a\":{\"b\":[1,2,{\"c\":3}]},\"d\":1}", "[1,[2,3],{\"a\":4}]", "1",
             "\"abc\"", "true", "{}", "[]"]
    paths = ["[]", "[\"a\"]", "[\"a\",\"b\"]", "[\"a\",\"b\",2,\"c\"]", "[\"a\",\"b\",-1]",
             "[0]", "[1,0]", "[-1]", "[5]", "[1.7]", "[\"d\",\"e\"]", "[{\"start\":1,\"end\":2}]",
             "[{\"start\":1}]", "[{\"start\":null,\"end\":-1}]", "[{\"start\":\"a\"}]",
             "[null]", "[true]", "[[1]]", "[[]]", "[nan]", "[1e1000]", "[-5]", "[\"a\",0]",
             "null", "\"a\"", "1", "{}"]
    for r in roots:
        for p in paths:
            c.add("getpath", r, p)
            c.add("setpath", r, p, "9")
    for r, p, v in [("null", "[\"a\",0,\"b\"]", "1"), ("[1,2,3]", "[{\"start\":1,\"end\":2}]", "[\"x\",\"y\"]"),
                    ("[1,2,3]", "[{\"start\":1,\"end\":2}]", "\"x\""), ("[1,2,3]", "[{\"start\":-1}]", "[]"),
                    ("[1,2,3]", "[10]", "1"), ("[1,2,3]", "[-4]", "1"), ("[1,2,3]", "[536870912]", "1"),
                    ("[1]", "[-1]", "5"), ("{\"a\":1}", "[\"a\"]", "{\"b\":2}"), ("\"abc\"", "[{\"start\":1}]", "\"x\""),
                    ("{\"a\":[1]}", "[\"a\",\"b\"]", "1"), ("[]", "[\"a\"]", "1"), ("null", "[-1]", "1"),
                    ("null", "[{\"start\":0,\"end\":0}]", "[1]"), ("[1,2,3]", "[{\"start\":2,\"end\":1}]", "[9]"),
                    ("[1,2,3]", "[{\"start\":0.5,\"end\":1.5}]", "[9]"), ("null", "[nan]", "1"),
                    ("[1,2,3]", "[{\"start\":1,\"end\":2},0]", "9")]:
        c.add("setpath", r, p, v)
    delroots = ["{\"a\":{\"b\":[1,2,{\"c\":3}]},\"d\":1}", "[1,[2,3],{\"a\":4},5,6]", "null", "1",
                "\"abc\"", "{}", "[]"]
    delpaths = ["[]", "[[]]", "[[\"a\"]]", "[[\"a\",\"b\",0],[\"a\",\"b\",2,\"c\"]]", "[[0],[2]]",
                "[[-1],[0]]", "[[{\"start\":1,\"end\":3}]]", "[[1,0],[1,1]]", "[[\"x\"]]",
                "[[\"d\"],[\"a\"]]", "[[5]]", "[[0.5]]", "[[\"a\",\"b\",\"c\"]]", "[1]", "[[1],2]",
                "[\"a\"]", "null", "{}", "[[null]]", "[[true]]", "[[\"a\"],[]]",
                "[[{\"start\":-2}],[0]]", "[[{\"start\":\"x\"}]]", "[[-10]]"]
    for r in delroots:
        for p in delpaths:
            c.add("delpaths", r, p)
    # bsearch
    for arr in ["[1,2,3]", "[]", "[1,3,5,7]", "[\"a\",\"c\"]", "[null,false,true,1,\"a\",[],{}]",
                "[1,1,1]", "[nan,1]", "[3,2,1]"]:
        for t in ["0", "1", "2", "3", "4", "7", "8", "\"b\"", "null", "[]", "nan", "1.0", "{}"]:
            c.add("bsearch", arr, t)
    for v in ["1", "\"a\"", "{}", "null"]:
        c.add("bsearch", v, "1")
    c.write("b1_general.json")


def gen_strings():
    c = Cases()
    strs = ['""', '"a"', '"abc"', '"abcabc"', '"h\\u00e9llo"', '"\\u00e9"', '"a\\u0000b"',
            '"\\ud83d\\ude00x"', '"x\\ud83d\\ude00"', '"bc"', '"ab"', '"ca"']
    others = ["null", "1", "[\"a\"]", "{}", "true"]
    for f in ["startswith", "endswith", "split", "_strindices"]:
        for a in strs:
            for b in strs:
                c.add(f, a, b)
        if f != "_strindices":  # jq aborts on non-strings
            for a in strs[:3] + others:
                for b in strs[:3] + others:
                    c.add(f, a, b)
    for a, b in [("\"a,b, cd, efg\"", "\", \""), ("\"aaaa\"", "\"aa\""), ("\"a,b,\"", "\",\""),
                 ("\",\"", "\",\""), ("\"a,,b\"", "\",\""), ("\"xaax\"", "\"aa\""),
                 ("\"\\u00e9a\\u00e9a\"", "\"a\""), ("\"a\\u00e9b\\u00e9c\"", "\"\\u00e9\""),
                 ("\"\\ud83d\\ude00\\ud83d\\ude00\"", "\"\\ude00\"")]:
        for f in ["split", "_strindices"]:
            c.add(f, a, b)
    for v in VALUES:
        for f in ["explode", "implode", "trim", "ltrim", "rtrim"]:
            c.add(f, v)
    ws = ['"  a  "', '"\\t\\n\\u000b\\f\\r a \\u0085\\u00a0\\u1680\\u2000\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000"',
          '"\\u200b a \\u200b"', '"   "', '"a"', '" a"', '"a "', '"\\u00a0"', '"\\u0000 a \\u0000"',
          '"\\u001f a \\u001c"', '"\\u180e a \\ufeff"', '" a b "', '"\\u3000\\u00e9\\u3000"']
    for v in ws:
        for f in ["trim", "ltrim", "rtrim", "explode"]:
            c.add(f, v)
    for v in ["[65,66]", "[128512]", "[55296]", "[57343]", "[55295]", "[-1]", "[1114111]",
              "[1114112]", "[65.9]", "[-0.5]", "[\"a\"]", "[nan]", "[null]", "[]", "[0]",
              "[1e10]", "[-1e10]", "[1e1000]", "[-1e1000]", "[65,\"a\"]", "[[65]]",
              "[104,233,108,108,111]", "[2147483648]", "[4294967361]"]:
        c.add("implode", v)
    c.write("b1_strings.json")


def gen_format():
    c = Cases()
    fmts = ["text", "json", "csv", "tsv", "html", "uri", "urid", "sh", "base64", "base64d"]
    for fmt in fmts:
        for v in VALUES:
            c.add("format", v, js(fmt))
    for v in ["[1,\"a\",null,true,false,1.000,nan,-0,1e1000,\"a\\\"b\",\"x,y\"]",
              "[\"a\\tb\\nc\\rd\\\\e\\u0000f\"]", "[[1]]", "[{}]", "[1,[2]]", "[\"\\u00e9\"]",
              "[100000000000000000001]", "[\"it's\"]", "[]", "[null]", "[\"\"]",
              "[1,{\"a\":1}]"]:
        for fmt in ["csv", "tsv", "sh"]:
            c.add("format", v, js(fmt))
            if has_number(v):
                c.add("format", v, js(fmt), native=True)
    for v in ['"<script>alert(\'x\')&\\"</script>"', '"\\u00e9 \\ud83d\\ude00 ~-_.!*\'()"',
              '"a\\u0000b"', "[\"<\"]"]:
        for fmt in ["html", "uri", "sh", "base64"]:
            c.add("format", v, js(fmt))
    for s in ["%C3%A9", "%c3%a9", "\u00e9", "a%20b", "%", "%4", "%4g", "%C3A9", "%80", "%ED%A0%80",
              "%F0%9F%98%80", "%FF", "%00x", "a\u0000%41", "%E2%82", "%E2%82%AC", "%C0%80",
              "%F4%90%80%80", "%41%42", "+", "%%41", "%2", "%C3%", "%F0%9F%98", "100%"]:
        c.add("format", js(s), js("urid"))
    for s in ["YWJj", "YWJ", "YW", "Y", "YQ==YQ==", "Y Q", "/w==", "gA==", "", "=", "==", "YWJjZA",
              "YWJjZA==", "Y=Q", "YWJj\n", "8J+YgA==", "wA==", "4pyTIMOgIGxhIG1vZGU=", "-_", "a+/b",
              "AAAA", "YQ", "Zm9v\u00e9", "YW\u0000Jj"]:
        c.add("format", js(s), js("base64d"))
    for s in ["", "a", "ab", "abc", "abcd", "\u00e9", "\ud83d\ude00", "\u0000", "hello world"]:
        c.add("format", js(s), js("base64"))
    for fmt in ["xml", "", "JSON", "json\u0000x", "base32", "base32d", "text ", "@json"]:
        c.add("format", "[1]", js(fmt))
    for fmt in ["1", "null", "[\"json\"]", "{}", "true"]:
        c.add("format", "\"x\"", fmt)
    c.write("b1_format.json")


if __name__ == "__main__":
    gen_binops()
    gen_general()
    gen_strings()
    gen_format()
