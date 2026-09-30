#!/usr/bin/env python3
"""Generate modules.toml: jq_diff CLI cases for linker.c — chains of imports,
the order they are loaded and bound in, reuse, data imports and the errors
(format: tests/jq_diff/cli.rs).

  python3 tests/jq_compat/corpus/gen_modules.py

qj links with a loop where jq's `load_library` and `process_dependencies`
recurse (see `docs/COMPATIBILITY.md`), so these cases pin the order down: a
module is recorded only after its own imports are loaded, imports bind last
first, and an error stops exactly what jq's stops.

The chains here are tens of modules, not the ~20,000 it takes to overflow jq's
stack: a case that long would mean that many files. `tests/compat_mode.rs`
covers the threshold, on a small stack so the chain stays short.
"""

import os

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "modules.toml")

CASES = []


def case(name, files, args, note=None):
    CASES.append((name, files, args, note))


L = ["-L", "."]


def chain(n, body="def f: %(i)d + m::f;", leaf="def f: %(i)d;"):
    """`n` modules, m0 importing m1 … importing m{n-1}."""
    files = {}
    for i in range(n):
        if i + 1 < n:
            files["m%d.jq" % i] = 'import "m%d" as m;\n%s\n' % (i + 1, body % {"i": i})
        else:
            files["m%d.jq" % i] = "%s\n" % (leaf % {"i": i})
    return files


# --- chains: every module in one, and the sum it computes ------------------
for n in (1, 2, 3, 12, 40):
    case(
        "chain-%d" % n,
        chain(n),
        L + ["-nc", 'include "m0"; f'],
        "a chain of %d modules: jq recurses once per module" % n,
    )

# The same chain reached by `import`, so every level is namespaced.
case(
    "chain-12-namespaced",
    chain(12),
    L + ["-nc", 'import "m0" as m; m::f'],
)

# A chain whose modules are all recorded in lib_state, then all bound: the
# deepest is loaded first, so `get_search_list` and the defs come back in jq's
# order.
case(
    "chain-12-defs-order",
    chain(12, body='def f: %(i)d + m::f;\ndef g%(i)d: "%(i)d";'),
    L + ["-nc", 'include "m0"; [f, g0, g5]'],
)

# --- order: imports bind last first ---------------------------------------
case(
    "two-imports-order",
    {"a.jq": 'def f: "a";\n', "b.jq": 'def f: "b";\n'},
    L + ["-nc", 'include "a"; include "b"; f'],
)
case(
    "two-imports-order-swapped",
    {"a.jq": 'def f: "a";\n', "b.jq": 'def f: "b";\n'},
    L + ["-nc", 'include "b"; include "a"; f'],
)
case(
    "module-def-sees-its-own",
    {"a.jq": 'def f: "a";\ndef g: f;\n', "b.jq": 'def f: "b";\n'},
    L + ["-nc", 'include "b"; include "a"; [f, g]'],
)

# --- a module reached twice is loaded once --------------------------------
case(
    "diamond",
    {
        "top.jq": 'import "left" as l;\nimport "right" as r;\ndef t: [l::v, r::v];\n',
        "left.jq": 'import "base" as b;\ndef v: ["l", b::v];\n',
        "right.jq": 'import "base" as b;\ndef v: ["r", b::v];\n',
        "base.jq": 'def v: "base";\n',
    },
    L + ["-nc", 'include "top"; t'],
)
case(
    "same-module-two-names",
    {"a.jq": "def f: 1;\n"},
    L + ["-nc", 'import "a" as x; import "a" as y; [x::f, y::f]'],
)
case(
    "include-and-import-the-same",
    {"a.jq": "def f: 1;\n"},
    L + ["-nc", 'include "a"; import "a" as a; [f, a::f]'],
)

# --- data imports, which never recurse ------------------------------------
case("data-import", {"d.json": '{"x":[1,2]}\n'}, L + ["-nc", 'import "d" as $d; $d'])
case(
    "data-import-namespaced",
    {"d.json": '{"x":1}\n'},
    L + ["-nc", 'import "d" as $d; $d::d'],
)
case(
    "data-import-raw",
    {"d.json": "not json\n"},
    L + ["-nc", 'import "d" as $d {"raw": true}; $d'],
)
case("data-import-bad-json", {"d.json": "{oops\n"}, L + ["-nc", 'import "d" as $d; $d'])
case(
    "data-import-in-a-chain",
    {
        "m0.jq": 'import "m1" as m;\ndef f: m::f;\n',
        "m1.jq": 'import "d" as $d;\ndef f: $d;\n',
        "d.json": "[1,2,3]\n",
    },
    L + ["-nc", 'include "m0"; f'],
)

# --- errors ---------------------------------------------------------------
case("missing-module", {}, L + ["-nc", 'import "nope" as n; 1'])
case(
    "missing-module-optional",
    {},
    L + ["-nc", 'include "nope" {"optional": true}; 1'],
)
case(
    "missing-after-a-good-one",
    {"a.jq": "def f: 1;\n"},
    L + ["-nc", 'include "a"; import "nope" as n; f'],
    "jq returns 1 from process_dependencies, whatever it counted before",
)
case(
    "missing-before-a-good-one",
    {"a.jq": "def f: 1;\n"},
    L + ["-nc", 'import "nope" as n; include "a"; f'],
)
case("two-missing", {}, L + ["-nc", 'import "nope1" as a; import "nope2" as b; 1'])
case(
    "missing-deep-in-a-chain",
    {
        "m0.jq": 'import "m1" as m;\ndef f: 1;\n',
        "m1.jq": 'import "nope" as n;\ndef f: 1;\n',
    },
    L + ["-nc", 'include "m0"; f'],
)
case(
    "missing-deep-then-another-import",
    {
        "m0.jq": 'import "m1" as m;\nimport "a" as a;\ndef f: 1;\n',
        "m1.jq": 'import "nope" as n;\ndef f: 1;\n',
        "a.jq": "def g: 1;\n",
    },
    L + ["-nc", 'include "m0"; f'],
)
case("syntax-error-module", {"bad.jq": "def f: ;\n"}, L + ["-nc", 'include "bad"; 1'])
case(
    "syntax-error-module-then-a-good-one",
    {"bad.jq": "def f: ;\n", "a.jq": "def g: 1;\n"},
    L + ["-nc", 'include "bad"; include "a"; g'],
    "jq records the module that didn't parse, and skips block_bind_self",
)
case(
    "syntax-error-deep-in-a-chain",
    {"m0.jq": 'import "m1" as m;\ndef f: 1;\n', "m1.jq": "def f: ;\n"},
    L + ["-nc", 'include "m0"; f'],
)
case(
    "module-with-an-import-that-does-not-parse",
    {
        "m0.jq": 'import "m1" as m;\ndef f: m::f;\n',
        "m1.jq": 'import "m2" as m;\ndef f: 1;\n',
        "m2.jq": "def f: (;\n",
    },
    L + ["-nc", 'include "m0"; f'],
)
case("undefined-after-an-import", {"a.jq": "def f: 1;\n"}, L + ["-nc", 'import "a" as a; nosuch'])
case("relpath-with-dotdot", {}, L + ["-nc", 'import "../a" as a; 1'])
case("relpath-absolute", {}, L + ["-nc", 'import "/a" as a; 1'])

# --- search paths --------------------------------------------------------
case(
    "search-path-order",
    {"one/a.jq": 'def f: "one";\n', "two/a.jq": 'def f: "two";\n'},
    ["-nc", "-L", "one", "-L", "two", 'include "a"; f'],
)
case(
    "search-path-order-reversed",
    {"one/a.jq": 'def f: "one";\n', "two/a.jq": 'def f: "two";\n'},
    ["-nc", "-L", "two", "-L", "one", 'include "a"; f'],
)
case("module-in-its-own-directory", {"a/a.jq": "def f: 1;\n"}, L + ["-nc", 'include "a"; f'])
case(
    "import-relative-to-the-importing-module",
    {"sub/a.jq": 'import "b" as b;\ndef f: b::g;\n', "sub/b.jq": "def g: 7;\n"},
    L + ["-nc", 'include "sub/a"; f'],
)
case(
    "import-with-a-search-path-of-its-own",
    {"x/a.jq": "def f: 9;\n"},
    L + ["-nc", 'include "a" {"search": "x"}; f'],
)

# --- modulemeta ---------------------------------------------------------
case(
    "modulemeta-of-a-module-with-imports",
    {
        "a.jq": 'module {"x": 1};\nimport "b" as b;\ndef f: 1;\ndef g(a): a;\n',
        "b.jq": "def h: 2;\n",
    },
    L + ["-c", "-n", '"a" | modulemeta'],
    "modulemeta lists the imports without loading them",
)
case("modulemeta-missing", {}, L + ["-c", "-n", '"nope" | modulemeta'])
case("modulemeta-syntax-error", {"bad.jq": "def f: ;\n"}, L + ["-c", "-n", '"bad" | modulemeta'])
case(
    "modulemeta-of-a-chain-head",
    chain(12),
    L + ["-c", "-n", '"m0" | modulemeta | .deps'],
)


def quote(s):
    """A TOML basic string."""
    out = s.replace("\\", "\\\\").replace('"', '\\"')
    return '"%s"' % out.replace("\n", "\\n")


def inline_files(files):
    return "{ " + ", ".join("%s = %s" % (quote(k), quote(v)) for k, v in files.items()) + " }"


# An inline table has to be one line, so a case with more files than this gets
# a `[case.files]` table instead.
MAX_INLINE_FILES = 4


lines = [
    "# jq_diff CLI cases for linker.c: chains of imports, the order they load and",
    "# bind in, reuse, data imports and the errors.",
    "#",
    "# GENERATED by tests/jq_compat/corpus/gen_modules.py — do not edit by hand.",
    "",
]
for name, files, args, note in CASES:
    lines.append("[[case]]")
    lines.append("name = %s" % quote(name))
    if note:
        lines.append("note = %s" % quote(note))
    lines.append("args = [%s]" % ", ".join(quote(a) for a in args))
    if files:
        if len(files) > MAX_INLINE_FILES:
            lines.append("")
            lines.append("[case.files]")
            for k, v in files.items():
                lines.append("%s = %s" % (quote(k), quote(v)))
        else:
            lines.append("files = %s" % inline_files(files))
    lines.append("")

with open(OUT, "w") as f:
    f.write("\n".join(lines))
print("wrote %s: %d cases" % (OUT, len(CASES)))
