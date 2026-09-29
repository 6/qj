"""Random jq programs for differential testing of the compiler (tests/jq_compile.rs).

Usage: python3 tests/jq_compile/gen_programs.py COUNT [SEED] > programs.jsonl, then
QJ_EXTRA=programs.jsonl cargo test --release --test jq_compile disasm_vs_live_jq -- --ignored.
Programs mix every construct of the grammar, bound and unbound names, shadowed
builtins, and nesting, so both disassembly and compile errors get exercised.
"""
import json, random, sys

BUILTINS = [
    ("length", 0), ("keys", 0), ("map", 1), ("select", 1), ("empty", 0), ("not", 0),
    ("add", 0), ("add", 1), ("range", 1), ("range", 2), ("range", 3), ("path", 1),
    ("paths", 0), ("paths", 1), ("first", 0), ("first", 1), ("last", 0), ("last", 1),
    ("limit", 2), ("until", 2), ("while", 2), ("repeat", 1), ("recurse", 0), ("recurse", 1),
    ("recurse", 2), ("tostring", 0), ("tojson", 0), ("fromjson", 0), ("type", 0),
    ("error", 0), ("error", 1), ("has", 1), ("in", 1), ("inside", 1), ("contains", 1),
    ("sort", 0), ("sort_by", 1), ("group_by", 1), ("unique_by", 1), ("min_by", 1),
    ("to_entries", 0), ("from_entries", 0), ("with_entries", 1), ("test", 1), ("test", 2),
    ("match", 1), ("capture", 1), ("sub", 2), ("sub", 3), ("gsub", 2), ("splits", 1),
    ("split", 1), ("split", 2), ("join", 1), ("ascii_downcase", 0), ("ltrimstr", 1),
    ("tostream", 0), ("fromstream", 1), ("getpath", 1), ("setpath", 2), ("delpaths", 1),
    ("del", 1), ("to_entries", 0), ("walk", 1), ("env", 0), ("input", 0), ("inputs", 0),
    ("debug", 0), ("debug", 1), ("isempty", 1), ("any", 0), ("all", 1), ("any", 2),
    ("flatten", 0), ("flatten", 1), ("indices", 1), ("index", 1), ("combinations", 0),
    ("IN", 1), ("INDEX", 2), ("pick", 1), ("abs", 0), ("toarray", 0), ("splits", 2),
    ("getpath", 1), ("halt", 0), ("input_filename", 0), ("builtins", 0), ("infinite", 0),
    ("nan", 0), ("floor", 0), ("pow", 2), ("ltrimstr", 1), ("trim", 0), ("implode", 0),
    ("strptime", 1), ("todate", 0), ("now", 0), ("modulemeta", 0), ("get_search_list", 0),
    ("significand", 0), ("ldexp", 2), ("@base64d", -1), ("nosuchfn", 0), ("map", 2),
]
FORMATS = ["@text", "@json", "@html", "@uri", "@csv", "@tsv", "@sh", "@base64", "@base64d"]
KEYWORDS = ["if", "then", "reduce", "and", "or", "def", "as", "label", "end", "try"]


class Gen:
    def __init__(self, rng):
        self.r = rng
        self.vars = []      # bound variable names
        self.funcs = []     # (name, arity) of user functions in scope
        self.params = []    # closure params in scope
        self.labels = []

    def choice(self, xs):
        return self.r.choice(xs)

    def var(self):
        if self.vars and self.r.random() < 0.85:
            return "$" + self.choice(self.vars)
        return self.choice(["$__loc__", "$ENV", "$unbound", "$ARGS", "$__prog_args"])

    def lit(self):
        return self.choice(["1", "0", "-1", "1.5", "1.000", "1e3", "100000000000000000001",
                            "null", "true", "false", '"a"', '"b\\n"', "[]", "{}", '"\\u00e9"'])

    def string(self, d):
        parts = []
        for _ in range(self.r.randint(0, 3)):
            if self.r.random() < 0.5:
                parts.append(self.choice(["x", "y z", "\\t", "\\\"", "é"]))
            else:
                parts.append("\\(" + self.expr(d - 1) + ")")
        s = '"' + "".join(parts) + '"'
        if self.r.random() < 0.2:
            s = self.choice(FORMATS) + " " + s
        return s

    def pattern(self, d, names):
        k = self.r.random()
        if d <= 0 or k < 0.5:
            n = self.choice(["a", "b", "c", "x"])
            names.append(n)
            return "$" + n
        if k < 0.75:
            return "[" + ", ".join(self.pattern(d - 1, names) for _ in range(self.r.randint(1, 3))) + "]"
        entries = []
        for _ in range(self.r.randint(1, 3)):
            kind = self.r.random()
            if kind < 0.3:
                n = self.choice(["a", "b", "c"])
                names.append(n)
                entries.append("$" + n)
            elif kind < 0.5:
                entries.append(self.choice(["a", "b", "if"]) + ": " + self.pattern(d - 1, names))
            elif kind < 0.65:
                entries.append(self.string(1) + ": " + self.pattern(d - 1, names))
            elif kind < 0.8:
                n = self.choice(["a", "b"])
                names.append(n)
                entries.append("$" + n + ": " + self.pattern(d - 1, names))
            else:
                entries.append("(" + self.expr(1) + "): " + self.pattern(d - 1, names))
        return "{" + ", ".join(entries) + "}"

    def call(self, d):
        cands = [(f, a) for f, a in self.funcs] + [(p, 0) for p in self.params]
        if cands and self.r.random() < 0.4:
            name, arity = self.choice(cands)
        else:
            name, arity = self.choice(BUILTINS)
        if arity == -1:
            return name
        if arity == 0:
            return name
        return name + "(" + "; ".join(self.expr(d - 1) for _ in range(arity)) + ")"

    def term(self, d):
        if d <= 0:
            return self.choice([".", ".a", ".[0]", self.lit(), self.var(), "..", ".b?"])
        k = self.r.randint(0, 26)
        if k == 0: return "."
        if k == 1: return self.lit()
        if k == 2: return self.var()
        if k == 3: return self.term(d - 1) + self.choice([".a", ".b", "[0]", "[]", "[]?", "?", ".[1:]", "[:2]", '."c"', "[.x]", "[1:2]?"])
        if k == 4: return "(" + self.query(d - 1) + ")"
        if k == 5: return "[" + self.query(d - 1) + "]"
        if k == 6: return "[]"
        if k == 7: return self.obj(d)
        if k == 8: return self.string(d)
        if k == 9: return self.call(d)
        if k == 10: return "-" + self.term(d - 1)
        if k == 11: return "try " + self.expr(d - 1) + (" catch " + self.expr(d - 1) if self.r.random() < 0.5 else "")
        if k == 12:
            s = "if " + self.query(d - 1) + " then " + self.query(d - 1)
            for _ in range(self.r.randint(0, 2)):
                s += " elif " + self.query(d - 1) + " then " + self.query(d - 1)
            if self.r.random() < 0.6:
                s += " else " + self.query(d - 1)
            return s + " end"
        if k == 13:
            names = []
            pats = " ?// ".join(self.pattern(2, names) for _ in range(self.r.randint(1, 2)))
            saved = list(self.vars)
            src = self.term(d - 1)
            init = self.query(d - 1)
            self.vars += names
            upd = self.query(d - 1)
            self.vars = saved
            return "reduce " + src + " as " + pats + " (" + init + "; " + upd + ")"
        if k == 14:
            names = []
            pats = " ?// ".join(self.pattern(2, names) for _ in range(self.r.randint(1, 2)))
            saved = list(self.vars)
            src = self.term(d - 1)
            init = self.query(d - 1)
            self.vars += names
            upd = self.query(d - 1)
            ext = ("; " + self.query(d - 1)) if self.r.random() < 0.5 else ""
            self.vars = saved
            return "foreach " + src + " as " + pats + " (" + init + "; " + upd + ext + ")"
        if k == 15: return self.choice(FORMATS)
        if k == 16:
            if self.labels and self.r.random() < 0.8:
                return "break $" + self.choice(self.labels)
            return "break $nolabel"
        if k == 17: return "$__loc__"
        if k == 18: return ".[" + self.query(d - 1) + "]"
        if k == 19: return self.term(d - 1) + "[" + self.query(d - 1) + ":" + self.query(d - 1) + "]"
        if k == 20: return "{" + self.choice(["a", "$__loc__", '"b"', "if"]) + "}"
        if k == 21: return self.term(d - 1) + "." + self.string(1)
        if k == 22: return "$$$$" + (self.choice(self.vars) if self.vars else "v")
        return self.call(d)

    def obj(self, d):
        pairs = []
        for _ in range(self.r.randint(0, 3)):
            k = self.r.randint(0, 9)
            if k == 0: pairs.append(self.choice(["a", "b", "if", "and"]) + ": " + self.expr(d - 1))
            elif k == 1: pairs.append(self.string(1) + ": " + self.expr(d - 1))
            elif k == 2: pairs.append(self.string(1))
            elif k == 3: pairs.append(self.var() + ": " + self.expr(d - 1))
            elif k == 4: pairs.append(self.var())
            elif k == 5: pairs.append(self.choice(["a", "b", "then"]))
            elif k == 6: pairs.append("$__loc__")
            elif k == 7: pairs.append("(" + self.query(d - 1) + "): " + self.expr(d - 1))
            elif k == 8: pairs.append(self.choice(["a", "b"]) + ": " + self.expr(d - 1) + " | " + self.expr(d - 1))
            else: pairs.append('"k": ' + self.lit())
        return "{" + ", ".join(pairs) + "}"

    def expr(self, d):
        if d <= 0:
            return self.term(0)
        k = self.r.random()
        if k < 0.45:
            return self.term(d)
        op = self.choice(["+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">=", "and", "or",
                          "//", "=", "|=", "+=", "-=", "*=", "/=", "%=", "//="])
        return self.term(d - 1) + " " + op + " " + self.term(d - 1)

    def query(self, d):
        if d <= 0:
            return self.expr(0)
        k = self.r.random()
        if k < 0.35:
            return self.expr(d)
        if k < 0.5:
            return self.expr(d - 1) + " | " + self.query(d - 1)
        if k < 0.62:
            return self.expr(d - 1) + ", " + self.query(d - 1)
        if k < 0.74:
            names = []
            pats = " ?// ".join(self.pattern(2, names) for _ in range(self.r.randint(1, 3)))
            src = self.term(d - 1)
            saved = list(self.vars)
            self.vars += names
            body = self.query(d - 1)
            self.vars = saved
            return src + " as " + pats + " | " + body
        if k < 0.82:
            n = self.choice(["out", "f", "l"])
            saved = list(self.labels)
            self.labels.append(n)
            body = self.query(d - 1)
            self.labels = saved
            return "label $" + n + " | " + body
        # def
        name = self.choice(["f", "g", "h", "map", "empty", "length", "_plus", "select"])
        nparams = self.r.randint(0, 2)
        params = []
        for i in range(nparams):
            params.append(self.choice(["$p", "q", "$r", "s"]) + str(i))
        saved = (list(self.vars), list(self.params), list(self.funcs))
        for p in params:
            if p.startswith("$"):
                self.vars.append(p[1:])
                self.params.append(p[1:])
            else:
                self.params.append(p)
        self.funcs.append((name, nparams))
        body = self.query(d - 1)
        self.vars, self.params, self.funcs = saved
        self.funcs.append((name, nparams))
        rest = self.query(d - 1)
        self.funcs = saved[2]
        head = "def " + name + ("(" + "; ".join(params) + ")" if params else "") + ": "
        return head + body + "; " + rest


def main():
    n = int(sys.argv[1])
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    rng = random.Random(seed)
    for i in range(n):
        g = Gen(rng)
        prog = g.query(rng.randint(1, 5))
        print(json.dumps({"origin": "rand:%d:%d" % (seed, i), "src": prog}))


main()
