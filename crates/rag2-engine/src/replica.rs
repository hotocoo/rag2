//! Replica side of replication: apply a primary's log entries to local storage and indexes.

use crate::Engine;
use crate::ingest::IndexedDoc;
use rag2_config::schema::ClusterRole;

use rag2_core::{ChunkId, Result, TenantId};
use rag2_storage::replication::{Applied, LogEntry, LogPosition};

impl Engine {
    pub fn role(&self) -> ClusterRole {
        self.config.current().profile.cluster.role
    }

    /// This node's applied position for a tenant (replicas).
    pub fn replication_position(&self, t: &TenantId) -> Result<LogPosition> {
        self.storage.applied_position(t)
    }

    /// The tenant's log head (primaries).
    pub fn log_head(&self, t: &TenantId) -> Result<LogPosition> {
        self.storage.log_head(t)
    }

    pub fn read_log(&self, t: &TenantId, after: u64, limit: usize) -> Result<Vec<(u64, LogEntry)>> {
        self.storage.read_log(t, after, limit)
    }

    /// Apply a primary's entries (in order) for one tenant. When the primary's epoch differs from
    /// the one applied here, the tenant was deleted and recreated on the primary: the local copy is
    /// discarded and replay starts from the first entry. Returns how many entries changed data.
    pub fn apply_replicated(
        &self,
        t: &TenantId,
        epoch: u64,
        entries: &[(u64, LogEntry)],
    ) -> Result<usize> {
        let lock = self.ingest_lock(t);
        let _g = lock.lock();
        let pos = self.storage.applied_position(t)?;
        if pos.epoch != 0 && pos.epoch != epoch {
            tracing::warn!(tenant = %t, local = pos.epoch, primary = epoch, "primary epoch changed; resetting local copy");
            self.storage.delete_tenant(t)?;
            self.lexical.drop_tenant(t)?;
            self.vectors.write().remove(t);
            self.chunk_cache.invalidate_all();
            self.doc_cache.invalidate_all();
        }
        let tv = self.tenant_vectors(t)?;
        let lex = self.lexical.get(t)?;
        let mut changed = 0;
        for (seq, entry) in entries {
            match (self.storage.apply_log_entry(t, epoch, *seq, entry)?, entry) {
                (Applied::Skipped, _) => {}
                (
                    Applied::Put {
                        replaced,
                        generation,
                    },
                    LogEntry::Put { documents },
                ) => {
                    let decoded: Vec<Vec<(&str, ChunkId, Vec<f32>)>> = documents
                        .iter()
                        .map(|d| {
                            d.vectors
                                .iter()
                                .map(|v| Ok((v.embedder.as_str(), v.chunk, v.decode()?)))
                                .collect::<Result<_>>()
                        })
                        .collect::<Result<_>>()?;
                    let docs: Vec<IndexedDoc<'_>> = documents
                        .iter()
                        .zip(&decoded)
                        .map(|(d, v)| (&d.record, d.chunks.as_slice(), v.as_slice()))
                        .collect();
                    self.index_put(t, &tv, &lex, &docs, &replaced, generation)?;
                    changed += 1;
                }
                (Applied::Delete { gone, generation }, _) => {
                    self.index_delete(t, &tv, &lex, &gone, generation)?;
                    changed += 1;
                }
                (Applied::Put { .. }, LogEntry::Delete { .. }) => {
                    return Err(rag2_core::Error::Internal(
                        "log entry and apply result disagree".into(),
                    ));
                }
            }
        }
        if changed > 0 {
            if self
                .config
                .current()
                .profile
                .indexes
                .lexical
                .commit_interval_ms
                == 0
            {
                lex.commit()?;
            }
            self.maybe_rebuild_vectors(t, &tv)?;
        }
        Ok(changed)
    }
}
