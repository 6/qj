#!/usr/bin/env python3
"""The programs where jq timed out in any run: in how many runs of each
condition, and the program. Usage: ub_hangs.py results.json"""
import collections
import json
import sys

data = json.load(open(sys.argv[1]))
for cid, r in data["results"].items():
    runs = r["runs"]
    hung = {c: sum(o[0] == "timeout" for o in outs) for c, outs in runs.items() if c != "qj"}
    total = {c: len(outs) for c, outs in runs.items() if c != "qj"}
    if not any(hung.values()):
        continue
    all_runs = sum(total.values())
    all_hung = sum(hung.values())
    others = collections.Counter(o[0] for c, outs in runs.items() if c != "qj" for o in outs if o[0] != "timeout")
    prog = " ".join(r["args"])
    print(f"{cid}: hung {all_hung}/{all_runs}; other outcomes {dict(others)} | {prog} <<< {r['stdin']!r}")
