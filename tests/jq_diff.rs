//! jq_diff: strict differential conformance harness, qj vs jq 1.8.1.
//!
//! Every case runs jq and qj with identical argv, stdin, environment and
//! working directory, and compares stdout bytes, exit code, and stderr after
//! rewriting a line-initial `qj:` program-name prefix to `jq:`. Nothing else
//! is normalized. jq's results are the only expectations: the `.test` files'
//! expected-output lines are never used.
//!
//! Levels: `pass` (all three equal), `stdout` (stdout + exit code equal,
//! stderr differs), `fail`. A case is skipped only when jq itself times out or
//! floods the output cap.
//!
//! Case sources (see `tests/jq_diff/cases.rs` for modes):
//! - `tests/jq_compat/*.test`: jq 1.8.1's own suites (`upstream/...`).
//! - `tests/jq_compat/corpus/*.test`: qj's corpus in the same format.
//! - `tests/jq_compat/corpus/*.toml`: CLI cases (`tests/jq_diff/cli.rs`).
//!
//! Run: `cargo test --release jq_diff -- --ignored` (the scoreboard goes to
//! stderr and is visible without `--nocapture`). Environment knobs:
//! - `JQ_DIFF_FILTER=a,b`: only cases whose id contains one of the substrings
//!   (ids look like `upstream/man.test:280:compact`).
//! - `JQ_DIFF_MODES=compact,pretty,file,ndjson,fail,cli`: subset of modes.
//! - `JQ_DIFF_VERBOSE=1`: print every non-passing case with its program,
//!   input, and jq vs qj stdout/stderr/exit code.
//! - `JQ_DIFF_QJ_ENV="K=V K2=V2"`: extra environment (e.g. `QJ_CORE=port`).
//!   It is given to jq too, so `env`/`$ENV` output stays comparable.
//! - `JQ_DIFF_BASELINE=path`: ratchet baseline (default
//!   `tests/jq_compat/diff_baseline.txt` on macOS,
//!   `tests/jq_compat/diff_baseline_<os>.txt` elsewhere).
//! - `JQ_DIFF_UPDATE_BASELINE=1`: rewrite the baseline from this run.
//! - `JQ_DIFF_JQ`, `JQ_DIFF_QJ`: binaries (default: `jq` on PATH, cargo's qj).
//! - `JQ_DIFF_TIMEOUT` (seconds, default 10), `JQ_DIFF_MEM_MB` (resident
//!   memory cap per process, default 2048), `JQ_DIFF_JOBS` (parallelism).
//!
//! The test fails when a case in the baseline drops to a lower level. Full
//! details of every non-passing case are written to
//! `target/tmp/jq_diff/report.txt`, and one line per case to `results.tsv`.

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
    fn from_env() -> Result<Config, String> {
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
        let extra_env = env_nonempty("JQ_DIFF_QJ_ENV")
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
        let default_baseline = if std::env::consts::OS == "macos" {
            "tests/jq_compat/diff_baseline.txt".to_string()
        } else {
            format!("tests/jq_compat/diff_baseline_{}.txt", std::env::consts::OS)
        };
        let baseline = root().join(env_nonempty("JQ_DIFF_BASELINE").unwrap_or(default_baseline));
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
        self.filters.is_empty() && self.modes.len() == cases::ALL_MODES.len()
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

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &dest)?;
        } else {
            std::fs::copy(e.path(), dest)?;
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
}

fn observe(ctx: &Ctx, bin: &Path, job: &Job, keep_verbatim: bool) -> (Observed, Vec<u8>, Duration) {
    let env: Vec<(String, String)> = ctx.env.iter().chain(&job.inv.env).cloned().collect();
    let started = Instant::now();
    let out = exec::run(&exec::Spec {
        bin,
        args: &job.inv.args,
        cwd: &ctx.work.join(&job.inv.cwd),
        env: &env,
        stdin: job.inv.stdin.as_deref(),
        timeout: ctx.timeout,
        max_output: MAX_OUTPUT,
        max_rss: ctx.max_rss,
    })
    .unwrap_or_else(|e| panic!("{}: {e}", job.id));
    let elapsed = started.elapsed();
    let normalized = compare::normalize_stderr(&out.stderr);
    (
        Observed {
            status: out.status,
            stdout: Blob::new(out.stdout, keep_verbatim),
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
    if let exec::Status::Timeout | exec::Status::OutputLimit | exec::Status::MemoryLimit =
        jq_obs.status
    {
        return Outcome {
            verdict: compare::classify(&jq_obs, &jq_obs),
            detail: None,
            jq_time,
            qj_time: None,
        };
    }
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
        Verdict::Skipped(_) => "SKIP",
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
    if let Verdict::Skipped(why) = outcome.verdict {
        let _ = writeln!(s, "  skipped: {why}");
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
    pass: usize,
    stdout_only: usize,
    /// stdout-only cases where jq exited 3 (compile error wording).
    compile_err: usize,
    fail: usize,
    skip: usize,
}

impl Tally {
    fn add(&mut self, outcome: &Outcome) {
        self.cases += 1;
        match outcome.verdict {
            Verdict::Level(Level::Pass) => self.pass += 1,
            Verdict::Level(Level::Stdout) => {
                self.stdout_only += 1;
                if matches!(&outcome.detail, Some((jq, _, _)) if jq.status == exec::Status::Exit(3))
                {
                    self.compile_err += 1;
                }
            }
            Verdict::Level(Level::Fail) => self.fail += 1,
            Verdict::Skipped(_) => self.skip += 1,
        }
    }

    fn merge(&mut self, o: &Tally) {
        self.cases += o.cases;
        self.pass += o.pass;
        self.stdout_only += o.stdout_only;
        self.compile_err += o.compile_err;
        self.fail += o.fail;
        self.skip += o.skip;
    }

    fn row(&self, label: &str, mode: &str) -> String {
        let judged = self.cases - self.skip;
        let pct = |n: usize| {
            if judged == 0 {
                0.0
            } else {
                n as f64 * 100.0 / judged as f64
            }
        };
        let out = self.pass + self.stdout_only;
        format!(
            "{label:<34} {mode:<8} {:>6} {:>6} {:>6.1}% {:>6} {:>6.1}% {:>6} {:>5} {:>5}",
            self.cases,
            self.pass,
            pct(self.pass),
            out,
            pct(out),
            self.fail,
            self.compile_err,
            self.skip
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
        "{:<34} {:<8} {:>6} {:>6} {:>7} {:>6} {:>7} {:>6} {:>5} {:>5}",
        "group", "mode", "cases", "strict", "", "out", "", "fail", "cerr", "skip"
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

#[test]
#[ignore]
fn jq_diff() {
    let started = Instant::now();
    let cfg = Config::from_env().unwrap_or_else(|e| panic!("jq_diff: {e}"));

    let Some(jq) = cfg
        .jq
        .clone()
        .or_else(|| find_on_path("jq").map(resolve_shim))
    else {
        say!("jq_diff: skipped: jq not found on PATH (set JQ_DIFF_JQ)");
        return;
    };
    let version = jq_version(&jq).unwrap_or_default();
    if version != REQUIRED_JQ {
        say!(
            "jq_diff: skipped: {} is {version:?}, need {REQUIRED_JQ} (set JQ_DIFF_JQ)",
            jq.display()
        );
        return;
    }

    // Work directory: cwd for every case, HOME, test modules, input files.
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("jq_diff");
    let modules = work.join("modules");
    let _ = std::fs::remove_dir_all(&modules);
    copy_dir(&root().join("tests/jq_compat/modules"), &modules).expect("copy modules");
    std::fs::create_dir_all(work.join("home")).expect("create home");

    let all_jobs = cases::collect(root(), &cfg.modes).unwrap_or_else(|e| panic!("jq_diff: {e}"));
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
    hash_dir(&mut fixtures, &modules, "modules");
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
    let cache_path = root().join("tests/jq_compat/.cache/jq_diff.json");
    let cache = cache::Cache::load(&cache_path, header);

    let ctx = Ctx {
        work: &work,
        env,
        timeout: cfg.timeout,
        max_rss: cfg.max_rss,
    };
    let keys: Vec<String> = jobs.iter().map(|j| j.inv.key(&cfg.extra_env)).collect();
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
        say!("jq_diff: warning: could not save cache: {e}");
    }

    // Reports.
    let mut report = String::new();
    let mut tsv = String::from("id\tlevel\tgroup\tmode\torigin\n");
    for (job, outcome) in jobs.iter().zip(&outcomes) {
        let level = match outcome.verdict {
            Verdict::Level(l) => l.name(),
            Verdict::Skipped(_) => "skip",
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
    let _ = std::fs::write(work.join("report.txt"), &report);
    let _ = std::fs::write(work.join("results.tsv"), &tsv);

    say!(
        "\njq_diff: {version} vs {} | {} cases ({} selected) | {} jobs | {:.1}s | jq cache: {} loaded{}, {} new",
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
    if !cfg.extra_env.is_empty() {
        say!("jq_diff: extra env: {:?}", cfg.extra_env);
    }
    say!(
        "strict = stdout + exit code + stderr match; out = stdout + exit code match;\n\
         cerr = stderr-only cases where jq exited 3 (compile error wording)\n"
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
        work.join("report.txt").display(),
        work.join("results.tsv").display(),
        work.join("baseline_candidate.txt").display()
    );

    // Ratchet.
    let current: Vec<baseline::Current> = jobs
        .iter()
        .zip(&outcomes)
        .map(|(job, o)| baseline::Current {
            id: &job.id,
            fp: &job.fp,
            level: match o.verdict {
                Verdict::Level(l) => Some(l),
                Verdict::Skipped(_) => None,
            },
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
            Some(baseline::parse(&text).unwrap_or_else(|e| panic!("jq_diff: {rel_baseline}: {e}")))
        }
        Err(_) => None,
    };

    // What the baseline would become from this run, written on every run
    // (e.g. for CI to publish a baseline for its platform).
    let new = baseline::update(old.as_deref().unwrap_or(&[]), &current, !cfg.complete());
    let rendered = baseline::render(&new);
    let _ = std::fs::write(work.join("baseline_candidate.txt"), &rendered);

    if cfg.update_baseline {
        std::fs::write(&cfg.baseline, &rendered).expect("write baseline");
        let passing = new.iter().filter(|e| e.level == Level::Pass).count();
        say!(
            "\njq_diff: wrote {rel_baseline}: {} entries ({passing} pass, {} stdout)",
            new.len(),
            new.len() - passing
        );
        return;
    }

    let Some(old) = old else {
        say!(
            "\njq_diff: no baseline at {rel_baseline}; nothing to ratchet against.\n\
             Create it with JQ_DIFF_UPDATE_BASELINE=1, or commit baseline_candidate.txt."
        );
        return;
    };
    let d = baseline::diff(&old, &current);
    if !d.improvements.is_empty() {
        say!(
            "\njq_diff: {} cases improved on {rel_baseline}:",
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
            "\njq_diff: {} baseline entries match no case (edited or removed); \
             refresh with JQ_DIFF_UPDATE_BASELINE=1:",
            d.unmatched.len()
        );
        for id in d.unmatched.iter().take(20) {
            say!("  {id}");
        }
    }
    if !d.regressions.is_empty() {
        say!(
            "\njq_diff: {} REGRESSIONS against {rel_baseline}:",
            d.regressions.len()
        );
        for (id, before, now) in &d.regressions {
            say!("  {id}: {} -> {}", before.name(), now.name());
        }
        panic!(
            "jq_diff: {} cases regressed (details in {})",
            d.regressions.len(),
            work.join("report.txt").display()
        );
    }
    say!(
        "\njq_diff: no regressions against {rel_baseline} ({} entries)",
        old.len()
    );
}
