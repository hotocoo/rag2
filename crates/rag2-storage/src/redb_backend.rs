//! redb (copy-on-write B-tree) backend: the alternative. Read-optimized with serializable
//! transactions; pick it for read-heavy tenants with modest ingest (see docs/benchmarks/storage.md).

use crate::backend::{KvBackend, Op, Pairs, Space};
use rag2_core::{Error, Result};
use redb::{Database, Durability, ReadableDatabase, TableDefinition};
use std::path::Path;

fn table(s: Space) -> TableDefinition<'static, &'static [u8], &'static [u8]> {
    TableDefinition::new(s.name())
}

pub struct RedbBackend {
    db: Database,
    sync: bool,
}

impl RedbBackend {
    pub fn open(path: &Path, cache_bytes: u64, sync: bool) -> Result<Self> {
        std::fs::create_dir_all(path).map_err(Error::storage)?;
        let db = Database::builder()
            .set_cache_size(cache_bytes as usize)
            .create(path.join("rag2.redb"))
            .map_err(Error::storage)?;
        // Create every table up front so reads never race table creation.
        let w = db.begin_write().map_err(Error::storage)?;
        for s in Space::ALL {
            w.open_table(table(s)).map_err(Error::storage)?;
        }
        w.commit().map_err(Error::storage)?;
        Ok(RedbBackend { db, sync })
    }
}

impl KvBackend for RedbBackend {
    fn name(&self) -> &'static str {
        "redb"
    }

    fn get(&self, space: Space, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let r = self.db.begin_read().map_err(Error::storage)?;
        let t = r.open_table(table(space)).map_err(Error::storage)?;
        Ok(t.get(key)
            .map_err(Error::storage)?
            .map(|v| v.value().to_vec()))
    }

    fn scan(
        &self,
        space: Space,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Pairs> {
        let r = self.db.begin_read().map_err(Error::storage)?;
        let t = r.open_table(table(space)).map_err(Error::storage)?;
        let start: Vec<u8> = match after {
            Some(a) => {
                let mut s = a.to_vec();
                s.push(0);
                s
            }
            None => prefix.to_vec(),
        };
        let mut out = Vec::new();
        for item in t
            .range::<&[u8]>(start.as_slice()..)
            .map_err(Error::storage)?
        {
            let (k, v) = item.map_err(Error::storage)?;
            if !k.value().starts_with(prefix) {
                break;
            }
            out.push((k.value().to_vec(), v.value().to_vec()));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    fn write(&self, ops: Vec<Op>) -> Result<()> {
        let mut w = self.db.begin_write().map_err(Error::storage)?;
        if !self.sync {
            w.set_durability(Durability::None).map_err(Error::storage)?;
        }
        for op in ops {
            match op {
                Op::Put(s, k, v) => {
                    w.open_table(table(s))
                        .map_err(Error::storage)?
                        .insert(k.as_slice(), v.as_slice())
                        .map_err(Error::storage)?;
                }
                Op::Delete(s, k) => {
                    w.open_table(table(s))
                        .map_err(Error::storage)?
                        .remove(k.as_slice())
                        .map_err(Error::storage)?;
                }
            }
        }
        w.commit().map_err(Error::storage)
    }

    fn persist(&self) -> Result<()> {
        // An empty durable commit flushes earlier non-durable ones.
        let w = self.db.begin_write().map_err(Error::storage)?;
        w.commit().map_err(Error::storage)
    }
}
