//! On-disk cache of jq's results (qj is never cached).
//!
//! Entries are keyed by `Invocation::key` (argv, stdin, env, cwd, files). The
//! header covers the global state: jq's `--version` and binary identity, the
//! work directory (outputs can contain absolute paths), the base environment,
//! the test modules, and the limits. Any header change discards the cache.

use crate::compare::Observed;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Header {
    pub schema: u32,
    pub jq_version: String,
    /// Path, size and mtime of the jq binary.
    pub jq_binary: String,
    pub work_dir: String,
    /// Hash of the base environment and the test modules.
    pub fixtures: String,
    pub timeout_ms: u64,
    pub max_output: usize,
    pub max_rss: u64,
}

pub const SCHEMA: u32 = 2;

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheFile {
    header: Header,
    entries: BTreeMap<String, Observed>,
}

pub struct Cache {
    header: Header,
    old: HashMap<String, Observed>,
    new: Mutex<HashMap<String, Observed>>,
    pub loaded: usize,
    pub invalidated: bool,
}

impl Cache {
    pub fn load(path: &Path, header: Header) -> Cache {
        let mut invalidated = false;
        let old = match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<CacheFile>(&bytes) {
                Ok(f) if f.header == header => f.entries.into_iter().collect(),
                _ => {
                    invalidated = true;
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(),
        };
        Cache {
            header,
            loaded: old.len(),
            old,
            new: Mutex::new(HashMap::new()),
            invalidated,
        }
    }

    pub fn get(&self, key: &str) -> Option<Observed> {
        if let Some(o) = self.old.get(key) {
            return Some(o.clone());
        }
        self.new.lock().unwrap().get(key).cloned()
    }

    pub fn put(&self, key: String, obs: Observed) {
        self.new.lock().unwrap().insert(key, obs);
    }

    pub fn misses(&self) -> usize {
        self.new.lock().unwrap().len()
    }

    /// Write the cache atomically. With `keep = Some(keys)`, entries not in
    /// `keys` are dropped (used after complete runs to prune stale entries).
    pub fn save(&self, path: &Path, keep: Option<&HashSet<String>>) -> std::io::Result<()> {
        let new = self.new.lock().unwrap();
        let entries: BTreeMap<String, Observed> = self
            .old
            .iter()
            .chain(new.iter())
            .filter(|(k, _)| keep.is_none_or(|keep| keep.contains(*k)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let file = CacheFile {
            header: self.header.clone(),
            entries,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(&file)?)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::Blob;
    use crate::exec::Status;

    fn header(v: &str) -> Header {
        Header {
            schema: SCHEMA,
            jq_version: v.into(),
            jq_binary: "/bin/jq 1 2".into(),
            work_dir: "/w".into(),
            fixtures: "f".into(),
            timeout_ms: 10,
            max_output: 1,
            max_rss: 1,
        }
    }

    fn obs(out: &[u8]) -> Observed {
        Observed {
            status: Status::Exit(0),
            stdout: Blob::new(out.to_vec(), true),
            stderr: Blob::new(vec![0xff], true),
        }
    }

    #[test]
    fn roundtrip_invalidation_and_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let c = Cache::load(&path, header("jq-1.8.1"));
        assert!(!c.invalidated && c.loaded == 0);
        c.put("k1".into(), obs(b"1\n"));
        c.put("k2".into(), obs(b"2\n"));
        c.save(&path, None).unwrap();

        let c = Cache::load(&path, header("jq-1.8.1"));
        assert_eq!(c.loaded, 2);
        assert!(
            c.get("k1")
                .unwrap()
                .stdout
                .same(&Blob::new(b"1\n".to_vec(), true))
        );
        assert!(
            c.get("k1")
                .unwrap()
                .stderr
                .same(&Blob::new(vec![0xff], true))
        );
        let keep: HashSet<String> = ["k2".to_string()].into();
        c.save(&path, Some(&keep)).unwrap();
        let c = Cache::load(&path, header("jq-1.8.1"));
        assert!(c.get("k1").is_none() && c.get("k2").is_some());

        let c = Cache::load(&path, header("jq-1.8.2"));
        assert!(c.invalidated && c.get("k2").is_none());
    }
}
