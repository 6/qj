"""Focused generator modes for fuzz.py (see gen.py for the general generator).

Each `mode_*` returns a program string. They concentrate on one area each:
builtins with arbitrary arguments, value conversions, paths and assignment,
generators and control flow, regex, and dates. Input generators for the
parser (mutated JSON, deep nesting, jq's 4096-byte read buffer boundaries) and
for raw (-R) input are here too.
"""

import re

from gen import (A, E, FLAGS, FMTS, Q, RARE_STRS, REGEXES, T, Gen, gen_input_bytes, gen_value,
                 input_to_bytes, jstr, lf, render, ser)

# jq 1.8.1 `builtins`, plus internal helpers that programs can call.
ALL_BUILTINS = """IN/1 IN/2 INDEX/1 INDEX/2 JOIN/2 JOIN/3 JOIN/4 abs/0 acos/0 acosh/0 add/0 add/1 all/0
all/1 all/2 any/0 any/1 any/2 arrays/0 ascii_downcase/0 ascii_upcase/0 asin/0 asinh/0 atan/0
atan2/2 atanh/0 booleans/0 bsearch/1 capture/1 capture/2 cbrt/0 ceil/0 combinations/0
combinations/1 contains/1 copysign/2 cos/0 cosh/0 debug/0 debug/1 del/1 delpaths/1 drem/2 empty/0
endswith/1 env/0 erf/0 erfc/0 error/0 error/1 exp/0 exp10/0 exp2/0 explode/0 expm1/0 fabs/0
fdim/2 finites/0 first/0 first/1 flatten/0 flatten/1 floor/0 fma/3 fmax/2 fmin/2 fmod/2 format/1
frexp/0 from_entries/0 fromdate/0 fromdateiso8601/0 fromjson/0 fromstream/1 gamma/0
getpath/1 gmtime/0 group_by/1 gsub/2 gsub/3 halt_error/0 halt_error/1 has/1 have_decnum/0
have_literal_numbers/0 hypot/2 implode/0 in/1 index/1 indices/1 infinite/0 input/0
input_filename/0 input_line_number/0 inputs/0 inside/1 isempty/1 isfinite/0 isinfinite/0 isnan/0
isnormal/0 iterables/0 j0/0 j1/0 jn/2 join/1 keys/0 keys_unsorted/0 last/0 last/1 ldexp/2
length/0 lgamma/0 limit/2 localtime/0 log/0 log10/0 log1p/0 log2/0 logb/0 ltrim/0
ltrimstr/1 map/1 map_values/1 match/1 match/2 max/0 max_by/1 min/0 min_by/1 mktime/0 modf/0
nan/0 nearbyint/0 nextafter/2 nexttoward/2 normals/0 not/0 nth/1 nth/2 nulls/0
numbers/0 objects/0 path/1 paths/0 paths/1 pick/1 pow/2 range/1 range/2 range/3 recurse/0
recurse/1 recurse/2 remainder/2 repeat/1 reverse/0 rindex/1 rint/0 round/0 rtrim/0 rtrimstr/1
scalars/0 scalb/2 scalbln/2 scan/1 scan/2 select/1 setpath/2 significand/0 sin/0 sinh/0 skip/2
sort/0 sort_by/1 split/1 split/2 splits/1 splits/2 sqrt/0 startswith/1 stderr/0 strflocaltime/1
strftime/1 strings/0 strptime/1 sub/2 sub/3 tan/0 tanh/0 test/1 test/2 tgamma/0 to_entries/0
toboolean/0 todate/0 todateiso8601/0 tojson/0 tonumber/0 tostream/0 tostring/0 transpose/0
trim/0 trimstr/1 trunc/0 truncate_stream/1 type/0 unique/0 unique_by/1 until/2 utf8bytelength/0
values/0 walk/1 while/2 with_entries/1 y0/0 y1/0 yn/2 _negate/0 _plus/2 _minus/2 _multiply/2
_divide/2 _mod/2 _equal/2 _notequal/2 _less/2 _lesseq/2 _greater/2 _greatereq/2 _strindices/1
_sort_by_impl/1 _group_by_impl/1 _unique_by_impl/1 _min_by_impl/1 _max_by_impl/1 _match_impl/3
_modify/2 _assign/2 _flatten/1 getpath/1 splits/1 error/1 tojson/0 ltrimstr/1""".split()
# Unbounded (or possibly unbounded) with arbitrary arguments: wrapped in limit().
INFINITE_BUILTINS = {"repeat/1", "recurse/1", "recurse/2", "while/2", "until/2", "range/1",
                     "range/2", "range/3", "combinations/0", "combinations/1", "recurse/0",
                     "inputs/0", "walk/1", "paths/0", "paths/1"}

VALUE_POOL = [
    "null", "true", "false", "0", "1", "-1", "2", "3", "-0", "0.5", "1.5", "-2.5", "1.0", "1.000",
    "1e2", "1E+2", "100000000000000000001", "1e1000", "-1e1000", "1e-400", "5e-324", "1e17",
    "3.14159", "9007199254740993", "nan", "infinite", "-infinite", '""', '"a"', '"abc"', '"A"',
    '"é"', '"a,b"', '"a b"', '"1"', '"1.5"', '"-1"', '"1e1000"', '"nan"', '" 1"', '"0x10"',
    '"\\u0000"', '"😀"', '"\\n"', '"true"', '"null"', '"[1]"', '"{}"', '"%Y"', '"g"', '"x"',
    '"ab12"', '"2015-03-05T23:51:47Z"', '"aGk="', '"<&>"', '"\\u007f"', '"\\u2028"', "[]",
    "[1]", "[1,2]", "[2,1]", "[1,2,3]", '["a"]', '["a","b"]', "[null]", "[[]]", "[[1,2],[3]]",
    '[{"a":1}]', '[{"a":1},{"a":2}]', "[1,[2,[3]]]", '["a",1,null]', "[0,-1]", "[1.5]",
    '[{"start":1,"end":2}]', '["a",0]', "{}", '{"a":1}', '{"a":1,"b":2}', '{"a":{"b":1}}',
    '{"b":2,"a":1}', '{"a":[1,2]}', '{"key":"k","value":1}', '{"k":"a","v":1}',
    '{"name":"n","value":2}', '{"a":null}', '[[0],1]', '[["a"],2]', '[[0]]', "[1,1,2]",
    '[3,"a",null,true,[],{}]', '[2015,2,5,23,51,47,4,63]', '[2015,2,5]', "1425599621",
    "-1425599621", "1e10", "1.5e9", "[1e1000]", '{"a":"x","b":"y"}', '[{"key":1,"value":2}]',
    '[["a",1],["b",2]]', '[{"name":null}]', '["b","a","c"]', '"abcabc"', '"a\\tb"', '"ǅ"',
]
ARG_POOL = VALUE_POOL + [
    ".", "empty", "error", "(1,2)", "(.,.)", ".[]?", ".a?", "tostring", "length", "(null,1)",
    "error(\"x\")", "(1,error(\"e\"))", "range(3)", "\"g\"", "\"gi\"", "\"x\"", "\"%s\"",
    "\"%Y-%m-%d\"", "[.[]?]", "{a:1}", "-.", "not", ". + 1", "..", "keys?", "(\"a\",\"b\")",
    "input", "$__loc__", "type", "true, false",
]


def gen_program_text(r, depth):
    return render(Gen(r).q(depth))


def mode_builtins(r, cli_vars):
    """A random builtin with random arguments, on a random input value."""
    b = r.choice(ALL_BUILTINS)
    name, arity = b.rsplit("/", 1)
    arity = int(arity)
    args = [r.choice(ARG_POOL) if r.random() < 0.85 else gen_program_text(r, 1)
            for _ in range(arity)]
    callt = name if arity == 0 else "%s(%s)" % (name, "; ".join(args))
    if b in INFINITE_BUILTINS:
        callt = "limit(%d; %s)" % (r.randint(0, 6), callt)
    k = r.random()
    if k < 0.35:
        prog = "try (%s) catch ." % callt
    elif k < 0.6:
        prog = "[%s]" % callt
    elif k < 0.7:
        prog = "[%s] | tojson" % callt
    elif k < 0.8:
        prog = "%s | %s" % (r.choice(VALUE_POOL), callt)
    else:
        prog = callt
    return prog


VALUE_OPS = [
    "tostring", "tojson", "@text", "@json", "@csv", "@tsv", "@sh", "@html", "@uri", "@base64",
    "@base32", "@base64d", "@base32d", "tonumber", "length", "utf8bytelength", "-.", "abs",
    ". + 0", ". * 1", ". - 1", ". / 3", ". % 3", "floor", "ceil", "round", "sqrt", "fabs",
    "trunc", "significand", "frexp", "modf", "logb", "type", "not", "ascii_downcase",
    "ascii_upcase", "explode", "explode | implode", "ltrimstr(\"a\")", "rtrimstr(\"c\")",
    "trimstr(\"a\")", "trim", "ltrim", "rtrim", "split(\"\")", "split(\",\")", "[splits(\"a*\")]",
    "test(\"a\")", "[match(\".\"; \"g\") | .offset]", "tojson | fromjson", "keys",
    "to_entries", "add", "min", "max", "sort", "unique", "reverse", "flatten", "tostream",
    "[paths]", "[..]", "isnan", "isinfinite", "isnormal", "infinite", "toboolean", ". == 1",
    ". < 1", "[., 1] | sort", "[., \"a\"] | sort", "[.] | implode", "@text \"v=\\(.)\"",
    "\"\\(.)\"", "tojson | length", ". as $x | [$x, -$x]", "[limit(2; .[]?)]", "indices(1)",
    "index(\"a\")", "rindex(\"a\")", "indices(\"a\")", ".[0:2]", ".[-1:]", ".[1:]", "has(0)",
    "has(\"a\")", "contains(\"a\")", "inside(\"abc\")", "startswith(\"a\")", "endswith(\"a\")",
    "todate", "fromjson", "tojson|tojson", "getpath([\"a\"])", "[.[]?] | length", "gmtime",
    "gmtime | mktime", "strftime(\"%Y-%m-%dT%H:%M:%SZ\")", "@sh \"x \\(.)\"", "ldexp(.; 2)",
    "pow(.; 2)", "log", "exp10", "[., .] | add", "{a: .} | tojson", "[.] | tojson",
    "tostring | tonumber", "tojson | tonumber", "@text | length", ". == .", "[.] == [.]",
    "splits(\", \")", "sub(\"a\"; \"b\")", "gsub(\"\"; \"-\")", "ascii_downcase | explode",
    "utf8bytelength == length", "implode", "[.[]? | tostring]", "map(tojson)?", "@uri \"\\(.)\"",
    "@base64d | explode", "@base32d | utf8bytelength", "ltrimstr(\"\")", "split(\"a\"; null)",
    "tojson | .[1:-1]", "tostring | explode | implode", ".[2:] | length", ".[:1]",
    "[.[]?] | sort_by(tojson)", "[.[]?] | group_by(type) | map(length)", "keys_unsorted",
    "[to_entries[]? | .key]", "del(.[0])?", "to_entries? | from_entries", "tostream | tojson",
]


def mode_values(r, cli_vars):
    """Value-level operations on random inputs (numbers, strings, unicode)."""
    ops = [r.choice(VALUE_OPS) for _ in range(r.choice([1, 1, 2, 3]))]
    k = r.random()
    if k < 0.4:
        return "[.[]? | try (%s) catch \"E: \\(.)\"]" % " | ".join(ops)
    if k < 0.6:
        return "try (%s) catch ." % " | ".join(ops)
    if k < 0.8:
        return "[%s]" % ", ".join("(%s)?" % o for o in ops)
    return " | ".join(ops)


PATH_OPS = [
    "path({P})", "[path({P})]", "del({P})", "{P} = {V}", "{P} |= {U}", "{P} += {V}", "{P} -= 1",
    "{P} *= 2", "{P} //= {V}", "pick({P})", "[paths]", "[paths({C})]", "to_entries",
    "with_entries({E})", "tostream", "[tostream] | fromstream(.[])", "fromstream(tostream)",
    "getpath({PA})", "setpath({PA}; {V})", "delpaths({PAS})", "[paths] | length",
    "reduce paths as $p (.; setpath($p; 1))", "delpaths([paths])", "[..] | length",
    "(.. | select(type == \"object\")) |= with_entries({E})", "walk({U})", "[path(..)]",
    "del(.. | select(. == null))", "del(.[]?)", "(.[]? | select(type == \"number\")) |= {U}",
    "[getpath(paths)]", "to_entries | from_entries", "del({P}, {P})", "{P} |= empty",
    "[limit(3; paths)]", "first(paths)", "path(first(.[]?))", "(first({P})) = {V}",
    "try ({P} = {V}) catch .", "try del({P}) catch .", "try path({P}) catch .",
    "[{P}]", "[{P} | tostring]", "({P}) as $v | {P} = [$v]", "reduce ({P}) as $x (.; .)",
    "paths(type == \"number\") as $p | getpath($p)", "input as $x | {P} = $x",
    "getpath([\"a\",\"b\"]) = 1", "setpath([]; {V})", "delpaths([[]])", ".[] |= {U}",
    "map_values({U})", "map({U})", "(.a, .b) = {V}", ".. |= {U}",
    "to_entries | map(select(.value)) | from_entries", "[paths(..)]", "[paths(true)]",
    "{P} |= ({P})", "{P} = ({P})", "{P} += ({P})", "[{P}] = [1, 2]", "{P} |= (., .)",
    "del({P}) | del({P})", "[path({P}), path({P})]", "getpath([path({P})][0])",
    "try pick({P}) catch .", "try ({P} |= {U}) catch .", "{P} |= try error catch .",
]


def mode_paths(r, cli_vars):
    g = Gen(r, cli_vars)
    op = r.choice(PATH_OPS)

    def sub(m):
        key = m.group(1)
        if key == "P":
            return render(g.path(r.choice([0, 1, 1, 2])), E)
        if key == "V":
            return render(g.pick([(3, g.literal), (1, lambda: g.gen(0)), (1, lambda: g.t(1))]), T)
        if key == "U":
            return render(g.upd(r.choice([0, 0, 1])), T)
        if key == "C":
            return render(g.cond(r.choice([0, 1])), Q)
        if key == "E":
            return render(g.entry_upd(1), Q)
        if key == "PA":
            return render(g.patharr(), Q)
        return render(g.patharrs(1), Q)

    prog = re.sub(r"\{(P|V|U|C|E|PA|PAS)\}", sub, op)
    if r.random() < 0.3:
        prog = prog + " | " + r.choice(["tojson", "keys?", "length", "[paths]", "tostream",
                                        "to_entries?", "map(type)?", "."])
    return prog


CONTROL = [
    "[limit({N}; {G})]", "first({G})", "[first({G}), last({G})]", "isempty({G})",
    "[label $out | {G} | if {C} then ., break $out else . end]", "try ({G}) catch .",
    "[.[]? | try {X} catch \"c\"]", "[{G}] | length", "reduce {G} as $x ({I}; {R})",
    "[foreach {G} as $x ({I}; {R}; [$x, .])]", "[foreach {G} as $x ({I}; {R})]",
    "[{G} as [$a, $b] ?// {a: $a, b: $b} ?// $a | [$a, $b]]", "[{G} as {$a} ?// [$a] | $a]",
    "[.[]? as [$a] ?// $a | if ($a | type) == \"array\" then error(\"arr\") else $a end]",
    "any({G}; {C})", "all({G}; {C})", "[nth({N}; {G})]", "[skip({N}; {G})]", "add({G})",
    "[range({N}; {N}; {N})]", "[limit({N}; repeat({U}))]", "until({C2})",
    "[limit(8; recurse({U}; {C}))] | length", "[label $a | label $b | {G} | ., break $b]",
    "[{G}] as $xs | $xs | length", "def f: {G}; [f, f]", "def f(g): [g, g]; f({G})",
    "def f($x): [$x, .]; [f({G})]", "[{G}] | (first, last)?", "[{G} | select({C})]",
    "try error({G}) catch .", "[.[]? | (try error catch .)]", "(try error(\"\\({G})\") catch .)",
    "[limit({N}; {G}) | try {X} catch \"c\"]", "isempty(error)", "isempty(empty)",
    "first(empty)", "[first(range(10; 0; -3))]", "[limit(0; {G})]", "[limit(-1; {G})]",
    "[{G}] | @json", "label $f | first({G}), break $f", "[.[]?|tostring] | join(\",\")",
    "[(label $x | {G}), 99]", "try (label $x | error(\"in\")) catch .", "[{G}]?",
    "reduce empty as $x (1; 2)", "foreach empty as $x (1; 2; 3)", "[{G} | {X}?]",
    "getpath([\"a\"]) as $x | [$x, {G}]", "{G} | {G}", "[{G}, {G}]", "[.[]? // {V}]",
    "[{G} // {V}]", "({G}) // error(\"none\")", "[{X} // {V}]", "{a: {G}}", "[{({G}|tostring): 1}]",
    "[\"\\({G}),\\({G})\"]", "input as $x | [$x, {G}]", "[inputs] | length", "[., input]?",
    "$__loc__", "{G} as $x | $__loc__", "try input catch .", "[.[]? | input?]",
    "[{G} as $x | {G} as $y | [$x, $y]]", "[{G}] | min_by(tojson)?", "[{G} | {X}] | length",
    "try ([{G}] | error) catch .", "[.[]? | try (if . == null then error else . end) catch \"N\"]",
    "reduce {G} as [$a, $b] ({I}; . + [$a])", "[foreach {G} as {a: $a} ({I}; $a; .)]",
    "[label $l | foreach {G} as $x ({I}; {R}; if . == null then break $l else . end)]",
    "try (reduce {G} as $x ({I}; error(\"r\"))) catch .", "first({G}; {G})",
    "[{G}] | [limit(2; .[])]", "[.[]? | [{G}]]", "[limit(1; {G}), first({G})]",
]


def mode_control(r, cli_vars):
    g = Gen(r, cli_vars)
    tpl = r.choice(CONTROL)

    def sub(m):
        key = m.group(1)
        if key == "G":
            return render(g.gen(r.choice([0, 1, 1, 2])), T)
        if key == "N":
            return render(g.small_int(), T)
        if key == "C":
            return render(g.cond(r.choice([0, 1])), T)
        if key == "C2":
            c, u = r.choice(Gen.UNTILS)
            return c + "; " + u
        if key == "X":
            return render(g.errorish(1), T)
        if key == "I":
            return render(g.literal(), T)
        if key == "R":
            return render(r.choice([lf(". + $x", E), lf("$x", E), lf("[., $x]", A),
                                    lf("if $x == null then error(\"n\") else . end", A),
                                    lf("empty", A), lf(".[$x|tostring] = 1", E),
                                    lf("(., $x)", A), g.t(1)]), T)
        if key == "U":
            return render(g.upd(0), T)
        return render(g.literal(), T)

    return re.sub(r"\{(G|N|C2|C|X|I|R|U|V)\}", sub, tpl)


REGEX_TPL = [
    "test({R})", "test({R}; {F})", "[match({R}; {F})]", "[match({R})] | length",
    "capture({R})", "[capture({R}; {F})]", "[scan({R})]", "[scan({R}; {F})]",
    "split({R}; {F})", "[splits({R})]", "[splits({R}; {F})]", "sub({R}; {S})",
    "sub({R}; {S}; {F})", "gsub({R}; {S})", "gsub({R}; {S}; {F})", "test([{R}, {F}])",
    "[match([{R}, {F}])]", "ascii_downcase | test({R}; {F})",
    "[match({R}; \"g\") | [.offset, .length, .string, [.captures[] | [.offset, .length, .string, .name]]]]",
    "[.[]? | strings | test({R}; {F})]", "[.[]? | try sub({R}; {S}; {F}) catch .]",
    "try test({R}; {F}) catch .", "try [match({R}; {F})] catch .", "[_match_impl({R}; {F}; false)]",
    "[_match_impl({R}; {F}; true)]", "[.[]? | strings | [scan({R})]]", "[splits({R}; {F})]",
    "[match({R}; {F}) | .captures | map(.name)]", "gsub({R}; {S}; \"g\")", "[scan({R}; \"g\")]",
]
REPLS = ['"x"', '""', '"<\\(.)>"', '"\\(.x // "?")"', '"\\(.captures | length)"', '("a","b")',
         'empty', '1', '"\\(.a)-\\(.b)"', '"$1"', '"\\\\1"', 'ascii_upcase', '"[\\(.n // .x // "")]"',
         '(.x | ascii_downcase)', '"\\(.)"', 'tojson', 'error("r")']
REGEX_INPUTS = ['"abc"', '"aaa"', '"a.b"', '"abcabc"', '""', '"é"', '"éa😀b"', '"a\\nb"',
                '"Ab AB ab"', '"123 456"', '"a1b22c333"', '"x"', '"  a  "', '"a,b, c"',
                '["abc", "x"]', '"😀😀"', '"aé"', '"\\u0000a"', '"ab\\u2028c"', '"xyz"',
                '"test test"', '"aAbB"', '"a-b_c d"', '"1.5e3"', '"İi"']


def mode_regex(r, cli_vars):
    fl = r.choice(FLAGS + ["null"] * 2)
    fl = fl if fl == "null" else jstr(fl)
    prog = r.choice(REGEX_TPL).replace("{R}", jstr(r.choice(REGEXES))).replace("{F}", fl)
    prog = prog.replace("{S}", r.choice(REPLS))
    if r.random() < 0.6:
        prog = r.choice(REGEX_INPUTS) + " | " + prog
    return prog


DATE_TPL = [
    "todate", "todateiso8601", "gmtime", "localtime", "gmtime | todate", "gmtime | mktime",
    "localtime | mktime", "gmtime | strftime({FMT})", "localtime | strftime({FMT})",
    "strftime({FMT})", "strflocaltime({FMT})", "gmtime | strflocaltime({FMT})", "fromdate",
    "fromdateiso8601", "strptime({FMT})", "strptime({FMT}) | mktime", "strptime({FMT}) | todate",
    "todate | fromdate", "[gmtime, localtime]", "mktime",
    "strptime({FMT}) | strftime({FMT})", "gmtime | .[5] += 1 | mktime",
    "gmtime | .[0] = 1900 | todate", "try strftime({FMT}) catch .", "try strptime({FMT}) catch .",
    "try mktime catch .", "try gmtime catch .", "try todate catch .", "try fromdate catch .",
    "localtime | todate", "gmtime | map(type)", "strptime({FMT}) | map(tostring) | join(\",\")",
    "now | type", "[localtime, gmtime] | map(mktime)", "gmtime | strftime(\"%s %j %U %W %u %w\")",
    "localtime | strftime(\"%Z %z %s\")", "strflocaltime(\"%c %Z\")", "gmtime | todateiso8601",
]
DATE_INPUTS = ["0", "1", "-1", "1425599621", "1425599621.123", "-62135596800", "253402300799",
               "1e10", "1e20", "-1e20", "nan", "infinite", "1.5", "2147483648", "-2147483649",
               '"2015-03-05T23:51:47Z"', '"1970-01-01T00:00:00Z"', '"2015-03-05"', '"10:15"',
               '"Thursday, March 05, 2015"', '""', '"x"', '"2015-03-05T23:51:47.123Z"',
               "[2015,2,5,23,51,47,4,63]", "[2015,2,5]", "[1900,0,1,0,0,0,0,0]",
               "[2024,1,29,12,0,0.5,0,0]", "[2015,13,40,25,61,61,0,0]", "[-1,0,1]", "[]", "{}",
               '["2015",2,5,0,0,0,0,0]', "[2015,2,5,23,51,47.9,4,63]", "[1e10,0,1,0,0,0,0,0]",
               "1709251200", "1710054000", "1710057600", "1699164000", "1699167600",
               '"2024-03-10T02:30:00Z"', '"12/31/99"', '"1 Jan 2000"', "-0", "0.999",
               "[1970,0,1,0,0,0,0,0]", "[2000,0,0,0,0,0,0,0]", "[2015,2,5,23,51,47,4,63,1]",
               "[null,0,1,0,0,0,0,0]", '[2015,2,5,23,51,"47",4,63]', "-86400.5"]


def mode_dates(r, cli_vars):
    fmt = jstr(r.choice(FMTS + ["%Y-%m-%dT%H:%M:%SZ"] * 3))
    tpl = r.choice(DATE_TPL).replace("{FMT}", fmt)
    return "%s | %s" % (r.choice(DATE_INPUTS), tpl)


MUTATION_BYTES = [b"{", b"}", b"[", b"]", b",", b":", b'"', b"\\", b"-", b"0", b"e", b".", b" ",
                  b"\n", b"\x00", b"\xff", b"\xc3", b"\x1e", b"n", b"t", b"/", b"u", b"\\u",
                  b"1", b"\t", b"\r", b"\x7f", b"\xe2\x80\xa8", b"\xef\xbb\xbf", b"nan", b"+",
                  b"E", b"\xed\xa0\x80", b"\xf0\x9f\x98", b"true", b"\x0c"]


def mutate_bytes(r, b):
    b = bytearray(b)
    for _ in range(r.randint(1, 3)):
        k = r.random()
        pos = r.randint(0, len(b))
        if k < 0.3 and b:
            del b[min(pos, len(b) - 1)]
        elif k < 0.6:
            b[pos:pos] = r.choice(MUTATION_BYTES)
        elif k < 0.75:
            b = b[:pos]
        elif b:
            s = r.randint(0, len(b) - 1)
            e = min(len(b), s + r.randint(1, 8))
            b[pos:pos] = b[s:e]
    return bytes(b)


def gen_parse_input(r):
    k = r.random()
    if k < 0.6:
        return mutate_bytes(r, input_to_bytes(gen_input_bytes(r)))
    if k < 0.72:
        depth = r.choice([10, 100, 1000, 9999, 10000, 10001, 10002, 20000])
        o, c = r.choice([(b"[", b"]"), (b'{"a":', b"}"), (b"[[", b"]]")])
        tail = c * depth if r.random() < 0.7 else c * r.randint(0, depth)
        return o * depth + (b"1" if r.random() < 0.5 else b"") + tail
    if k < 0.8:
        # A multibyte character (or a broken one) across a read-chunk boundary:
        # jq reads lines in fgets chunks of 4095 bytes, qj in larger reads.
        boundary = r.choice([4095, 4096, 8190, 8191, 65535, 65536, 1 << 20])
        ch = r.choice([b"\xc3\xa9", b"\xe2\x82\xac", b"\xf0\x9f\x98\x80", b"\xc3", b"\xe2\x82",
                       b"\xf0\x9f\x98", b"\xff", b"\\u00e9", b"\\ud83d\\ude00", b"\\ud83d"])
        pre = r.choice([b'"', b'["', b'{"k":"', b'1 "', b"\n\""])
        n = max(0, boundary - len(pre) - r.randint(0, len(ch)))
        body = pre + b"a" * n + ch + r.choice([b'"', b'"]', b'"}', b"", b'" 1', b'"\n'])
        return body + r.choice([b"\n", b"", b"\n{", b'\n"\xc3\xa9"\n'])
    if k < 0.9:
        # Around jq's 4096-byte read buffer (and larger).
        n = r.choice([4094, 4095, 4096, 4097, 8191, 8192, 8193, 65536, 65537])
        filler = r.choice([b"1 ", b"[] ", b'"x" ', b"\n", b"  ", b"12345678 ", b'{"a":1}\n',
                           b"1\n", b"\xc3\xa9"])
        body = (filler * (n // len(filler) + 1))[:n]
        return body + r.choice([b"", b"{", b"1", b'"\xff"', b"[1,", b"}", b"\n", b"\xc3",
                                b'"\xc3\xa9"', b"\x1e", b"nan"])
    n = r.choice([100, 1000, 4095, 4096, 5000])
    return r.choice([b'"' + b"a" * n + b'"', b"1" * n, b"0." + b"1" * n, b"1e" + b"9" * 10,
                     b'"' + b"\xc3\xa9" * n + b'"', b'"' + b"\\u00e9" * (n // 6) + b'"',
                     b"-" + b"9" * n, b'["' + b"a" * n + b'", 1]', b"1." + b"0" * n,
                     b"0.0000" + b"0" * n + b"1", b"1" + b"0" * n + b"e-" + str(n).encode()])


def gen_raw_input(r):
    lines = [r.choice([b"a", b"", b"abc", b"1", b"1.0", b'"q"', b"a,b,c", b"x y", b"\xc3\xa9",
                       b"\xff", b"{\"a\":1}", b"  sp  ", b"\t", b"a\rb", b"null", b"\x00",
                       b"2015-03-05T23:51:47Z", b"\xe2\x80\xa8", b"\\u00e9", b"\x1e1"])
             for _ in range(r.randint(0, 6))]
    return r.choice([b"\n", b"\n", b"\r\n", b"\x00"]).join(lines) + r.choice([b"\n", b"", b"\r\n"])


PARSE_PROGRAMS = [".", ".", "-c .", "tostream", "[inputs]", "input", "[., input?]",
                  "input_line_number", "[.,input_line_number]", "length?", "type",
                  "try input catch .", "[inputs?]", "first(inputs)", "$__loc__", "tojson",
                  "[limit(2; inputs)]", "reduce inputs as $x (0; . + 1)", "..|numbers?"]
PARSE_FLAGS = [[], ["-c"], ["-c"], ["-c", "--stream"], ["-c", "--stream-errors"], ["-c", "--seq"],
               ["-c", "-s"], ["-c", "-n"], ["-c", "-n", "--stream"], ["-c", "--seq", "--stream"],
               ["-c", "-s", "--stream"], ["--seq"], ["-c", "-e"], ["-c", "-R"], ["-c", "-Rs"],
               ["-c", "-sn"], ["-c", "--stream-errors", "-n"], ["-r"], ["-a", "-c"]]


CLI_PROGRAMS = [
    ".", ".[]?", ".a?", "..", "[.[]?]", "tostring", "tojson", "keys?", "input", "[inputs]",
    "$ARGS", "$__loc__", "input_filename", "[., input_filename]", "input_line_number",
    "halt_error", "halt_error(1)", "halt_error(0)", "halt", "error", "error(\"x\")", "debug",
    "debug(\"m\")", "stderr", ". as $x | $x", "empty", "false", "null", "true", "1",
    "(., .)", ".[]? | tostring", "tostream", "fromstream(inputs)", "[.[]?|tostring]",
    "@text", "@json", "@csv?", "@tsv?", "@sh?", "\"\\u0000\"", "\"a\\u0000b\"", "[\"\\u0000\"]",
    "\"é😀\"", "\"\\u007f\\u2028\"", "{a:\"\\u0000\"}", ".[]?|strings", "if . == null then false else . end",
    "limit(1; .[]?)", "$ENV.PAGER", "env.TZ", "$ARGS.positional", "$ARGS.named", "[$__prog_args]?",
    "{\"b\":1,\"a\":{\"d\":2,\"c\":[3,{\"z\":1,\"y\":2}]}}", "[1,[2,[]],{},{\"a\":[]}]",
    "\"x\" | halt_error", "{} | halt_error", "\"bye\\n\" | halt_error(3)", "[1] | halt_error(2)",
    "null | halt_error", "1 | halt_error(-1)", "\"\" | halt_error", "halt_error(256)",
    "[., .] | .[]", ".. | numbers?", "select(. != null)", "input? // \"none\"",
    "getpath([\"a\"])?", "try error catch .", "error(null)", "error({})", "error([1])",
    "\"a\\nb\"", "[\"a\\tb\"]", "-1", "1.0", "1e1000", "100000000000000000001", "[1.0, -0]",
    "nan", "[nan]", "{a: nan}", "infinite", "-infinite", "[infinite]",
]


# -- multi-line programs (-f files): locfile line/column handling ---------------

PROG_TOKEN_RE = re.compile(r'''
    "(?:[^"\\]|\\.)*"
  | \d+\.?\d*(?:[eE][+-]?\d+)? | \.\d+(?:[eE][+-]?\d+)?
  | \$?[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*
  | @[A-Za-z0-9_]+
  | \?// | //= | \|= | \+= | -= | \*= | /= | %= | == | != | <= | >= | // | \.\.
  | \.[A-Za-z_][A-Za-z_0-9]*
  | \S
''', re.X)
LAYOUT = ["\n", "\n", "\r\n", " # comment\n", "\t", "  ", "\n\t", " # c \\\n still comment\n",
          "\n# é 😀\n", "\n\n", " #\n", "\r\n\t\t", " # trailing \\\r\n more\r\n", "\f", "\v"]


def multiline(r, text):
    """Insert newlines, tabs and comments between tokens of a program."""
    toks = [(m.start(), m.end()) for m in PROG_TOKEN_RE.finditer(text)]
    if len(toks) < 2:
        return text + r.choice(["\n", "", " # end\n"])
    cuts = sorted(r.sample(range(1, len(toks)), min(len(toks) - 1, r.randint(1, 5))))
    out = []
    prev = 0
    for c in cuts:
        pos = toks[c][0]
        out.append(text[prev:pos])
        out.append(r.choice(LAYOUT))
        prev = pos
    out.append(text[prev:])
    s = "".join(out)
    if r.random() < 0.3:
        s = "# header comment\n" + s
    if r.random() < 0.25:
        # A syntax error somewhere past the first line.
        pos = r.randint(len(s) // 2, len(s))
        s = s[:pos] + r.choice([" |", " )", " ]", " }", " +", " $", " @", " .[", " \"",
                                " reduce", " if", " 1 1", " as", " ;", " ?//"]) + s[pos:]
    if r.random() < 0.2:
        pos = r.randint(0, len(s))
        s = s[:pos] + r.choice([" | $__loc__", ", $__loc__", " | [$__loc__]"]) + s[pos:]
        if not s.startswith(("|", ",")):
            pass
    return s + r.choice(["\n", "", "\n\n", "\r\n", " "])


# -- modules --------------------------------------------------------------------

MODULE_DEFS = [
    "def f: . + 1;", "def g(x): [x, x];", "def h($a): $a * 2;", "def k: \"k\";",
    "def f: .a;", "def rec: if . > 3 then . else . + 1 | rec end;", "def two: 1, 2;",
    "def f(x; $y): x + $y;", "def e: error(\"from module\");", "def loc: $__loc__;",
    "def m: 1;", "def ms: [m, m];", "def f: 1; def f: 2;", "def g: f;",
]
MODULE_META = ['module {"name": "m"};', 'module {"version": 1, "x": [1, 2]};', 'module 1;',
               'module {};', 'module {"search": "./"};', 'module {"a": .};']


def gen_modules(r):
    """Files for a module directory `mods/`, plus programs that use them."""
    files = {}
    names = r.sample(["m", "n", "lib", "a", "b"], r.randint(1, 3))
    for i, name in enumerate(names):
        body = []
        if r.random() < 0.3:
            body.append(r.choice(MODULE_META))
        if i + 1 < len(names) and r.random() < 0.4:
            dep = names[i + 1]
            body.append(r.choice(['import "{d}" as {d};', 'include "{d}";',
                                  'import "{d}" as ${d};']).format(d=dep))
        # (No import cycles: jq recurses in its linker until it overflows the
        # stack and dies of SIGSEGV, e.g. a module that imports itself.)
        body += r.sample(MODULE_DEFS, r.randint(1, 4))
        if r.random() < 0.1:
            body.append(r.choice(["def broken: ;", "def x: 1", ".", "1 +", "def: 1;"]))
        text = "\n".join(body) + "\n"
        where = r.choice(["mods/%s.jq", "mods/%s.jq", "mods/%s/%s.jq", "mods/%s/jq/main.jq"])
        path = where % ((name, name) if where.count("%s") == 2 else (name,))
        files[path] = text.encode()
    if r.random() < 0.5:
        files["mods/d.json"] = r.choice([b'{"x":1}', b"1 2 3", b"[]", b"", b"{", b'"s"\n',
                                         b'{"a":[1,{"b":null}]}\n{"c":2}'])
    uses = []
    for name in names:
        uses += ['import "%s" as %s; [%s::%s]' % (name, name, name, r.choice(["f", "g(.)", "k", "m", "ms", "two", "h(2)", "rec", "loc", "e", "nope"])),
                 'include "%s"; [%s]' % (name, r.choice(["f", "g(1)", "k", "m", "two", "rec", "e", "loc", "nope"])),
                 '"%s" | modulemeta' % name,
                 'import "%s" as %s {search: "./"}; %s::m?' % (name, name, name),
                 'import "%s" as $%s; $%s' % (name, name, name)]
    uses += ['import "d" as $d; $d', 'import "d" as $d; $d::d', 'include "d"; .',
             'import "nope" as n; 1', '"nope" | modulemeta', '"d" | modulemeta',
             'import "m" as m; import "n" as m; 1', 'import "m" as $m; $m', '1 as $x | 2',
             'import "m" as m {search: 1}; 1', 'import "m" as m {search: ["./", "../"]}; m::m',
             'import "../mods/m" as m; m::m', 'import "m" as m; def f: m::f; f',
             'include "m" {search: "./"}; m']
    prog = r.choice(uses)
    if r.random() < 0.3:
        prog = prog + " | " + r.choice(["tojson", "length", ".[0]?", "keys?", "."])
    lflag = r.choice([["-L", "mods"], ["-Lmods"], ["--library-path", "mods"], ["-L", "mods"],
                      [], ["-L", "nowhere", "-L", "mods"], ["-L", "mods/m"]])
    return files, lflag, prog


# -- --run-tests files ------------------------------------------------------------


def gen_runtests(r):
    parts = []
    if r.random() < 0.3:
        parts.append("# a test file\n\n")
    for _ in range(r.randint(1, 5)):
        k = r.random()
        prog = gen_program_text(r, r.choice([1, 1, 2]))
        if k < 0.65:
            inp = r.choice(VALUE_POOL + ["{\"a\":1,\"b\":[1,2]}", "[1,2,3]", "null"])
            outs = [r.choice(VALUE_POOL) for _ in range(r.choice([0, 1, 1, 2]))]
            parts.append("%s\n%s\n%s\n" % (prog, inp, "".join(o + "\n" for o in outs)))
        elif k < 0.85:
            msg = r.choice(["", "jq: error: x is not defined at <top-level>, line 1:\n",
                            "syntax error\n"])
            parts.append("%%%%FAIL\n%s\n%s" % (r.choice([prog, prog + " |", "{", "$x", ". as [$a] |"]), msg))
        else:
            parts.append("%%%%FAIL IGNORE MSG\n%s\nwhatever\n" % r.choice([prog, "{", "$undefined"]))
        parts.append(r.choice(["\n", "\n", "# c\n", "\n\n"]))
    if r.random() < 0.1:
        parts.append(r.choice(["lonely program\n", "%%FAIL\n", ".\n"]))
    return "".join(parts).encode()


# -- bulk inputs: qj's parallel record engine -------------------------------------

# Programs that keep records independent (the engine runs them on worker
# threads), and a few that force the sequential reader.
BULK_PROGRAMS = [
    ".", ".a", ".a?", ".[]?", "keys?", "select(.a? > 1)", "select(type == \"object\")",
    "tostring", "tojson", "length?", "type", ".a |= (. // 0) + 1?", "[.[]?] | length",
    "input_filename", "input_line_number", "[input_filename, input_line_number]",
    "if type == \"number\" and . > 50 then error(\"big\") else . end", "error?", "try error catch .",
    "(.a? // .) | tostring", ".. | numbers?", "paths?", "[paths?] | length", "to_entries?",
    "del(.a?)", "{b: .a?}", "select(.c? != null)", ".[0]?", "@json", "@text", "@csv?", "@sh?",
    "values", "not", "empty", "if . == null then empty else . end", "-(.a? // 0)?",
    ". as $x | $x", "[., .]", "(., .)", "tostream", "$ENV.PAGER", "env.TZ", "splits(\"a\")?",
    "ascii_downcase?", "has(\"a\")?", "map_values(. + 1)?", "with_entries(.value |= tostring)?",
    "[.[]?|numbers] | add", "walk(if type == \"number\" then . + 1 else . end)",
    "if .a? == 1 then halt_error else . end", "input? // \"none\"", "$__loc__", "[., input?]",
    "label $f | ., break $f", "first(inputs)?", "debug | .a?", "stderr | empty",
]
BULK_LINES = [b"", b"  ", b"\t", b"nan", b"1 2", b"[1,\n2]", b'"\xff"', b'{"a":nan}', b"-0",
              b"1e1000", b"100000000000000000001", b'{"a":1,"a":2}',
              b"[]", b"{}", b"null", b"true", b'"x"', b"0.10", b"1E2", b"\r"]
BULK_ERRORS = [b"\xef\xbb\xbf{}", b"{", b"}", b"[1,]", b"tru", b"nul", b"{\"a\" 1}", b"1.2.3", b"]", b"'x'", b"\x00"]


def fast_path_ok(text):
    """Whether qj's simdjson fast path takes this text (strict RFC 8259 JSON,
    valid UTF-8, no lone surrogates, numbers within double range): only such
    records keep a job on the parallel engine's workers."""
    import json
    import math

    def num(x):
        f = float(x)
        if math.isinf(f):
            raise ValueError(x)
        return f

    def integer(x):
        # simdjson's big-integer error: outside int64/uint64.
        if not -(1 << 63) <= int(x) < (1 << 64):
            raise ValueError(x)
        return int(x)

    def no_const(x):
        raise ValueError(x)

    def clean(v):
        if isinstance(v, str):
            return not any("\ud800" <= c <= "\udfff" for c in v)
        if isinstance(v, list):
            return all(clean(x) for x in v)
        if isinstance(v, dict):
            return all(clean(k) and clean(x) for k, x in v.items())
        return True
    try:
        v = json.loads(text.decode("utf-8"), parse_float=num, parse_int=integer,
                       parse_constant=no_const)
    except ValueError:
        return False
    return clean(v)


def gen_bulk_spec(r):
    """A large NDJSON-like input (64 KB to about 3 MB) as a spec of documents."""
    target = r.choice([70 << 10, 100 << 10, 300 << 10, 1 << 20, 3 << 20])
    docs = []
    size = 0
    clean = r.random() < 0.6
    p_pretty = r.choice([0, 0, 0.02, 0.1]) if not clean else r.choice([0, 0, 0.001])
    p_odd = r.choice([0, 0.01, 0.03]) if not clean else r.choice([0, 0, 0.001])
    p_long = r.choice([0, 0, 0.002])
    error_at = r.choice([None, None, None, r.random()])
    while size < target:
        k = r.random()
        if error_at is not None and size > error_at * target:
            docs.append(("raw", r.choice(BULK_ERRORS)))
            error_at = None
            continue
        if k < p_odd:
            d = ("raw", r.choice(BULK_LINES))
        elif k < p_odd + p_pretty:
            d = ("raw", ser(gen_value(r, 2), "pretty", r))
            if any(bad in d[1] for bad in RARE_STRS):
                continue
        elif k < p_odd + p_pretty + p_long:
            d = ("raw", b'{"s":"' + b"x" * r.choice([4000, 4095, 4096, 5000, 70000]) + b'"}')
        else:
            d = gen_value(r, r.choice([0, 1, 2, 2, 3]))
            text = ser(d, "compact", r)
            if any(bad in text for bad in RARE_STRS):
                continue  # lone surrogates are parse errors: only where intended
            if clean and not fast_path_ok(text):
                continue
        docs.append(d)
        size += len(ser(d, "compact", r)) + 1
    return {"docs": docs, "style": "compact",
            "sep": r.choice([b"\n", b"\n", b"\n", b"\r\n", b"\n\n"]),
            "trail": r.choice([b"\n", b"\n", b""]), "seed": r.randrange(1 << 30)}


# -- environment ------------------------------------------------------------------

TZS = ["UTC", "Asia/Kolkata", "Australia/Lord_Howe", "America/St_Johns", "Europe/London",
       "Pacific/Chatham", "Foo/Bar", "", "EST5EDT", "<+0330>-3:30", "UTC0", ":America/New_York",
       "America/New_York", "Asia/Tokyo", "Europe/Dublin", "Africa/Casablanca", "Etc/GMT+12",
       "Pacific/Kiritimati", "America/Sao_Paulo", "Antarctica/Troll"]
ENV_PROGRAMS = [
    "$ENV | keys", "env | keys", "$ENV.X", "env.X", "[$ENV.TZ, env.TZ]", "$ENV | length",
    "localtime | mktime", "gmtime | mktime", "localtime | todate", "strflocaltime(\"%c %Z %z\")",
    "localtime | strftime(\"%Z %z %s\")", "gmtime | strflocaltime(\"%H %Z\")", "mktime?",
    "todate", "[localtime, gmtime]", "strptime(\"%Y-%m-%dT%H:%M:%SZ\") | mktime",
    "localtime | .[8]?", "$ENV | tojson | length", "env | to_entries | map(.key) | sort",
    "$ENV.EMPTY", "$ENV[\"É\"]", "[$ENV.A, $ENV.B]", "$__loc__", "input_filename",
]
ENV_INPUTS = ["0", "1425599621", "-1", "1e10", "1709251200", "1710054000", "1699164000",
              "-62135596800", "\"2015-03-05T23:51:47Z\"", "[2015,2,5,23,51,47,4,63]", "86400.5",
              "1719792000", "1735689599", "null"]


LOCALES = ["C", "POSIX", "en_US.UTF-8", "fr_FR.UTF-8", "de_DE.UTF-8", "ja_JP.UTF-8",
           "ru_RU.UTF-8", "tr_TR.UTF-8", "fr_FR.ISO8859-1", "ja_JP.SJIS", "xx_YY.UTF-8", "",
           "en_US", "de_DE.ISO8859-15", "zh_CN.UTF-8", "C.UTF-8"]
LOCALE_VARS = ["LC_ALL", "LC_ALL", "LANG", "LC_TIME", "LC_CTYPE", "LC_MESSAGES", "LC_NUMERIC"]
LOCALE_PROGRAMS = [
    "strftime(\"%c\")", "strftime(\"%a %A %b %B %p\")", "strftime(\"%x %X\")",
    "strftime(\"%Ec %Ex %EX %Oy\")", "strflocaltime(\"%c %Z\")", "todate", "gmtime | todate",
    "strftime(\"%A\") | ascii_downcase", "strptime(\"%a %b %d %Y\")?",
    "\"jeudi 5 mars 2015\" | strptime(\"%A %d %B %Y\")?", "\"Donnerstag\" | strptime(\"%A\")?",
    "input_filename", "tostring", "tojson", "@text", "ascii_downcase", "test(\"\\\\w\")?",
    "\"éÉ\" | ascii_upcase", "[.] | @sh", "1.5 | tostring", "\"1,5\" | tonumber?", "$ENV.LANG",
]


def gen_env(r):
    env = {}
    if r.random() < 0.35:
        env[r.choice(LOCALE_VARS)] = r.choice(LOCALES)
        if r.random() < 0.3:
            env[r.choice(LOCALE_VARS)] = r.choice(LOCALES)
        if "LC_ALL" not in env:
            env["LC_ALL"] = None  # unset the harness's LC_ALL=C so the others count
    if r.random() < 0.8:
        env["TZ"] = r.choice(TZS)
    for _ in range(r.choice([0, 1, 2])):
        k, v = r.choice([("X", "1"), ("X", "a=b"), ("EMPTY", ""), ("É", "é"), ("A", "😀"),
                         ("B", " spaced "), ("X", "\t"), ("LONG", "x" * 300), ("A", "[1]")])
        env[k] = v
    return env

