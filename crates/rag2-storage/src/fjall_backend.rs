//! Fjall (LSM tree) backend: the default. Write-optimized, which suits ingest-heavy workloads.

use crate::backend::{KvBackend, Op, Pairs, Space};
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use rag2_core::{Error, Result};
use std::collections::HashMap;
use std::path::Path;

pub struct FjallBackend {
    db: Database,
    spaces: HashMap<Space, Keyspace>,
    /// When true every batch is fsynced before `write` returns.
    sync: bool,
}

impl FjallBackend {
    pub fn open(path: &Path, cache_bytes: u64, sync: bool) -> Result<Self> {
        let db = Database::builder(path)
            .cache_size(cache_bytes)
            .open()
            .map_err(Error::storage)?;
        let mut spaces = HashMap::new();
        for s in Space::ALL {
            spaces.insert(
                s,
                db.keyspace(s.name(), KeyspaceCreateOptions::default)
                    .map_err(Error::storage)?,
            );
        }
        Ok(FjallBackend { db, spaces, sync })
    }

    fn ks(&self, s: Space) -> Result<&Keyspace> {
        self.spaces
            .get(&s)
            .ok_or_else(|| Error::Internal(format!("keyspace {} not open", s.name())))
    }
}

impl KvBackend for FjallBackend {
    fn name(&self) -> &'static str {
        "fjall"
    }

    fn get(&self, space: Space, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self
            .ks(space)?
            .get(key)
            .map_err(Error::storage)?
            .map(|v| v.to_vec()))
    }

    fn scan(
        &self,
        space: Space,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Pairs> {
        let ks = self.ks(space)?;
        let mut out = Vec::new();
        let iter = match after {
            Some(a) => {
                // Smallest key strictly greater than `a`.
                let mut start = a.to_vec();
                start.push(0);
                ks.range(start..)
            }
            None => ks.prefix(prefix),
        };
        for g in iter {
            let (k, v) = g.into_inner().map_err(Error::storage)?;
            if !k.starts_with(prefix) {
                break;
            }
            out.push((k.to_vec(), v.to_vec()));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn write(&self, ops: Vec<Op>) -> Result<()> {
        let mut b = self.db.batch();
        if self.sync {
            b = b.durability(Some(PersistMode::SyncData));
        }
        for op in ops {
            match op {
                Op::Put(s, k, v) => b.insert(self.ks(s)?, k, v),
                Op::Delete(s, k) => b.remove(self.ks(s)?, k),
            }
        }
        b.commit().map_err(Error::storage)
    }

    fn persist(&self) -> Result<()> {
        self.db
            .persist(PersistMode::SyncAll)
            .map_err(Error::storage)
    }
}
