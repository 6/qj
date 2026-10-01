#!/usr/bin/env python3
"""Programs whose outcome under CONDITION differs from the default one, as
(default outcome -> condition outcome) counts, with short examples.
Usage: ub_conddiff.py results.json FAMILY CONDITION [N]"""
import collections
import json
import sys

data = json.load(open(sys.argv[1]))
fam, cond = sys.argv[2], sys.argv[3]
n = int(sys.argv[4]) if len(sys.argv) > 4 else 3
res, samples = data["results"], data["samples"]


def short(o):
    st, out, err = o
    e = samples.get(err, "")
    e = e.splitlines()[-1][:110] if e else ""
    return f"{st} | stderr: {e!r}" if e else st


kinds = collections.Counter()
ex = collections.defaultdict(list)
for cid, r in res.items():
    if r["family"] != fam or cond not in r["runs"]:
        continue
    d = tuple(r["runs"]["default"][0])
    for o in r["runs"][cond]:
        if tuple(o) != d:
            k = (short(d), short(o))
            kinds[k] += 1
            if len(ex[k]) < n:
                prog = " ".join(a if len(a) < 70 else a[:67] + "..." for a in r["args"])
                ex[k].append(f"{prog} <<< {r['stdin']!r}" if r["stdin"] is not None else prog)
            break
for (a, b), c in kinds.most_common():
    print(f"{c:4d}  default: {a}\n      {cond}: {b}")
    for e in ex[(a, b)]:
        print(f"        e.g. {e}")
