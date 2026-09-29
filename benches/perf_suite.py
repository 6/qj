#!/usr/bin/env python3
"""qj performance suite: single documents, NDJSON and startup, against jq.

Runs each workload with several tools and records wall time and peak RSS.

Tools (select with --tools, comma separated):
  qj      $QJ (default target/release/qj), default thread count
  qj1     the same with --threads 1
  old     $QJ_OLD with QJ_CORE=old (qj's pre-port core, built from 8331ec0)
  old1    the same with --threads 1
  base    $QJ_BASE: another qj build to A/B against (e.g. before a change)
  base1   the same with --threads 1
  jq      $JQ (default jq)

Suites (select with --suite): json (large_twitter.json, 51 MB), array (a
200 MB array of GH Archive events), ndjson (gharchive.ndjson, 1.1 GB),
slurp (-s over 200 MB of NDJSON), stdin (NDJSON through a pipe), vm (-n
programs: the interpreter and builtins without input), startup.

Timing: by default runs are interleaved (warmup, then run r of every tool
before run r+1 of any), output goes to /dev/null, and wall time and peak RSS
come from wait4(). With --hyperfine, times come from hyperfine instead
($HYPERFINE, default `hyperfine`; --output=pipe, as benches/results_*.md
always did), and RSS from one extra run.

  bash benches/download_data.sh --json --gharchive && bash benches/generate_data.sh
  python3 benches/perf_suite.py --suite json --tools qj,qj1,jq --runs 3 \\
      --json-out /tmp/r.json --out /tmp/r.md
  python3 benches/perf_suite.py --merge a.json,b.json --out benches/results_x.md

--check first compares every tool's stdout and exit status with jq's (the
port must match byte for byte; the old core may not). --filter picks
workloads by id substrings (comma separated).
"""

import argparse
import hashlib
import json
import os
import shlex
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "benches" / "data"

# (id, flags, filter, input) where input names a data file (see data_files).
JSON_WORKLOADS = [
    ("identity -c", ["-c"], ".", "twitter"),
    ("identity pretty", [], ".", "twitter"),
    ("length", [], ".statuses|length", "twitter"),
    ("keys", ["-c"], "keys", "twitter"),
    ("iterate field", ["-c"], ".statuses[]|.user.name", "twitter"),
    ("map object", ["-c"], ".statuses|map({user, text})", "twitter"),
    ("select construct", ["-c"],
     ".statuses[]|select(.retweet_count>0)|{user:.user.screen_name,n:.retweet_count}", "twitter"),
    ("sort_by", ["-c"], ".statuses|sort_by(.retweet_count)|.[-1].user.screen_name", "twitter"),
    ("group_by", [], ".statuses|group_by(.user.screen_name)|length", "twitter"),
    ("unique", [], "[.statuses[]|.user.screen_name]|unique|length", "twitter"),
    ("reduce", [], "reduce .statuses[] as $s (0; . + $s.retweet_count)", "twitter"),
    ("add", [], "[.statuses[]|.retweet_count]|add", "twitter"),
    ("walk", ["-c"], 'walk(if type == "boolean" then not else . end)', "twitter"),
    ("paths", [], "[paths]|length", "twitter"),
    ("paths(scalars)", [], "[paths(scalars)]|length", "twitter"),
    ("to_entries", ["-c"], "[.statuses[]|to_entries|map(.key)]|length", "twitter"),
    ("from_entries", ["-c"], "[.statuses[]|.user|to_entries|from_entries]|length", "twitter"),
    ("with_entries", ["-c"], ".statuses|map(with_entries(select(.value != null)))", "twitter"),
    ("tostream", [], "[tostream]|length", "twitter"),
    ("ascii_downcase", ["-c"], "[.statuses[]|.text|ascii_downcase]", "twitter"),
    ("test", [], '[.statuses[]|select(.text|test("RT @"))]|length', "twitter"),
    ("sub", ["-c"], '[.statuses[]|.user.screen_name|sub("_"; "-")]', "twitter"),
    ("gsub", ["-c"], '[.statuses[]|.user.screen_name|gsub("_"; "-")]', "twitter"),
    ("split join", ["-c"], '[.statuses[]|.user.screen_name|split("_")|join("-")]', "twitter"),
    ("interpolation", ["-c"], '[.statuses[]|"@\\(.user.screen_name): \\(.text[0:30])"]', "twitter"),
    ("tojson", [], "[.statuses[]|tojson|length]|add", "twitter"),
    ("update |=", ["-c"], ".statuses[] |= (.retweet_count += 1)", "twitter"),
    ("del", ["-c"], "del(.statuses[].user)", "twitter"),
    ("def", ["-c"],
     'def hi(rt): if rt > 10 then "viral" elif rt > 0 then "shared" else "none" end; '
     "[.statuses[] | hi(.retweet_count)]", "twitter"),
    ("limit", ["-c"], "[limit(5; .statuses[])]|length", "twitter"),
]

ARRAY_WORKLOADS = [
    ("array identity -c", ["-c"], ".", "array"),
    ("array identity pretty", [], ".", "array"),
    ("array length", [], "length", "array"),
    ("array iterate field", ["-c"], ".[]|.actor.login", "array"),
    ("array map object", ["-c"], "map({id, type})", "array"),
    ("array select", ["-c"], 'map(select(.type == "PushEvent"))|length', "array"),
    ("array sort_by", ["-c"], "sort_by(.created_at)|map(.id)|length", "array"),
    ("array group_by", ["-c"], "group_by(.type)|map({type: .[0].type, n: length})", "array"),
    ("array reduce", ["-c"], "reduce .[] as $e ({}; .[$e.type] += 1)", "array"),
    ("array to_entries", ["-c"], "map(to_entries|length)|add", "array"),
    ("array with_entries", ["-c"], '.[]|with_entries(select(.key != "payload"))', "array"),
    ("array paths", [], "[paths]|length", "array"),
    ("array walk", ["-c"], 'walk(if type == "string" then ascii_downcase else . end)|length', "array"),
    ("array tostream", [], "[tostream]|length", "array"),
    ("array tojson", [], "map(tojson|length)|add", "array"),
]

NDJSON_WORKLOADS = [
    ("ndjson field", [], ".actor.login", "ndjson"),
    ("ndjson length", ["-c"], "length", "ndjson"),
    ("ndjson keys", ["-c"], "keys", "ndjson"),
    ("ndjson select", ["-c"], 'select(.type == "PushEvent")', "ndjson"),
    ("ndjson reshape", ["-c"], "{type, repo: .repo.name, actor: .actor.login}", "ndjson"),
    ("ndjson evaluator", ["-c"], "{type, commits: [.payload.commits[]?.message]}", "ndjson"),
    ("ndjson identity -c", ["-c"], ".", "ndjson"),
    ("ndjson def select", ["-c"], 'def is_push: .type == "PushEvent"; select(is_push)', "ndjson"),
    ("ndjson reduce", ["-c"], 'reduce .payload.commits[]? as $c (""; . + $c.message[0:1])', "ndjson"),
    ("ndjson with_entries", ["-c"], 'with_entries(select(.key != "payload"))', "ndjson"),
    ("ndjson test", ["-c"], 'select(.actor.login|test("bot"))|.id', "ndjson"),
    ("ndjson paths", ["-c"], "[paths]|length", "ndjson"),
]

SLURP_WORKLOADS = [
    ("slurp length", ["-s"], "length", "ndjson200"),
    ("slurp group_by", ["-s", "-c"], "group_by(.type)|map({type: .[0].type, count: length})", "ndjson200"),
    ("slurp top users", ["-s", "-c"],
     "map(.actor.login)|group_by(.)|map({user: .[0], events: length})|sort_by(.events)|reverse|.[:10]",
     "ndjson200"),
]

STDIN_WORKLOADS = [
    ("stdin field", [], ".actor.login", "pipe:ndjson"),
    ("stdin select", ["-c"], 'select(.type == "PushEvent")', "pipe:ndjson"),
]

# No input: the interpreter and builtins alone.
VM_WORKLOADS = [
    ("vm reduce", ["-n"], "reduce range(5000000) as $i (0; . + $i)", None),
    ("vm map add", ["-n"], "[range(2000000)] | map(. * 2) | add", None),
    ("vm select", ["-n"], "[range(2000000) | select(. % 3 == 0)] | length", None),
    ("vm tostring join", ["-n"], '[range(300000) | tostring] | join(",") | length', None),
    ("vm recursion", ["-n"],
     "def f: if . < 1 then 0 else (. - 1 | f) + 1 end; [range(3000) | f] | add", None),
    ("vm limit", ["-n"], "[limit(1000000; repeat(1))] | length", None),
    ("vm object update", ["-n"],
     'reduce range(300000) as $i ({}; .["k\\($i % 1000)"] += 1) | length', None),
    ("vm walk", ["-n"],
     '[range(100000)] | map({a: ., b: [., .]}) | walk(if type == "number" then . + 1 else . end) | length',
     None),
    ("vm to_entries", ["-n"], "[range(200000) | {a: ., b: 1}] | map(to_entries) | length", None),
    ("vm paths", ["-n"], "[range(100000) | [., [., .]]] | [paths] | length", None),
    ("vm update |=", ["-n"], "[range(1000000)] | .[] |= . + 1 | length", None),
    ("vm ascii_downcase", ["-n"], '[range(200000) | "AbC\\(.)XyZ" | ascii_downcase] | length', None),
]

STARTUP_WORKLOADS = [
    ("startup -n 1", ["-n"], "1", None),
    ("startup tiny file", ["-c"], ".a", "tiny"),
    ("startup builtins", ["-n", "-c"],
     '{"a":1,"b":"x"} | to_entries | from_entries | with_entries(.value |= tostring) '
     '| [paths] | map(tostring | test("a")) | any', None),
    ("startup 100 runs", ["-n"], "1", None),  # 100 sequential invocations
]

SUITES = {
    "json": JSON_WORKLOADS,
    "array": ARRAY_WORKLOADS,
    "ndjson": NDJSON_WORKLOADS,
    "slurp": SLURP_WORKLOADS,
    "stdin": STDIN_WORKLOADS,
    "vm": VM_WORKLOADS,
    "startup": STARTUP_WORKLOADS,
}

TOOL_ORDER = ["qj", "qj1", "base", "base1", "old", "old1", "jq"]
TOOL_LABEL = {
    "qj": "qj",
    "qj1": "qj (1T)",
    "base": "qj-before",
    "base1": "qj-before (1T)",
    "old": "qj-old",
    "old1": "qj-old (1T)",
    "jq": "jq",
}


def data_files():
    return {
        "twitter": DATA / "large_twitter.json",
        "array": DATA / "gharchive_array.json",
        "ndjson": DATA / "gharchive.ndjson",
        "ndjson200": DATA / "gharchive_200mb.ndjson",
        "tiny": DATA / "tiny.json",
    }


def ensure_data(needed):
    files = data_files()
    src = files["ndjson"]
    for key in needed:
        path = files[key]
        if path.exists():
            continue
        if key == "tiny":
            path.write_text('{"a":1}\n')
        elif key in ("array", "ndjson200"):
            if not src.exists():
                sys.exit(f"{src} missing: run bash benches/download_data.sh --gharchive")
            # The first 73,000 GH Archive events (~200 MB), as an array or as NDJSON.
            with open(src, "rb") as f, open(path, "wb") as out:
                if key == "array":
                    out.write(b"[")
                for i, line in enumerate(f):
                    if i == 73000:
                        break
                    if key == "array":
                        if i:
                            out.write(b",\n")
                        out.write(line.rstrip(b"\n"))
                    else:
                        out.write(line)
                if key == "array":
                    out.write(b"]\n")
        else:
            sys.exit(f"{path} missing: run benches/download_data.sh / generate_data.sh")


def tool_command(tool):
    """(argv prefix, extra env) for a tool."""
    qj = os.environ.get("QJ", str(ROOT / "target" / "release" / "qj"))
    if tool in ("qj", "qj1"):
        return [qj] + (["--threads", "1"] if tool == "qj1" else []), {}
    if tool in ("base", "base1"):
        base = os.environ.get("QJ_BASE")
        if not base:
            sys.exit("QJ_BASE is not set")
        return [base] + (["--threads", "1"] if tool == "base1" else []), {}
    if tool in ("old", "old1"):
        old = os.environ.get("QJ_OLD")
        if not old:
            sys.exit("QJ_OLD is not set")
        return [old] + (["--threads", "1"] if tool == "old1" else []), {"QJ_CORE": "old"}
    if tool == "jq":
        return [os.environ.get("JQ", "jq")], {}
    sys.exit(f"unknown tool {tool}")


def build(tool, workload):
    """(argv, env, input file, input is a pipe, repeat count)."""
    wid, flags, filt, inp = workload
    prefix, extra = tool_command(tool)
    env = dict(os.environ)
    env.update(extra)
    argv = prefix + flags + [filt]
    stdin_file = None
    pipe = False
    if inp is not None:
        if inp.startswith("pipe:"):
            stdin_file = data_files()[inp[5:]]
            pipe = True
        else:
            argv.append(str(data_files()[inp]))
    repeat = 100 if wid == "startup 100 runs" else 1
    return argv, env, stdin_file, pipe, repeat


def run_once(argv, env, stdin_file, pipe, out=subprocess.DEVNULL):
    """Wall seconds, peak RSS bytes and exit status of one run."""
    cat = None
    stdin = subprocess.DEVNULL
    if stdin_file is not None:
        if pipe:
            cat = subprocess.Popen(["cat", str(stdin_file)], stdout=subprocess.PIPE)
            stdin = cat.stdout
        else:
            stdin = open(stdin_file, "rb")
    t0 = time.perf_counter()
    p = subprocess.Popen(argv, env=env, stdin=stdin, stdout=out, stderr=subprocess.DEVNULL)
    _, status, ru = os.wait4(p.pid, 0)
    t1 = time.perf_counter()
    p.returncode = os.waitstatus_to_exitcode(status)
    if cat is not None:
        cat.stdout.close()
        cat.wait()
    elif stdin_file is not None:
        stdin.close()
    rss = ru.ru_maxrss if sys.platform == "darwin" else ru.ru_maxrss * 1024
    return t1 - t0, rss, p.returncode


def output_digest(argv, env, stdin_file, pipe):
    with tempfile.TemporaryFile() as f:
        _, _, code = run_once(argv, env, stdin_file, pipe, out=f)
        f.seek(0)
        h = hashlib.sha256()
        while chunk := f.read(1 << 20):
            h.update(chunk)
        return h.hexdigest(), code


def measure_interleaved(workload, tools, runs, warmup):
    res = {t: {"times": [], "rss": [], "status": None} for t in tools}
    cmds = {t: build(t, workload) for t in tools}
    for r in range(warmup + runs):
        for t in tools:
            argv, env, stdin_file, pipe, repeat = cmds[t]
            total, rss, code = 0.0, 0, 0
            for _ in range(repeat):
                dt, m, code = run_once(argv, env, stdin_file, pipe)
                total += dt
                rss = max(rss, m)
            if r >= warmup:
                res[t]["times"].append(total)
                res[t]["rss"].append(rss)
                res[t]["status"] = code
    return res


def measure_hyperfine(workload, tools, runs, warmup):
    hf = os.environ.get("HYPERFINE", "hyperfine")
    res = {}
    for t in tools:
        argv, env, stdin_file, pipe, repeat = build(t, workload)
        cmd = shlex.join(argv)
        if stdin_file is not None:
            cmd = f"cat {shlex.quote(str(stdin_file))} | {cmd}" if pipe else f"{cmd} < {shlex.quote(str(stdin_file))}"
        if repeat > 1:
            cmd = f"for i in $(seq {repeat}); do {cmd} > /dev/null; done"
        with tempfile.NamedTemporaryFile(suffix=".json") as j:
            hargs = [hf, "--warmup", str(warmup), "--runs", str(runs), "--output=pipe",
                     "--export-json", j.name, "-i", cmd]
            if stdin_file is None and repeat == 1:
                hargs.insert(1, "-N")
            subprocess.run(hargs, env=env, check=True, stdout=subprocess.DEVNULL,
                           stderr=subprocess.DEVNULL)
            data = json.load(open(j.name))["results"][0]
        _, rss, code = run_once(argv, env, stdin_file, pipe)
        res[t] = {"times": data["times"], "rss": [rss], "status": code}
    return res


def fmt_time(s):
    if s is None:
        return "—"
    if s < 1:
        return f"{s * 1000:.1f} ms"
    return f"{s:.2f} s"


def fmt_rss(b):
    if b is None:
        return "—"
    return f"{b / (1 << 20):.0f} MB"


def render(results, suites_order):
    tools = [t for t in TOOL_ORDER if any(t in r for r in results.values())]
    out = []
    by_suite = {}
    for wid, r in results.items():
        by_suite.setdefault(r["suite"], []).append((wid, r))
    for suite in suites_order:
        rows = by_suite.get(suite)
        if not rows:
            continue
        out.append(f"### {suite}\n")
        out.append("Median wall time (peak RSS):\n")
        out.append("| workload | " + " | ".join(TOOL_LABEL[t] for t in tools) + " |")
        out.append("|---|" + "---:|" * len(tools))
        for wid, r in rows:
            flags = " ".join(r["flags"])
            label = f"`{flags + ' ' if flags else ''}{r['filter']}`".replace("|", "\\|")
            cells = []
            for t in tools:
                m = r.get(t)
                if not m or not m["times"]:
                    cells.append("—")
                    continue
                med = statistics.median(m["times"])
                cells.append(f"{fmt_time(med)} ({fmt_rss(max(m['rss']))})")
            out.append(f"| {wid}: {label} | " + " | ".join(cells) + " |")
        out.append("")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", default="json,array,ndjson,slurp,stdin,vm,startup")
    ap.add_argument("--tools", default="qj,qj1,jq")
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--warmup", type=int, default=1)
    ap.add_argument("--startup-runs", type=int, default=20)
    ap.add_argument("--filter", default="")
    ap.add_argument("--hyperfine", action="store_true")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--json-out")
    ap.add_argument("--out")
    ap.add_argument("--merge", help="comma-separated result JSON files to merge and render")
    args = ap.parse_args()

    suites = args.suite.split(",")
    if args.merge:
        results = {}
        for f in args.merge.split(","):
            for wid, r in json.load(open(f)).items():
                cur = results.setdefault(wid, {k: v for k, v in r.items() if k not in TOOL_LABEL})
                for t in TOOL_LABEL:
                    if t in r:
                        cur[t] = r[t]
        md = render(results, list(SUITES))
        if args.out:
            Path(args.out).write_text(md + "\n")
        print(md)
        return

    tools = args.tools.split(",")
    filters = args.filter.split(",")
    workloads = [(s, w) for s in suites for w in SUITES[s] if any(f in w[0] for f in filters)]
    needed = {w[3].removeprefix("pipe:") for _, w in workloads if w[3]}
    ensure_data(needed)

    results = {}
    if args.json_out and Path(args.json_out).exists():
        results = json.load(open(args.json_out))
    for suite, w in workloads:
        wid = w[0]
        if args.check:
            ref = output_digest(*build("jq", w)[:4])
            for t in tools:
                if t != "jq" and output_digest(*build(t, w)[:4]) != ref:
                    print(f"MISMATCH vs jq: {TOOL_LABEL[t]} on {wid}", file=sys.stderr)
        runs = args.startup_runs if suite == "startup" and wid != "startup 100 runs" else args.runs
        measure = measure_hyperfine if args.hyperfine else measure_interleaved
        res = measure(w, tools, runs, args.warmup)
        entry = results.setdefault(wid, {})
        entry.update({"suite": suite, "flags": w[1], "filter": w[2], "input": w[3]})
        entry.update(res)
        line = "  ".join(
            f"{TOOL_LABEL[t]} {fmt_time(statistics.median(res[t]['times']))}"
            f" {fmt_rss(max(res[t]['rss']))}" + ("" if res[t]["status"] == 0 else f" rc={res[t]['status']}")
            for t in tools)
        print(f"{wid:28} {line}", file=sys.stderr, flush=True)
        if args.json_out:
            Path(args.json_out).write_text(json.dumps(results, indent=1))
    md = render(results, suites)
    if args.out:
        Path(args.out).write_text(md + "\n")
    print(md)


if __name__ == "__main__":
    main()
