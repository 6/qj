#!/usr/bin/env python3
"""Compact view of where jq's default outcome differs from qj's, per family:
counts by (jq status, qj status), and a few short examples.
Usage: ub_mismatch.py results.json FAMILY [N]"""
import collections
import json
import sys

data = json.load(open(sys.argv[1]))
fam = sys.argv[2]
n = int(sys.argv[3]) if len(sys.argv) > 3 else 5
res, samples = data["results"], data["samples"]
kinds = collections.Counter()
examples = collections.defaultdict(list)
for cid, r in res.items():
    if r["family"] != fam or "qj" not in r["runs"]:
        continue
    d = r["runs"]["default"][0]
    q = r["runs"]["qj"][0]
    if d == q:
        continue
    k = (d[0], q[0], "same stdout" if d[1] == q[1] else "other stdout",
         "same stderr" if d[2] == q[2] else "other stderr")
    kinds[k] += 1
    if len(examples[k]) < n:
        prog = " ".join(a if len(a) < 60 else a[:57] + "..." for a in r["args"])
        examples[k].append((cid, prog, r["stdin"], samples.get(d[1], "")[-200:], samples.get(q[1], "")[-200:],
                            samples.get(d[2], "")[:200]))
for k, c in kinds.most_common():
    print(f"{c:4d}  jq {k[0]:10s} qj {k[1]:10s} {k[2]}, {k[3]}")
    for cid, prog, stdin, jo, qo, je in examples[k]:
        print(f"      {cid}: {prog} <<< {stdin!r}")
        if k[2] == "other stdout":
            print(f"        jq stdout tail: {jo!r}\n        qj stdout tail: {qo!r}")
        if je:
            print(f"        jq stderr: {je!r}")
