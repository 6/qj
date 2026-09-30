//! jq_diff: strict differential conformance harness, qj vs jq 1.8.1.
//!
//! Every case runs jq and qj with identical argv, stdin, environment and
//! working directory, and compares stdout bytes, exit code (or the signal,
//! and whether the kernel dumped a core), and stderr. jq's results are the
//! only expectations: the `.test` files' expected-output lines are never used.
//!
//! Two scoreboards run the same cases, one after the other:
//! - `jq_diff`: qj as it is. stderr is compared after rewriting the program
//!   name: a line-initial `qj:` prefix to `jq:`, and the usage-hint line
//!   `Use qj --help for help with command-line options,` to jq's. Nothing else
//!   is normalized.
//! - `jq_diff_compat`: `QJ_JQ_COMPAT=1` (given to both tools, so `$ENV` stays
//!   comparable), both started as `argv[0]` = `jq`, and nothing normalized at
//!   all: qj has to be jq, name, help and version text included.
//!
//! Levels: `pass` (all three equal), `stdout` (stdout + exit code equal,
//! stderr differs), `fail`. Where jq never finishes (killed at the timeout,
//! the output cap or the memory cap), qj matches only by not finishing the
//! same way: the same cap, and the same stdout and stderr up to it. That is a
//! pass, shown in the scoreboard's `unfin` column; anything else is a fail.
//!
//! Case sources (see `tests/jq_diff/cases.rs` for modes):
//! - `tests/jq_compat/*.test`: jq 1.8.1's own suites (`upstream/...`).
//! - `tests/jq_compat/corpus/*.test`: qj's corpus in the same format.
//! - `tests/jq_compat/corpus/*.toml`: CLI cases (`tests/jq_diff/cli.rs`).
//!
//! Run: `cargo test --release jq_diff -- --ignored` runs both scoreboards;
//! `cargo test --release jq_diff_compat -- --ignored` only compat's, and
//! `cargo test --release jq_diff -- --ignored --exact` only the default one.
//! (The scoreboards go to stderr and are visible without `--nocapture`.)
//! Environment knobs:
//! - `JQ_DIFF_FILTER=a,b`: only cases whose id contains one of the substrings
//!   (ids look like `upstream/man.test:280:compact`).
//! - `JQ_DIFF_MODES=compact,pretty,file,ndjson,fail,cli`: subset of modes.
//! - `JQ_DIFF_VERBOSE=1`: print every non-passing case with its program,
//!   input, and jq vs qj stdout/stderr/exit code.
//! - `JQ_DIFF_QJ_ENV="K=V K2=V2"`: extra environment (e.g. `QJ_NO_TAPE=1`).
//!   It is given to jq too, so `env`/`$ENV` output stays comparable.
//! - `JQ_DIFF_BASELINE=path`, `JQ_DIFF_COMPAT_BASELINE=path`: ratchet
//!   baselines (defaults `tests/jq_compat/diff_baseline.txt` and
//!   `diff_baseline_compat.txt` on macOS, `diff_baseline_<os>.txt` and
//!   `diff_baseline_compat_<os>.txt` elsewhere).
//! - `JQ_DIFF_UPDATE_BASELINE=1`: rewrite the baselines from this run.
//! - `JQ_DIFF_JQ`, `JQ_DIFF_QJ`: binaries (default: `jq` on PATH, cargo's qj).
//! - `JQ_DIFF_TIMEOUT` (seconds, default 10), `JQ_DIFF_MEM_MB` (resident
//!   memory cap per process, default 2048), `JQ_DIFF_JOBS` (parallelism).
//!
//! The test fails when a case in the baseline drops to a lower level. Full
//! details of every non-passing case are written to
//! `target/tmp/jq_diff/report.txt` (compat: `target/tmp/jq_diff/compat/`),
//! one line per case to `results.tsv`, and the baseline this run would
//! produce to `baseline_candidate.txt`.

#[path = "jq_diff/baseline.rs"]
mod baseline;
#[path = "jq_diff/cache.rs"]
mod cache;
#[path = "jq_diff/cases.rs"]
mod cases;
#[path = "jq_diff/cli.rs"]
mod cli;
#[path = "jq_diff/compare.rs"]
mod compare;
#[path = "jq_diff/exec.rs"]
mod exec;
#[path = "jq_diff/hash.rs"]
mod hash;
#[path = "jq_diff/testfile.rs"]
mod testfile;

use cases::{Job, Mode};
use compare::{Blob, Level, Observed, Verdict};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REQUIRED_JQ: &str = "jq-1.8.1";
const MAX_OUTPUT: usize = 16 << 20;

/// Write to the real stderr, bypassing libtest's output capture.
macro_rules! say {
    ($($arg:tt)*) => {{
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// Which scoreboard a run is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Board {
    /// qj as it is, with its name mapped back to jq's in stderr.
    Default,
    /// `QJ_JQ_COMPAT=1` for both tools, both started as `jq`, and nothing
    /// normalized.
    Compat,
}

impl Board {
    fn name(self) -> &'static str {
        match self {
            Board::Default => "jq_diff",
            Board::Compat => "jq_diff_compat",
        }
    }

    /// The environment variable naming this board's baseline, and its
    /// default for this OS.
    fn baseline(self) -> (&'static str, String) {
        let (var, stem) = match self {
            Board::Default => ("JQ_DIFF_BASELINE", "diff_baseline"),
            Board::Compat => ("JQ_DIFF_COMPAT_BASELINE", "diff_baseline_compat"),
        };
        let file = if std::env::consts::OS == "macos" {
            format!("tests/jq_compat/{stem}.txt")
        } else {
            format!("tests/jq_compat/{stem}_{}.txt", std::env::consts::OS)
        };
        (var, file)
    }

    /// Where jq's results are cached. One file per board: a complete run
    /// prunes what it didn't run, which would be the other board's entries.
    fn cache_file(self) -> &'static str {
        match self {
            Board::Default => "tests/jq_compat/.cache/jq_diff.json",
            Board::Compat => "tests/jq_compat/.cache/jq_diff_compat.json",
        }
    }

    /// Where the reports go, under the shared work directory.
    fn out_dir(self, work: &Path) -> PathBuf {
        match self {
            Board::Default => work.to_path_buf(),
            Board::Compat => work.join("compat"),
        }
    }

    /// `argv[0]` for both tools (`None`: the binary's path).
    fn arg0(self) -> Option<&'static str> {
        match self {
            Board::Default => None,
            Board::Compat => Some("jq"),
        }
    }

    /// Whether stderr's program name is mapped back to jq's.
    fn normalizes(self) -> bool {
        self == Board::Default
    }

    /// Environment this board adds for both tools, ahead of `JQ_DIFF_QJ_ENV`.
    fn env(self) -> Vec<(String, String)> {
        match self {
            Board::Default => Vec::new(),
            Board::Compat => vec![("QJ_JQ_COMPAT".into(), "1".into())],
        }
    }
}

struct Config {
    filters: Vec<String>,
    modes: Vec<Mode>,
    verbose: bool,
    extra_env: Vec<(String, String)>,
    baseline: PathBuf,
    update_baseline: bool,
    timeout: Duration,
    max_rss: u64,
    jobs: usize,
    jq: Option<PathBuf>,
    qj: PathBuf,
}

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    fn from_env(board: Board) -> Result<Config, String> {
        let filters = env_nonempty("JQ_DIFF_FILTER")
            .map(|f| f.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        let modes = match env_nonempty("JQ_DIFF_MODES") {
            None => cases::ALL_MODES.to_vec(),
            Some(m) => m
                .split(',')
                .map(|s| Mode::parse(s.trim()).ok_or_else(|| format!("unknown mode {s:?}")))
                .collect::<Result<_, _>>()?,
        };
        let user_env = env_nonempty("JQ_DIFF_QJ_ENV")
            .map(|e| {
                e.split_whitespace()
                    .map(|kv| {
                        kv.split_once('=')
                            .map(|(k, v)| (k.to_string(), v.to_string()))
                            .ok_or_else(|| format!("JQ_DIFF_QJ_ENV: expected K=V, got {kv:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let extra_env = board.env().into_iter().chain(user_env).collect();
        let (baseline_var, default_baseline) = board.baseline();
        let baseline = root().join(env_nonempty(baseline_var).unwrap_or(default_baseline));
        let timeout = env_nonempty("JQ_DIFF_TIMEOUT")
            .map(|t| {
                t.parse::<f64>()
                    .map_err(|e| format!("JQ_DIFF_TIMEOUT: {e}"))
            })
            .transpose()?
            .unwrap_or(10.0);
        let mem_mb = env_nonempty("JQ_DIFF_MEM_MB")
            .map(|m| m.parse::<u64>().map_err(|e| format!("JQ_DIFF_MEM_MB: {e}")))
            .transpose()?
            .unwrap_or(2048);
        let jobs = env_nonempty("JQ_DIFF_JOBS")
            .map(|j| j.parse::<usize>().map_err(|e| format!("JQ_DIFF_JOBS: {e}")))
            .transpose()?
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
            });
        Ok(Config {
            filters,
            modes,
            verbose: env_nonempty("JQ_DIFF_VERBOSE").is_some_and(|v| v != "0"),
            extra_env,
            baseline,
            update_baseline: env_nonempty("JQ_DIFF_UPDATE_BASELINE").is_some_and(|v| v != "0"),
            timeout: Duration::from_secs_f64(timeout),
            max_rss: mem_mb << 20,
            jobs: jobs.max(1),
            jq: env_nonempty("JQ_DIFF_JQ").map(PathBuf::from),
            qj: env_nonempty("JQ_DIFF_QJ")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_qj"))),
        })
    }

    /// Whether this run covers every case (so the baseline can be pruned).
    fn complete(&self) -> bool {
        self.filters.is_empty() && cases::ALL_MODES.iter().all(|m| self.modes.contains(m))
    }

    fn selects(&self, job: &Job) -> bool {
        self.filters.is_empty() || self.filters.iter().any(|f| job.id.contains(f.as_str()))
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
}

/// Cases run with a cleared environment, which breaks version-manager shims
/// (mise's `jq` shim is the `mise` binary and needs mise's environment). When
/// `jq` on PATH resolves to something not named `jq`, ask mise for the real
/// binary.
fn resolve_shim(jq: PathBuf) -> PathBuf {
    let real = std::fs::canonicalize(&jq).unwrap_or_else(|_| jq.clone());
    if real.file_name().is_some_and(|n| n == "mise") {
        let out = std::process::Command::new(&real)
            .args(["which", "jq"])
            .current_dir(root())
            .output();
        if let Ok(out) = out {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() && !path.is_empty() {
                return PathBuf::from(path);
            }
        }
    }
    jq
}

/// The environment both tools run with (plus any case-specific variables).
fn base_env(work: &Path) -> Vec<(String, String)> {
    vec![
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("HOME".into(), work.join("home").display().to_string()),
        // As in jq's tests/setup: some output is locale-dependent.
        ("LC_ALL".into(), "C".into()),
        // Fixed, non-UTC zone: local-time builtins are deterministic but
        // still exercised. jq's man.test expects PAGER=less (tests/mantest).
        ("TZ".into(), "America/New_York".into()),
        ("PAGER".into(), "less".into()),
    ]
}

/// Copy a directory tree, rewriting only files whose content differs, so a
/// concurrent run in the same checkout never sees a missing file.
fn sync_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            sync_dir(&e.path(), &dest)?;
        } else {
            let content = std::fs::read(e.path())?;
            if std::fs::read(&dest).ok().as_deref() != Some(content.as_slice()) {
                std::fs::write(dest, content)?;
            }
        }
    }
    Ok(())
}

fn hash_dir(h: &mut hash::Fnv128, dir: &Path, rel: &str) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_default();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = format!("{rel}/{}", e.file_name().to_string_lossy());
        if e.path().is_dir() {
            hash_dir(h, &e.path(), &name);
        } else {
            h.str(&name)
                .field(&std::fs::read(e.path()).unwrap_or_default());
        }
    }
}

/// Write every input file the jobs need (content-addressed, so existing
/// files are reused).
fn materialize(work: &Path, jobs: &[&Job]) -> std::io::Result<()> {
    let mut done = HashSet::new();
    for job in jobs {
        for (rel, content) in &job.inv.files {
            if !done.insert(rel.as_str()) {
                continue;
            }
            let path = work.join(rel);
            if std::fs::read(&path).ok().as_deref() == Some(content.as_slice()) {
                continue;
            }
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, content)?;
        }
        std::fs::create_dir_all(work.join(&job.inv.cwd))?;
    }
    Ok(())
}

struct Ctx<'a> {
    work: &'a Path,
    env: Vec<(String, String)>,
    timeout: Duration,
    max_rss: u64,
    board: Board,
}

fn observe(ctx: &Ctx, bin: &Path, job: &Job, keep_verbatim: bool) -> (Observed, Vec<u8>, Duration) {
    let env: Vec<(String, String)> = ctx.env.iter().chain(&job.inv.env).cloned().collect();
    let started = Instant::now();
    let out = exec::run(&exec::Spec {
        bin,
        arg0: ctx.board.arg0(),
        args: &job.inv.args,
        cwd: &ctx.work.join(&job.inv.cwd),
        env: &env,
        stdin: job.inv.stdin.as_deref(),
        timeout: ctx.timeout,
        max_output: MAX_OUTPUT,
        max_rss: job.inv.mem_mb.map_or(ctx.max_rss, |mb| mb << 20),
        merge: job.inv.merge,
        close_fds: &job.inv.close_fds,
    })
    .unwrap_or_else(|e| panic!("{}: {e}", job.id));
    let elapsed = started.elapsed();
    let (normalized, stdout) = if !ctx.board.normalizes() {
        (out.stderr.clone(), out.stdout)
    } else if job.inv.merge == exec::Merge::No {
        (compare::normalize_stderr(&out.stderr), out.stdout)
    } else {
        // Merged, stdout carries stderr's messages too.
        (
            compare::normalize_stderr(&out.stderr),
            compare::normalize_merged(&out.stdout),
        )
    };
    (
        Observed {
            status: out.status,
            stdout: Blob::new(stdout, keep_verbatim),
            stderr: Blob::new(normalized, keep_verbatim),
        },
        out.stderr,
        elapsed,
    )
}

struct Outcome {
    verdict: Verdict,
    /// jq, qj and qj's raw stderr; kept only for non-passing cases.
    detail: Option<(Observed, Observed, Vec<u8>)>,
    /// Wall time of jq (when not cached) and qj.
    jq_time: Option<Duration>,
    qj_time: Option<Duration>,
}

fn run_job(ctx: &Ctx, cache: &cache::Cache, key: &str, jq: &Path, qj: &Path, job: &Job) -> Outcome {
    let (jq_obs, jq_time) = match cache.get(key) {
        Some(o) => (o, None),
        None => {
            let (o, _, t) = observe(ctx, jq, job, false);
            cache.put(key.to_string(), o.clone());
            (o, Some(t))
        }
    };
    // qj runs even when jq never finished: it must not finish either, and in
    // the same way (see `compare::classify`).
    let (qj_obs, qj_raw_err, qj_time) = observe(ctx, qj, job, true);
    let verdict = compare::classify(&jq_obs, &qj_obs);
    let detail = (verdict != Verdict::Level(Level::Pass)).then_some((jq_obs, qj_obs, qj_raw_err));
    Outcome {
        verdict,
        detail,
        jq_time,
        qj_time: Some(qj_time),
    }
}

fn show_bytes(b: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(b);
    let mut out = String::new();
    for (n, c) in s.chars().enumerate() {
        if n == max {
            let _ = write!(out, "…(+{} bytes)", b.len().saturating_sub(out.len()));
            break;
        }
        out.push(c);
    }
    format!("{out:?}")
}

fn show_blob(b: &Blob, max: usize) -> String {
    match b.shown() {
        (bytes, None) => show_bytes(bytes, max),
        (head, Some(len)) => format!("{} …({len} bytes total)", show_bytes(head, max)),
    }
}

fn describe(job: &Job, outcome: &Outcome, max: usize) -> String {
    let mut s = String::new();
    let label = match outcome.verdict {
        Verdict::Level(Level::Fail) => "FAIL",
        Verdict::Level(Level::Stdout) => "STDERR",
        Verdict::Level(Level::Pass) => "PASS",
        Verdict::Unfinished => "UNFINISHED (a pass: neither tool finished, the same way)",
    };
    let _ = writeln!(s, "{label} {}  ({})", job.id, job.origin);
    if let Some(p) = &job.program {
        let _ = writeln!(s, "  program: {p}");
    }
    if let Some(i) = &job.input {
        let _ = writeln!(s, "  input:   {}", show_bytes(i.as_bytes(), max));
    } else if let Some(stdin) = &job.inv.stdin {
        let _ = writeln!(s, "  stdin:   {}", show_bytes(stdin, max));
    }
    let _ = writeln!(s, "  argv:    {:?}", job.inv.args);
    if !job.inv.env.is_empty() {
        let _ = writeln!(s, "  env:     {:?}", job.inv.env);
    }
    if let Some((jq, qj, qj_err)) = &outcome.detail {
        let _ = writeln!(
            s,
            "  jq exit {}\n    stdout: {}\n    stderr: {}",
            jq.status,
            show_blob(&jq.stdout, max),
            show_blob(&jq.stderr, max)
        );
        let _ = writeln!(
            s,
            "  qj exit {}\n    stdout: {}\n    stderr: {}",
            qj.status,
            show_blob(&qj.stdout, max),
            show_bytes(qj_err, max)
        );
    }
    s
}

#[derive(Default, Clone, Copy)]
struct Tally {
    cases: usize,
    /// Strict: stdout, exit code and stderr match, including `unfinished`.
    pass: usize,
    /// Of `pass`: programs jq never finishes, which qj didn't finish the same
    /// way.
    unfinished: usize,
    stdout_only: usize,
    /// stdout-only cases where jq exited 3 (compile error wording).
    compile_err: usize,
    fail: usize,
}

impl Tally {
    fn add(&mut self, outcome: &Outcome) {
        self.cases += 1;
        match outcome.verdict {
            Verdict::Level(Level::Pass) => self.pass += 1,
            Verdict::Unfinished => {
                self.pass += 1;
                self.unfinished += 1;
            }
            Verdict::Level(Level::Stdout) => {
                self.stdout_only += 1;
                if matches!(&outcome.detail, Some((jq, _, _)) if jq.status == exec::Status::Exit(3))
                {
                    self.compile_err += 1;
                }
            }
            Verdict::Level(Level::Fail) => self.fail += 1,
        }
    }

    fn merge(&mut self, o: &Tally) {
        self.cases += o.cases;
        self.pass += o.pass;
        self.unfinished += o.unfinished;
        self.stdout_only += o.stdout_only;
        self.compile_err += o.compile_err;
        self.fail += o.fail;
    }

    fn row(&self, label: &str, mode: &str) -> String {
        let pct = |n: usize| {
            if self.cases == 0 {
                0.0
            } else {
                n as f64 * 100.0 / self.cases as f64
            }
        };
        let out = self.pass + self.stdout_only;
        format!(
            "{label:<34} {mode:<8} {:>6} {:>6} {:>6.1}% {:>5} {:>6} {:>6.1}% {:>6} {:>5}",
            self.cases,
            self.pass,
            pct(self.pass),
            self.unfinished,
            out,
            pct(out),
            self.fail,
            self.compile_err,
        )
    }
}

fn scoreboard(jobs: &[&Job], outcomes: &[Outcome]) -> String {
    let mut groups: Vec<&str> = Vec::new();
    let mut cells: BTreeMap<(&str, Mode), Tally> = BTreeMap::new();
    for (job, outcome) in jobs.iter().zip(outcomes) {
        if !groups.contains(&job.group.as_str()) {
            groups.push(&job.group);
        }
        cells
            .entry((job.group.as_str(), job.mode))
            .or_default()
            .add(outcome);
    }
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<34} {:<8} {:>6} {:>6} {:>7} {:>5} {:>6} {:>7} {:>6} {:>5}",
        "group", "mode", "cases", "strict", "", "unfin", "out", "", "fail", "cerr"
    );
    let mut by_mode: BTreeMap<Mode, Tally> = BTreeMap::new();
    let mut by_source: Vec<(&str, Tally)> = Vec::new();
    for g in &groups {
        let source = g.split('/').next().unwrap_or("");
        let mut group_total = Tally::default();
        let mut rows = 0;
        for mode in cases::ALL_MODES {
            if let Some(t) = cells.get(&(g, mode)) {
                let _ = writeln!(s, "{}", t.row(if rows == 0 { g } else { "" }, mode.name()));
                rows += 1;
                group_total.merge(t);
                by_mode.entry(mode).or_default().merge(t);
            }
        }
        if rows > 1 {
            let _ = writeln!(s, "{}", group_total.row("", "all"));
        }
        match by_source.iter_mut().find(|(src, _)| *src == source) {
            Some((_, t)) => t.merge(&group_total),
            None => by_source.push((source, group_total)),
        }
    }
    let _ = writeln!(s);
    let mut total = Tally::default();
    for (src, t) in &by_source {
        let _ = writeln!(s, "{}", t.row(&format!("{src} (all)"), "all"));
        total.merge(t);
    }
    for (mode, t) in &by_mode {
        let _ = writeln!(s, "{}", t.row("TOTAL", mode.name()));
    }
    let _ = writeln!(s, "{}", total.row("TOTAL", "all"));
    s
}

fn jq_version(jq: &Path) -> Option<String> {
    let out = std::process::Command::new(jq)
        .arg("--version")
        .env_clear()
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn binary_identity(path: &Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{} {} {mtime}",
        path.display(),
        meta.map(|m| m.len()).unwrap_or(0)
    )
}

/// The two scoreboards never run at once: each already runs a job per core,
/// and libtest would start them side by side.
static ONE_BOARD_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The default scoreboard: qj as it is.
#[test]
#[ignore]
fn jq_diff() {
    run_board(Board::Default);
}

/// The compat scoreboard: `QJ_JQ_COMPAT=1`, both tools started as `jq`, and
/// nothing normalized.
#[test]
#[ignore]
fn jq_diff_compat() {
    run_board(Board::Compat);
}

fn run_board(board: Board) {
    // A board that failed poisons the lock; the other still runs.
    let _one = ONE_BOARD_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let started = Instant::now();
    let name = board.name();
    let cfg = Config::from_env(board).unwrap_or_else(|e| panic!("{name}: {e}"));

    let Some(jq) = cfg
        .jq
        .clone()
        .or_else(|| find_on_path("jq").map(resolve_shim))
    else {
        say!("{name}: skipped: jq not found on PATH (set JQ_DIFF_JQ)");
        return;
    };
    let version = jq_version(&jq).unwrap_or_default();
    if version != REQUIRED_JQ {
        say!(
            "{name}: skipped: {} is {version:?}, need {REQUIRED_JQ} (set JQ_DIFF_JQ)",
            jq.display()
        );
        return;
    }

    // Work directory: cwd for every case, HOME, test modules, input files.
    // Both boards share it (outputs can contain its path); each writes its
    // reports to its own directory.
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("jq_diff");
    let out_dir = board.out_dir(&work);
    std::fs::create_dir_all(&out_dir).expect("create the report directory");
    let modules_src = root().join("tests/jq_compat/modules");
    sync_dir(&modules_src, &work.join("modules")).expect("copy modules");
    std::fs::create_dir_all(work.join("home")).expect("create home");

    let all_jobs = cases::collect(root(), &cfg.modes).unwrap_or_else(|e| panic!("{name}: {e}"));
    let jobs: Vec<&Job> = all_jobs.iter().filter(|j| cfg.selects(j)).collect();
    materialize(&work, &jobs).expect("write input files");

    // The extra env is part of each entry's key (not the header), so runs
    // with and without it share one cache.
    let base = base_env(&work);
    let mut fixtures = hash::Fnv128::new();
    for (k, v) in &base {
        fixtures.str(k).str(v);
    }
    let env: Vec<(String, String)> = base
        .into_iter()
        .chain(cfg.extra_env.iter().cloned())
        .collect();
    hash_dir(&mut fixtures, &modules_src, "modules");
    let header = cache::Header {
        schema: cache::SCHEMA,
        jq_version: version.clone(),
        jq_binary: binary_identity(&jq),
        work_dir: work.display().to_string(),
        fixtures: fixtures.hex(),
        timeout_ms: cfg.timeout.as_millis() as u64,
        max_output: MAX_OUTPUT,
        max_rss: cfg.max_rss,
    };
    let cache_path = root().join(board.cache_file());
    let cache = cache::Cache::load(&cache_path, header);

    let ctx = Ctx {
        work: &work,
        env,
        timeout: cfg.timeout,
        max_rss: cfg.max_rss,
        board,
    };
    let keys: Vec<String> = jobs
        .iter()
        .map(|j| j.inv.key(&cfg.extra_env, board.arg0()))
        .collect();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.jobs)
        .build()
        .expect("thread pool");
    let outcomes: Vec<Outcome> = pool.install(|| {
        jobs.par_iter()
            .zip(keys.par_iter())
            .map(|(job, key)| run_job(&ctx, &cache, key, &jq, &cfg.qj, job))
            .collect()
    });

    let keep: HashSet<String> = keys.iter().cloned().collect();
    if let Err(e) = cache.save(&cache_path, cfg.complete().then_some(&keep)) {
        say!("{name}: warning: could not save cache: {e}");
    }

    // Reports.
    let mut report = String::new();
    let mut tsv = String::from("id\tlevel\tgroup\tmode\torigin\n");
    for (job, outcome) in jobs.iter().zip(&outcomes) {
        let level = match outcome.verdict {
            Verdict::Level(l) => l.name(),
            Verdict::Unfinished => "unfinished",
        };
        let _ = writeln!(
            tsv,
            "{}\t{level}\t{}\t{}\t{}",
            job.id,
            job.group,
            job.mode.name(),
            job.origin
        );
        if outcome.verdict != Verdict::Level(Level::Pass) {
            let _ = writeln!(report, "{}", describe(job, outcome, 4000));
            if cfg.verbose {
                say!("{}", describe(job, outcome, 600));
            }
        }
    }
    let _ = std::fs::write(out_dir.join("report.txt"), &report);
    let _ = std::fs::write(out_dir.join("results.tsv"), &tsv);

    say!(
        "\n{name}: {version} vs {} | {} cases ({} selected) | {} jobs | {:.1}s | jq cache: {} loaded{}, {} new",
        cfg.qj.display(),
        all_jobs.len(),
        jobs.len(),
        cfg.jobs,
        started.elapsed().as_secs_f64(),
        cache.loaded,
        if cache.invalidated {
            " (invalidated)"
        } else {
            ""
        },
        cache.misses()
    );
    match board {
        Board::Default => say!("{name}: qj's name in stderr mapped back to jq's"),
        Board::Compat => say!("{name}: argv[0] = jq for both tools, nothing normalized"),
    }
    if !cfg.extra_env.is_empty() {
        say!("{name}: extra env: {:?}", cfg.extra_env);
    }
    say!(
        "strict = stdout + exit code + stderr match; unfin = of those, programs jq never\n\
         finishes that qj didn't finish either, the same way; out = stdout + exit code\n\
         match; cerr = stderr-only cases where jq exited 3 (compile error wording)\n"
    );
    say!("{}", scoreboard(&jobs, &outcomes));
    // Slow cases are where a loaded machine could turn a result into a
    // timeout; keep an eye on anything approaching the limit.
    let slowest = |tool: &str, time_of: fn(&Outcome) -> Option<Duration>| {
        let mut timed: Vec<(Duration, &str)> = jobs
            .iter()
            .zip(&outcomes)
            .filter_map(|(j, o)| time_of(o).map(|t| (t, j.id.as_str())))
            .filter(|(t, _)| *t < cfg.timeout)
            .collect();
        timed.sort_by(|a, b| b.0.cmp(&a.0));
        if let Some((t, _)) = timed.first() {
            let top: Vec<String> = timed
                .iter()
                .take(3)
                .map(|(t, id)| format!("{:.2}s {id}", t.as_secs_f64()))
                .collect();
            let warn = if *t * 2 > cfg.timeout {
                "  (WARNING: over half the timeout)"
            } else {
                ""
            };
            say!("slowest {tool}: {}{warn}", top.join(", "));
        }
    };
    slowest("jq", |o| o.jq_time);
    slowest("qj", |o| o.qj_time);
    say!(
        "details: {}\n         {}\n         {}",
        out_dir.join("report.txt").display(),
        out_dir.join("results.tsv").display(),
        out_dir.join("baseline_candidate.txt").display()
    );

    // Ratchet.
    let current: Vec<baseline::Current> = jobs
        .iter()
        .zip(&outcomes)
        .map(|(job, o)| baseline::Current {
            id: &job.id,
            fp: &job.fp,
            level: Some(o.verdict.level()),
        })
        .collect();
    let rel_baseline = cfg
        .baseline
        .strip_prefix(root())
        .unwrap_or(&cfg.baseline)
        .display()
        .to_string();
    let old = match std::fs::read_to_string(&cfg.baseline) {
        Ok(text) => {
            Some(baseline::parse(&text).unwrap_or_else(|e| panic!("{name}: {rel_baseline}: {e}")))
        }
        Err(_) => None,
    };

    // What the baseline would become from this run, written on every run
    // (e.g. for CI to publish a baseline for its platform).
    let new = baseline::update(old.as_deref().unwrap_or(&[]), &current, !cfg.complete());
    let rendered = baseline::render(&new);
    let _ = std::fs::write(out_dir.join("baseline_candidate.txt"), &rendered);

    if cfg.update_baseline {
        std::fs::write(&cfg.baseline, &rendered).expect("write baseline");
        let passing = new.iter().filter(|e| e.level == Level::Pass).count();
        say!(
            "\n{name}: wrote {rel_baseline}: {} entries ({passing} pass, {} stdout)",
            new.len(),
            new.len() - passing
        );
        return;
    }

    let Some(old) = old else {
        say!(
            "\n{name}: no baseline at {rel_baseline}; nothing to ratchet against.\n\
             Create it with JQ_DIFF_UPDATE_BASELINE=1, or commit baseline_candidate.txt."
        );
        return;
    };
    let d = baseline::diff(&old, &current);
    if !d.improvements.is_empty() {
        say!(
            "\n{name}: {} cases improved on {rel_baseline}:",
            d.improvements.len()
        );
        for (id, before, now) in d.improvements.iter().take(200) {
            say!("  {id}: {} -> {}", before.name(), now.name());
        }
        if d.improvements.len() > 200 {
            say!("  ... and {} more", d.improvements.len() - 200);
        }
        say!("Record them with JQ_DIFF_UPDATE_BASELINE=1.");
    }
    if cfg.complete() && !d.unmatched.is_empty() {
        say!(
            "\n{name}: {} baseline entries match no case (edited or removed); \
             refresh with JQ_DIFF_UPDATE_BASELINE=1:",
            d.unmatched.len()
        );
        for id in d.unmatched.iter().take(20) {
            say!("  {id}");
        }
    }
    if !d.regressions.is_empty() {
        say!(
            "\n{name}: {} REGRESSIONS against {rel_baseline}:",
            d.regressions.len()
        );
        for (id, before, now) in &d.regressions {
            say!("  {id}: {} -> {}", before.name(), now.name());
        }
        panic!(
            "{name}: {} cases regressed (details in {})",
            d.regressions.len(),
            out_dir.join("report.txt").display()
        );
    }
    say!(
        "\n{name}: no regressions against {rel_baseline} ({} entries)",
        old.len()
    );
}
