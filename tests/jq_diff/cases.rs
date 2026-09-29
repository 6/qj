//! Turning case sources into concrete invocations ("jobs"), one per mode.
//!
//! Modes for `.test` filter/input cases (jq and qj always run identically):
//! - `compact`: `-c PROGRAM`, input line on stdin.
//! - `pretty`:  `PROGRAM`, input line on stdin.
//! - `file`:    `-c PROGRAM in/<hash>.json`, input written to a file.
//! - `ndjson`:  `-c PROGRAM in/<hash>.ndjson`, the input line twice. Only when
//!   the input is a single object or array and the program doesn't use
//!   `input*`, `$__loc__` or `halt*`.
//!
//! `%%FAIL` cases run once, in mode `fail`: `-c -n PROGRAM`.
//! CLI cases (`*.toml`) run once, in mode `cli`, with exactly their argv.
//!
//! `-L modules` is added when the program uses modules (`import`, `include`,
//! `modulemeta`, `get_search_list`), and `--` before programs starting with
//! `-`.

use crate::cli::{self, CliCase};
use crate::hash::Fnv128;
use crate::testfile::{self, Kind};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    Compact,
    Pretty,
    File,
    Ndjson,
    Fail,
    Cli,
}

pub const ALL_MODES: [Mode; 6] = [
    Mode::Compact,
    Mode::Pretty,
    Mode::File,
    Mode::Ndjson,
    Mode::Fail,
    Mode::Cli,
];

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Compact => "compact",
            Mode::Pretty => "pretty",
            Mode::File => "file",
            Mode::Ndjson => "ndjson",
            Mode::Fail => "fail",
            Mode::Cli => "cli",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        ALL_MODES.into_iter().find(|m| m.name() == s)
    }
}

/// One process invocation, run identically for jq and qj.
#[derive(Debug, Clone)]
pub struct Invocation {
    pub args: Vec<String>,
    /// `None`: stdin is /dev/null.
    pub stdin: Option<Vec<u8>>,
    /// Added to the base environment.
    pub env: Vec<(String, String)>,
    /// Working directory, relative to the harness work directory.
    pub cwd: String,
    /// Files to create first: (path relative to the work directory, content).
    pub files: Vec<(String, Vec<u8>)>,
}

impl Invocation {
    /// Cache key: everything that can influence the tool's behavior, apart
    /// from the global state covered by the cache header.
    pub fn key(&self, extra_env: &[(String, String)]) -> String {
        let mut h = Fnv128::new();
        h.str("jq_diff-invocation-v1");
        h.field(&(self.args.len() as u64).to_le_bytes());
        for a in &self.args {
            h.str(a);
        }
        match &self.stdin {
            None => h.str("stdin:null"),
            Some(b) => h.str("stdin:bytes").field(b),
        };
        for (k, v) in self.env.iter().chain(extra_env) {
            h.str("env").str(k).str(v);
        }
        h.str("cwd").str(&self.cwd);
        for (p, c) in &self.files {
            h.str("file").str(p).field(c);
        }
        h.hex()
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    /// Stable, human-readable id: `<group>:<line or name>:<mode>`.
    pub id: String,
    /// Scoreboard row, e.g. `upstream/jq.test` or `corpus/paths.test`.
    pub group: String,
    pub mode: Mode,
    /// Identifies the case independently of its position in the file; the
    /// baseline matches on (group, mode, fingerprint).
    pub fp: String,
    /// Where the case is defined, e.g. `tests/jq_compat/jq.test:12`.
    pub origin: String,
    pub program: Option<String>,
    pub input: Option<String>,
    pub inv: Invocation,
}

pub fn uses_modules(program: &str) -> bool {
    ["import", "include", "modulemeta", "get_search_list"]
        .iter()
        .any(|w| program.contains(w))
}

pub fn ndjson_eligible(program: &str, input: &str) -> bool {
    testfile::is_single_container(input)
        && !["input", "$__loc__", "halt"]
            .iter()
            .any(|w| program.contains(w))
}

fn test_args(mode: Mode, program: &str, file: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    match mode {
        Mode::Pretty => {}
        Mode::Fail => a.extend(["-c".into(), "-n".into()]),
        _ => a.push("-c".into()),
    }
    if uses_modules(program) {
        a.extend(["-L".into(), "modules".into()]);
    }
    if program.starts_with('-') {
        a.push("--".into());
    }
    a.push(program.into());
    if let Some(f) = file {
        a.push(f.into());
    }
    a
}

fn short_fp(h: &Fnv128) -> String {
    h.hex()[..12].to_string()
}

fn content_name(content: &[u8], ext: &str) -> String {
    let mut h = Fnv128::new();
    h.field(content);
    format!("in/{}.{ext}", &h.hex()[..16])
}

/// Jobs for one parsed `.test` file.
pub fn test_file_jobs(
    group: &str,
    origin_path: &str,
    file: &testfile::TestFile,
    modes: &[Mode],
) -> Vec<Job> {
    let file_modes: Option<Vec<Mode>> = file
        .modes
        .as_ref()
        .map(|ms| ms.iter().filter_map(|m| Mode::parse(m)).collect());
    let enabled = |m: Mode| {
        modes.contains(&m)
            && (m == Mode::Fail || file_modes.as_ref().is_none_or(|fm| fm.contains(&m)))
    };
    let mut jobs = Vec::new();
    for case in &file.cases {
        let origin = format!("{origin_path}:{}", case.line);
        match &case.kind {
            Kind::Fail { .. } => {
                if !enabled(Mode::Fail) {
                    continue;
                }
                let mut h = Fnv128::new();
                h.str("fail").str(&case.program);
                jobs.push(Job {
                    id: format!("{group}:{}:fail", case.line),
                    group: group.to_string(),
                    mode: Mode::Fail,
                    fp: short_fp(&h),
                    origin,
                    program: Some(case.program.clone()),
                    input: None,
                    inv: Invocation {
                        args: test_args(Mode::Fail, &case.program, None),
                        stdin: None,
                        env: Vec::new(),
                        cwd: String::new(),
                        files: Vec::new(),
                    },
                });
            }
            Kind::Normal { input, .. } => {
                let mut h = Fnv128::new();
                h.str("normal").str(&case.program).str(input);
                let fp = short_fp(&h);
                for mode in [Mode::Compact, Mode::Pretty, Mode::File, Mode::Ndjson] {
                    if !enabled(mode) {
                        continue;
                    }
                    if mode == Mode::Ndjson && !ndjson_eligible(&case.program, input) {
                        continue;
                    }
                    let line = format!("{input}\n");
                    let inv = match mode {
                        Mode::Compact | Mode::Pretty => Invocation {
                            args: test_args(mode, &case.program, None),
                            stdin: Some(line.into_bytes()),
                            env: Vec::new(),
                            cwd: String::new(),
                            files: Vec::new(),
                        },
                        _ => {
                            let (content, ext) = if mode == Mode::File {
                                (line.into_bytes(), "json")
                            } else {
                                (format!("{input}\n{input}\n").into_bytes(), "ndjson")
                            };
                            let name = content_name(&content, ext);
                            Invocation {
                                args: test_args(mode, &case.program, Some(&name)),
                                stdin: None,
                                env: Vec::new(),
                                cwd: String::new(),
                                files: vec![(name, content)],
                            }
                        }
                    };
                    jobs.push(Job {
                        id: format!("{group}:{}:{}", case.line, mode.name()),
                        group: group.to_string(),
                        mode,
                        fp: fp.clone(),
                        origin: origin.clone(),
                        program: Some(case.program.clone()),
                        input: Some(input.clone()),
                        inv,
                    });
                }
            }
        }
    }
    jobs
}

/// Jobs for one CLI case file.
pub fn cli_jobs(group: &str, origin_path: &str, cases: &[CliCase]) -> Vec<Job> {
    cases
        .iter()
        .map(|c| {
            let mut h = Fnv128::new();
            h.str("cli");
            for a in &c.args {
                h.str(a);
            }
            match &c.stdin {
                None => h.str("stdin:null"),
                Some(b) => h.str("stdin").field(b),
            };
            for (p, content) in &c.files {
                h.str("file").str(p).field(content);
            }
            for (k, v) in &c.env {
                h.str("env").str(k).str(v);
            }
            let dir = format!("cli/{}", &h.hex()[..16]);
            Job {
                id: format!("{group}:{}:cli", c.name),
                group: group.to_string(),
                mode: Mode::Cli,
                fp: short_fp(&h),
                origin: format!("{origin_path} [{}]", c.name),
                program: c.program.clone(),
                input: None,
                inv: Invocation {
                    args: c.args.clone(),
                    stdin: c.stdin.clone(),
                    env: c.env.clone(),
                    files: c
                        .files
                        .iter()
                        .map(|(p, content)| (format!("{dir}/{p}"), content.clone()))
                        .collect(),
                    cwd: dir,
                },
            }
        })
        .collect()
}

/// The upstream suites, in scoreboard order.
pub const UPSTREAM: [&str; 7] = [
    "jq.test",
    "man.test",
    "manonig.test",
    "onig.test",
    "base64.test",
    "uri.test",
    "optional.test",
];

/// Collect every job from the upstream suites and the corpus.
pub fn collect(root: &Path, modes: &[Mode]) -> Result<Vec<Job>, String> {
    let compat = root.join("tests/jq_compat");
    let mut jobs = Vec::new();
    for name in UPSTREAM {
        let path = compat.join(name);
        let content =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = testfile::parse(&content).map_err(|e| format!("{name}: {e}"))?;
        jobs.extend(test_file_jobs(
            &format!("upstream/{name}"),
            &format!("tests/jq_compat/{name}"),
            &file,
            modes,
        ));
    }

    let corpus = compat.join("corpus");
    let mut entries: Vec<_> = match std::fs::read_dir(&corpus) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_file())
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    for path in entries {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let origin = format!("tests/jq_compat/corpus/{name}");
        let group = format!("corpus/{name}");
        if name.ends_with(".test") {
            let content = std::fs::read_to_string(&path).map_err(|e| format!("{origin}: {e}"))?;
            let file = testfile::parse(&content).map_err(|e| format!("{origin}: {e}"))?;
            if let Some(bad) = file
                .modes
                .iter()
                .flatten()
                .find(|m| Mode::parse(m).is_none())
            {
                return Err(format!("{origin}: unknown mode {bad:?}"));
            }
            jobs.extend(test_file_jobs(&group, &origin, &file, modes));
        } else if name.ends_with(".toml") {
            if !modes.contains(&Mode::Cli) {
                continue;
            }
            let content = std::fs::read_to_string(&path).map_err(|e| format!("{origin}: {e}"))?;
            let cases = cli::parse(&content, &corpus).map_err(|e| format!("{origin}: {e}"))?;
            jobs.extend(cli_jobs(&group, &origin, &cases));
        }
    }

    let mut seen = std::collections::HashSet::new();
    for j in &jobs {
        if !seen.insert(j.id.as_str()) {
            return Err(format!("duplicate case id {}", j.id));
        }
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> testfile::TestFile {
        testfile::parse(src).unwrap()
    }

    #[test]
    fn modes_and_args() {
        let f = parse(".a\n{\"a\":1}\n1\n\n-.\n1\n-1\n\n%%FAIL\n{\nerr\n");
        let jobs = test_file_jobs("g", "g.test", &f, &ALL_MODES);
        let ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "g:1:compact",
                "g:1:pretty",
                "g:1:file",
                "g:1:ndjson",
                "g:5:compact",
                "g:5:pretty",
                "g:5:file",
                "g:10:fail",
            ]
        );
        assert_eq!(jobs[0].inv.args, vec!["-c", ".a"]);
        assert_eq!(jobs[0].inv.stdin.as_deref(), Some(&b"{\"a\":1}\n"[..]));
        assert_eq!(jobs[1].inv.args, vec![".a"]);
        let file = &jobs[2].inv;
        assert_eq!(file.args[..2], ["-c", ".a"]);
        assert_eq!(file.args[2], file.files[0].0);
        assert!(file.stdin.is_none());
        assert_eq!(file.files[0].1, b"{\"a\":1}\n");
        assert_eq!(jobs[3].inv.files[0].1, b"{\"a\":1}\n{\"a\":1}\n");
        assert_eq!(jobs[4].inv.args, vec!["-c", "--", "-."]);
        assert_eq!(jobs[7].inv.args, vec!["-c", "-n", "{"]);
        // All modes of one case share a fingerprint; cases differ.
        assert_eq!(jobs[0].fp, jobs[3].fp);
        assert_ne!(jobs[0].fp, jobs[4].fp);
    }

    #[test]
    fn fingerprint_ignores_line_numbers() {
        let a = test_file_jobs("g", "g", &parse(".\n1\n"), &[Mode::Compact]);
        let b = test_file_jobs("g", "g", &parse("# moved\n\n.\n1\n"), &[Mode::Compact]);
        assert_ne!(a[0].id, b[0].id);
        assert_eq!(a[0].fp, b[0].fp);
    }

    #[test]
    fn modules_flag_and_ndjson_eligibility() {
        let f = parse("include \"a\"; .\n[1]\n\n[inputs]\n[1]\n\n.\n1\n");
        let jobs = test_file_jobs("g", "g", &f, &ALL_MODES);
        assert_eq!(
            jobs[0].inv.args,
            vec!["-c", "-L", "modules", "include \"a\"; ."]
        );
        assert!(jobs.iter().any(|j| j.id == "g:1:ndjson"));
        assert!(!jobs.iter().any(|j| j.id == "g:4:ndjson"));
        assert!(!jobs.iter().any(|j| j.id == "g:7:ndjson"));
    }

    #[test]
    fn file_directive_restricts_modes() {
        let f = parse("# jq_diff: modes=compact\n.\n1\n\n%%FAIL\n{\n");
        let jobs = test_file_jobs("g", "g", &f, &ALL_MODES);
        let ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["g:2:compact", "g:6:fail"]);
        let only_pretty = test_file_jobs("g", "g", &f, &[Mode::Pretty]);
        assert!(only_pretty.is_empty());
    }

    #[test]
    fn cache_key_covers_everything() {
        let inv = Invocation {
            args: vec!["-c".into(), ".".into()],
            stdin: Some(b"1\n".to_vec()),
            env: vec![],
            cwd: String::new(),
            files: vec![],
        };
        let k = inv.key(&[]);
        let mut other = inv.clone();
        other.stdin = None;
        assert_ne!(k, other.key(&[]));
        let mut other = inv.clone();
        other.args = vec!["-c .".into()];
        assert_ne!(k, other.key(&[]));
        assert_ne!(k, inv.key(&[("QJ_NO_SIMD_INPUT".into(), "1".into())]));
        let mut other = inv.clone();
        other.files = vec![("in/x".into(), b"1".to_vec())];
        assert_ne!(k, other.key(&[]));
        assert_eq!(k, inv.clone().key(&[]));
    }
}
