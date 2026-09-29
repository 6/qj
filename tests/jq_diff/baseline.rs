//! Ratchet baseline: the level each case has reached.
//!
//! File format, one case per line (`#` comments and blank lines ignored):
//!
//! ```text
//! pass upstream/jq.test:12:compact 3f2a91c07d1e
//! stdout corpus/cli.toml:args-after-filter:cli 81b0c2d93e4f
//! ```
//!
//! `pass` means stdout, exit code and stderr match jq; `stdout` means stdout
//! and exit code match. Cases below `stdout` are not listed. Entries are
//! matched on (group, mode, fingerprint) rather than on the id, so moving a
//! case within its file keeps its baseline entry.

use crate::compare::Level;
use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub level: Level,
    pub id: String,
    pub fp: String,
}

pub type Key = (String, String, String);

/// (group, mode, fingerprint) of a case id + fingerprint.
pub fn key(id: &str, fp: &str) -> Key {
    let group = id.split(':').next().unwrap_or("").to_string();
    let mode = id.rsplit(':').next().unwrap_or("").to_string();
    (group, mode, fp.to_string())
}

impl Entry {
    pub fn key(&self) -> Key {
        key(&self.id, &self.fp)
    }
}

pub fn parse(content: &str) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    for (n, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [level, id, fp] = fields[..] else {
            return Err(format!(
                "line {}: expected `<level> <id> <fingerprint>`",
                n + 1
            ));
        };
        let level =
            Level::parse(level).ok_or_else(|| format!("line {}: bad level {level:?}", n + 1))?;
        if level == Level::Fail {
            return Err(format!("line {}: `fail` entries are not recorded", n + 1));
        }
        out.push(Entry {
            level,
            id: id.to_string(),
            fp: fp.to_string(),
        });
    }
    Ok(out)
}

/// Compare ids with digit runs compared numerically, so `jq.test:9` sorts
/// before `jq.test:10`.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => {
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                tx.len()
                    .cmp(&ty.len())
                    .then(tx.cmp(ty))
                    .then(x.len().cmp(&y.len()))
            }
            ((_, x), (_, y)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

pub fn render(entries: &[Entry]) -> String {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| natural_cmp(&a.id, &b.id).then(a.fp.cmp(&b.fp)));
    let mut out = String::from(
        "# jq_diff ratchet baseline (see tests/jq_diff.rs and CLAUDE.md).\n\
         # <level> <case id> <fingerprint>; level `pass` = stdout, exit code and stderr\n\
         # match jq 1.8.1, `stdout` = stdout and exit code match. Unlisted cases fail.\n\
         # Regenerate with JQ_DIFF_UPDATE_BASELINE=1; don't edit by hand.\n",
    );
    for e in sorted {
        out.push_str(&format!("{} {} {}\n", e.level.name(), e.id, e.fp));
    }
    out
}

pub struct Current<'a> {
    pub id: &'a str,
    pub fp: &'a str,
    /// `None` when the case was skipped (e.g. jq timed out).
    pub level: Option<Level>,
}

#[derive(Debug, Default)]
pub struct Diff {
    /// (id, baseline level, current level)
    pub regressions: Vec<(String, Level, Level)>,
    pub improvements: Vec<(String, Level, Level)>,
    /// Baseline entries that match no case that ran (only meaningful for
    /// unfiltered runs).
    pub unmatched: Vec<String>,
}

pub fn diff(baseline: &[Entry], current: &[Current]) -> Diff {
    let base: HashMap<Key, Level> = baseline.iter().map(|e| (e.key(), e.level)).collect();
    let mut d = Diff::default();
    let mut matched = std::collections::HashSet::new();
    for c in current {
        let k = key(c.id, c.fp);
        let before = base.get(&k).copied();
        if before.is_some() {
            matched.insert(k);
        }
        let Some(now) = c.level else { continue };
        let before = before.unwrap_or(Level::Fail);
        match now.cmp(&before) {
            Ordering::Less => d.regressions.push((c.id.to_string(), before, now)),
            Ordering::Greater => d.improvements.push((c.id.to_string(), before, now)),
            Ordering::Equal => {}
        }
    }
    d.unmatched = baseline
        .iter()
        .filter(|e| !matched.contains(&e.key()))
        .map(|e| e.id.clone())
        .collect();
    d
}

/// The new baseline after a run. Skipped cases keep their old entry. With
/// `partial` (a filtered run), entries for cases that didn't run are kept.
pub fn update(baseline: &[Entry], current: &[Current], partial: bool) -> Vec<Entry> {
    let base: HashMap<Key, &Entry> = baseline.iter().map(|e| (e.key(), e)).collect();
    let mut out: Vec<Entry> = Vec::new();
    let mut ran = std::collections::HashSet::new();
    for c in current {
        let k = key(c.id, c.fp);
        ran.insert(k.clone());
        let level = match c.level {
            Some(l) => Some(l),
            None => base.get(&k).map(|e| e.level),
        };
        if let Some(level) = level.filter(|l| *l > Level::Fail) {
            out.push(Entry {
                level,
                id: c.id.to_string(),
                fp: c.fp.to_string(),
            });
        }
    }
    if partial {
        out.extend(baseline.iter().filter(|e| !ran.contains(&e.key())).cloned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cur<'a>(id: &'a str, fp: &'a str, level: Option<Level>) -> Current<'a> {
        Current { id, fp, level }
    }

    #[test]
    fn parse_and_render_roundtrip() {
        let entries = vec![
            Entry {
                level: Level::Pass,
                id: "upstream/jq.test:10:compact".into(),
                fp: "b".into(),
            },
            Entry {
                level: Level::Stdout,
                id: "upstream/jq.test:9:compact".into(),
                fp: "a".into(),
            },
        ];
        let text = render(&entries);
        let parsed = parse(&text).unwrap();
        // Natural order: line 9 before line 10.
        assert_eq!(parsed[0].id, "upstream/jq.test:9:compact");
        assert_eq!(parsed[1].level, Level::Pass);
        assert!(parse("pass only-two-fields\n").is_err());
        assert!(parse("fail g:1:compact fp\n").is_err());
    }

    #[test]
    fn detects_regressions_and_improvements_by_fingerprint() {
        let baseline =
            parse("pass g:1:compact aa\nstdout g:5:compact bb\npass g:9:cli cc\n").unwrap();
        // Case `aa` moved from line 1 to line 3 and still passes; `bb` now
        // passes fully; `cc` regressed; `dd` is new and at stdout level.
        let current = [
            cur("g:3:compact", "aa", Some(Level::Pass)),
            cur("g:5:compact", "bb", Some(Level::Pass)),
            cur("g:9:cli", "cc", Some(Level::Stdout)),
            cur("g:12:compact", "dd", Some(Level::Stdout)),
        ];
        let d = diff(&baseline, &current);
        assert_eq!(
            d.regressions,
            vec![("g:9:cli".to_string(), Level::Pass, Level::Stdout)]
        );
        assert_eq!(d.improvements.len(), 2);
        assert!(d.unmatched.is_empty());
        // Same fingerprint in another mode or group is a different case.
        let d = diff(&baseline, &[cur("g:1:pretty", "aa", Some(Level::Fail))]);
        assert!(d.regressions.is_empty());
        assert_eq!(d.unmatched.len(), 3);
    }

    #[test]
    fn skipped_cases_neither_regress_nor_drop_out() {
        let baseline = parse("pass g:1:compact aa\n").unwrap();
        let current = [cur("g:1:compact", "aa", None)];
        assert!(diff(&baseline, &current).regressions.is_empty());
        assert_eq!(update(&baseline, &current, false), baseline);
    }

    #[test]
    fn update_full_and_partial() {
        let baseline = parse("pass g:1:compact aa\npass h:1:compact zz\n").unwrap();
        let current = [
            cur("g:1:compact", "aa", Some(Level::Fail)),
            cur("g:2:compact", "bb", Some(Level::Stdout)),
        ];
        let full = update(&baseline, &current, false);
        assert_eq!(
            full,
            vec![Entry {
                level: Level::Stdout,
                id: "g:2:compact".into(),
                fp: "bb".into()
            }]
        );
        let partial = update(&baseline, &current, true);
        assert_eq!(partial.len(), 2);
        assert_eq!(partial[1].id, "h:1:compact");
    }

    #[test]
    fn natural_ordering() {
        let mut v = vec!["a:10:x", "a:9:x", "a:100:x", "b:1:x", "a:9:w"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a:9:w", "a:9:x", "a:10:x", "a:100:x", "b:1:x"]);
    }
}
