#!/usr/bin/env python3
"""Regenerate cases.jsonl: expectations for the platform-backed C builtins.

Every expectation comes from running the jq 1.8.1 binary:

    jq -nc --argjson in <input> --argjson a0 <arg0> ... \
        'try ($in | <builtin>($a0; ...) | {ok: tojson}) catch {err: .}'

Usage: python3 src/jq/builtins/platform/gen_cases.py [path/to/jq]

Each output row is one JSON object (parsed by qj's own JSON parser in the tests, so
inputs like `nan` and `1E+2` keep their jq meaning):

- `f`: the builtin (`name` as in `function_list`), `input`, `args`;
- the result: `ok` (the output as `tojson` prints it), `err` (the error message), or
  `abort` (jq died with SIGABRT; the assertion line it printed);
- `tz` (optional): run with this `TZ` and `LC_ALL=C` in a child process. Rows without
  it must not depend on the time zone or locale; this script checks that by running them
  under several zones and locales;
- `only` (optional): `macos`, `aarch64` or `macos-aarch64`, for results that depend on
  macOS libc/libm or on how the CPU converts NaN/out-of-range doubles to integers.
"""

import json
import os
import subprocess
import sys

JQ = sys.argv[1] if len(sys.argv) > 1 else "jq"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "cases.jsonl")

# (TZ, LC_ALL) combinations that rows without `tz` must agree under.
INDEPENDENT_ENVS = [
    ("UTC", "C"),
    ("Asia/Tokyo", "de_DE.UTF-8"),
    ("America/New_York", "ja_JP.UTF-8"),
]

ISO = '"%Y-%m-%dT%H:%M:%SZ"'
LONG_OBJ = '{"a":"verylongstringhere"}'

# (builtin, input, [args], options)
CASES = []


def case(f, input, *args, **opts):
    CASES.append((f, input, list(args), opts))


# ---------------------------------------------------------------------------------
# _match_impl(re; modifiers; testmode)
# ---------------------------------------------------------------------------------
m = "_match_impl"
case(m, '"foo bar"', '"(?<x>o+)"', '"g"', "false")
case(m, '"foo bar"', '"(?<x>o+)"', '"g"', "true")
case(m, '"foo bar"', '"z"', '"g"', "true")
case(m, '"foo bar"', '"z"', "null", "false")
# testmode is test mode only if it equals true.
for t in ["1", '"true"', "null", "[true]", "{}", "0"]:
    case(m, '"foo bar"', '"(?<x>o+)"', '"g"', t)
# Key orders: an unmatched group and an empty capture use "offset, string, length".
case(m, '"foo bar"', '"(?<x>o+)|(z)"', '"g"', "false")
case(m, '"abc"', '"(a)(x?)(b)"', "null", "false")
case(m, '"foo"', '"(?=(o+))"', '"g"', "false")
case(m, '"xyzzy-14"', '"(?<x>[a-z]+)-(?<n>[0-9]+)"', "null", "false")
case(m, '"aéb日c"', '"(é)(b)(日)"', "null", "false")
case(m, '"éé"', '""', '"g"', "false")
case(m, '""', '""', '"g"', "false")
case(m, '"a\\u0000b"', '"a.b"', "null", "false")
case(m, '"abc"', '"B"', '"gi"', "false")
case(m, '"test"', '"t"', '""', "false")
# Input type errors (checked first).
for bad in ["1", "null", "true", "false", "[1,2]", LONG_OBJ, "1E+2", "nan", "1.000",
            '["ééééééééé"]', "[1,2,3,4,5,6,7,8,9]", '{"é":"😀😀😀😀"}']:
    case(m, bad, '"a"', "null", "false")
# Regex type errors.
for bad in ["1", "null", '["x"]', '{"a":1}', "true", LONG_OBJ]:
    case(m, '"a"', bad, "null", "false")
# Modifier type errors (a string or null is required).
for bad in ["1", "true", "false", "[]", "{}", '["g"]', "[1,2,3,4,5,6,7,8,9]"]:
    case(m, '"a"', '"a"', bad, "false")
    case(m, '"a"', '"a"', bad, "true")
# The order of the checks.
case(m, "1", "2", "3", "false")
case(m, '"a"', "2", "3", "false")
case(m, "1", '"a"', "3", "false")
case(m, '"a"', '"("', "3", "false")
case(m, '"a"', '"("', '"q"', "false")
case(m, '"a"', '"("', '"q"', "true")
case(m, "null", '"("', '"q"', "true")
# Invalid modifier strings name the whole string.
for mods in ['"q"', '"gq"', '"G"', '"é"', '"g\\u0000"', '"gixsnlpm "']:
    case(m, '"a"', '"a"', mods, "false")
# Every valid modifier.
for mods in ["null", '"g"', '"i"', '"x"', '"n"', '"s"', '"m"', '"p"', '"l"', '"gixsnlpm"']:
    case(m, '"ab\\ncd AB"', '"b.c"', mods, "false")
case(m, '"ab\\ncd AB"', '"ab"', '"gi"', "false")
case(m, '"ab\\ncd AB"', '"a b"', '"x"', "false")
case(m, '"ab\\ncd AB"', '"^cd"', "null", "false")
case(m, '"ab\\ncd AB"', '"^cd"', '"s"', "false")
case(m, '"aaa"', '""', '"gn"', "false")
case(m, '"aaa"', '"a|aa|aaa"', '"gl"', "false")
case(m, '"aaa"', '"a|aa|aaa"', '"g"', "false")
# Oniguruma errors, at compile time and at search time (also in test mode).
case(m, '"a"', '"("', "null", "false")
case(m, '"a"', '"("', "null", "true")
case(m, '"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaab!"', '"(a|a)*b$"', "null", "false")
case(m, '"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaab!"', '"(a|a)*b$"', "null", "true")
case(m, '"x"', '"\\\\k<a\\u0000b>"', "null", "false")

# ---------------------------------------------------------------------------------
# Dates
# ---------------------------------------------------------------------------------
case("gmtime", "1425599507")
case("gmtime", "1425599507.5")
case("gmtime", "1425599507.822351")
case("gmtime", "0")
case("gmtime", "-1.5")
case("gmtime", "-0.25")
case("gmtime", "1E+2")
case("gmtime", "253402300799")
case("gmtime", "-62135596800")
for bad in ["1e30", "-1e30", "1e18", "1e1000", "-1e1000"]:
    case("gmtime", bad)
case("gmtime", "nan", only="aarch64")
case("gmtime", "67767976233316800", only="macos")
case("gmtime", "67768036191676799", only="macos")
for bad in ['"x"', "null", "[1]", "{}", "true", '"1425599507"']:
    case("gmtime", bad)

for bad in ['"x"', "null", "[1]", "{}", "false"]:
    case("localtime", bad)
for bad in ["1e30", "-1e30", "1e1000"]:
    case("localtime", bad)
for tz in ["UTC", "Asia/Tokyo", "America/New_York"]:
    for t in ["1425599507", "1425599507.25", "1436140307.75", "-1.5", "0"]:
        case("localtime", t, tz=tz)
case("localtime", "nan", tz="Asia/Tokyo", only="aarch64")

case("mktime", "[2015,2,5,23,51,47,4,63]")
case("mktime", "[2015,2,5,23,51,47]")
case("mktime", "[2024,8,21]")
case("mktime", "[2015,2,5]")
case("mktime", "[2015]")
case("mktime", '[2015,2,5,23,51,47,4,63,"rest"]')
case("mktime", '[2015,2,5,23,51,47,"x","y"]')
case("mktime", '[2015,2,5,23,51,47,0,0,{"ninth":"ignored"}]')
case("mktime", "[2015.9,2.9,5.9,23.9,51.9,47.9]")
case("mktime", "[2015,14,5,23,51,47]")
case("mktime", "[2015,-1,5,23,51,47]")
case("mktime", "[2015,2,5,23,51,1e10]")
case("mktime", "[2015,2,5,23,51,-1e10]")
case("mktime", "[2.015E+3,2,5,23,51,47]")
case("mktime", "[1970,0,1,0,0,0]")
case("mktime", "[1969,11,31,23,59,59]")
case("mktime", "[1969,11,31,23,59,58]")
case("mktime", "[1969,11,31,23,59,57]")
case("mktime", "[2037,1,11,1,2,3,3,41]")
case("mktime", "[10000,0,1]")
case("mktime", '["a",1,2,3,4,5,6,7]')
case("mktime", '[2015,"x"]')
case("mktime", "[2015,2,5,23,51,47,4,null]")
case("mktime", "[2015,2,5,23,51,nan]")
case("mktime", "[null]")
for bad in ['"x"', "1", "null", "{}", "true"]:
    case("mktime", bad)
case("mktime", "[]", only="macos")
case("mktime", "[1899,11,31]", only="macos")
case("mktime", "[1900,0,1]", only="macos")
case("mktime", "[1e30,2,5]", only="macos")
case("mktime", "[-1e30,2,5]", only="macos")

case("strftime", "1425599507", ISO)
case("strftime", "[2015,2,5,23,51,47,4,63]", ISO)
case("strftime", "[2024,2,15]", ISO)
case("strftime", "1425599507.9", '"%j %U %W %u %w %e %C %y %G %g %V %k %l %I %M %S %H %d %m %Y"')
case("strftime", "1425599507", '"%D %F %T %R %n %t %%"')
case("strftime", "[2015,2,5,23,51,47,0,0]", '"%u %w %j"')
case("strftime", "[2015,14,40,25,61,61]", '"%F %T %j %u"')
case("strftime", "[2015,2,5,23,51,47,4,63,\"rest\"]", ISO)
case("strftime", "1425599507", '""')
case("strftime", "0", '"abc\\u0000def"')
case("strftime", "1E+2", ISO)
case("strftime", "-1.5", ISO)
case("strftime", "[2015.9,2.9,5.9,23.9,51.9,47.9]", ISO)
case("strftime", "1425599507", '"%Z %z %s"', only="macos")
case("strftime", "1435677542.822351", '"%A, %B %d, %Y"', tz="UTC")
case("strftime", "1425599507", '"%c | %x | %X | %r | %p | %a %b %h"', tz="UTC")
case("strftime", "0", '"%c%c%c%c"', tz="UTC")
case("strftime", "0", '"%c%c%c%c%c"', tz="UTC")
# Errors: input kind, then format, then the elements.
for bad in ['"x"', "null", "{}", "true"]:
    case("strftime", bad, ISO)
    case("strftime", bad, "1")
for fmt in ["1", "[]", "{}", "null", "true"]:
    case("strftime", "0", fmt)
    case("strftime", "[2015,2,5]", fmt)
    case("strftime", '["a",1,2,3,4,5,6,7]', fmt)
case("strftime", '["a",1,2,3,4,5,6,7]', ISO)
case("strftime", "[2015,2,5,23,51,nan]", ISO)
case("strftime", "[2015,2,5,23,51,47,4,null]", ISO)
case("strftime", "1e30", ISO)
case("strftime", "1e30", "1")
case("strftime", "1e1000", ISO)
case("strftime", "nan", ISO, only="aarch64")

for tz in ["UTC", "Asia/Tokyo", "America/New_York"]:
    case("strflocaltime", "1425599507", '"%Y-%m-%dT%H:%M:%S %Z %z %s %j %u"', tz=tz)
    case("strflocaltime", "[2015,2,5,23,51,47]", '"%c %Z %z %s"', tz=tz)
    case("strflocaltime", "[2015,6,5,23,51,47]", '"%c %Z %z %s"', tz=tz)
    case("strflocaltime", "1425599507.25", ISO, tz=tz)
case("strflocaltime", "[2015,2,8,2,30,0]", '"%c %Z %z %s"', tz="America/New_York",
     only="macos")
case("strflocaltime", "0", '"%c%c%c%c%c"', tz="UTC")
case("strflocaltime", "0", '"%c%c%c%c"', tz="UTC")
case("strflocaltime", "0", '""')
case("strflocaltime", "1", '"abc\\u0000def"')
for bad in ['"x"', "null", "{}", "true"]:
    case("strflocaltime", bad, ISO)
    case("strflocaltime", bad, "1")
for fmt in ["1", "[]", "{}", "null"]:
    case("strflocaltime", "0", fmt)
    case("strflocaltime", "[2015,2,5]", fmt)
    case("strflocaltime", '["a",1,2,3,4,5,6,7]', fmt)
case("strflocaltime", '["a",1,2,3,4,5,6,7]', ISO)
case("strflocaltime", "[2015,2,5,23,51,nan]", ISO)
# localtime's failure is not checked: the format error wins, or jq aborts.
case("strflocaltime", "1e30", "1")
case("strflocaltime", "1e30", "{}")
case("strflocaltime", "1e30", ISO)
case("strflocaltime", "-1e1000", '"%c"')
case("strflocaltime", "nan", ISO, only="aarch64")

case("strptime", '"2015-03-05T23:51:47Z"', ISO)
case("strptime", '"2025-06-07T08:09:10"', '"%FT%T"')
case("strptime", '"2015-03-05T23:51:47Z"', '"%FT%TZ"')
case("strptime", '"12/31/99"', '"%D"')
case("strptime", '"2015-03-05T23:51:47Z \\n x"', ISO)
case("strptime", '"2015\\t\\n"', '"%Y"', only="macos")
case("strptime", '"1970-03-01T01:02:03Z"', ISO)
case("strptime", '"2037-02-11T01:02:03Z"', ISO)
case("strptime", '"2000-02-29"', '"%Y-%m-%d"')
case("strptime", '"Mar 5 2015"', '"%b %d %Y"', tz="UTC")
case("strptime", '"March 5 2015 11:51:47 PM"', '"%B %d %Y %I:%M:%S %p"', tz="UTC")
case("strptime", '"Thursday 1970"', '"%A %Y"', tz="UTC", only="macos")
case("strptime", '"2015-03-05T23:51:47Zx"', ISO)
case("strptime", '"2015-03-05 23:51:47"', ISO)
case("strptime", '"2015-13-01"', '"%Y-%m-%d"')
case("strptime", '"abc"', '"%Y"')
case("strptime", '"abc\\u0000"', '"abcd"')
case("strptime", '"abc\\u0000x"', '"abc\\u0000y"', only="macos")
case("strptime", '"é2015"', '"%Y"')
case("strptime", '"2015\\u00a0"', '"%Y"', only="macos")
for bad in ["1", "null", "[]", "{}", "1E+2"]:
    case("strptime", bad, ISO)
    case("strptime", '"2015"', bad)
case("strptime", "1", "2")
case("strptime", '"12"', '"%H"', only="macos")
case("strptime", '"1900-01-01"', '"%Y-%m-%d"', only="macos")
case("strptime", '" 2015"', '"%Y"', only="macos")
case("strptime", '"2015 100"', '"%Y %j"', only="macos")
case("strptime", '"100"', '"%j"', only="macos")
case("strptime", '"2015 3 5 100"', '"%Y %m %d %j"', only="macos")
case("strptime", '"1425599507"', '"%s"', tz="UTC", only="macos")
case("strptime", '"1425599507"', '"%s"', tz="America/New_York", only="macos")
case("strptime", '"2015-03-05 +0900"', '"%Y-%m-%d %z"', tz="America/New_York",
     only="macos")

# ---------------------------------------------------------------------------------
# libm (one wrapper per entry; the Value-level corpus test covers the values)
# ---------------------------------------------------------------------------------
case("floor", "-2.5")
case("floor", "1E+2")
case("floor", "1.000")
case("sqrt", "2")
case("sqrt", "1E+2")
case("sqrt", "-1")
case("exp", "1000")
case("exp", "-1000")
case("fabs", "-0.5")
case("floor", "nan")
case("floor", "1e1000")
case("trunc", "-0.5")
for bad in ['"a"', "null", "true", "[1,2,3,4,5,6,7,8,9]", LONG_OBJ, '"1"']:
    case("floor", bad)
    case("frexp", bad)
case("pow", "null", "2", "10")
case("pow", '"x"', "2", "0.5")
case("pow", "null", "1E+1", "2")
case("atan2", "null", "1", "1")
case("fmod", "null", "5", "3")
case("ldexp", "null", "1", "2.9")
case("scalbln", "null", "1", "-1.9")
case("nextafter", "null", "0", "1")
case("nexttoward", "null", "0.5", "2")
case("copysign", "null", "1", "-0")
for a, b in [('"a"', "1"), ("1", '"b"'), ('"a"', '"b"'), ("null", "[]"), ("{}", "null")]:
    case("pow", "0", a, b)
    case("fmin", '"input ignored"', a, b)
case("fma", "null", "2", "3", "4")
case("fma", '"x"', "0.1", "10", "-1")
for a, b, c in [('"a"', "1", "1"), ("1", '"b"', "1"), ("1", "1", '"c"'),
                ('"a"', '"b"', '"c"'), ("1", "{}", '"c"'), ("[]", "null", "true")]:
    case("fma", "0", a, b, c)
case("frexp", "8")
case("frexp", "0")
case("frexp", "-0")
case("frexp", "1E+2")
case("frexp", "5e-324")
case("frexp", "1e1000")
case("frexp", "nan")
case("modf", "-3.5")
case("modf", "1e1000")
case("modf", "nan")
case("lgamma_r", "1")
case("lgamma_r", "0.5", only="macos")
case("lgamma_r", "-0.5", only="macos")
case("significand", "12", only="macos")
case("gamma", "0.5", only="macos")
case("drem", "null", "5", "3", only="macos")
case("exp10", "2", only="macos")
case("jn", "null", "nan", "1", only="macos-aarch64")
case("ldexp", "null", "1", "1e10", only="aarch64")


def program(f, nargs):
    call = f if nargs == 0 else f + "(" + "; ".join(f"$a{i}" for i in range(nargs)) + ")"
    return f"try ($in | {call} | {{ok: tojson}}) catch {{err: .}}"


def run(f, input, args, tz, lc_all):
    cmd = [JQ, "-nc", "--argjson", "in", input]
    for i, a in enumerate(args):
        cmd += ["--argjson", f"a{i}", a]
    cmd.append(program(f, len(args)))
    env = dict(os.environ, TZ=tz, LC_ALL=lc_all)
    p = subprocess.run(cmd, capture_output=True, env=env)
    if p.returncode == -6:
        line = p.stderr.decode().strip()
        return '"abort":' + json.dumps(line, ensure_ascii=False)
    if p.returncode != 0 or p.stderr:
        sys.exit(f"jq failed on {cmd}: rc={p.returncode} {p.stderr!r}")
    out = p.stdout.decode()
    assert out.endswith("}\n") and out.startswith("{"), out
    # Keep jq's own bytes for the result.
    return out[1:-2]


def main():
    rows = []
    for f, input, args, opts in CASES:
        tz = opts.get("tz")
        if tz is None:
            results = {run(f, input, args, z, lc) for z, lc in INDEPENDENT_ENVS}
            if len(results) != 1:
                sys.exit(f"{f} {input} {args} depends on TZ or locale; give it a tz: {results}")
            result = results.pop()
        else:
            result = run(f, input, args, tz, "C")
        row = '{"f":' + json.dumps(f) + ',"input":' + input + ',"args":[' + ",".join(args) + "]"
        if tz is not None:
            row += ',"tz":' + json.dumps(tz)
        if "only" in opts:
            row += ',"only":' + json.dumps(opts["only"])
        rows.append(row + "," + result + "}")
    with open(OUT, "w") as out:
        for row in rows:
            out.write(row + "\n")
    print(f"wrote {len(rows)} cases to {OUT}")


if __name__ == "__main__":
    main()
