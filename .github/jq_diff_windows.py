#!/usr/bin/env python3
"""jq_diff's cases, for a platform the Rust harness doesn't run on (Windows).

tests/jq_diff.rs is the conformance gate, but its runner is Unix-only (process
groups, rlimits, signals). This runs the same filter/input cases, from the same
files, the same way, under qj and under a jq 1.8.1 binary, and compares stdout
bytes, the exit code and stderr (jq_diff's normalization: a line-initial `qj:`
reads as `jq:`, as does qj's usage hint). jq is the only expectation.

- Cases: tests/jq_compat/*.test (jq's suites) and tests/jq_compat/corpus/*.test,
  parsed as tests/jq_diff/testfile.rs does, with its `# jq_diff: modes=/os=`
  directives; and the CLI cases, tests/jq_compat/corpus/*.toml, expanded as
  tests/jq_diff/cli.rs does (cases and sweeps, files, environment, stdin,
  stderr merged into stdout's file or pipe), each in its own directory two
  levels below the work directory. Left out: cases for another OS (`os`), and
  where this runner can't do what the harness does, cases that start with a
  standard descriptor closed (`close_fds`) and memory-capped ones (`mem_mb`,
  programs that grow until a cap stops them), and on Windows, cases with a file
  name Windows can't hold (`*.json`, for globbing) and compat-mode cases
  (QJ_JQ_COMPAT=1, which qj refuses on Windows); their count is printed.
- Modes, as tests/jq_diff/cases.rs builds them: compact (`-c`, stdin), pretty
  (stdin), file (`-c`, input as a file), ndjson (`-c`, the input twice in a
  file, for a single object or array and no input/$__loc__/halt), and fail
  (`-c -n`) for %%FAIL programs. `-L modules` for programs using modules, `--`
  before programs starting with `-`.
- Environment: jq_diff's (HOME in the work directory, LC_ALL=C,
  TZ=America/New_York, PAGER=less), plus what Windows needs to start a process.
- A case both tools time out on matches; there is no memory cap.

It prints a scoreboard per suite and mode, writes every mismatch to
OUT/report.txt and one line per case to OUT/results.tsv, and with --known FILE
fails on any mismatch that FILE doesn't list (one case id per line, `#`
comments; the differences Windows support accepts), and names listed cases that
now match, so the list can shrink.

    python3 .github/jq_diff_windows.py --qj path/to/qj --jq path/to/jq [--out DIR]
        [--modes compact,pretty,file,ndjson,fail] [--filter SUBSTR] [--jobs N] [--known FILE]
"""

import argparse
import base64
import concurrent.futures
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ALL_MODES = ["compact", "pretty", "file", "ndjson", "fail", "cli"]
OS_NAME = {"win32": "windows", "darwin": "macos"}.get(sys.platform, sys.platform)
if OS_NAME.startswith("linux"):
    OS_NAME = "linux"


# --- tests/jq_diff/testfile.rs ---------------------------------------------


def skipline(line):
    rest = line.lstrip(" \t")
    return rest == "" or rest.startswith("#") or rest.startswith("\n")


def parse_test_file(content):
    """(cases, modes, oses): cases are (line, program, input or None for %%FAIL)."""
    lines = content.splitlines(keepends=True)
    cases, modes, oses = [], None, None
    i, must_fail = 0, False
    while i < len(lines):
        raw = lines[i]
        lineno = i + 1
        i += 1
        if skipline(raw):
            rest = raw.rstrip()
            if rest.startswith("# jq_diff:"):
                for kv in rest[len("# jq_diff:"):].split():
                    k, _, v = kv.partition("=")
                    if k == "modes":
                        modes = v.split(",")
                    elif k == "os":
                        oses = v.split(",")
                    else:
                        raise ValueError(f"line {lineno}: unknown jq_diff directive {kv!r}")
            continue
        if raw in ("%%FAIL\n", "%%FAIL IGNORE MSG\n"):
            must_fail = True
            continue
        program = raw[:-1] if raw.endswith("\n") else raw
        if must_fail:
            must_fail = False
            while i < len(lines):
                i += 1
                if skipline(lines[i - 1]):
                    break
            cases.append((lineno, program, None))
            continue
        if i >= len(lines):
            raise ValueError(f"line {lineno}: program {program!r} has no input line")
        inp = lines[i]
        i += 1
        inp = inp[:-1] if inp.endswith("\n") else inp
        while i < len(lines):
            i += 1
            if skipline(lines[i - 1]):
                break
        cases.append((lineno, program, inp))
    return cases, modes, oses


def is_single_container(text):
    t = text.strip(" \t\r\n")
    if not t or t[0] not in "{[":
        return False
    depth, in_str, escaped = 0, False, False
    for pos, ch in enumerate(t):
        if in_str:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_str = False
            continue
        if ch == '"':
            in_str = True
        elif ch in "{[":
            depth += 1
        elif ch in "}]":
            if depth == 0:
                return False
            depth -= 1
            if depth == 0:
                return pos == len(t) - 1
    return False


# --- tests/jq_diff/cases.rs ------------------------------------------------


def uses_modules(program):
    return any(w in program for w in ("import", "include", "modulemeta", "get_search_list"))


def ndjson_eligible(program, inp):
    return is_single_container(inp) and not any(w in program for w in ("input", "$__loc__", "halt"))


def test_args(mode, program, file=None):
    a = {"pretty": [], "fail": ["-c", "-n"]}.get(mode, ["-c"])
    if uses_modules(program):
        a += ["-L", "modules"]
    if program.startswith("-"):
        a.append("--")
    a.append(program)
    if file:
        a.append(file)
    return a


def jobs_for(group, cases, file_modes, modes):
    def enabled(m):
        return m in modes and (m == "fail" or file_modes is None or m in file_modes)

    def job(mode, line, args, stdin=None, files=()):
        return {"id": f"{group}:{line}:{mode}", "group": group, "mode": mode, "args": args,
                "stdin": stdin, "files": list(files), "cwd": "", "env": {}, "merge": None}

    jobs = []
    for line, program, inp in cases:
        if inp is None:
            if enabled("fail"):
                jobs.append(job("fail", line, test_args("fail", program)))
            continue
        for mode in ("compact", "pretty", "file", "ndjson"):
            if not enabled(mode) or (mode == "ndjson" and not ndjson_eligible(program, inp)):
                continue
            data = (inp + "\n").encode()
            if mode in ("compact", "pretty"):
                jobs.append(job(mode, line, test_args(mode, program), stdin=data))
            else:
                ext = "json" if mode == "file" else "ndjson"
                content = data if mode == "file" else (inp + "\n" + inp + "\n").encode()
                name = f"in/{hashlib.sha1(content).hexdigest()[:16]}.{ext}"
                jobs.append(job(mode, line, test_args(mode, program, name), files=[(name, content)]))
    return jobs


# --- tests/jq_diff/cli.rs --------------------------------------------------


def content_bytes(c, base):
    """A case's `stdin` or file content: text, {b64}, {path} or {repeat}."""
    if isinstance(c, str):
        return c.encode()
    if "b64" in c:
        return base64.b64decode(c["b64"])
    if "path" in c:
        return (base / c["path"]).read_bytes()
    return b"".join(s.encode() * n for s, n in c["repeat"])


WINDOWS_RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)),
                    *(f"LPT{i}" for i in range(1, 10))}


def windows_file_name(name):
    """Whether Windows can hold a file at this relative path."""
    for seg in name.split("/"):
        if any(c in '<>:"|?*\\' or ord(c) < 32 for c in seg) or seg.endswith((".", " ")):
            return False
        if seg.split(".")[0].upper() in WINDOWS_RESERVED:
            return False
    return True


def cli_jobs(group, path, skipped):
    """The CLI cases of one TOML file, as tests/jq_diff/cli.rs expands them."""
    base = path.parent
    doc = tomllib.loads(path.read_text(encoding="utf-8"))
    cases = []
    for c in doc.get("case", []):
        cases.append((c["name"], c["args"], c, None))
    for sw in doc.get("sweep", []):
        for vname, template in sw["variants"].items():
            for i, program in enumerate(sw["programs"]):
                args = [program if a == "{program}" else a for a in template]
                cases.append((f"{sw['name']}/{vname}/{i}", args, sw, program))
    jobs = []
    for name, args, d, _ in cases:
        reason = ("os" if d.get("os") not in (None, OS_NAME) else
                  "close_fds" if d.get("close_fds") else
                  "mem_mb" if d.get("mem_mb") else
                  "compat mode" if OS_NAME == "windows" and d.get("env", {}).get(
                      "QJ_JQ_COMPAT", "") not in ("", "0") else
                  "file name" if OS_NAME == "windows" and not all(
                      windows_file_name(f) for f in d.get("files", {})) else None)
        if reason:
            skipped[reason] += 1
            continue
        stdin = content_bytes(d["stdin"], base) if "stdin" in d else None
        files = [(p, content_bytes(v, base)) for p, v in sorted(d.get("files", {}).items())]
        env = dict(d.get("env", {}))
        h = hashlib.sha1(repr((args, stdin, files, sorted(env.items()), d.get("merge"))).encode())
        cwd = f"cli/{h.hexdigest()[:16]}"
        jobs.append({"id": f"{group}:{name}:cli", "group": group, "mode": "cli", "args": args,
                     "stdin": stdin, "files": [(f"{cwd}/{p}", b) for p, b in files], "cwd": cwd,
                     "env": env, "merge": d.get("merge")})
    return jobs


def collect(modes, filt, skipped):
    jobs = []
    files = sorted((ROOT / "tests/jq_compat").glob("*.test")) + sorted(
        (ROOT / "tests/jq_compat/corpus").glob("*.test")
    )
    for path in files:
        rel = path.relative_to(ROOT / "tests/jq_compat").as_posix()
        group = rel if rel.startswith("corpus/") else f"upstream/{rel}"
        cases, file_modes, oses = parse_test_file(path.read_text(encoding="utf-8", errors="surrogateescape"))
        if oses is not None and OS_NAME not in oses:
            continue
        jobs += [j for j in jobs_for(group, cases, file_modes, modes) if filt in j["id"]]
    if "cli" in modes:
        for path in sorted((ROOT / "tests/jq_compat/corpus").glob("*.toml")):
            group = path.relative_to(ROOT / "tests/jq_compat").as_posix()
            jobs += [j for j in cli_jobs(group, path, skipped) if filt in j["id"]]
    return jobs


# --- running ---------------------------------------------------------------

QJ_PREFIX = re.compile(rb"(?m)^qj:")


def normalize_stderr(err):
    err = QJ_PREFIX.sub(b"jq:", err)
    return err.replace(b"Use qj --help for help with command-line options,", b"Use jq --help for help with command-line options,")


def run(tool, job, work, env, timeout):
    """(stdout, exit code or "timeout", stderr). Without stdin, stdin is the null
    device. Merged, stderr goes to stdout's file or pipe, compared as stdout."""
    cwd = work / job["cwd"]
    env = {**env, **job["env"]}
    stdin = subprocess.DEVNULL if job["stdin"] is None else subprocess.PIPE
    with tempfile.TemporaryFile() as merged:
        if job["merge"] == "file":
            out, err = merged, subprocess.STDOUT
        elif job["merge"] == "pipe":
            out, err = subprocess.PIPE, subprocess.STDOUT
        else:
            out, err = subprocess.PIPE, subprocess.PIPE
        p = subprocess.Popen([tool] + job["args"], stdin=stdin, stdout=out, stderr=err, cwd=cwd, env=env)
        try:
            o, e = p.communicate(job["stdin"], timeout=timeout)
            code = p.returncode
        except subprocess.TimeoutExpired:
            p.kill()
            o, e = p.communicate()
            code = "timeout"
        if job["merge"] == "file":
            merged.seek(0)
            o = merged.read()
    return (o or b"", code, e or b"")


def normalize_merged(stream):
    """tests/jq_diff/compare.rs normalize_merged: stdout's buffer goes out in
    blocks that split lines, so in a merged stream stderr's `qj: ` can start
    mid-line; it's rewritten wherever it appears, on both sides."""
    return normalize_stderr(stream.replace(b"qj: ", b"jq: "))


def compare(jq, qj, merged):
    jout, jcode, jerr = jq
    qout, qcode, qerr = qj
    if merged:
        jout, qout = normalize_merged(jout), normalize_merged(qout)
    if jcode == "timeout" or qcode == "timeout":
        return "pass" if jcode == qcode else "fail"
    if jout != qout or jcode != qcode:
        return "fail"
    return "pass" if jerr == normalize_stderr(qerr) else "stdout"


def show(b, limit=600):
    s = b.decode("utf-8", errors="backslashreplace")
    return (s[:limit] + "...") if len(s) > limit else s


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--qj", required=True)
    ap.add_argument("--jq", required=True)
    ap.add_argument("--out", default=str(ROOT / "target/tmp/jq_diff_windows"))
    ap.add_argument("--modes", default=",".join(ALL_MODES))
    ap.add_argument("--filter", default="")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--timeout", type=float, default=10.0)
    ap.add_argument("--known", help="file of case ids allowed to mismatch")
    a = ap.parse_args()

    qj, jq = os.path.abspath(a.qj), os.path.abspath(a.jq)
    version = subprocess.run([jq, "--version"], capture_output=True).stdout.decode().strip()
    if version != "jq-1.8.1":
        sys.exit(f"{jq} is {version!r}; jq_diff's cases need jq-1.8.1")
    modes = a.modes.split(",")
    skipped = defaultdict(int)
    jobs = collect(modes, a.filter, skipped)

    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="jq_diff_"))
    shutil.copytree(ROOT / "tests/jq_compat/modules", work / "modules")
    (work / "home").mkdir()
    (work / "in").mkdir()
    for job in jobs:
        (work / job["cwd"]).mkdir(parents=True, exist_ok=True)
        for name, content in job["files"]:
            (work / name).parent.mkdir(parents=True, exist_ok=True)
            (work / name).write_bytes(content)
    env = {"HOME": str(work / "home"), "LC_ALL": "C", "TZ": "America/New_York", "PAGER": "less"}
    # What a process needs to start (and its C runtime to find its files).
    keep = ["PATH", "SystemRoot", "SystemDrive", "WINDIR", "TEMP", "TMP", "ComSpec"] if OS_NAME == "windows" else []
    env.update({k: os.environ[k] for k in keep if k in os.environ})
    if OS_NAME != "windows":
        env["PATH"] = "/usr/bin:/bin"

    def one(job):
        j = run(jq, job, work, env, a.timeout)
        q = run(qj, job, work, env, a.timeout)
        return job, compare(j, q, job["merge"] is not None), j, q

    left_out = ", ".join(f"{n} {k}" for k, n in sorted(skipped.items())) or "none"
    print(f"jq_diff_windows: {version} vs {qj} | {len(jobs)} cases | {a.jobs} jobs | {OS_NAME}"
          f" | CLI cases left out: {left_out}", flush=True)
    board = defaultdict(lambda: defaultdict(int))
    report, results = [], []
    with concurrent.futures.ThreadPoolExecutor(max_workers=a.jobs) as pool:
        for n, (job, level, j, q) in enumerate(pool.map(one, jobs), 1):
            board[(job["group"], job["mode"])][level] += 1
            results.append(f"{job['id']}\t{level}\n")
            if level != "pass":
                extra = "".join(f"{k}: {job[k]!r}\n" for k in ("env", "merge") if job[k])
                report.append(
                    f"=== {job['id']} [{level}]\nargs: {job['args']}\n{extra}stdin: {show(job['stdin'] or b'')!r}\n"
                    f"jq exit {j[1]}\n--- jq stdout\n{show(j[0])}\n--- qj stdout\n{show(q[0])}\n"
                    f"--- jq stderr\n{show(j[2])}\n--- qj stderr\n{show(q[2])}\n"
                    f"qj exit {q[1]}\n"
                )
            if n % 2000 == 0:
                print(f"  {n}/{len(jobs)}", flush=True)
    shutil.rmtree(work, ignore_errors=True)

    # "\n" lines on every platform, as jq_diff's reports have.
    (out / "report.txt").write_text("".join(report), encoding="utf-8", newline="\n")
    (out / "results.tsv").write_text("".join(results), encoding="utf-8", newline="\n")
    rows, tot = [], defaultdict(int)
    for (group, mode), c in sorted(board.items()):
        n = sum(c.values())
        for k, v in c.items():
            tot[k] += v
        rows.append(f"{group:<38} {mode:<8} {c['pass']:>6}/{n:<6} stdout {c['stdout']:>4}  fail {c['fail']:>4}")
    n = sum(tot.values())
    rows.append(f"{'TOTAL':<38} {'all':<8} {tot['pass']:>6}/{n:<6} stdout {tot['stdout']:>4}  fail {tot['fail']:>4}")
    print("\n".join(rows))
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as f:
            f.write(f"### jq_diff's cases on {OS_NAME} ({version})\n```\n" + "\n".join(rows) + "\n```\n")
    if a.known:
        known = set()
        for line in Path(a.known).read_text(encoding="utf-8").splitlines():
            line = line.split("#", 1)[0].strip()
            if line:
                known.add(line)
        levels = dict(r.rstrip("\n").split("\t") for r in results)
        new = sorted(i for i, lv in levels.items() if lv != "pass" and i not in known)
        fixed = sorted(i for i in known if levels.get(i) == "pass")
        for i in fixed:
            print(f"now matches (remove from {a.known}): {i}")
        for i in new:
            print(f"MISMATCH not in {a.known}: {i} [{levels[i]}]")
        if new:
            sys.exit(1)


if __name__ == "__main__":
    main()
