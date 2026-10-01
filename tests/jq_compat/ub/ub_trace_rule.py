#!/usr/bin/env python3
"""For the trace family: does the program reach an instruction that runs with
an empty data stack (a trace line with nothing after the tab, not
backtracking)? Checked with qj's trace, which is jq's with the uninitialized
read as 0. Then cross-tabulate with jq's default outcome in a results file.

Usage: ub_trace_rule.py QJ results.json
"""
import collections
import json
import re
import subprocess
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import runproc  # noqa: E402
import ub_corpus  # noqa: E402

qj, path = sys.argv[1], sys.argv[2]
data = json.load(open(path))
res = data["results"]
EMPTY = re.compile(rb"^\d{4} [A-Z_]+[^\t\n]*\t$", re.M)
table = collections.Counter()
examples = collections.defaultdict(list)
for c in ub_corpus.corpus():
    if c["family"] != "trace":
        continue
    _, stdout, _ = runproc.run([qj] + c["args"], stdin=c["stdin"],
                               env={"PATH": "/usr/bin:/bin", "LC_ALL": "C", "QJ_JQ_COMPAT": "1"},
                               timeout=20)
    empties = EMPTY.findall(stdout)
    reaches = bool(empties)
    jq_status = res[c["id"]]["runs"]["default"][0][0]
    qj_match = res[c["id"]]["runs"].get("qj", [None])[0] == res[c["id"]]["runs"]["default"][0]
    k = (reaches, jq_status, qj_match)
    table[k] += 1
    if len(examples[k]) < 3:
        examples[k].append((c["id"], empties[:2]))
print(f"{'empty-stack line':18s} {'jq default':12s} {'qj == jq':9s} count")
for (reaches, st, m), n in sorted(table.items(), key=lambda kv: -kv[1]):
    print(f"{str(reaches):18s} {st:12s} {str(m):9s} {n}")
    for cid, e in examples[(reaches, st, m)]:
        print(f"      {cid}: {e}")
