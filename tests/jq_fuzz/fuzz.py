#!/usr/bin/env python3
"""Differential fuzzer: the qj binary against jq 1.8.1.

Every case is a random jq program, input and command line (gen.py for the
general generator, modes.py for focused ones), run under both tools with the
same argv, stdin, files, environment and working directory. stdout bytes and
exit codes must be identical; stderr is compared after normalizing the program
name (jq_diff's rules, plus a `qj: ` prefix that `stderr` output without a
newline pushed mid-line). A divergence is re-run to rule out nondeterminism,
minimized (AST reductions, then token and byte deletion), and logged once.

Usage (from the repository root, after `cargo build --release`):

    python3 tests/jq_fuzz/fuzz.py run --cases 100000 --jobs 8 [--seed N] [--modes a,b]
    python3 tests/jq_fuzz/fuzz.py replay target/jq_fuzz/divergences.jsonl [-v | --index I]
    python3 tests/jq_fuzz/fuzz.py profile --cases 3000   # jq only: outcome histogram
    python3 tests/jq_fuzz/fuzz.py sample --cases 20      # print generated cases

`run` prints progress and a summary, and writes (under --out, default
target/jq_fuzz) divergences.jsonl (one minimized divergence per line,
deduplicated across runs) and stats.json. Case K of seed S is reproducible:
`run --seed S --start K --cases 1`. Modes: general, builtins, values, paths,
control, regex, dates, cli, parse, debug (--debug-trace, --debug-dump-disasm),
runtests (--run-tests), progfile (multi-line -f programs), modules (-L, import,
include, ~/.jq), env (TZ and other variables). `probe '{"args": [...],
"stdin": "...", "files": {...}, "env": {...}}'` runs one invocation under both
tools (`--diff` for a line diff). Environment: JQ (default: jq on PATH, must be
jq-1.8.1), QJ (default: target/release/qj).

Both tools run with a pinned environment (PATH, HOME, LC_ALL=C,
TZ=America/New_York, PAGER=less). A case is skipped when jq times out or hits
the output or memory cap. jq crashes that qj reproduces exactly (jq's assertion
failures) count as matches; other jq crashes are reported as `jqcrash` (jq
bugs, to exclude). Never generated, because jq itself is nondeterministic
there: `now` values, `lgamma_r` at its poles, and `get_jq_origin`,
`get_search_list` and `modulemeta` (they depend on the binary's location).
"""

import argparse
import base64
import ctypes
import hashlib
import json
import multiprocessing as mp
import os
import random
import re
import select
import shutil
import subprocess
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
sys.dont_write_bytecode = True  # no __pycache__ in the source tree

import gen  # noqa: E402
import modes  # noqa: E402

TIMEOUT = 3.0
MAX_OUTPUT = 4 << 20
MAX_RSS = 1 << 30

# ---------------------------------------------------------------------------
# Running one process
# ---------------------------------------------------------------------------

_libproc = None


def rss_bytes(pid):
    global _libproc
    if sys.platform != "darwin":
        try:
            with open(f"/proc/{pid}/statm") as f:
                return int(f.read().split()[1]) * os.sysconf("SC_PAGE_SIZE")
        except (OSError, ValueError, IndexError):
            return None
    if _libproc is None:
        _libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
    buf = ctypes.create_string_buffer(96)
    n = _libproc.proc_pidinfo(pid, 4, ctypes.c_uint64(0), buf, 96)  # PROC_PIDTASKINFO
    if n != 96:
        return None
    return int.from_bytes(buf.raw[8:16], "little")


class Result:
    __slots__ = ("status", "stdout", "stderr")

    def __init__(self, status, stdout, stderr):
        self.status = status  # int exit code, or "signal N", "timeout", "output", "memory"
        self.stdout = stdout
        self.stderr = stderr

    def to_json(self):
        return {"status": self.status, "stdout": enc(self.stdout), "stderr": enc(self.stderr)}


def enc(b):
    if b is None:
        return None
    try:
        return b.decode("utf-8")
    except UnicodeDecodeError:
        return {"b64": base64.b64encode(b).decode()}


def dec(x):
    if x is None:
        return None
    if isinstance(x, dict):
        return base64.b64decode(x["b64"])
    return x.encode("utf-8")


def run_proc(argv, cwd, env, stdin, stdin_mode, scratch):
    """Run argv; stdin is bytes (piped or via a regular file) or None (/dev/null)."""
    out_path = os.path.join(scratch, "out")
    err_path = os.path.join(scratch, "err")
    fout = open(out_path, "w+b")
    ferr = open(err_path, "w+b")
    fin = None
    writer = None
    try:
        if stdin is None:
            stdin_arg = subprocess.DEVNULL
        elif stdin_mode == "file":
            in_path = os.path.join(scratch, "stdin")
            with open(in_path, "wb") as f:
                f.write(stdin)
            fin = open(in_path, "rb")
            stdin_arg = fin
        else:
            stdin_arg = subprocess.PIPE
        p = subprocess.Popen(argv, cwd=cwd, env=env, stdin=stdin_arg, stdout=fout, stderr=ferr,
                             close_fds=True)
        if stdin_arg is subprocess.PIPE:
            def feed(pipe=p.stdin, data=stdin):
                try:
                    pipe.write(data)
                except (BrokenPipeError, OSError):
                    pass
                try:
                    pipe.close()
                except OSError:
                    pass
            if len(stdin) <= 8192:
                feed()
            else:
                writer = threading.Thread(target=feed, daemon=True)
                writer.start()
        status = wait_capped(p, fout, ferr)
        if writer:
            writer.join(1)
        fout.seek(0)
        ferr.seek(0)
        out = fout.read(MAX_OUTPUT + 1)
        err = ferr.read(MAX_OUTPUT + 1)
        return Result(status, out, err)
    finally:
        fout.close()
        ferr.close()
        if fin:
            fin.close()


def wait_capped(p, fout, ferr):
    deadline = time.monotonic() + TIMEOUT
    kq = None
    if hasattr(select, "kqueue"):
        try:
            kq = select.kqueue()
            ev = select.kevent(p.pid, filter=select.KQ_FILTER_PROC,
                               flags=select.KQ_EV_ADD | select.KQ_EV_ONESHOT,
                               fflags=select.KQ_NOTE_EXIT)
            kq.control([ev], 0, 0)
        except OSError:
            kq = None  # already exited
    why = None
    try:
        while True:
            rc = p.poll()
            if rc is not None:
                break
            now = time.monotonic()
            if now >= deadline:
                why = "timeout"
            elif os.fstat(fout.fileno()).st_size > MAX_OUTPUT or \
                    os.fstat(ferr.fileno()).st_size > MAX_OUTPUT:
                why = "output"
            else:
                rss = rss_bytes(p.pid)
                if rss is not None and rss > MAX_RSS:
                    why = "memory"
            if why:
                p.kill()
                p.wait()
                return why
            if kq is not None:
                kq.control(None, 1, min(0.05, deadline - now))
            else:
                try:
                    p.wait(timeout=min(0.02, deadline - now))
                except subprocess.TimeoutExpired:
                    pass
    finally:
        if kq is not None:
            kq.close()
    if rc < 0:
        return "signal %d" % -rc
    return rc


QJ_USAGE_HINT = b"Use qj --help for help with command-line options,"


def normalize_stderr(b):
    """jq_diff's normalization, plus the `qj: ` prefix anywhere in a line.

    `stderr` output has no trailing newline, which puts the next message's
    program-name prefix mid-line, where jq_diff's line-start rule doesn't see
    it (the plan's policy is that mid-line program names say qj). User text
    reaches both tools' stderr identically, so rewriting it on both sides is
    harmless.
    """
    b = b.replace(b"qj: ", b"jq: ")
    out = []
    for line in b.split(b"\n"):
        if line.startswith(b"qj:"):
            line = b"jq:" + line[3:]
        elif line == QJ_USAGE_HINT:
            line = b"Use jq" + line[6:]
        out.append(line)
    return b"\n".join(out)


# ---------------------------------------------------------------------------
# Cases
# ---------------------------------------------------------------------------


class Case:
    """A structured case. `prog` is an AST (gen.N) or a program string."""

    def __init__(self, flags, prog, stdin=None, stdin_mode="pipe", files=None, file_args=None,
                 positional=None, env=None, prog_file=False):
        self.flags = flags              # list of arg groups
        self.prog = prog
        self.stdin = stdin              # None, input spec dict, or bytes
        self.stdin_mode = stdin_mode    # "pipe" or "file"
        self.files = files or {}        # name -> input spec or bytes
        self.file_args = file_args or []
        self.positional = positional    # None or (flag, [values])
        self.env = env or {}
        self.prog_file = prog_file

    def copy(self, **kw):
        c = Case(self.flags, self.prog, self.stdin, self.stdin_mode, self.files, self.file_args,
                 self.positional, self.env, self.prog_file)
        if getattr(self, "raw_args", None) is not None:
            c.raw_args = self.raw_args
        for k, v in kw.items():
            setattr(c, k, v)
        return c

    def program_text(self):
        return self.prog if isinstance(self.prog, str) else gen.render(self.prog)

    def invocation(self):
        args = []
        for g in self.flags:
            args += g
        prog = self.program_text()
        files = {}
        for name, spec in self.files.items():
            files[name] = content_bytes(spec)
        if self.prog_file:
            files["prog.jq"] = prog.encode() + b"\n"
            args += ["-f", "prog.jq"]
        elif getattr(self, "raw_args", None) is not None:
            args += self.raw_args
        else:
            if prog.startswith("-"):
                args.append("--")
            args.append(prog)
        args += self.file_args
        if self.positional:
            args.append(self.positional[0])
            args += self.positional[1]
        stdin = None if self.stdin is None else content_bytes(self.stdin)
        return {"args": args, "stdin": stdin, "stdin_mode": self.stdin_mode, "files": files,
                "env": dict(self.env)}


def content_bytes(spec):
    if isinstance(spec, bytes):
        return spec
    return gen.input_to_bytes(spec)


def inv_key(inv):
    h = hashlib.sha256()
    h.update(json.dumps(inv["args"]).encode())
    h.update(b"\0stdin\0" + (b"<none>" if inv["stdin"] is None else inv["stdin"]))
    h.update(inv["stdin_mode"].encode())
    for k in sorted(inv["files"]):
        h.update(b"\0file\0" + k.encode() + b"\0" + inv["files"][k])
    h.update(json.dumps(sorted(inv["env"].items())).encode())
    return h.hexdigest()


def inv_to_json(inv):
    return {"args": inv["args"], "stdin": enc(inv["stdin"]), "stdin_mode": inv["stdin_mode"],
            "files": {k: enc(v) for k, v in inv["files"].items()}, "env": inv["env"]}


def inv_from_json(j):
    return {"args": j["args"], "stdin": dec(j["stdin"]), "stdin_mode": j.get("stdin_mode", "pipe"),
            "files": {k: dec(v) for k, v in j["files"].items()}, "env": j.get("env", {})}


# Everything a program could reference through the environment is pinned.
def base_env(home):
    return {
        "PATH": "/usr/bin:/bin",
        # The case directory, so a case can provide ~/.jq.
        "HOME": home,
        "LC_ALL": "C",
        "TZ": "America/New_York",
        "PAGER": "less",
    }


class Runner:
    def __init__(self, jq, qj, work):
        self.jq = jq
        self.qj = qj
        self.work = work
        self.casedir = os.path.join(work, "case")
        self.scratch = os.path.join(work, "scratch")
        os.makedirs(os.path.join(work, "home"), exist_ok=True)
        os.makedirs(self.scratch, exist_ok=True)
        self.cache = {}
        self.runs = 0

    def prepare(self, inv):
        if os.path.isdir(self.casedir):
            shutil.rmtree(self.casedir)
        os.makedirs(self.casedir)
        for name, content in inv["files"].items():
            path = os.path.join(self.casedir, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as f:
                f.write(content)

    def run_tool(self, tool, inv):
        env = base_env(self.casedir)
        env.update(inv["env"])
        self.runs += 1
        return run_proc([tool] + inv["args"], self.casedir, env, inv["stdin"], inv["stdin_mode"],
                        self.scratch)

    def observe(self, inv, use_cache=True):
        """(jq result, qj result) for an invocation."""
        key = inv_key(inv)
        if use_cache and key in self.cache:
            return self.cache[key]
        self.prepare(inv)
        j = self.run_tool(self.jq, inv)
        q = self.run_tool(self.qj, inv)
        if use_cache:
            if len(self.cache) > 20000:
                self.cache.clear()
            self.cache[key] = (j, q)
        return j, q


def verdict(j, q):
    """None when equivalent (or jq's result can't be judged), else a signature tuple."""
    if j.status in ("timeout", "output", "memory"):
        return None
    diff = []
    if j.status != q.status:
        diff.append("rc")
    if j.stdout != q.stdout:
        diff.append("stdout")
    if normalize_stderr(j.stderr) != normalize_stderr(q.stderr):
        diff.append("stderr")
    if diff and isinstance(j.status, str) and j.status.startswith("signal"):
        # jq crashed and qj didn't reproduce it exactly: a jq bug, not
        # something qj must match (it becomes an exclusion).
        return ("jqcrash",)
    return tuple(diff) or None


# ---------------------------------------------------------------------------
# Generating a case
# ---------------------------------------------------------------------------


MODES = [
    (30, "general"), (13, "builtins"), (8, "values"), (11, "paths"), (9, "control"),
    (5, "regex"), (4, "dates"), (11, "cli"), (9, "parse"), (4, "debug"), (2, "runtests"),
    (4, "progfile"), (3, "modules"), (3, "env"),
]


def pick_mode(r, only=None):
    if only:
        return r.choice(only)
    total = sum(w for w, _ in MODES)
    x = r.random() * total
    for w, m in MODES:
        x -= w
        if x < 0:
            return m
    return MODES[-1][1]


def gen_case(r, only=None):
    mode = pick_mode(r, only)
    if mode == "general":
        return gen_general(r)
    if mode == "parse":
        return gen_parse(r)
    if mode == "cli":
        return gen_cli_case(r)
    if mode == "debug":
        return gen_debug_case(r)
    if mode == "runtests":
        return gen_runtests_case(r)
    if mode == "progfile":
        return gen_progfile_case(r)
    if mode == "modules":
        return gen_modules_case(r)
    if mode == "env":
        return gen_env_case(r)
    return gen_focused(r, mode)


def small_input(r):
    spec = gen.gen_input_bytes(r)
    spec["docs"] = spec["docs"][:2]
    return spec


def gen_debug_case(r):
    """--debug-dump-disasm / --debug-trace: the compiler's and VM's steps."""
    flag = r.choice([["--debug-dump-disasm"], ["--debug-trace"], ["--debug-trace=all"],
                     ["--debug-dump-disasm", "--debug-trace"]])
    while True:
        prog = gen.gen_program(r, depth=r.choice([1, 1, 2, 2, 3]))
        text = gen.render(prog)
        # `now`: the trace would show the clock. `$ARGS`: known divergence in
        # its refcount (main.c's references to ARGS aren't mirrored yet).
        if "now" not in text and "$ARGS" not in text:
            break
    return Case([["-c"], flag], prog, small_input(r) if r.random() < 0.8 else None)


def gen_runtests_case(r):
    content = modes.gen_runtests(r)
    k = r.random()
    if k < 0.7:
        return Case([["--run-tests"]], "tests.txt", None, files={"tests.txt": content})
    if k < 0.9:
        # No file: the tests come from stdin.
        c = Case([], "", content)
        c.raw_args = ["--run-tests"]
        return c
    return Case([["-L", "mods"], ["--run-tests"]], "tests.txt", None,
                files={"tests.txt": content, "mods/m.jq": b"def m: 1;\n"})


def gen_progfile_case(r):
    text = modes.multiline(r, gen.render(gen.gen_program(r, depth=r.choice([1, 2, 3]))))
    spec = small_input(r) if r.random() < 0.85 else None
    flags = simple_flags(r)
    if r.random() < 0.75:
        c = Case(flags + [["-f", "prog.jq"]], "", spec, files={"prog.jq": text.encode()})
        c.raw_args = []
        return c
    return Case(flags, text, spec)


def gen_modules_case(r):
    files, lflag, prog = modes.gen_modules(r)
    flags = simple_flags(r) + ([lflag] if lflag else [])
    if r.random() < 0.15:
        files[".jq"] = r.choice([b"def hello: \"hi\";\n", b"def m: 99;\n", b"1 +\n",
                                 b'include "m";\n'])
        prog = r.choice([prog, "hello", "m", "[hello, m]?"])
    return Case(flags, prog, small_input(r) if r.random() < 0.3 else None, files=files)


def gen_env_case(r):
    env = modes.gen_env(r)
    prog = r.choice(modes.ENV_PROGRAMS)
    if r.random() < 0.5:
        prog = r.choice(modes.ENV_INPUTS) + " | " + prog
    return Case(simple_flags(r) + [["-n"]], prog, None, env=env)


def simple_flags(r):
    k = r.random()
    if k < 0.75:
        return [["-c"]]
    if k < 0.85:
        return []
    return [r.choice(gen.OUTPUT_FLAGS)]


def gen_focused(r, mode):
    fn = getattr(modes, "mode_" + mode)
    prog = fn(r, [])
    flags = simple_flags(r)
    if mode == "values" and r.random() < 0.5:
        spec = {"docs": [("a", [gen.gen_scalar(r) for _ in range(r.randint(1, 6))])],
                "style": "compact", "sep": b"\n", "trail": b"\n", "seed": 0}
    else:
        spec = gen.gen_input_bytes(r)
    k = r.random()
    if k < 0.8:
        return Case(flags, prog, spec)
    if k < 0.9:
        return Case(flags + [["-n"]], prog, spec if r.random() < 0.5 else None)
    return Case(flags, prog, None, files={"in0.json": spec}, file_args=["in0.json"])


def gen_parse(r):
    flags = [list(r.choice(modes.PARSE_FLAGS))]
    prog = r.choice(modes.PARSE_PROGRAMS)
    if prog.startswith("-c "):
        prog = prog[3:]
    raw = "-R" in flags[0] or "-Rs" in flags[0]
    data = modes.gen_raw_input(r) if raw and r.random() < 0.7 else modes.gen_parse_input(r)
    k = r.random()
    if k < 0.6:
        return Case(flags, prog, data)
    if k < 0.75:
        return Case(flags, prog, data, stdin_mode="file")
    files = {"a.json": data}
    args = ["a.json"]
    if r.random() < 0.4:
        files["b.json"] = modes.gen_parse_input(r)
        args.append("b.json")
    return Case(flags, prog, None, files=files, file_args=args)


def gen_cli_case(r):
    flags, cli_vars, positional, env = gen.gen_cli(r)
    # More and stranger flags than the general mode.
    for _ in range(r.choice([0, 1, 1, 2])):
        flags.append(r.choice(gen.OUTPUT_FLAGS + gen.INPUT_FLAGS))
    if r.random() < 0.3:
        prog = gen.gen_program(r, depth=r.choice([1, 2]), vars_=cli_vars)
    else:
        prog = r.choice(modes.CLI_PROGRAMS)
        if cli_vars and r.random() < 0.5:
            prog = "[$%s, %s]" % (r.choice(cli_vars), prog)
    flat = [a for g in flags for a in g]
    raw = any(a in ("-R", "-Rs", "-Rn", "-nR", "--raw-input", "-sR") for a in flat)

    def content():
        if raw:
            return modes.gen_raw_input(r)
        spec = gen.gen_input_bytes(r)
        if "--seq" in flat and r.random() < 0.5:
            spec["sep"] = r.choice([b"\x1e", b"\n\x1e", b"\x1e\n", b"\x1e\x1e"])
        return spec

    files = {}
    file_args = []
    stdin = None
    k = r.random()
    if k < 0.5:
        stdin = content()
    elif k < 0.85:
        for i in range(r.choice([1, 2, 2, 3])):
            files["in%d.json" % i] = content()
            file_args.append("in%d.json" % i)
        if r.random() < 0.15:
            file_args.insert(r.randint(0, len(file_args)), r.choice(["missing.json", "-"]))
        if r.random() < 0.3:
            stdin = content()
    for g in flags:
        if len(g) == 3 and g[0] in ("--slurpfile", "--rawfile") and g[2] not in files \
                and g[2] != "missing.json":
            files[g[2]] = content() if g[2] != "raw.txt" else modes.gen_raw_input(r)
    return Case(flags, prog, stdin, "pipe", files, file_args, positional, env,
                prog_file=r.random() < 0.08)


def gen_general(r):
    flags, cli_vars, positional, env = gen.gen_cli(r)
    flat = [a for g in flags for a in g]
    raw = any(a in ("-R", "-Rs", "-Rn", "-nR", "--raw-input") for a in flat)
    seq = "--seq" in flat
    vars_ = list(cli_vars)
    prog = gen.gen_program(r, vars_=vars_)
    files = {}
    file_args = []
    stdin = None
    stdin_mode = "pipe"

    def content():
        if raw:
            return gen.raw_text_input(r)
        spec = gen.gen_input_bytes(r)
        if seq and r.random() < 0.5:
            spec["sep"] = r.choice([b"\x1e", b"\n\x1e", b"\x1e\n"])
        return spec

    k = r.random()
    if k < 0.68:
        stdin = content()
    elif k < 0.78:
        stdin = content()
        stdin_mode = "file"
    elif k < 0.93:
        for i in range(r.choice([1, 1, 2, 3])):
            files["in%d.json" % i] = content()
            file_args.append("in%d.json" % i)
        if r.random() < 0.1:
            file_args.append(r.choice(["missing.json", "in0.json", "-"]))
    # else: /dev/null
    # --slurpfile/--rawfile targets.
    for g in flags:
        if len(g) == 3 and g[0] in ("--slurpfile", "--rawfile") and g[2] not in files and g[2] != "missing.json":
            files[g[2]] = content() if g[2] != "raw.txt" else gen.raw_text_input(r)
    prog_file = r.random() < 0.04
    return Case(flags, prog, stdin, stdin_mode, files, file_args, positional, env, prog_file)


# ---------------------------------------------------------------------------
# Minimization
# ---------------------------------------------------------------------------


def ast_nodes(node, path=()):
    """Pre-order (path, node) for replaceable expression nodes."""
    if node.kind == "x":
        yield path, node
    for i, p in enumerate(node.parts):
        if not isinstance(p, str):
            yield from ast_nodes(p[1], path + (i,))


def ast_replace(node, path, new):
    if not path:
        return new
    i = path[0]
    parts = list(node.parts)
    need, child = parts[i]
    parts[i] = (need, ast_replace(child, path[1:], new))
    return gen.N(node.lvl, parts, node.kind)


def ast_descendants(node):
    out = []
    for p in node.parts:
        if not isinstance(p, str):
            for _, n in ast_nodes(p[1]):
                out.append(n)
    return out


def ast_size(node):
    return sum(1 for _ in ast_nodes(node)) if not isinstance(node, str) else len(node)


SIMPLE = [gen.lf("."), gen.lf("empty"), gen.lf("null"), gen.lf("1", gen.T), gen.lf('"a"')]


TOKEN_RE = re.compile(r'''
    "(?:[^"\\]|\\.)*"
  | \d+\.?\d*(?:[eE][+-]?\d+)? | \.\d+(?:[eE][+-]?\d+)?
  | \$?[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*
  | @[A-Za-z0-9_]+
  | \?// | //= | \|= | \+= | -= | \*= | /= | %= | == | != | <= | >= | // | \.\.
  | \.[A-Za-z_][A-Za-z_0-9]*
  | \S
''', re.X)
OPEN = {"(": ")", "[": "]", "{": "}"}


def text_reductions(text):
    """Smaller variants of a program string: token-chunk deletions and
    bracketed groups replaced by `.` (or removed)."""
    toks = [(m.start(), m.end()) for m in TOKEN_RE.finditer(text)]
    n = len(toks)
    seen = {text}

    def emit(s):
        s = s.strip()
        if s and s not in seen:
            seen.add(s)
            return s
        return None

    # Bracketed groups, outermost first.
    stack = []
    groups = []
    for i, (s, e) in enumerate(toks):
        t = text[s:e]
        if t in OPEN:
            stack.append(i)
        elif t in (")", "]", "}") and stack:
            groups.append((stack.pop(), i))
    groups.sort(key=lambda g: g[0] - g[1])
    for a, b in groups:
        s, e = toks[a][0], toks[b][1]
        inner = text[toks[a][1]:toks[b][0]]
        for rep in (".", inner, ""):
            out = emit(text[:s] + rep + text[e:])
            if out:
                yield out
    size = n // 2
    while size >= 1:
        for start in range(0, n - size + 1, max(1, size // 2) if size > 1 else 1):
            s = toks[start][0]
            e = toks[start + size - 1][1]
            out = emit(text[:s] + text[e:])
            if out:
                yield out
        size //= 2
    for i, (s, e) in enumerate(toks):
        t = text[s:e]
        for rep in ('"a"' if t.startswith('"') and t != '"a"' else None,
                    "1" if t[:1].isdigit() and t != "1" else None,
                    "." if t.startswith(".") and len(t) > 1 else None):
            if rep:
                out = emit(text[:s] + rep + text[e:])
                if out:
                    yield out


def prog_reductions(prog):
    if isinstance(prog, str):
        yield from text_reductions(prog)
        return
    base = gen.render(prog)
    for path, node in list(ast_nodes(prog)):
        cands = []
        for s in SIMPLE:
            cands.append(s)
        cands += ast_descendants(node)
        seen = set()
        for c in cands:
            new = ast_replace(prog, path, c)
            text = gen.render(new)
            if text == base or text in seen or len(text) >= len(base):
                continue
            seen.add(text)
            yield new


def value_reductions(v):
    """Smaller variants of a JSON value tree."""
    t = v[0]
    if t in ("a", "o"):
        items = v[1]
        for i in range(len(items)):
            yield (t, items[:i] + items[i + 1:])
        for i, it in enumerate(items):
            child = it if t == "a" else it[1]
            yield child
        for i, it in enumerate(items):
            child = it if t == "a" else it[1]
            for red in value_reductions(child):
                new = list(items)
                new[i] = red if t == "a" else (it[0], red)
                yield (t, new)
        if t == "o":
            for i, it in enumerate(items):
                if it[0] != b'"a"':
                    new = list(items)
                    new[i] = (b'"a"', it[1])
                    yield (t, new)
    elif t == "raw":
        b = v[1]
        for red in bytes_reductions(b):
            yield ("raw", red)
    else:
        if v != ("k", b"null"):
            yield ("k", b"null")
        if t == "n" and v[1] != b"1":
            yield ("n", b"1")
        if t == "s" and v[1] != b'"a"':
            yield ("s", b'"a"')


def bytes_reductions(b):
    n = len(b)
    size = n // 2
    while size >= 1:
        for start in range(0, n, size):
            cand = b[:start] + b[start + size:]
            if cand != b:
                yield cand
        size //= 2


def spec_reductions(spec):
    if isinstance(spec, bytes):
        for red in bytes_reductions(spec):
            yield red
        return
    docs = spec["docs"]
    for i in range(len(docs)):
        s = dict(spec)
        s["docs"] = docs[:i] + docs[i + 1:]
        yield s
    for i, d in enumerate(docs):
        for red in value_reductions(d):
            s = dict(spec)
            nd = list(docs)
            nd[i] = red
            s["docs"] = nd
            yield s
    if spec["style"] != "compact":
        yield dict(spec, style="compact")
    if spec["sep"] != b"\n":
        yield dict(spec, sep=b"\n")
    if spec["trail"] != b"\n":
        yield dict(spec, trail=b"\n")
    # Finally: as plain bytes (to shave whitespace etc.).
    yield gen.input_to_bytes(spec)


def case_reductions(c):
    for i in range(len(c.flags)):
        yield c.copy(flags=c.flags[:i] + c.flags[i + 1:])
    for k in list(c.env):
        e = dict(c.env)
        del e[k]
        yield c.copy(env=e)
    if c.positional:
        yield c.copy(positional=None)
        vals = c.positional[1]
        for i in range(len(vals)):
            yield c.copy(positional=(c.positional[0], vals[:i] + vals[i + 1:]))
    if c.prog_file:
        yield c.copy(prog_file=False)
    for i in range(len(c.file_args)):
        yield c.copy(file_args=c.file_args[:i] + c.file_args[i + 1:])
    if c.stdin_mode == "file":
        yield c.copy(stdin_mode="pipe")
    for red in prog_reductions(c.prog):
        yield c.copy(prog=red)
    if c.stdin is not None:
        yield c.copy(stdin=None)
        for red in spec_reductions(c.stdin):
            yield c.copy(stdin=red)
    for name in list(c.files):
        for red in spec_reductions(c.files[name]):
            f = dict(c.files)
            f[name] = red
            yield c.copy(files=f)
    for i, g in enumerate(c.flags):
        # Merge-free simplifications of a group: e.g. "-cr" -> "-c", "-r".
        if len(g) == 1 and g[0].startswith("-") and not g[0].startswith("--") and len(g[0]) > 2:
            for ch in g[0][1:]:
                nf = list(c.flags)
                nf[i] = ["-" + ch]
                yield c.copy(flags=nf)
    # Last resort for AST programs: continue on the text.
    if not isinstance(c.prog, str):
        yield c.copy(prog=gen.render(c.prog))


def minimize(runner, case, sig, budget=1500):
    """Greedy reduction keeping the same divergence signature."""
    steps = 0
    progress = True
    while progress and steps < budget:
        progress = False
        for cand in case_reductions(case):
            steps += 1
            if steps >= budget:
                break
            j, q = runner.observe(cand.invocation())
            if verdict(j, q) == sig:
                case = cand
                progress = True
                break
    return case


# ---------------------------------------------------------------------------
# Campaign
# ---------------------------------------------------------------------------

_worker = {}


def worker_init(jq, qj, work_root):
    wid = mp.current_process().name.split("-")[-1]
    work = os.path.join(work_root, "w" + wid)
    os.makedirs(work, exist_ok=True)
    _worker["runner"] = Runner(jq, qj, work)


def case_seed(seed, index):
    return int.from_bytes(hashlib.sha256(b"%d:%d" % (seed, index)).digest()[:8], "little")


def run_one(args):
    seed, index, do_minimize, only = args
    runner = _worker["runner"]
    r = random.Random(case_seed(seed, index))
    # Bias toward programs that run: when jq fails, usually try another case
    # (only jq runs for the rejected ones).
    for attempt in range(4):
        case = gen_case(r, only)
        inv = case.invocation()
        if attempt == 3:
            break
        runner.prepare(inv)
        j0 = runner.run_tool(runner.jq, inv)
        if j0.status in ("timeout", "memory", "output"):
            continue
        if j0.status == 0 and j0.stdout:
            break
        if r.random() < 0.35:
            break
    j, q = runner.observe(inv, use_cache=False)
    sig = verdict(j, q)
    res = {"index": index, "jq_status": j.status, "qj_status": q.status,
           "jq_out": len(j.stdout) > 0}
    if sig is None:
        return res
    # Recheck for nondeterminism.
    j2, q2 = runner.observe(inv, use_cache=False)
    flaky = []
    if (j2.status, j2.stdout, j2.stderr) != (j.status, j.stdout, j.stderr):
        flaky.append("jq")
    if (q2.status, q2.stdout, q2.stderr) != (q.status, q.stdout, q.stderr):
        flaky.append("qj")
    res["sig"] = list(sig)
    res["flaky"] = flaky
    res["orig"] = inv_to_json(inv)
    res["orig_program"] = case.program_text()
    mcase = case
    if do_minimize and not flaky:
        runner.cache.clear()
        mcase = minimize(runner, case, sig)
    minv = mcase.invocation()
    mj, mq = runner.observe(minv)
    res["min"] = inv_to_json(minv)
    res["program"] = mcase.program_text()
    res["jq"] = mj.to_json()
    res["qj"] = mq.to_json()
    return res


def signature_key(res):
    """Dedup key for a minimized divergence."""
    m = res["min"]
    return json.dumps([m["args"], m["stdin"], sorted(m["files"].items(), key=lambda kv: kv[0]),
                       sorted(m["env"].items())], sort_keys=True)


def find_jq():
    jq = os.environ.get("JQ") or shutil.which("jq")
    if not jq:
        sys.exit("jq not found")
    real = os.path.realpath(jq)
    if os.path.basename(real) == "mise":
        out = subprocess.run([real, "which", "jq"], capture_output=True, text=True, cwd=ROOT)
        jq = out.stdout.strip() or jq
    else:
        jq = real
    v = subprocess.run([jq, "--version"], capture_output=True, text=True).stdout.strip()
    if v != "jq-1.8.1":
        sys.exit(f"{jq} is {v!r}, need jq-1.8.1")
    return jq


def cmd_run(a):
    jq = find_jq()
    qj = os.path.abspath(os.environ.get("QJ") or os.path.join(ROOT, "target/release/qj"))
    out_dir = os.path.abspath(a.out)
    os.makedirs(out_dir, exist_ok=True)
    work_root = os.path.join(out_dir, "work")
    div_path = os.path.join(out_dir, "divergences.jsonl")
    seen = set()
    if os.path.exists(div_path):
        with open(div_path) as f:
            for line in f:
                try:
                    seen.add(signature_key(json.loads(line)))
                except (ValueError, KeyError):
                    pass
    stats = {"cases": 0, "divergent": 0, "unique": 0, "flaky_jq": 0, "flaky_qj": 0,
             "jq_crash": 0, "skipped": 0, "by_sig": {}, "jq_rc": {}, "with_output": 0,
             "clean_run": 0, "max_clean_run": 0}
    started = time.time()
    only = a.modes.split(",") if a.modes else None
    tasks = ((a.seed, i, not a.no_minimize, only) for i in range(a.start, a.start + a.cases))
    with mp.Pool(a.jobs, initializer=worker_init, initargs=(jq, qj, work_root)) as pool, \
            open(div_path, "a") as div_f:
        for res in pool.imap_unordered(run_one, tasks, chunksize=4):
            stats["cases"] += 1
            js = res["jq_status"]
            stats["jq_rc"][str(js)] = stats["jq_rc"].get(str(js), 0) + 1
            if res["jq_out"]:
                stats["with_output"] += 1
            if js in ("timeout", "output", "memory"):
                stats["skipped"] += 1
            sig = res.get("sig")
            if sig is None:
                stats["clean_run"] += 1
                stats["max_clean_run"] = max(stats["max_clean_run"], stats["clean_run"])
            else:
                stats["clean_run"] = 0
                stats["divergent"] += 1
                if res["flaky"]:
                    stats["flaky_" + res["flaky"][0]] += 1
                if sig == ["jqcrash"]:
                    stats["jq_crash"] += 1
                s = ",".join(sig)
                stats["by_sig"][s] = stats["by_sig"].get(s, 0) + 1
                key = signature_key(res)
                if key not in seen:
                    seen.add(key)
                    stats["unique"] += 1
                    res["seed"] = a.seed
                    div_f.write(json.dumps(res) + "\n")
                    div_f.flush()
                    if not a.quiet:
                        print("DIVERGENCE #%d [%s]%s: %s" % (
                            res["index"], s, " flaky=" + ",".join(res["flaky"]) if res["flaky"] else "",
                            json.dumps(res["min"]["args"])[:300]), flush=True)
            if stats["cases"] % a.progress == 0:
                el = time.time() - started
                print("%d cases, %d divergent (%d unique), %.0f cases/s, clean run %d" % (
                    stats["cases"], stats["divergent"], stats["unique"], stats["cases"] / el,
                    stats["clean_run"]), flush=True)
    stats["seconds"] = round(time.time() - started, 1)
    with open(os.path.join(out_dir, "stats.json"), "w") as f:
        json.dump(stats, f, indent=1, sort_keys=True)
    print(json.dumps(stats, indent=1, sort_keys=True))


def cmd_replay(a):
    jq = find_jq()
    qj = os.path.abspath(os.environ.get("QJ") or os.path.join(ROOT, "target/release/qj"))
    work = os.path.join(os.path.abspath(a.out), "work", "replay")
    runner = Runner(jq, qj, work)
    with open(a.file) as f:
        lines = [json.loads(l) for l in f if l.strip()]
    idxs = range(len(lines)) if a.index is None else [a.index]
    still = 0
    for i in idxs:
        res = lines[i]
        inv = inv_from_json(res["min"])
        j, q = runner.observe(inv, use_cache=False)
        sig = verdict(j, q)
        if sig:
            still += 1
        if a.index is not None or (sig and a.verbose):
            show(i, res, inv, j, q, sig)
        elif not a.quiet:
            print("%4d %-20s %s" % (i, ",".join(sig) if sig else "fixed", json.dumps(inv["args"])[:200]))
    print("%d of %d still diverge" % (still, len(list(idxs))))


def cmd_probe(a):
    """Run invocations given as JSON: {"args": [...], "stdin": "...", "files": {...}, "env": {...}}."""
    jq = find_jq()
    qj = os.path.abspath(os.environ.get("QJ") or os.path.join(ROOT, "target/release/qj"))
    runner = Runner(jq, qj, os.path.join(os.path.abspath(a.out), "work", "probe"))
    for i, spec in enumerate(a.specs):
        inv = inv_from_json(dict({"stdin": None, "files": {}, "env": {}}, **json.loads(spec)))
        j, q = runner.observe(inv, use_cache=False)
        if a.diff:
            import difflib
            print("#%d %s sig=%s exit jq=%s qj=%s" % (i, json.dumps(inv["args"])[:200],
                                                      verdict(j, q), j.status, q.status))
            for name, x, y in (("stdout", j.stdout, q.stdout),
                               ("stderr", normalize_stderr(j.stderr), normalize_stderr(q.stderr))):
                for line in difflib.unified_diff(x.decode("utf-8", "replace").splitlines(),
                                                 y.decode("utf-8", "replace").splitlines(),
                                                 "jq " + name, "qj " + name, lineterm="", n=1):
                    print("   " + line[:300])
        else:
            show(i, None, inv, j, q, verdict(j, q))


def show(i, res, inv, j, q, sig):
    print("=" * 78)
    print("#%d sig=%s" % (i, sig))
    print("args:  %s" % json.dumps(inv["args"]))
    if inv["stdin"] is not None:
        print("stdin: %r (%s)" % (inv["stdin"][:500], inv["stdin_mode"]))
    for k, v in inv["files"].items():
        print("file %s: %r" % (k, v[:500]))
    if inv["env"]:
        print("env:   %s" % inv["env"])
    for name, r_ in (("jq", j), ("qj", q)):
        print("%s exit %s\n  stdout: %r\n  stderr: %r" % (name, r_.status, r_.stdout[:1500],
                                                        r_.stderr[:1500]))


def profile_one(args):
    seed, index, only = args
    runner = _worker["runner"]
    r = random.Random(case_seed(seed, index))
    case = gen_case(r, only)
    inv = case.invocation()
    runner.prepare(inv)
    j = runner.run_tool(runner.jq, inv)
    first = j.stderr.split(b"\n")[0].decode("utf-8", "replace")
    import re
    first = re.sub(r"\(at [^)]*\)", "(at X)", first)
    first = re.sub(r'"[^"]*"', '"S"', first)
    first = re.sub(r"\d+", "N", first)[:90]
    return j.status, len(j.stdout) > 0, first, case.program_text() if j.status in ("timeout", "memory", "output") else None


def cmd_profile(a):
    """Run only jq over generated cases; histogram exit codes and error messages."""
    jq = find_jq()
    out_dir = os.path.abspath(a.out)
    counts = {}
    msgs = {}
    slow = []
    with_out = 0
    with mp.Pool(a.jobs, initializer=worker_init, initargs=(jq, jq, os.path.join(out_dir, "work"))) as pool:
        for status, has_out, first, prog in pool.imap_unordered(
                profile_one, ((a.seed, i, a.modes.split(",") if a.modes else None)
                              for i in range(a.start, a.start + a.cases)), chunksize=8):
            counts[str(status)] = counts.get(str(status), 0) + 1
            with_out += has_out
            if status != 0 and first:
                msgs[first] = msgs.get(first, 0) + 1
            if prog:
                slow.append((status, prog))
    print("exit codes:", json.dumps(dict(sorted(counts.items(), key=lambda kv: -kv[1]))))
    print("cases with stdout: %d / %d" % (with_out, a.cases))
    for m, n in sorted(msgs.items(), key=lambda kv: -kv[1])[:a.top]:
        print("%6d  %s" % (n, m))
    for status, prog in slow[:20]:
        print("SLOW %s: %s" % (status, prog[:300]))


def cmd_sample(a):
    for i in range(a.start, a.start + a.cases):
        r = random.Random(case_seed(a.seed, i))
        c = gen_case(r, a.modes.split(",") if a.modes else None)
        inv = c.invocation()
        print(json.dumps(inv["args"]), repr(inv["stdin"])[:200] if inv["stdin"] is not None else "")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    default_out = os.path.join(ROOT, "target", "jq_fuzz")
    p = sub.add_parser("run")
    p.add_argument("--cases", type=int, default=10000)
    p.add_argument("--start", type=int, default=0)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--jobs", type=int, default=8)
    p.add_argument("--out", default=default_out)
    p.add_argument("--progress", type=int, default=2000)
    p.add_argument("--no-minimize", action="store_true")
    p.add_argument("--modes", help="comma-separated subset of: " + ",".join(m for _, m in MODES))
    p.add_argument("--quiet", action="store_true")
    p.set_defaults(fn=cmd_run)
    p = sub.add_parser("replay")
    p.add_argument("file")
    p.add_argument("--index", type=int)
    p.add_argument("--out", default=default_out)
    p.add_argument("--verbose", "-v", action="store_true")
    p.add_argument("--quiet", action="store_true")
    p.set_defaults(fn=cmd_replay)
    p = sub.add_parser("probe")
    p.add_argument("specs", nargs="+")
    p.add_argument("--diff", action="store_true", help="show a line diff of stdout and stderr")
    p.add_argument("--out", default=default_out)
    p.set_defaults(fn=cmd_probe)
    p = sub.add_parser("profile")
    p.add_argument("--cases", type=int, default=2000)
    p.add_argument("--start", type=int, default=0)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--jobs", type=int, default=8)
    p.add_argument("--top", type=int, default=40)
    p.add_argument("--modes")
    p.add_argument("--out", default=default_out)
    p.set_defaults(fn=cmd_profile)
    p = sub.add_parser("sample")
    p.add_argument("--cases", type=int, default=20)
    p.add_argument("--start", type=int, default=0)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--modes")
    p.set_defaults(fn=cmd_sample)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
