# jq 1.8.1's undefined behaviour

These scripts produced the evidence in `docs/COMPATIBILITY.md`, under "jq's undefined
behaviour". They check whether jq's outcome on the programs that reach one of its three
undefined behaviours is a function of the program and input at all:
- `jv_dels`' double free;
- `--debug-trace=all` reading a stack word nothing wrote;
- `lgamma_r`'s uninitialized sign.

They aren't part of any test suite; run them by hand.

```
python3 ub_corpus.py > corpus.json              # the corpus as JSON, counts on stderr (ub_run.py imports it)
cc -o noaslr noaslr.c                            # macOS only: exec a program with ASLR off
python3 ub_run.py --jq "$(mise which jq)" --qj ../../../target/release/qj \
    --noaslr ./noaslr --jobs 4 --out results.json
python3 ub_analyze.py results.json               # stable per program? matches qj?
python3 ub_conddiff.py results.json              # what each condition changes
python3 ub_mismatch.py results.json dels         # where jq and qj differ, per family
python3 ub_hangs.py results.json                 # programs jq never finished
python3 ub_repeat.py "$(mise which jq)" 100 --stdin '[1]' -- -c 'try delpaths([[{}]]) catch .'  # one command, N runs
```

`ub_run.py` runs each case under several conditions, none of which change a program's
meaning:
- repeats in the default environment;
- the environment padded by 1, 4 and 16 KB;
- ASLR off (`setarch -R` on Linux, `noaslr` on macOS);
- allocator settings: `MallocNanoZone=0`, `MallocScribble=1` and `MallocGuardEdges=1` on
  macOS; `MALLOC_PERTURB_` and `glibc.malloc.tcache_count=0` on glibc.

`runproc.py` runs every child in its own process group under a timeout, and kills the group,
because the double free can spin forever at constant memory.

`ub_trace_rule.py` checks which trace programs actually reach an instruction that runs with
an empty data stack.
