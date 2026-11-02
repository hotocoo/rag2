//! Versioned profile storage. Versions are immutable once written; activation moves a pointer.
//!
//! On disk (`FsConfigStore`), one directory per profile:
//! ```text
//! <root>/<profile>/versions/000001.json   {meta, profile}
//! <root>/<profile>/ACTIVE                  "3"
//! <root>/<profile>/activations.jsonl       one line per activation (rollback history)
//! ```
//! Every write goes to a temporary file, is fsynced, then renamed into place, so a crash leaves
//! either the old state or the new one, never a torn file.

use parking_lot::Mutex;
use rag2_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VersionMeta {
    pub version: u64,
    pub created_at_ms: u64,
    pub author: String,
    #[serde(default)]
    pub comment: String,
    /// xxh3 of the canonical profile JSON, hex.
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVersion {
    pub meta: VersionMeta,
    pub profile: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Activation {
    pub version: u64,
    pub at_ms: u64,
    pub author: String,
    /// `activate`, `rollback` or `reload`.
    pub action: String,
    #[serde(default)]
    pub environment: Option<String>,
}

pub trait ConfigStore: Send + Sync {
    fn list(&self, profile: &str) -> Result<Vec<VersionMeta>>;
    fn get(&self, profile: &str, version: u64) -> Result<StoredVersion>;
    /// Store a new version and return its metadata. Versions number from 1, gap-free.
    fn put(
        &self,
        profile: &str,
        value: &serde_json::Value,
        author: &str,
        comment: &str,
    ) -> Result<VersionMeta>;
    fn active(&self, profile: &str) -> Result<Option<u64>>;
    fn record_activation(&self, profile: &str, a: &Activation) -> Result<()>;
    fn activations(&self, profile: &str) -> Result<Vec<Activation>>;
    /// Drop the oldest versions beyond `keep`, never the active one.
    fn prune(&self, profile: &str, keep: usize) -> Result<usize>;
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

pub fn content_hash(v: &serde_json::Value) -> String {
    format!(
        "{:016x}",
        xxhash_rust::xxh3::xxh3_64(v.to_string().as_bytes())
    )
}

/// Profile names become directory names: same safe charset as tenant ids.
pub fn check_profile_name(name: &str) -> Result<()> {
    rag2_core::TenantId::new(name)
        .map(|_| ())
        .map_err(|_| Error::InvalidInput("profile names use [a-z0-9_-], 1..=64 chars".into()))
}

pub struct FsConfigStore {
    root: PathBuf,
    write: Mutex<()>,
}

impl FsConfigStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(Error::storage)?;
        Ok(FsConfigStore {
            root,
            write: Mutex::new(()),
        })
    }

    fn dir(&self, profile: &str) -> Result<PathBuf> {
        check_profile_name(profile)?;
        Ok(self.root.join(profile))
    }

    fn version_path(&self, profile: &str, v: u64) -> Result<PathBuf> {
        Ok(self
            .dir(profile)?
            .join("versions")
            .join(format!("{v:06}.json")))
    }
}

/// Write `bytes` to `path` atomically: temp file, fsync, rename, fsync the directory.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| Error::Internal("path has no parent".into()))?;
    fs::create_dir_all(dir).map_err(Error::storage)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("write")
    ));
    {
        let mut f = File::create(&tmp).map_err(Error::storage)?;
        f.write_all(bytes).map_err(Error::storage)?;
        f.sync_all().map_err(Error::storage)?;
    }
    fs::rename(&tmp, path).map_err(Error::storage)?;
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(Error::storage)?;
    Ok(())
}

impl ConfigStore for FsConfigStore {
    fn list(&self, profile: &str) -> Result<Vec<VersionMeta>> {
        let dir = self.dir(profile)?.join("versions");
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name();
            let Some(v) = name
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
            else {
                continue;
            };
            out.push(self.get(profile, v)?.meta);
        }
        out.sort_by_key(|m| m.version);
        Ok(out)
    }

    fn get(&self, profile: &str, version: u64) -> Result<StoredVersion> {
        let p = self.version_path(profile, version)?;
        let bytes = fs::read(&p)
            .map_err(|_| Error::NotFound(format!("profile {profile} version {version}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("corrupt version file {}: {e}", p.display())))
    }

    fn put(
        &self,
        profile: &str,
        value: &serde_json::Value,
        author: &str,
        comment: &str,
    ) -> Result<VersionMeta> {
        let _g = self.write.lock();
        let next = self.list(profile)?.last().map_or(1, |m| m.version + 1);
        let meta = VersionMeta {
            version: next,
            created_at_ms: now_ms(),
            author: author.into(),
            comment: comment.into(),
            hash: content_hash(value),
        };
        let stored = StoredVersion {
            meta: meta.clone(),
            profile: value.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&stored).map_err(Error::internal)?;
        atomic_write(&self.version_path(profile, next)?, &bytes)?;
        Ok(meta)
    }

    fn active(&self, profile: &str) -> Result<Option<u64>> {
        match fs::read_to_string(self.dir(profile)?.join("ACTIVE")) {
            Ok(s) => s
                .trim()
                .parse()
                .map(Some)
                .map_err(|_| Error::Storage(format!("corrupt ACTIVE for {profile}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::storage(e)),
        }
    }

    fn record_activation(&self, profile: &str, a: &Activation) -> Result<()> {
        let _g = self.write.lock();
        let dir = self.dir(profile)?;
        // The log line first: if we crash between the two writes, ACTIVE still names the old
        // version and the log merely has an extra entry the next activation supersedes.
        let mut line = serde_json::to_vec(a).map_err(Error::internal)?;
        line.push(b'\n');
        fs::create_dir_all(&dir).map_err(Error::storage)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("activations.jsonl"))
            .map_err(Error::storage)?;
        f.write_all(&line)
            .and_then(|_| f.sync_all())
            .map_err(Error::storage)?;
        atomic_write(&dir.join("ACTIVE"), a.version.to_string().as_bytes())
    }

    fn activations(&self, profile: &str) -> Result<Vec<Activation>> {
        let p = self.dir(profile)?.join("activations.jsonl");
        let Ok(s) = fs::read_to_string(&p) else {
            return Ok(Vec::new());
        };
        // A torn final line (crash mid-append) is skipped, earlier lines are kept.
        Ok(s.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    fn prune(&self, profile: &str, keep: usize) -> Result<usize> {
        let _g = self.write.lock();
        let active = self.active(profile)?;
        let all = self.list(profile)?;
        let excess = all.len().saturating_sub(keep);
        let mut removed = 0;
        for m in all.iter().take(excess) {
            if Some(m.version) == active {
                continue;
            }
            fs::remove_file(self.version_path(profile, m.version)?).map_err(Error::storage)?;
            removed += 1;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn versions_are_gap_free_immutable_and_survive_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = FsConfigStore::open(dir.path()).expect("open");
        let a = s
            .put("prod", &json!({"x": 1}), "alice", "first")
            .expect("put");
        let b = s.put("prod", &json!({"x": 2}), "bob", "").expect("put");
        assert_eq!((a.version, b.version), (1, 2));
        assert_ne!(a.hash, b.hash);
        let s = FsConfigStore::open(dir.path()).expect("reopen");
        assert_eq!(s.get("prod", 1).expect("get").profile, json!({"x": 1}));
        assert_eq!(s.list("prod").expect("list").len(), 2);
        assert!(matches!(s.get("prod", 9), Err(Error::NotFound(_))));
    }

    #[test]
    fn activation_moves_the_pointer_and_logs_history() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = FsConfigStore::open(dir.path()).expect("open");
        assert_eq!(s.active("prod").expect("active"), None);
        let act = |v| Activation {
            version: v,
            at_ms: 0,
            author: "a".into(),
            action: "activate".into(),
            environment: None,
        };
        s.record_activation("prod", &act(1)).expect("record");
        s.record_activation("prod", &act(2)).expect("record");
        assert_eq!(s.active("prod").expect("active"), Some(2));
        assert_eq!(s.activations("prod").expect("log").len(), 2);
    }

    #[test]
    fn prune_keeps_the_active_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = FsConfigStore::open(dir.path()).expect("open");
        for i in 0..5 {
            s.put("p", &json!({ "i": i }), "a", "").expect("put");
        }
        s.record_activation(
            "p",
            &Activation {
                version: 1,
                at_ms: 0,
                author: "a".into(),
                action: "activate".into(),
                environment: None,
            },
        )
        .expect("record");
        assert_eq!(s.prune("p", 2).expect("prune"), 2);
        let left: Vec<u64> = s
            .list("p")
            .expect("list")
            .iter()
            .map(|m| m.version)
            .collect();
        assert_eq!(left, vec![1, 4, 5]);
    }

    #[test]
    fn unsafe_profile_names_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = FsConfigStore::open(dir.path()).expect("open");
        assert!(s.put("../etc", &json!({}), "a", "").is_err());
    }
}
