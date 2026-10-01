#!/usr/bin/env python3
"""Run the undefined-behavior corpus (ub_corpus.py) against jq (and qj)
under conditions that must not change a program's meaning, and record every
outcome.

Usage: ub_run.py --jq PATH [--qj PATH] [--noaslr PATH] [--jobs N]
                 [--families dels,trace,lgamma] [--quick] --out results.json

Conditions (repeats): the default environment several times; the
environment padded by 1, 4 and 16 KB; ASLR off (`setarch -R` on Linux, a
posix_spawn launcher on macOS); and allocator settings: MallocNanoZone=0,
MallocScribble=1, MallocGuardEdges=1 on macOS, MALLOC_PERTURB_ and
glibc.malloc.tcache_count=0 on glibc. qj runs once per case, default
environment, QJ_JQ_COMPAT=1.

Every run has a 10 s timeout, core dumps off, and (Linux) a 2 GB address
space limit; a monitor kills any child over 1 GB resident.
"""
import argparse
import concurrent.futures as cf
import hashlib
import json
import os
import platform
import resource
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runproc  # noqa: E402
import ub_corpus  # noqa: E402

LINUX = platform.system() == "Linux"
TIMEOUT = 3
RSS_KILL_KB = 1 << 20  # 1 GB

BASE_ENV = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "HOME": "/tmp", "TZ": "UTC"}


def monitor():
    """Kill the process group of any child over RSS_KILL_KB."""
    while True:
        time.sleep(1.0)
        pids = runproc.live_pids()
        if not pids:
            continue
        try:
            out = subprocess.run(["ps", "-o", "pid=,rss=", "-p", ",".join(map(str, pids))],
                                 capture_output=True, text=True, timeout=5).stdout
        except Exception:
            continue
        for line in out.split("\n"):
            f = line.split()
            if len(f) == 2 and int(f[1]) > RSS_KILL_KB:
                try:
                    os.killpg(int(f[0]), 9)
                    print(f"monitor: killed group {f[0]} at {f[1]} KB", file=sys.stderr)
                except OSError:
                    pass


def limits():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    if LINUX:
        try:
            resource.setrlimit(resource.RLIMIT_AS, (2 << 30, 2 << 30))
        except (ValueError, OSError):
            pass


def conditions(noaslr):
    pad = lambda n: {"PAD": "x" * n}  # noqa: E731
    conds = [("default", {}, None, 5)]
    conds += [(f"env{n // 1024}k", pad(n), None, 2) for n in (1024, 4096, 16384)]
    if LINUX:
        conds += [("noaslr", {}, "setarch", 3), ("noaslr-env4k", pad(4096), "setarch", 2),
                  ("perturb85", {"MALLOC_PERTURB_": "85"}, None, 2),
                  ("perturb170", {"MALLOC_PERTURB_": "170"}, None, 1),
                  ("tcache0", {"GLIBC_TUNABLES": "glibc.malloc.tcache_count=0"}, None, 2)]
    else:
        if noaslr:
            conds += [("noaslr", {}, "noaslr", 3), ("noaslr-env4k", pad(4096), "noaslr", 2)]
        conds += [("nano0", {"MallocNanoZone": "0"}, None, 2),
                  ("scribble", {"MallocScribble": "1"}, None, 2),
                  ("guard", {"MallocGuardEdges": "1"}, None, 2)]
    return conds


def run_once(exe, args, stdin, env, wrap, noaslr):
    full_env = dict(BASE_ENV)
    full_env.update(env)
    if wrap == "setarch":
        argv = ["setarch", platform.machine(), "-R", exe] + args
        executable = None
    elif wrap == "noaslr":
        argv = [noaslr, exe, exe] + args
        executable = None
    else:
        argv = [exe] + args
        executable = None
    return runproc.run(argv, executable=executable, stdin=stdin, env=full_env,
                       timeout=TIMEOUT, preexec=limits)


def digest(b):
    return hashlib.sha256(b).hexdigest()[:16]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--jq", required=True)
    ap.add_argument("--qj")
    ap.add_argument("--noaslr")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--families", default="dels,trace,lgamma")
    ap.add_argument("--quick", action="store_true", help="default condition only, 2 repeats")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    fams = set(a.families.split(","))
    cases = [c for c in ub_corpus.corpus() if c["family"] in fams]
    conds = [("default", {}, None, 2)] if a.quick else conditions(a.noaslr)
    samples = {}

    def remember(out, err):
        for b in (out, err):
            d = digest(b)
            if d not in samples:
                samples[d] = b[:600].decode("utf-8", "replace") + (f"…(+{len(b) - 600})" if len(b) > 600 else "")

    tasks = []
    for c in cases:
        for (name, env, wrap, reps) in conds:
            for r in range(reps):
                tasks.append((c, name, env, wrap, r))
        if a.qj:
            tasks.append((c, "qj", {"QJ_JQ_COMPAT": "1"}, "qj", 0))

    threading.Thread(target=monitor, daemon=True).start()
    results = {c["id"]: {"args": c["args"], "stdin": c["stdin"], "family": c["family"], "runs": {}}
               for c in cases}
    lock = threading.Lock()
    started = time.time()

    def work(t):
        c, name, env, wrap, r = t
        if wrap == "qj":
            status, out, err = run_once(a.qj, c["args"], c["stdin"], env, None, None)
        else:
            status, out, err = run_once(a.jq, c["args"], c["stdin"], env, wrap, a.noaslr)
        with lock:
            remember(out, err)
            results[c["id"]]["runs"].setdefault(name, []).append([status, digest(out), digest(err)])

    with cf.ThreadPoolExecutor(max_workers=a.jobs) as ex:
        for n, _ in enumerate(ex.map(work, tasks)):
            if n % 2000 == 0:
                print(f"{n}/{len(tasks)} runs, {time.time() - started:.0f}s", file=sys.stderr)
    meta = {"os": platform.system(), "machine": platform.machine(), "jq": a.jq,
            "conditions": [(n, e, w, r) for (n, e, w, r) in conds], "seconds": time.time() - started}
    with open(a.out, "w") as f:
        json.dump({"meta": meta, "results": results, "samples": samples}, f)
    print(f"done: {len(tasks)} runs in {time.time() - started:.0f}s -> {a.out}", file=sys.stderr)


if __name__ == "__main__":
    main()
