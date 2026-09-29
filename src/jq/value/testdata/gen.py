#!/usr/bin/env python3
"""Regenerate the value-layer test fixtures from the jq 1.8.1 binary.

    python3 src/jq/value/testdata/gen.py            # uses `jq` on PATH
    JQ=/path/to/jq python3 src/jq/value/testdata/gen.py

Every expectation in the JSON files next to this script comes from running
the real jq binary; `src/jq/value/tests.rs` replays them against the port.
Byte strings that are not valid UTF-8 are stored hex-encoded (`*_hex`).
"""

import json
import os
import random
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
JQ = os.environ.get("JQ", "jq")


def run(args, inp=b"", env=None):
    e = {k: v for k, v in os.environ.items() if k not in ("JQ_COLORS", "NO_COLOR")}
    if env:
        e.update(env)
    p = subprocess.run([JQ] + args, input=inp, capture_output=True, env=e)
    return p.stdout, p.stderr, p.returncode


def enc(case, key, b):
    """Store bytes as a string when valid UTF-8, else hex."""
    try:
        case[key] = b.decode("utf-8")
    except UnicodeDecodeError:
        case[key + "_hex"] = b.hex()


MAX_CASE_BYTES = 200_000
MAX_FILE_BYTES = 2_000_000


def write(name, cases):
    ver = run(["--version"])[0].decode().strip()
    path = os.path.join(HERE, name)
    lines = [json.dumps(c, ensure_ascii=True) for c in cases]
    for line in lines:
        if len(line) > MAX_CASE_BYTES:
            raise SystemExit(f"{name}: case too large ({len(line)} bytes): {line[:200]}")
    total = sum(len(line) + 2 for line in lines)
    if total > MAX_FILE_BYTES:
        raise SystemExit(f"{name}: fixture too large ({total} bytes)")
    with open(path, "w") as f:
        f.write('{"jq": %s, "cases": [\n' % json.dumps(ver))
        f.write(",\n".join(lines))
        f.write("\n]}\n")
    print(f"wrote {len(cases)} cases ({total} bytes) to {name}", file=sys.stderr)


def batch_lines(program, inputs):
    """Runs `jq -c program` over a JSON array of input texts; one output line per input."""
    doc = ("[" + ",".join(inputs) + "]").encode()
    out, err, rc = run(["-c", ".[] | " + program], doc)
    lines = out.decode().split("\n")
    assert lines[-1] == ""
    lines = lines[:-1]
    if len(lines) != len(inputs):
        raise SystemExit(f"batch mismatch for {program}: {len(lines)} vs {len(inputs)}\n{err.decode()}")
    return lines


# ---------------------------------------------------------------- numbers

NUMBER_LITERALS = [
    "0", "-0", "1", "-1", "1.0", "1.00", "1.50", "100", "1e2", "1E2", "1e+2", "1E-2", "1e-2", "3.0e0",
    "0e10", "0e-10", "0e-7", "0e-6", "0.00", "-0.0", "-0e5", "0.0e5", "1.5e-7", "1e-7", "1e-6", "0.000001",
    "0.0000001", "0.00000010", "123.456e-10", "12.34e5", "1.0e1", "007", "00", "-01", "0.5", "5e-1",
    "1e05", "100000000000000000000", "100000000000000000001", "123456789012345678901234567890",
    "1.2345678901234567890123e30", "12345678901234567890", "18446744073709551615",
    "18446744073709551616", "9007199254740993", "9007199254740992", "1e1000", "-1e1000", "1e308",
    "1.7976931348623157e308", "1.7976931348623159e308", "1.8e308", "1e309", "1e-400", "-1e-400",
    "4.9e-324", "2e-324", "3e-324", "2.4703282292062328e-324", "1e-999999999", "1e-1000000000",
    "1e-1147483647", "1e999999999", "1e1000000000", "10e999999999", "0e1000000000", "0e-2000000000",
    "1e0000000000005", "1e-0000000000005", "1e10000000000", "1e-10000000000", "0.1", "0.2", "0.3",
    "0.30000000000000004", "3.141592653589793", "2.718281828459045", "1e22", "1e23", "1e-5", "0.0001",
    "1.5e17", "15000000000000000", "1e16", "1e15", "123e-2", "123e2", "-123.4500", "5e-324",
    "1.0000000000000001110223024625156541", "1.00000000000000011102230246251565404236316680908203125",
    "1.0000000000000002220446049250313080847263336181640625", "0.1000000000000000055511151231257827",
    "9999999999999999999", "99999999999999999.5", "99999999999999995", "99999999999999985",
    "123456789012345675", "123456789012345665", "1234567890123456750", "0.12345678901234567500",
    "nan", "-nan", "NaN", "Infinity", "-Infinity", "inf", "-inf",
    "+1", ".5", "-.5", "5.", "-5.", "+.5e1", "1.e1", "0x", "1e", "1ee1", "--1", "+-1", "1.2.3",
]


def gen_numbers():
    valid = []
    for lit in NUMBER_LITERALS:
        out, err, rc = run(["-c", "."], lit.encode())
        if rc == 0:
            valid.append(lit)
    lines = batch_lines("[., . * 1, -., length, tostring]", valid)
    cases = [{"in": lit, "prog": "full", "out": line} for lit, line in zip(valid, lines)]
    # Long random literals exercise the 17-digit pre-rounding.
    rng = random.Random(1234)
    extra = []
    for _ in range(400):
        nd = rng.randint(16, 40)
        digits = str(rng.randint(1, 9)) + "".join(rng.choice("0123456789") for _ in range(nd - 1))
        if rng.random() < 0.3:
            # force a tie-ish pattern at digit 18
            digits = digits[:17] + "5" + "0" * rng.randint(0, 5) + ("" if rng.random() < 0.5 else "1")
        exp = rng.randint(-330, 300)
        point = rng.randint(1, len(digits))
        lit = digits[:point] + "." + digits[point:] + f"e{exp}"
        if rng.random() < 0.5:
            lit = "-" + lit
        extra.append(lit)
    lines = batch_lines("[., . * 1]", extra)
    cases += [{"in": lit, "prog": "short", "out": line} for lit, line in zip(extra, lines)]
    write("numbers.json", cases)


def gen_dtoa():
    rng = random.Random(42)
    xs = []

    def add(x):
        if x == x and x not in (float("inf"), float("-inf")):
            xs.append(x)

    for _ in range(1000):
        bits = rng.getrandbits(64)
        add(struct.unpack("<d", struct.pack("<Q", bits))[0])
    for _ in range(600):
        m = rng.randint(1, 10 ** rng.randint(1, 17))
        e = rng.randint(-30, 30)
        add(float(f"{m}e{e}") * rng.choice([1, -1]))
    for e in range(-330, 309):
        add(float(f"1e{e}"))
        add(float(f"1.5e{e}"))
        add(float(f"9.999999999999999e{e}"))
    for e in range(-1074, 1024, 7):
        add(2.0 ** e)
        add(-(2.0 ** e) * 3)
    for base in (2 ** 53, 10 ** 15, 10 ** 16, 10 ** 17, 10 ** 21, 10 ** 22, 2 ** 63, 2 ** 64):
        for d in range(-12, 13):
            add(float(base + d))
    for _ in range(120):
        add(struct.unpack("<d", struct.pack("<Q", rng.getrandbits(52)))[0])  # subnormals
    for x in (0.0, -0.0, 1e-4, 9.99e-5, 1.5e17, 1e16, 1e-5, 123456789.123, 5e-324, 2.2250738585072014e-308,
              1.7976931348623157e308, 0.1, 0.2, 0.30000000000000004, 1 / 3, 2 / 3, 100.0, 1e21, 1e22):
        add(x)
        add(-x)
    inputs = [repr(x) for x in xs]
    lines = batch_lines(". * 1", inputs)
    write("dtoa.json", [[i, o] for i, o in zip(inputs, lines)])


COMPARE_PAIRS = [
    ("1", "1.0"), ("1", "1.00001"), ("-0", "0"), ("0e5", "0.000"), ("100000000000000000001", "100000000000000000000"),
    ("-100000000000000000001", "-100000000000000000000"), ("1e1000", "1e999"), ("1e1000", "1e1000000000"),
    ("-1e1000", "-1e1001"), ("1e-1000", "0"), ("-1e-1000", "0"), ("0.5", "5e-1"), ("12", "9"), ("12", "100"),
    ("0.1", "0.10000000000000000555"), ("1.5", "1.49999999999999999999"), ("123e-2", "1.23"),
    ("99999999999999999999", "1e20"), ("nan", "nan"), ("nan", "1"), ("1", "nan"), ("-1", "1"),
    ("9007199254740993", "9007199254740992"), ("1e2", "100"), ("2e-1000000000", "1e-1000000000"),
]


def gen_compare():
    inputs = [f"[{a},{b}]" for a, b in COMPARE_PAIRS]
    lines = batch_lines("[.[0] < .[1], .[0] == .[1], .[0] > .[1], .[0] < (.[1] * 1), .[0] == (.[1] * 1)]",
                        inputs)
    write("compare.json", [{"a": a, "b": b, "out": o} for (a, b), o in zip(COMPARE_PAIRS, lines)])


# ------------------------------------------------------------------- parse

FROMJSON_CASES = [
    "1", "01", "-01", "00", "1.", ".5", "-.5", "+1", "1e", "1e+", "1e5", "1E5", "1e05", "-", "--1",
    "nan", "NaN", "-nan", "-NaN", "nan1", "NaN0", "NaN00", "sNaN", "nAn", "NaN12", "infinity", "-infinity",
    "Infinity", "inf", "-Inf", "INF", "infinit", "Infinityx", "true", "tru", "truex", "false", "null",
    "nul", "nulll", "nan ", "  1  ", "", " ", "[", "]", "{", "}", "[1,]", "[,1]", "[1 2]", "{\"a\"}",
    "{\"a\":}", "{\"a\":1,}", "{,}", "{1:2}", "{\"a\" 1}", "[1:2]", "1 2", "1,2", "{\"a\":1 \"b\":2}",
    "[1,2", "{\"a\":1", "\"abc", "\"a\\\"", "\"\\u12\"", "\"\\u12G4\"", "\"\\ud800\"", "\"\\ud800x\"",
    "\"\\ud800\\u0041\"", "\"\\ud800\\uZZZZ\"", "\"\\udc00\"", "\"\\ud83d\\ude00\"", "\"\\x\"", "\"\\a\"",
    "\"a\tb\"", "\"a\nb\"", "'a'", "True", "t", "f", "n", "nu", "x", "[nan]", "[-nan]", "{\"a\":nan}",
    "1.2.3", "1e1000", "-1e1000", "1ee5", "0x10", "1_000", "[1]]", "[1]}", "{}}", "{]", "[}", "\"\\/\"",
    "\"\\u0000\"", "\"\\uD834\\uDD1E\"", "[1,2]  3", "   [1]", "\u00a0 1", "1\u00a0", "[\"a\" , \"b\"]",
    "\ufeff1", "\ufeff", "{\"a\":1,\"a\":2}", "{\"b\":1,\"a\":2,\"b\":3}", "[[[]]]", "[{}]", "{\"\":{}}",
    "1\n2", "\n\n[1,\n2,\n", "[\n1\n2]", "{\"a\":1,\n\"b\"\n:\n}", "\"\\uDFFF\"", "\"\\uDBFF\\uDFFF\"",
    "[1e2, 1.0, 1.00, -0, 0.0, -0.0]", "1.0e1", "12345678901234567890", "0.1e-5", "0.0000001",
    "tr\u00fcue", "\"\u0001\"", "\"\u001f\"", "\"\u007f\"", "[1,\"\\q\"]", ":", ",", "[:", "{:",
    "{\"a\",", "[\"a\":1]", "nan nan", "truefalse", "true false", "[true false]", "{\"a\":true false}",
    "null1", "1null", "1true", "\"a\"\"b\"", "\"a\"1", "1\"a\"", "[1]1", "1[1]", "{}1", "[]{}", "-0",
    "-0.0e5", "[1,\u0000", "1\u00002", "\"a\u0000", "\u0000", "nu\u0000ll", "1 \u0000", "\u001e1",
    "\f1", "\u000b1", "[1,2,3]", "{\"a\":[1,{\"b\":2}]}", "\"\\u00e9\\u00E9\"", "\"\\uFFFF\"",
    "[\"\\ud800\\udc00\",\"\\udbff\\udfff\"]", "\"\\\\\"", "\"\\\"\"", "\"\\b\\f\\n\\r\\t\"",
    "{\"a\":1}{", "[1][", "\"x\" \"", "1 [", "[1,[2,[3,[4]]]]", "{\"a\":{\"b\":{\"c\":{}}}}",
    "\"\\u00\"", "\"\\u", "\"\\", "\"\\u123", "\"\\ud800\\u", "\"\\ud800\\udc0", "[01,02]", "[.5,5.]",
    "{\"a\":.5}", "-", "[-]", "[+]", "[e]", "[.]",
]


def gen_fromjson():
    inputs = [json.dumps(s) for s in FROMJSON_CASES]
    lines = batch_lines("try (fromjson | [0, .]) catch [1, .]", inputs)
    write("fromjson.json", [{"in": s, "out": o} for s, o in zip(FROMJSON_CASES, lines)])


def nest(n, open_, close_, inner=b""):
    return open_ * n + inner + close_ * n


PARSE_DOCS = [
    b"1 2 3", b"1\n2\n", b"[1] [2]", b"{\"a\":1}{\"b\":2}", b"\"a\"\"b\"", b"1\"a\"", b"true false",
    b"nullnull", b"[1,2", b"1 2 [3", b"1]", b"1,", b"[1,2]]", b"{\"a\":1}}", b"  ", b"", b"\n\n\n",
    b"1 2 \n 3 4 x", b"[1,\n2,\n3", b"\"abc", b"\"abc\ndef\"", b"\"abc\\", b"[\"a\",\n\"b\"\n,]", b"1e1000",
    b"-0", b"[nan,-nan,infinity,-infinity]", b"[1.000, 1E2, 0.0000001, 100e-2]",
    b"{\"b\":1,\"a\":2,\"b\":3}", b"{\"a\":1,\"a\":{\"x\":1},\"a\":[]}",
    b"\xef\xbb\xbf1", b"\xef\xbb\xbf\xef\xbb\xbf1", b"\xef\xbb1", b"\xef1", b"\xef", b"\xef\xbb",
    b" \xef\xbb\xbf1", b"1\x002", b"\"a\x00b\"", b"nu\x00ll", b"\x001", b"1\x00", b"\x1e1", b"1\x1e",
    b"\x0c1", b"\x0b1", b"\r\n1\r\n", b"\t1\t", b"[1,\t2]",
    b"\"\xe2a\"", b"\"\xe2ab\"", b"\"\xc0\x80\"", b"\"\xe0\x80\x80\"", b"\"\xed\xa0\x80\"",
    b"\"\xf4\x90\x80\x80\"", b"\"\xf5\x80\"", b"\"a\xffb\"", b"\"\x80\x80\"", b"\"\xe2\x82\"",
    b"\"\xf0\x9f\x98\"", b"\"\xf0\x9f\x98A\"", b"\"\xe2(\xa1\"", b"\"\xc3(\"", b"\"\xf8\x88\x80\x80\x80\"",
    b"\"\xed\xb0\x80\x80\"", b"\"\xe0\xa0\"", b"\"\xf0\x80\x80\x80\"", b"\"\xe0\\udc00\"", b"\"\\udc00\x80\"",
    b"\"\xe0\\u0080\"", b"\"\xe0\\u00a0\"", b"\"\xed\\udc00\"", b"[\"\xff\",\"\xfe\"]",
    b"{\"\xff\":1}", b"\"\xe2\x82\xac\"", b"x\xff", b"\xff", b"[\xff]", b"tru\xff",
    nest(10000, b"[", b"]"), nest(10001, b"[", b"]"), nest(5000, b"{\"a\":", b"}", b"1"),
    nest(5001, b"{\"a\":", b"}", b"1"), nest(4999, b"[{\"a\":", b"}]", b"1"),
    b"[" * 10001, b"[1," * 10001,
]

SEQ_DOCS = [
    b"1 2\n", b"\x1e1\n", b"\x1e1\x1e", b"\x1e1\x1e\x1e2", b"\x1e1 \x1e[1,\x1e\"ab\x1e3\n", b"\x1e[}2 3\x1e4\x1e",
    b"\x1e[1,2]\x1e{\"a\":1}\n", b"\x1e\"abc\"\x1e", b"\x1e\"abc\x1e", b"\x1e\"\x1e1\n", b"\x1etrue\x1e",
    b"\x1etrue", b"\x1enull \x1e", b"\x1e1", b"\x1e1\n\x1e2\n", b"\x1e\x1e\x1e", b"", b"\x1e", b"abc",
    b"\x1e{\"a\":\x1e1\n", b"\x1e[1,\n2\x1e3 ", b"\x1e]\x1e1 ", b"\xef\xbb\x1e1 ", b"\xef\xbb\xbf\x1e1 ",
    b"\x1e1 2 3\n", b"\x1e[1] [2]\n", b"\x1e1]\x1e2\n", b"\x1enu\x1e2\n",
]

STREAM_DOCS = [
    b"[1,[2,3],{\"a\":[]}]", b"{\"a\":{\"b\":1},\"c\":[]}", b"3", b"\"x\"", b"[]", b"{}", b"[[]]", b"[{}]",
    b"{\"a\":[{}]}", b"1 [2] {\"a\":3}", b"[1,2", b"{\"a\":1", b"[1,}", b"{\"a\" 1}", b"{\"a\":}", b"{[\"a\"]}",
    b"{\"a\":1,[\"b\"]}", b"{\"a\":1,{\"b\":2}}", b"{{\"a\":1}}", b"[1,]", b"{\"a\":1,}", b"]", b"}", b"[1]]",
    b"{\"a\":1}}", b"[1:2]", b":", b",", b"[,1]", b"{,}", b"{1:2}", b"{\"a\"}", b"{\"a\":1 \"b\":2}",
    b"[1 2]", b"1 2", b"[1,[2,[3]]]", b"[[1],[2]]", b"{\"a\":{\"b\":{\"c\":1}}}", b"[nan, 1.000, 1e2]",
    b"[\"a\",{\"b\":[1,2,{\"c\":\"d\"}]}]", b"[1,{\"a\":2},3]", b"{\"a\":[1,2],\"b\":{}}", b"[{\"a\":1},{\"b\":2}]",
    b"[" * 10001, b"[\"abc", b"[1e1000]", b"{\"a\":1}{\"b\":[]}", b"[true,false,null]",
    b"[]1", b"{}[]", b"1]", b"[1]2",
]


def gen_parse_cli():
    cases = []

    def add(doc, flags):
        out, err, rc = run(["-c"] + flags + ["."], doc)
        c = {"flags": flags}
        enc(c, "in", doc)
        enc(c, "out", out)
        enc(c, "err", err)
        c["rc"] = rc
        cases.append(c)

    for d in PARSE_DOCS:
        add(d, [])
    for d in SEQ_DOCS:
        add(d, ["--seq"])
    for d in STREAM_DOCS:
        add(d, ["--stream"])
    for d in STREAM_DOCS[:30]:
        add(d, ["--stream-errors"])
    for d in SEQ_DOCS[:12] + [b"\x1e[1,[2]]\x1e{\"a\":\x1e3\n", b"\x1e[1,2\x1e"]:
        add(d, ["--seq", "--stream"])
    write("parse_cli.json", cases)


# ------------------------------------------------------------------- print

PRINT_DOCS = [
    b'{"a":[1,{"b":null},[],{}],"c":"x","d":true,"e":false,"f":1.5,"g":[nan]}',
    b'[1,[2,[3,[]]],{"z":1,"a":{"y":2,"b":3}}]',
    b'"\\u0000\\u0001\\u001f\\u007f\\u0080\\u00ff\\u2028\\u2029\\ud83d\\ude00\\" \\\\ / \\b\\f\\n\\r\\t"',
    b'{"\\u00e9":"\\u00e9","\\ud83d\\ude00":["\\u0007"],"b":{"\\n":1}}',
    b'[1E2, 1.000, 0.0000001, -0, 100000000000000000000001, 1e1000, 3.0]',
    b'[]', b'{}', b'null', b'"x"', b'[[],[{}],{"a":[]}]', b'{"b":2,"a":1,"c":{"z":0,"y":[{"d":1,"c":2}]}}',
    b'[{"a":[{"b":[]}]}]',
]

# Nesting around MAX_PRINT_DEPTH (256): only printed with a few flag sets.
PRINT_DEEP_DOCS = [
    b'[' * 300 + b'1' + b']' * 300,
    b'[' * 257 + b']' * 257,
    b'[' * 258 + b']' * 258,
    b'{"a":' * 257 + b'1' + b'}' * 257,
    b'[' * 257 + b'{"a":1}' + b']' * 257,
]
PRINT_DEEP_FLAGS = [["-c"], ["-C", "-c"], ["--indent", "0"]]

PRINT_FLAGS = [
    ["-c"], [], ["--indent", "0"], ["--indent", "1"], ["--indent", "7"], ["--indent", "-1"], ["--tab"],
    ["-S"], ["-S", "-c"], ["-a"], ["-a", "-c"], ["-C"], ["-C", "-c"], ["-C", "-S"], ["-C", "--tab"],
    ["-C", "-a", "-c"], ["--tab", "-c"], ["-c", "--indent", "3"], ["--indent", "3", "--tab"],
]

COLOR_ENVS = [
    "", "0;31", "0;31:0;32:0;33:0;34:0;35:0;36:0;37:0;38", "1:2:3:4:5:6:7:8:9", "1:2:3:4:5:6:7:8x",
    "garbage", "0;31:", "::", "1;31;4", "0;31:x", ":0;31", "4;31:1;32:::::1;30",
]


def gen_print():
    cases = []
    combos = [(d, f) for d in PRINT_DOCS for f in PRINT_FLAGS]
    combos += [(d, f) for d in PRINT_DEEP_DOCS for f in PRINT_DEEP_FLAGS]
    for doc, flags in combos:
        if True:
            out, err, rc = run(flags + ["."], doc)
            c = {"flags": flags}
            enc(c, "in", doc)
            enc(c, "out", out)
            enc(c, "err", err)
            cases.append(c)
    for env in COLOR_ENVS:
        for doc in PRINT_DOCS[:2]:
            out, err, rc = run(["-C", "."], doc, {"JQ_COLORS": env})
            c = {"flags": ["-C"], "env": env}
            enc(c, "in", doc)
            enc(c, "out", out)
            enc(c, "err", err)
            cases.append(c)
    write("print.json", cases)


# --------------------------------------------------------------------- ops

OPS = {
    "get": "$a | .[$b]",
    "set": "$a | setpath([$b]; $c)",
    "has": "$a | has($b)",
    "getpath": "$a | getpath($b)",
    "setpath": "$a | setpath($b; $c)",
    "delpaths": "$a | delpaths($b)",
    "keys": "$a | keys",
    "keys_unsorted": "$a | keys_unsorted",
    "sort": "$a | sort",
    "sort_by_impl": "$a | _sort_by_impl($b)",
    "group_by_impl": "$a | _group_by_impl($b)",
    "unique": "$a | unique",
    "unique_by_impl": "$a | _unique_by_impl($b)",
    "contains": "$a | contains($b)",
    "cmp": "[$a < $b, $a == $b, $a > $b, $a <= $b, $a >= $b]",
    "tojson": "$a | tojson",
    "tostring": "$a | tostring",
    "negate": "$a | -.",
    "path": "null | path($a)",
    "split": "$a | split($b)",
    "explode": "$a | explode",
    "implode": "$a | implode",
    "strindices": "$a | _strindices($b)",
    "repeat": "$a * $b",
    "plus": "$a + $b",
    "multiply": "$a * $b",
    "length": "$a | length",
}

VALUES = [
    "null", "true", "false", "0", "1", "-1", "1.5", "1e2", "\"\"", "\"a\"", "\"abc\"", "[]", "[1]", "[1,2,3]",
    "{}", "{\"a\":1}", "{\"a\":1,\"b\":2}", "\"\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\"",
    "\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"", "[\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"]",
    "{\"aaaaaaaaaaaa\":\"bbbbbbbbbbbbbbbbbb\"}", "\"\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\"",
    "\"a\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\u00e9\"",
    "\"\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\"",
    "\"ab\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\u20ac\"",
    "\"\U0001F600\U0001F600\U0001F600\U0001F600\U0001F600\U0001F600\U0001F600\U0001F600\"",
    "123456789012345678901234567890", "1.000", "nan", "[nan]", "\"\\u0000\\u0001\\n\"",
    "\"123456789012\"", "\"1234567890123\"", "\"12345678901234567890123456\"", "\"123456789012345678901234567\"",
]

GET_CASES = [
    ("{\"a\":1}", "\"a\""), ("{\"a\":1}", "\"b\""), ("{}", "0"), ("[1,2,3]", "1"), ("[1,2,3]", "1.5"),
    ("[1,2,3]", "-1"), ("[1,2,3]", "-1.5"), ("[1,2,3]", "-0.5"), ("[1,2,3]", "-4"), ("[1,2,3]", "3"),
    ("[1,2,3]", "nan"), ("[1,2,3]", "1e300"), ("[1,2,3]", "-1e300"), ("[1,2,3]", "1e1000"),
    ("null", "\"a\""), ("null", "0"), ("null", "{\"start\":1,\"end\":2}"), ("null", "true"), ("null", "[]"),
    ("true", "null"), ("1", "\"a\""), ("1", "\"abcdefghijklmnopqrstuvwxyz123\""),
    ("1", "\"abcdefghijklmnopqrstuvwxyz1234\""), ("1", "\"a\\u0000bc\""), ("\"abc\"", "0"),
    ("[1]", "[]"), ("[1,2,1,2]", "[1,2]"), ("[1,2,1,2,1]", "[1,2,1]"), ("[1,2,3]", "[2,3,4]"),
    ("[0,1,2,3,4,5]", "{\"start\":1,\"end\":3}"), ("[0,1,2,3,4,5]", "{\"start\":null,\"end\":3}"),
    ("[0,1,2,3,4,5]", "{\"start\":-2,\"end\":null}"), ("[0,1,2,3,4,5]", "{\"start\":1.2,\"end\":3.7}"),
    ("[0,1,2,3,4,5]", "{\"start\":-1.5,\"end\":-0.5}"), ("[0,1,2,3,4,5]", "{\"start\":nan,\"end\":nan}"),
    ("[0,1,2,3,4,5]", "{\"start\":4,\"end\":2}"), ("[0,1,2,3,4,5]", "{\"start\":-100,\"end\":100}"),
    ("[0,1,2,3,4,5]", "{\"start\":1e300,\"end\":-1e300}"), ("[0,1,2,3,4,5]", "{\"start\":\"a\",\"end\":1}"),
    ("[0,1,2,3,4,5]", "{\"end\":2}"), ("[0,1,2,3,4,5]", "{\"start\":1}"), ("[0,1,2,3,4,5]", "{}"),
    ("\"a\u00e9\u20acd\"", "{\"start\":1,\"end\":3}"), ("\"abcdef\"", "{\"start\":-2,\"end\":null}"),
    ("\"abcdef\"", "{\"start\":0.5,\"end\":2.5}"), ("\"\"", "{\"start\":0,\"end\":1}"),
    ("{\"a\":1}", "{\"start\":0,\"end\":1}"), ("1", "{\"start\":0,\"end\":1}"), ("[1]", "true"),
    ("{\"a\":1}", "null"), ("[1]", "null"), ("[1]", "\"a\""), ("{\"a\":{\"b\":1}}", "\"a\""),
    ("\"abc\"", "\"a\""), ("[1,[2]]", "[[2]]"), ("[1,2]", "[1,2,3]"),
]

SET_CASES = [
    ("null", "\"a\"", "1"), ("null", "0", "1"), ("null", "2", "1"), ("null", "-1", "1"),
    ("[1,2]", "-1", "9"), ("[1,2]", "-3", "9"), ("[1,2]", "5", "9"), ("[1,2]", "1.7", "9"),
    ("[1,2]", "20", "9"), ("[1,2]", "536870912", "9"), ("[1,2]", "1e10", "9"), ("[1,2]", "-1e10", "9"),
    ("[1,2]", "nan", "9"), ("{\"a\":1}", "\"a\"", "2"), ("{\"a\":1}", "\"b\"", "2"), ("{\"a\":1}", "0", "2"),
    ("[1,2]", "\"a\"", "2"), ("1", "\"a\"", "2"), ("true", "0", "1"), ("\"abc\"", "0", "1"),
    ("\"abc\"", "{\"start\":0,\"end\":1}", "\"x\""), ("[0,1,2,3,4,5]", "{\"start\":1,\"end\":3}", "[\"x\"]"),
    ("[0,1,2,3,4,5]", "{\"start\":1,\"end\":3}", "[\"x\",\"y\",\"z\",\"w\"]"),
    ("[0,1,2,3,4,5]", "{\"start\":1,\"end\":3}", "[]"), ("[0,1,2,3,4,5]", "{\"start\":1,\"end\":3}", "\"x\""),
    ("[0,1,2]", "{\"start\":5,\"end\":7}", "[\"x\"]"), ("null", "{\"start\":0,\"end\":1}", "[1,2]"),
    ("[0,1,2]", "{\"start\":-1,\"end\":null}", "[9,9]"), ("[0,1,2]", "{\"start\":\"a\",\"end\":null}", "[9]"),
    ("{\"a\":1}", "{\"start\":0,\"end\":1}", "[1]"), ("null", "null", "1"), ("[1]", "[0]", "1"),
    ("{\"b\":1,\"a\":2}", "\"b\"", "3"),
]

PATHS_CASES = [
    ("null", "[\"a\",0,\"b\"]", "1"), ("{\"a\":1}", "[\"a\",\"b\"]", "1"), ("{\"a\":{\"b\":1}}", "[\"a\",\"b\"]", "2"),
    ("[1,[2,3]]", "[1,0]", "9"), ("[1,[2,3]]", "[1,5]", "9"), ("[1,[2,3]]", "[-1,-1]", "9"), ("{}", "[]", "5"),
    ("1", "[]", "5"), ("1", "\"a\"", "5"), ("[1,[2,3]]", "[1,{\"start\":0,\"end\":1}]", "[\"x\"]"),
    ("[1,[2,3]]", "[{\"start\":1,\"end\":null},0]", "[\"x\"]"), ("{\"a\":[1,2,3]}", "[\"a\",{\"start\":1,\"end\":2},0]", "9"),
    ("[1,2]", "[-5]", "9"), ("[1,2]", "[\"a\"]", "9"), ("null", "[{\"start\":1,\"end\":2}]", "[1]"),
    ("{\"a\":1}", "[\"a\",0]", "1"), ("null", "[null]", "1"), ("[[1]]", "[0,0,0]", "1"),
]

DELPATHS_CASES = [
    ("{\"a\":1,\"b\":2,\"c\":3}", "[[\"b\"]]"), ("{\"a\":1,\"b\":2,\"c\":3}", "[[\"b\"],[\"a\"]]"),
    ("{\"a\":{\"x\":1,\"y\":2},\"b\":2}", "[[\"a\",\"x\"],[\"b\"]]"), ("[0,1,2,3,4,5]", "[[1],[3]]"),
    ("[0,1,2,3,4,5]", "[[-1],[0]]"), ("[0,1,2,3,4,5]", "[[{\"start\":1,\"end\":3}]]"),
    ("[0,1,2,3,4,5]", "[[{\"start\":1,\"end\":3}],[5]]"), ("[0,1,2,3,4,5]", "[[1.5],[2.9]]"),
    ("[0,1,2,3,4,5]", "[[-1.5]]"), ("[0,1,2,3,4,5]", "[[10],[-10]]"),
    ("[0,1,2,3,4,5]", "[[\"a\"]]"), ("{\"a\":1}", "[[0]]"), ("1", "[[0]]"), ("1", "[[\"a\"]]"),
    ("null", "[[\"a\"]]"), ("{\"a\":1}", "[[]]"), ("{\"a\":1}", "[]"), ("{\"a\":1}", "[1]"), ("{\"a\":1}", "1"),
    ("{\"a\":[1,2,3]}", "[[\"a\",0],[\"a\",2],[\"a\"]]"), ("{\"a\":[1,2,3]}", "[[\"a\",0],[\"a\",2]]"),
    ("[[1,2],[3,4]]", "[[0,0],[1,1],[1,0]]"), ("{\"a\":null}", "[[\"a\",\"b\"]]"), ("{\"a\":1}", "[[\"a\",\"b\"]]"),
    ("{\"b\":1,\"a\":2,\"c\":3}", "[[\"a\"]]"), ("[0,1,2,3,4,5]", "[[-2],[1],[{\"start\":-1,\"end\":null}]]"),
    ("[0,1,2]", "[[{\"start\":\"x\",\"end\":1}]]"), ("[[0,1,2]]", "[[0,{\"start\":0,\"end\":2}]]"),
    ("{\"a\":{\"b\":{\"c\":1,\"d\":2}}}", "[[\"a\",\"b\",\"c\"]]"),
]


def gen_ops():
    cases = []

    def run_op(op, arg_lists):
        prog = ". as [$a, $b, $c] | try (" + OPS[op] + " | [0, .]) catch [1, .]"
        inputs = ["[" + ",".join(args + ["null"] * (3 - len(args))) + "]" for args in arg_lists]
        for args, line in zip(arg_lists, batch_lines(prog, inputs)):
            cases.append({"op": op, "args": args, "out": line})

    run_op("get", [list(c) for c in GET_CASES])
    run_op("set", [list(c) for c in SET_CASES])
    run_op("has", [[t, k] for t in ("null", "{\"a\":1}", "[1,2]", "1", "\"a\"", "true")
                   for k in ("\"a\"", "0", "1", "2", "-1", "0.9", "-0.5", "1.9", "nan", "1e10", "null", "[]", "{}")])
    run_op("getpath", [[a, p] for a, p, _ in PATHS_CASES])
    run_op("setpath", [list(c) for c in PATHS_CASES])
    run_op("delpaths", [list(c) for c in DELPATHS_CASES])
    for op in ("keys", "keys_unsorted", "sort", "unique", "tojson", "tostring", "negate", "path", "explode",
               "implode", "length"):
        run_op(op, [[v] for v in VALUES + ["{\"b\":1,\"a\":2,\"\u00e9\":3,\"A\":4,\"\":5}",
                                           "[3,1,null,\"a\",[],{},true,false,nan,-1,[1],{\"a\":1},\"B\"]",
                                           "[65,233,128512,55296,1114112,-1,65.9]", "[\"a\"]", "[1,\"a\"]",
                                           "[{\"b\":1,\"a\":2},{\"a\":2,\"b\":1},{\"a\":1},{\"c\":0}]",
                                           "[[1,2],[1],[1,1],[0,5],[]]", "[1e2, 100, 1.0, 1, 1.00, 0.1e1]"]])
    sortables = [
        ("[3,1,2]", "[[3],[1],[2]]"), ("[\"a\",\"b\",\"c\",\"d\"]", "[[1],[0],[1],[0]]"),
        ("[1,2,3,4]", "[[nan],[nan],[0],[nan]]"), ("[1,2,3,4]", "[[null],[false],[true],[0]]"),
        ("[1,2,3]", "[[{\"a\":1}],[{\"a\":0}],[{\"a\":1}]]"), ("[]", "[]"),
        ("[1,2,3,4,5]", "[[1e2],[100],[1],[1.0],[100.0]]"),
    ]
    for op in ("sort_by_impl", "group_by_impl", "unique_by_impl"):
        run_op(op, [list(c) for c in sortables])
    contains_cases = [
        ("\"foobar\"", "\"bar\""), ("\"foobar\"", "\"\""), ("\"foo\"", "\"foobar\""), ("[1,2,3]", "[1]"),
        ("[1,2,3]", "[]"), ("[1,[2,3]]", "[[2]]"), ("[\"foobar\"]", "[\"bar\"]"), ("{\"a\":1,\"b\":2}", "{\"a\":1}"),
        ("{\"a\":1}", "{\"a\":1,\"b\":2}"), ("{\"a\":{\"b\":\"xyz\"}}", "{\"a\":{\"b\":\"y\"}}"), ("1", "1"),
        ("1", "1.0"), ("true", "true"), ("true", "false"), ("null", "null"), ("1", "\"a\""),
        ("{\"a\":1}", "[1]"), ("nan", "nan"), ("[nan]", "[nan]"), ("\"a\\u0000b\"", "\"\\u0000\""),
        ("{\"a\":null}", "{\"b\":null}"), ("{\"aaaaaaaaaaaaaaaaaa\":1}", "[\"bbbbbbbbbbbbbbbbbbbbbbbb\"]"),
    ]
    run_op("contains", [list(c) for c in contains_cases])
    cmp_vals = ["null", "false", "true", "0", "-1", "1.5", "nan", "\"\"", "\"a\"", "\"ab\"", "\"b\"", "[]",
                "[1]", "[1,2]", "[2]", "{}", "{\"a\":1}", "{\"b\":0}", "{\"a\":2}", "{\"a\":1,\"b\":1}",
                "\"\u00e9\"", "\"z\""]
    run_op("cmp", [[a, b] for a in cmp_vals for b in cmp_vals])
    split_cases = [("\"a,b,\"", "\",\""), ("\"\"", "\",\""), ("\",\"", "\",\""), ("\"a,,b\"", "\",\""),
                   ("\"abc\"", "\"\""), ("\"a\u00e9b\"", "\"\""), ("\"abc\"", "\"abc\""), ("\"xaax\"", "\"aa\""),
                   ("\"aaa\"", "\"aa\""), ("1", "\",\""), ("\"a\"", "1")]
    run_op("split", [list(c) for c in split_cases])
    run_op("strindices", [list(c) for c in [("\"a,b, cd, efg\"", "\", \""), ("\"aaaa\"", "\"aa\""),
                                              ("\"\u00e9a\u00e9a\"", "\"a\""), ("\"abc\"", "\"\""),
                                              ("\"\U0001F600x\U0001F600x\"", "\"x\"")]])
    run_op("repeat", [[s, n] for s in ("\"ab\"", "\"\"") for n in ("-1", "0", "0.5", "1", "2.9", "3", "nan",
                                                                   "1e10", "1073741824", "-0.5")])
    run_op("plus", [list(c) for c in [("{\"a\":1,\"b\":2}", "{\"b\":3,\"c\":4}"), ("[1,2]", "[3]"),
                                        ("\"ab\"", "\"cd\""), ("null", "1"), ("1", "null"),
                                        ("{\"a\":1}", "{}"), ("[]", "[]")]])
    run_op("multiply", [list(c) for c in [("{\"a\":{\"b\":1,\"c\":2},\"d\":1}", "{\"a\":{\"b\":3,\"e\":4},\"d\":{\"x\":1}}"),
                                            ("{\"a\":1}", "{\"a\":{\"b\":1}}"), ("{\"a\":{\"b\":1}}", "{\"a\":1}"),
                                            ("{\"b\":1,\"a\":2}", "{\"c\":{},\"a\":3}")]])
    write("ops.json", cases)


def main():
    ver = run(["--version"])[0].decode().strip()
    if ver != "jq-1.8.1":
        raise SystemExit(f"expected jq-1.8.1, got {ver!r}")
    gen_numbers()
    gen_dtoa()
    gen_compare()
    gen_fromjson()
    gen_parse_cli()
    gen_print()
    gen_ops()


if __name__ == "__main__":
    main()
