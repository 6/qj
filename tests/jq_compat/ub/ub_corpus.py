#!/usr/bin/env python3
"""Programs that reach one of jq 1.8.1's three undefined behaviors.

- dels:   jv_dels' slice-delete error path frees the key twice
          (jv_aux.c: parse_slice consumes `key`, then `jv_free(key)`).
- trace:  --debug-trace=all of an instruction that runs with an empty data
          stack reads `*stack_block_next(&jq->stk, 0)`, 4 bytes at the top
          of the stack allocation that nothing ever wrote (execute.c).
- lgamma: `int value;` is uninitialized in LIBM_DA(lgamma_r) and a libm
          that returns early for 0, -0, nan and +-inf never writes it.

Each case is {"id", "family", "args", "stdin"}; `args` excludes argv[0].
The corpus is deterministic (no randomness), so every OS runs the same list.
"""
import itertools
import json
import sys


def dels():
    cases = []
    bad_keys = [
        "{}",
        '{"start":1}',
        '{"end":1}',
        '{"start":"x"}',
        '{"start":1,"end":"x"}',
        '{"a":1}',
        '{"start":[],"end":[]}',
        '{"start":1,"b":2,"c":3,"d":4,"e":5,"f":6,"g":7,"h":8,"i":9}',
        '{"end":' + '"' + "x" * 300 + '"' + "}",
    ]
    inputs = ["[]", "[1]", "[1,2]", "[1,2,3]", "[1,2,3,4]", "[range(5)]", "[range(8)]",
              "[range(9)]", "[range(16)]", "[range(17)]", "[range(100)]",
              '["a","b"]', "[{},{}]", "[[1],[2]]", "[null]"]
    # 1. the basic shape, try/catch, over inputs x keys.
    for inp, key in itertools.product(inputs, bad_keys):
        cases.append(["-nc", f"{inp} | try delpaths([[{key}]]) catch ."])
    # 2. without try (the error is uncaught).
    for inp, key in itertools.product(["[1]", "[1,2]", "[1,2,3]", "[range(10)]"], bad_keys[:5]):
        cases.append(["-nc", f"{inp} | delpaths([[{key}]])"])
    # 3. the key made at run time rather than a folded constant.
    runtime = [
        "({} | .)",
        "({} + {})",
        '("{}" | fromjson)',
        '({"start":1} | .)',
        '({"start":1} | del(.x))',
        '([1] | {start: .[0]})',
        '({} | .a = 1 | del(.a))',
    ]
    for inp, key in itertools.product(["[1]", "[1,2]", "[1,2,3]", "[range(6)]"], runtime):
        cases.append(["-nc", f"{inp} | try delpaths([[{key}]]) catch ."])
    # 4. the key bound to a variable, or from --argjson.
    for inp, key in itertools.product(["[1]", "[1,2]", "[1,2,3]"], bad_keys[:4]):
        cases.append(["-nc", f"{key} as $k | {inp} | try delpaths([[$k]]) catch ."])
        cases.append(["-nc", "--argjson", "k", key, f"{inp} | try delpaths([[$k]]) catch ."])
    # 5. other paths around the bad one.
    others = [
        "[[0],[{K}]]", "[[{K}],[0]]", "[[{K}],[{K}]]", "[[1],[{K}],[2]]",
        '[[{K}],["a"]]', "[[-1],[{K}]]", '[[{"start":0,"end":1}],[{K}]]',
    ]
    for inp, shape, key in itertools.product(["[1]", "[1,2]", "[1,2,3]"], others, ["{}", '{"start":1}']):
        cases.append(["-nc", f"{inp} | try delpaths({shape.replace('{K}', key)}) catch ."])
    # 6. deeper: the slice key one level down.
    for inp, key in itertools.product(["[[1]]", "[[1,2]]", "[[1,2],[3]]", "[[range(5)]]"], bad_keys[:4]):
        cases.append(["-nc", f"{inp} | try delpaths([[0,{key}]]) catch ."])
    # 7. what runs before and after.
    before = ["[range(100)] | length | ", '"x" * 1000 | length | ', "[range(20)|tostring] | length | ",
              "{a:1,b:2} | keys | ", "reduce range(50) as $i ({}; .[$i|tostring] = $i) | length | "]
    after = [", 1", ", ([range(1000)] | length)", ", ({} | tojson)", " | length", ", ([range(10)] | map(tostring))"]
    for b, key in itertools.product(before, ["{}", '{"start":1}']):
        cases.append(["-nc", f"{b}[1,2] | try delpaths([[{key}]]) catch ."])
    for a, key in itertools.product(after, ["{}", '{"start":1}']):
        cases.append(["-nc", f"([1,2] | try delpaths([[{key}]]) catch .){a}"])
        cases.append(["-nc", f"([1,2,3] | try delpaths([[{key}]]) catch .){a}"])
    # 8. inside other constructs.
    wraps = ["[{E}]", "[{E}, {E}]", "({E}) as $x | $x", "first({E})", "[limit(1; {E})]",
             "reduce ({E}) as $x (null; $x)", "if true then {E} else 0 end", "def f: {E}; f"]
    for w, key in itertools.product(wraps, ["{}", '{"start":1}']):
        e = f"([1,2] | try delpaths([[{key}]]) catch .)"
        cases.append(["-nc", w.replace("{E}", e)])
    # 9. over several inputs: the key is a program constant, so later inputs
    # run on freed memory.
    stdins = ["[1] {}", "[1] [1]", "[1] [1,2]", "[1,2] [1,2]", "[1] 1", "[1] {} {}", "[] []", "[1] null"]
    progs = ['delpaths([[{"start":1}],[0]])', "delpaths([[{}]])", 'try delpaths([[{"start":1}],[0]]) catch .',
             "try delpaths([[{}]]) catch .", 'delpaths([[{"start":1}]])', "try delpaths([[0],[{}]]) catch ."]
    for s, p in itertools.product(stdins, progs):
        cases.append({"args": ["-c", p], "stdin": s})
    # 10. output option variety.
    for opts in (["-n"], ["-nr"], ["-nj"], ["-nS"], ["-n", "--tab"]):
        for key in ["{}", '{"start":1}']:
            cases.append(opts + [f"[1,2] | try delpaths([[{key}]]) catch ."])
    out = []
    for c in cases:
        if isinstance(c, dict):
            out.append(c)
        else:
            out.append({"args": c, "stdin": None})
    return out


def trace():
    cases = []
    bodies = [
        "[.[] | . * 2]", "[.[]]", "[.[] | tostring]", "reduce .[] as $x (0; . + $x)",
        "foreach .[] as $x (0; . + $x)", "first(.[])", "[limit(2; .[])]", "[paths]",
        "[.. | numbers]", "label $f | .[] | ., break $f", "[.[] | select(. > 1)]",
        "map(. + 1)", "add", "[range(3)]", "[.[] as $x | $x]", "(. as [$a] | $a)",
        "[recurse | numbers]", "to_entries", "[.[] | {a: .}]", "try error(\"x\") catch .",
        "[.[] | if . > 1 then . else empty end]", "length", ".", "[.[], .[]]",
        "def f(x): [x]; f(.[])", "[.[] | [.]]", "[splits(\"a\")?]", "any", "all",
        "[.[] as $a | .[] as $b | $a + $b]",
    ]
    inputs = ["[1,2]", "[1,2,3]", "[]", "[1]", '["a","b"]', "[[1],[2]]", "[range(10)]", '[1,"a",null]']
    for b, i in itertools.product(bodies, inputs):
        if i.startswith("[range"):
            cases.append({"args": ["-nc", "--debug-trace=all", f"{i} | {b}"], "stdin": None})
        else:
            cases.append({"args": ["-c", "--debug-trace=all", b], "stdin": i})
    # Frame sizes: more top-level variables and closures make the first
    # stack allocation bigger (a different malloc size class).
    for n in [1, 2, 4, 8, 16, 32, 64]:
        binds = " | ".join(f". as $v{k}" for k in range(n))
        cases.append({"args": ["-c", "--debug-trace=all", f"{binds} | [.[] | . * 2]"], "stdin": "[1,2]"})
        defs = " ".join(f"def f{k}: .;" for k in range(n))
        cases.append({"args": ["-c", "--debug-trace=all", f"{defs} [.[] | . * 2]"], "stdin": "[1,2]"})
    # What the compiler allocated and freed before the run.
    for n in [0, 10, 100, 1000]:
        pre = "".join(f"def g{k}: {k};" for k in range(n))
        cases.append({"args": ["-c", "--debug-trace=all", f"{pre} [.[] | . * 2]"], "stdin": "[1,2]"})
    for extra in ["--arg", "--argjson"]:
        for n in [1, 5, 50]:
            args = ["-c", "--debug-trace=all"]
            for k in range(n):
                args += [extra, f"a{k}", "1"]
            args.append("[.[] | . * 2]")
            cases.append({"args": args, "stdin": "[1,2]"})
    return cases


def lgamma():
    cases = []
    xs = ["0", "-0", "nan", "infinite", "-infinite", "(0*1)", "(-0.0)", "(1e-400)", "(-1e-400)",
          "(nan|-.)", "(infinite|-.)", "(1/0?)", "(0|-.)", "0.0", "-0.0", "(1e400)"]
    for x in xs:
        cases.append(["-nc", f"{x} | lgamma_r"])
        cases.append(["-nc", f"[{x}] | map(lgamma_r)"])
        cases.append(["-nc", f"{x} | lgamma_r | .[1]"])
    before = ["[range(10)] | length", '"abc" | test("b")', "now | type", "[1,2] | sort",
              "{a:1} | tojson", "1 | lgamma_r", "2.5 | lgamma_r", "-2.5 | lgamma_r", "[.] | length",
              "(1 | frexp)", "(1.5 | modf)", "\"x\" | ascii_downcase", "[range(1000)] | add", "input_line_number"]
    for b, x in itertools.product(before, ["0", "-0", "nan", "infinite"]):
        cases.append(["-nc", f"({b}) as $_ | {x} | lgamma_r"])
        cases.append(["-nc", f"{b}, ({x} | lgamma_r)"])
    for inp in ["[0]", "[0,-0]", "[0,1,2]", "[1,0]", "[0,0,0,0]", '[0,"a"]']:
        cases.append({"args": ["-c", "[.[] | numbers | lgamma_r]"], "stdin": inp})
        cases.append({"args": ["-c", ".[] | numbers | lgamma_r"], "stdin": inp})
    for n in [1, 2, 3, 10]:
        cases.append(["-nc", f"[range({n}) | 0 | lgamma_r]"])
        cases.append(["-nc", f"reduce range({n}) as $i (null; 0 | lgamma_r)"])
    for w in ["[{E}]", "first({E})", "try {E} catch .", "def f: {E}; f", "{E} as $x | $x",
              "if true then {E} else 1 end", "[{E}, {E}]", "({E}) | tojson"]:
        cases.append(["-nc", w.replace("{E}", "(0 | lgamma_r)")])
    out = []
    for c in cases:
        out.append(c if isinstance(c, dict) else {"args": c, "stdin": None})
    return out


def corpus():
    all_cases = []
    for family, gen in (("dels", dels), ("trace", trace), ("lgamma", lgamma)):
        for n, c in enumerate(gen()):
            c["family"] = family
            c["id"] = f"{family}/{n}"
            all_cases.append(c)
    return all_cases


if __name__ == "__main__":
    cs = corpus()
    json.dump(cs, sys.stdout, indent=0)
    counts = {}
    for c in cs:
        counts[c["family"]] = counts.get(c["family"], 0) + 1
    print(counts, file=sys.stderr)
