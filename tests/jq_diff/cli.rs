//! Loader for CLI cases (`tests/jq_compat/corpus/*.toml`).
//!
//! These cover what the one-line `.test` format can't express: arbitrary argv,
//! several input files, environment variables, multi-line or binary stdin.
//!
//! ```toml
//! [[case]]
//! name = "args-after-filter"          # unique in the file: [A-Za-z0-9_.-]+
//! args = ["-n", "$ARGS", "--args", "a", "b"]
//! stdin = "1 2 3"                      # optional; see `Content` below
//! files = { "a.json" = '{"a":1}' }     # optional; written into the case's cwd
//! env = { NO_COLOR = "1" }             # optional; added to the base environment
//! close_fds = [1]                      # optional: standard descriptors the tool
//!                                      # starts with closed (`>&-`, `<&-`, `2>&-`)
//! merge = "file"                       # optional: stderr into stdout's regular
//!                                      # file (`>out 2>&1`), or "pipe" (`2>&1 |`);
//!                                      # the stream is compared as stdout, with
//!                                      # stderr's name normalization
//!
//! # A sweep runs every program under every variant. `{program}` in a
//! # variant's args is replaced by the program.
//! [[sweep]]
//! name = "adv"
//! programs = ['select(.type == "PushEvent")', '.type']
//! variants = { c = ["-c", "{program}", "adv.ndjson"] }
//! files = { "adv.ndjson" = { path = "data/adv.ndjson" } }
//! ```
//!
//! `Content` (for `stdin` and `files` values) is one of:
//! - a string: the UTF-8 bytes of the string;
//! - `{ b64 = "..." }`: base64-decoded bytes (for invalid UTF-8);
//! - `{ path = "data/x" }`: a file, relative to the TOML file's directory;
//! - `{ repeat = [["[", 1025], ["]", 1025]] }`: each string repeated N
//!   times, concatenated (for deep nesting).
//!
//! Without `stdin`, stdin is /dev/null. Each case runs in its own directory
//! (so `files` of different cases can't collide), which is two levels below
//! the harness work directory: jq's test modules are at `../../modules`.

use crate::exec::Merge;
use base64::Engine;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    B64 { b64: String },
    Path { path: String },
    Repeat { repeat: Vec<(String, usize)> },
}

impl Content {
    pub fn bytes(&self, base: &Path) -> Result<Vec<u8>, String> {
        match self {
            Content::Text(s) => Ok(s.as_bytes().to_vec()),
            Content::B64 { b64 } => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("bad base64 {b64:?}: {e}")),
            Content::Path { path } => {
                std::fs::read(base.join(path)).map_err(|e| format!("reading {path}: {e}"))
            }
            Content::Repeat { repeat } => {
                let mut out = Vec::new();
                for (s, n) in repeat {
                    for _ in 0..*n {
                        out.extend_from_slice(s.as_bytes());
                    }
                }
                Ok(out)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseDef {
    pub name: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub stdin: Option<Content>,
    #[serde(default)]
    pub files: BTreeMap<String, Content>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `"file"` or `"pipe"`: stderr goes where stdout goes (see `Merge`).
    #[serde(default)]
    pub merge: Option<String>,
    /// Standard descriptors (0, 1, 2) the tool starts with closed.
    #[serde(default)]
    pub close_fds: Vec<i32>,
    /// Free-form note for humans; not used by the harness.
    #[serde(default)]
    #[allow(dead_code)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SweepDef {
    pub name: String,
    pub programs: Vec<String>,
    pub variants: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub stdin: Option<Content>,
    #[serde(default)]
    pub files: BTreeMap<String, Content>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `"file"` or `"pipe"`: stderr goes where stdout goes (see `Merge`).
    #[serde(default)]
    pub merge: Option<String>,
    /// Standard descriptors (0, 1, 2) the tool starts with closed.
    #[serde(default)]
    pub close_fds: Vec<i32>,
    /// Free-form note for humans; not used by the harness.
    #[serde(default)]
    #[allow(dead_code)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliFile {
    #[serde(default)]
    pub case: Vec<CaseDef>,
    #[serde(default)]
    pub sweep: Vec<SweepDef>,
}

/// One concrete CLI invocation, before it is placed in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCase {
    /// Unique within the file, e.g. `args-after-filter` or `adv/c/3`.
    pub name: String,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
    /// (file name relative to the case directory, content)
    pub files: Vec<(String, Vec<u8>)>,
    pub env: Vec<(String, String)>,
    /// The program, when known (sweeps), for display.
    pub program: Option<String>,
    pub merge: Merge,
    /// Standard descriptors the tool starts with closed.
    pub close_fds: Vec<i32>,
}

/// Only 0, 1 and 2 can be closed: anything else is the harness's own.
fn check_close_fds(fds: &[i32]) -> Result<Vec<i32>, String> {
    if let Some(bad) = fds.iter().find(|f| !(0..=2).contains(*f)) {
        return Err(format!("close_fds: {bad} is not a standard descriptor"));
    }
    Ok(fds.to_vec())
}

fn merge_mode(merge: &Option<String>) -> Result<Merge, String> {
    match merge {
        None => Ok(Merge::No),
        Some(m) => Merge::parse(m).ok_or_else(|| format!("unknown merge {m:?} (file or pipe)")),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && name.split('/').all(|seg| !seg.is_empty() && seg != "..")
}

/// Resolved stdin, files and environment of a case or sweep.
type Io = (
    Option<Vec<u8>>,
    Vec<(String, Vec<u8>)>,
    Vec<(String, String)>,
);

fn load_io(
    stdin: &Option<Content>,
    files: &BTreeMap<String, Content>,
    env: &BTreeMap<String, String>,
    base: &Path,
) -> Result<Io, String> {
    let stdin = stdin.as_ref().map(|c| c.bytes(base)).transpose()?;
    let mut out_files = Vec::new();
    for (name, content) in files {
        if !valid_file_name(name) {
            return Err(format!("invalid file name {name:?}"));
        }
        out_files.push((name.clone(), content.bytes(base)?));
    }
    let env = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    Ok((stdin, out_files, env))
}

/// Parse a CLI case file. `base` is the directory `path` contents resolve
/// against.
pub fn parse(toml_src: &str, base: &Path) -> Result<Vec<CliCase>, String> {
    let file: CliFile = toml::from_str(toml_src).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for c in &file.case {
        if !valid_name(&c.name) {
            return Err(format!("invalid case name {:?}", c.name));
        }
        let (stdin, files, env) =
            load_io(&c.stdin, &c.files, &c.env, base).map_err(|e| format!("{}: {e}", c.name))?;
        out.push(CliCase {
            name: c.name.clone(),
            args: c.args.clone(),
            stdin,
            files,
            env,
            program: None,
            merge: merge_mode(&c.merge).map_err(|e| format!("{}: {e}", c.name))?,
            close_fds: check_close_fds(&c.close_fds).map_err(|e| format!("{}: {e}", c.name))?,
        });
    }
    for s in &file.sweep {
        if !valid_name(&s.name) {
            return Err(format!("invalid sweep name {:?}", s.name));
        }
        let (stdin, files, env) =
            load_io(&s.stdin, &s.files, &s.env, base).map_err(|e| format!("{}: {e}", s.name))?;
        for (vname, template) in &s.variants {
            if !valid_name(vname) {
                return Err(format!("{}: invalid variant name {vname:?}", s.name));
            }
            if !template.iter().any(|a| a == "{program}") {
                return Err(format!("{}: variant {vname} has no {{program}}", s.name));
            }
            for (i, program) in s.programs.iter().enumerate() {
                let args = template
                    .iter()
                    .map(|a| {
                        if a == "{program}" {
                            program.clone()
                        } else {
                            a.clone()
                        }
                    })
                    .collect();
                out.push(CliCase {
                    name: format!("{}/{vname}/{i}", s.name),
                    args,
                    stdin: stdin.clone(),
                    files: files.clone(),
                    env: env.clone(),
                    program: Some(program.clone()),
                    merge: merge_mode(&s.merge).map_err(|e| format!("{}: {e}", s.name))?,
                    close_fds: check_close_fds(&s.close_fds)
                        .map_err(|e| format!("{}: {e}", s.name))?,
                });
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    for c in &out {
        if !seen.insert(c.name.as_str()) {
            return Err(format!("duplicate case name {:?}", c.name));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cases_and_content_kinds() {
        let src = r#"
[[case]]
name = "a"
args = ["-c", "."]
stdin = "1 2"
env = { X = "1" }

[[case]]
name = "b"
args = ["."]
stdin = { b64 = "/w==" }
files = { "x.json" = { repeat = [["[", 2], ["]", 2]] } }
"#;
        let cases = parse(src, Path::new("/")).unwrap();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].stdin.as_deref(), Some(&b"1 2"[..]));
        assert_eq!(cases[0].env, vec![("X".to_string(), "1".to_string())]);
        assert_eq!(cases[1].stdin.as_deref(), Some(&[0xffu8][..]));
        assert_eq!(
            cases[1].files,
            vec![("x.json".to_string(), b"[[]]".to_vec())]
        );
    }

    #[test]
    fn expands_sweeps() {
        let src = r#"
[[sweep]]
name = "s"
programs = [".a", ".b"]
variants = { c = ["-c", "{program}", "f"], p = ["{program}"] }
"#;
        let cases = parse(src, Path::new("/")).unwrap();
        let names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["s/c/0", "s/c/1", "s/p/0", "s/p/1"]);
        assert_eq!(cases[1].args, vec!["-c", ".b", "f"]);
        assert_eq!(cases[1].program.as_deref(), Some(".b"));
    }

    #[test]
    fn parses_close_fds() {
        let src = "[[case]]\nname = \"a\"\nargs = [\"-n\", \"1\"]\nclose_fds = [1, 2]\n";
        let cases = parse(src, Path::new("/")).unwrap();
        assert_eq!(cases[0].close_fds, vec![1, 2]);
        let bad = "[[case]]\nname = \"a\"\nargs = []\nclose_fds = [3]\n";
        assert!(parse(bad, Path::new("/")).is_err());
    }

    #[test]
    fn rejects_bad_definitions() {
        let dup = "[[case]]\nname = \"a\"\nargs = []\n[[case]]\nname = \"a\"\nargs = []\n";
        assert!(parse(dup, Path::new("/")).is_err());
        let unknown = "[[case]]\nname = \"a\"\nargs = []\nstdn = \"typo\"\n";
        assert!(parse(unknown, Path::new("/")).is_err());
        let bad_name = "[[case]]\nname = \"a b\"\nargs = []\n";
        assert!(parse(bad_name, Path::new("/")).is_err());
        let escape = "[[case]]\nname = \"a\"\nargs = []\nfiles = { \"../x\" = \"\" }\n";
        assert!(parse(escape, Path::new("/")).is_err());
        let no_placeholder =
            "[[sweep]]\nname = \"s\"\nprograms = [\".\"]\nvariants = { c = [\"-c\"] }\n";
        assert!(parse(no_placeholder, Path::new("/")).is_err());
    }
}
