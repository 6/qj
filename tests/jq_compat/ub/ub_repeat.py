#!/usr/bin/env python3
"""Run one jq command line N times and count the distinct outcomes.

Usage: ub_repeat.py JQ N [--env K=V] [--stdin TEXT] [--setarch] [--timeout S] -- ARGS...
Prints `count  status  stdout  stderr` per distinct outcome. Each run is in
its own process group, killed whole on the timeout (default 3 s) or when
this runner is stopped (runproc).
"""
import collections
import os
import platform
import resource
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import runproc  # noqa: E402


def limits():
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


def main():
    a = sys.argv[1:]
    jq, n = a.pop(0), int(a.pop(0))
    env = {"PATH": "/usr/bin:/bin", "LC_ALL": "C", "HOME": "/tmp", "TZ": "UTC"}
    stdin = None
    setarch = False
    timeout = 3.0
    while a and a[0] != "--":
        o = a.pop(0)
        if o == "--env":
            k, v = a.pop(0).split("=", 1)
            env[k] = v
        elif o == "--stdin":
            stdin = a.pop(0)
        elif o == "--setarch":
            setarch = True
        elif o == "--timeout":
            timeout = float(a.pop(0))
    a.pop(0)
    argv = (["setarch", platform.machine(), "-R"] if setarch else []) + [jq] + a
    counts = collections.Counter()
    for _ in range(n):
        st, out, err = runproc.run(argv, stdin=stdin, env=env, timeout=timeout, preexec=limits)
        counts[(st, out[:300], err[:300])] += 1
    extra = " ".join(f"{k}={v}" for k, v in env.items() if k not in ("PATH", "LC_ALL", "HOME", "TZ"))
    print(f"$ {extra} jq {a}{' <<< ' + repr(stdin) if stdin is not None else ''}"
          f"{'  (setarch -R)' if setarch else ''}  (x{n})")
    for (st, out, err), c in counts.most_common():
        print(f"  {c:4d}  {st:10s} stdout={out!r} stderr={err!r}")


if __name__ == "__main__":
    main()
