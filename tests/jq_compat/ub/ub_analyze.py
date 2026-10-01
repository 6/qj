#!/usr/bin/env python3
"""Summarize ub_run.py results: is jq's outcome a function of the program?

For each family:
- how many programs give the same outcome on every default run;
- how many change with the environment's size, with ASLR off, or with an
  allocator setting (each condition against the default outcome);
- the distribution of default outcomes (status, stdout/stderr kinds);
- how many match qj (QJ_JQ_COMPAT=1), when qj ran.
Usage: ub_analyze.py results.json [--show N] [--family F]
"""
import collections
import json
import sys


def main():
    path = sys.argv[1]
    show = 0
    fam_only = None
    a = sys.argv[2:]
    while a:
        o = a.pop(0)
        if o == "--show":
            show = int(a.pop(0))
        elif o == "--family":
            fam_only = a.pop(0)
    data = json.load(open(path))
    res = data["results"]
    samples = data["samples"]
    meta = data["meta"]
    print(f"# {meta['os']} {meta['machine']}, {len(res)} programs, {meta['seconds']:.0f}s")
    by_fam = collections.defaultdict(list)
    for cid, r in res.items():
        by_fam[r["family"]].append((cid, r))
    for fam, items in by_fam.items():
        if fam_only and fam != fam_only:
            continue
        n = len(items)
        stable = 0
        differs = collections.Counter()
        cond_names = []
        statuses = collections.Counter()
        qj_match = qj_total = 0
        unstable_examples = []
        cond_examples = collections.defaultdict(list)
        mismatch_examples = []
        for cid, r in items:
            runs = r["runs"]
            d = runs.get("default", [])
            dset = {tuple(x) for x in d}
            if len(dset) == 1:
                stable += 1
            else:
                unstable_examples.append((cid, r))
            base = tuple(d[0]) if d else None
            statuses[base[0] if base else "?"] += 1
            for cname, outs in runs.items():
                if cname in ("default", "qj"):
                    continue
                if cname not in cond_names:
                    cond_names.append(cname)
                if any(tuple(o) not in dset for o in outs):
                    differs[cname] += 1
                    cond_examples[cname].append((cid, r))
            if "qj" in runs:
                qj_total += 1
                if len(dset) == 1 and tuple(runs["qj"][0]) in dset:
                    qj_match += 1
                else:
                    mismatch_examples.append((cid, r))
        print(f"\n## {fam}: {n} programs")
        print(f"- same outcome on every default run: {stable}/{n}")
        for c in cond_names:
            print(f"- outcome differs from default with {c}: {differs[c]}/{n}")
        print(f"- default outcomes by status: {dict(statuses)}")
        if qj_total:
            print(f"- qj (QJ_JQ_COMPAT=1) matches jq's (stable) default outcome: {qj_match}/{qj_total}")

        def dump(title, ex):
            if not ex or not show:
                return
            print(f"\n### {title} ({len(ex)}), first {min(show, len(ex))}:")
            for cid, r in ex[:show]:
                print(f"- {cid}: args={r['args']} stdin={r['stdin']!r}")
                for cname, outs in r["runs"].items():
                    shown = []
                    for st, o, e in outs:
                        shown.append(f"{st} out={samples.get(o, o)[:160]!r} err={samples.get(e, e)[:160]!r}")
                    uniq = list(dict.fromkeys(shown))
                    print(f"    {cname}: " + "\n      ".join(uniq))

        dump("unstable across default runs", unstable_examples)
        for c in cond_names:
            dump(f"differs with {c}", cond_examples[c])
        dump("qj differs", mismatch_examples)


if __name__ == "__main__":
    main()
