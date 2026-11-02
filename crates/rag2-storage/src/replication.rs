//! Per-tenant replication log. On a primary, every write batch appends one entry in the same
//! atomic storage batch, so the log and the data can never disagree. Replicas pull entries in
//! order and apply them with chunk ids preserved and vectors included (no re-embedding); the
//! applied position is stored atomically with the applied data, so applying is exactly-once
//! across crashes.
//!
//! A tenant's log has an `epoch` (fresh after the tenant is deleted and recreated). A replica
//! whose epoch differs from the primary's discards its copy of the tenant and replays from the
//! start.

use super::backend::{Op, Space};
use super::{DocumentWrite, Storage, be_u64, tkey, tprefix};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rag2_core::model::{Chunk, DocumentRecord};
use rag2_core::{ChunkId, DocumentId, Error, Result, TenantId};
use serde::{Deserialize, Serialize};

const LOG_SEQ: &[u8] = b"log_seq";
const LOG_EPOCH: &[u8] = b"log_epoch";
const APPLIED_SEQ: &[u8] = b"repl_applied_seq";
const APPLIED_EPOCH: &[u8] = b"repl_applied_epoch";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedVector {
    pub embedder: String,
    pub chunk: ChunkId,
    /// Little-endian f32, base64 (a third of the size of a JSON float array).
    pub data: String,
}

impl LoggedVector {
    pub fn decode(&self) -> Result<Vec<f32>> {
        let bytes = B64
            .decode(&self.data)
            .map_err(|e| Error::Storage(format!("log vector: {e}")))?;
        super::decode_vector(&bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedDocument {
    pub record: DocumentRecord,
    pub chunks: Vec<Chunk>,
    pub vectors: Vec<LoggedVector>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LogEntry {
    Put { documents: Vec<LoggedDocument> },
    Delete { ids: Vec<DocumentId> },
}

impl LogEntry {
    pub fn put(writes: &[DocumentWrite<'_>]) -> LogEntry {
        LogEntry::Put {
            documents: writes
                .iter()
                .map(|w| LoggedDocument {
                    record: w.record.clone(),
                    chunks: w.chunks.to_vec(),
                    vectors: w
                        .vectors
                        .iter()
                        .map(|(e, c, v)| LoggedVector {
                            embedder: (*e).to_string(),
                            chunk: *c,
                            data: B64.encode(super::encode_vector(v)),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// Where a log stands: its epoch and the last sequence number written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LogPosition {
    pub epoch: u64,
    pub seq: u64,
}

fn new_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        .max(1)
}

impl Storage {
    fn meta_u64(&self, t: &TenantId, name: &[u8]) -> Result<u64> {
        self.kv
            .get(Space::Meta, &tkey(t, name))?
            .map(|b| be_u64(&b))
            .transpose()
            .map(|v| v.unwrap_or(0))
    }

    /// Ops that append `entry` to the tenant's log. Call with the tenant lock held.
    pub(crate) fn log_ops(&self, t: &TenantId, entry: &LogEntry) -> Result<Vec<Op>> {
        let mut ops = Vec::with_capacity(3);
        if self.meta_u64(t, LOG_EPOCH)? == 0 {
            ops.push(Op::Put(
                Space::Meta,
                tkey(t, LOG_EPOCH),
                new_epoch().to_be_bytes().to_vec(),
            ));
        }
        let seq = self.meta_u64(t, LOG_SEQ)? + 1;
        ops.push(Op::Put(
            Space::Meta,
            tkey(t, LOG_SEQ),
            seq.to_be_bytes().to_vec(),
        ));
        ops.push(self.put_json(Space::Log, tkey(t, &seq.to_be_bytes()), entry)?);
        Ok(ops)
    }

    /// The primary-side log head of a tenant.
    pub fn log_head(&self, t: &TenantId) -> Result<LogPosition> {
        Ok(LogPosition {
            epoch: self.meta_u64(t, LOG_EPOCH)?,
            seq: self.meta_u64(t, LOG_SEQ)?,
        })
    }

    /// Entries after `after`, in order, at most `limit`.
    pub fn read_log(&self, t: &TenantId, after: u64, limit: usize) -> Result<Vec<(u64, LogEntry)>> {
        let after_key = (after > 0).then(|| tkey(t, &after.to_be_bytes()));
        self.kv
            .scan(Space::Log, &tprefix(t), after_key.as_deref(), limit)?
            .into_iter()
            .map(|(k, v)| {
                let seq = be_u64(&k[k.len() - 8..])?;
                let plain = self.unseal(Space::Log, &k, v)?;
                let e = serde_json::from_slice(&plain)
                    .map_err(|e| Error::Storage(format!("corrupt log entry: {e}")))?;
                Ok((seq, e))
            })
            .collect()
    }

    /// Drop log entries up to and including `upto` (only once every replica has applied them).
    pub fn trim_log(&self, t: &TenantId, upto: u64) -> Result<u64> {
        let mut removed = 0;
        loop {
            let page = self.kv.scan(Space::Log, &tprefix(t), None, 1_000)?;
            let doomed: Vec<Op> = page
                .into_iter()
                .filter(|(k, _)| be_u64(&k[k.len() - 8..]).is_ok_and(|s| s <= upto))
                .map(|(k, _)| Op::Delete(Space::Log, k))
                .collect();
            if doomed.is_empty() {
                return Ok(removed);
            }
            removed += doomed.len() as u64;
            self.kv.write(doomed)?;
        }
    }

    /// Replica-side position: the last primary entry applied for this tenant.
    pub fn applied_position(&self, t: &TenantId) -> Result<LogPosition> {
        Ok(LogPosition {
            epoch: self.meta_u64(t, APPLIED_EPOCH)?,
            seq: self.meta_u64(t, APPLIED_SEQ)?,
        })
    }

    fn position_ops(t: &TenantId, epoch: u64, seq: u64) -> Vec<Op> {
        vec![
            Op::Put(
                Space::Meta,
                tkey(t, APPLIED_EPOCH),
                epoch.to_be_bytes().to_vec(),
            ),
            Op::Put(
                Space::Meta,
                tkey(t, APPLIED_SEQ),
                seq.to_be_bytes().to_vec(),
            ),
        ]
    }

    /// Apply one primary log entry on a replica, atomically with the new applied position.
    /// Entries at or below the applied position are ignored (idempotent). Returns what changed
    /// so the caller can update derived indexes.
    pub fn apply_log_entry(
        &self,
        t: &TenantId,
        epoch: u64,
        seq: u64,
        entry: &LogEntry,
    ) -> Result<Applied> {
        let pos = self.applied_position(t)?;
        if pos.epoch == epoch && seq <= pos.seq {
            return Ok(Applied::Skipped);
        }
        if pos.epoch == epoch && seq != pos.seq + 1 || pos.epoch != epoch && seq != 1 {
            return Err(Error::Conflict(format!(
                "replication gap for tenant {t}: applied {}:{}, next entry {epoch}:{seq}",
                pos.epoch, pos.seq
            )));
        }
        let pos_ops = Self::position_ops(t, epoch, seq);
        match entry {
            LogEntry::Put { documents } => {
                let decoded: Vec<Vec<(String, ChunkId, Vec<f32>)>> = documents
                    .iter()
                    .map(|d| {
                        d.vectors
                            .iter()
                            .map(|v| Ok((v.embedder.clone(), v.chunk, v.decode()?)))
                            .collect::<Result<Vec<_>>>()
                    })
                    .collect::<Result<_>>()?;
                let borrowed: Vec<Vec<(&str, ChunkId, Vec<f32>)>> = decoded
                    .iter()
                    .map(|vs| {
                        vs.iter()
                            .map(|(e, c, v)| (e.as_str(), *c, v.clone()))
                            .collect()
                    })
                    .collect();
                let writes: Vec<DocumentWrite<'_>> = documents
                    .iter()
                    .zip(&borrowed)
                    .map(|(d, vectors)| DocumentWrite {
                        record: &d.record,
                        chunks: &d.chunks,
                        vectors,
                    })
                    .collect();
                // Keep chunk ids unique on the replica too, so a later promotion never reuses one.
                if let Some(max) = documents
                    .iter()
                    .flat_map(|d| d.chunks.iter().map(|c| c.id.0))
                    .max()
                {
                    self.bump_chunk_counter(t, max + 1)?;
                }
                let (replaced, generation) = self.put_documents_with(t, &writes, pos_ops, false)?;
                Ok(Applied::Put {
                    replaced,
                    generation,
                })
            }
            LogEntry::Delete { ids } => {
                let (gone, generation) = self.delete_documents_with(t, ids, pos_ops, false)?;
                Ok(Applied::Delete { gone, generation })
            }
        }
    }

    /// Raise the tenant's chunk id counter to at least `next`.
    fn bump_chunk_counter(&self, t: &TenantId, next: u64) -> Result<()> {
        let mut counters = self.counters.lock();
        let cur = match counters.get(t) {
            Some(v) => *v,
            None => self.meta_u64(t, super::CHUNK_COUNTER)?,
        };
        if next > cur {
            self.kv.write(vec![Op::Put(
                Space::Meta,
                tkey(t, super::CHUNK_COUNTER),
                next.to_be_bytes().to_vec(),
            )])?;
            counters.insert(t.clone(), next);
        }
        Ok(())
    }
}

/// What applying a log entry changed.
#[derive(Debug)]
pub enum Applied {
    Skipped,
    Put {
        replaced: Vec<Option<DocumentRecord>>,
        generation: u64,
    },
    Delete {
        gone: Vec<DocumentRecord>,
        generation: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BackendKind, StorageOptions};
    use rag2_core::model::Metadata;

    fn open(dir: &std::path::Path, log: bool) -> Storage {
        Storage::open(StorageOptions {
            backend: BackendKind::Fjall,
            path: dir.to_path_buf(),
            cache_bytes: 8 << 20,
            sync_writes: false,
            encryption_key: Some(vec![9u8; 32]),
            replication_log: log,
        })
        .expect("open")
    }

    fn rec(id: &str, chunks: &[u64]) -> DocumentRecord {
        DocumentRecord {
            id: DocumentId::new(id).expect("id"),
            title: None,
            source: None,
            metadata: Metadata::new(),
            version: 1,
            content_hash: 1,
            chunks: chunks.iter().map(|c| ChunkId(*c)).collect(),
            embedders: vec!["e".into()],
            ingested_at_ms: 0,
        }
    }

    fn chunk(doc: &str, id: u64) -> Chunk {
        Chunk {
            id: ChunkId(id),
            doc_id: DocumentId::new(doc).expect("id"),
            ordinal: 0,
            text: format!("text {id}"),
            start: 0,
            end: 6,
            hash: id,
        }
    }

    #[test]
    fn a_replica_converges_on_the_primary_exactly_once() {
        let (pd, rd) = (
            tempfile::tempdir().expect("dir"),
            tempfile::tempdir().expect("dir"),
        );
        let (primary, replica) = (open(pd.path(), true), open(rd.path(), false));
        let t = TenantId::new("acme").expect("t");
        let r1 = rec("a", &[1, 2]);
        primary
            .put_documents(
                &t,
                &[DocumentWrite {
                    record: &r1,
                    chunks: &[chunk("a", 1), chunk("a", 2)],
                    vectors: &[("e", ChunkId(1), vec![0.5, 0.5])],
                }],
            )
            .expect("put");
        primary
            .delete_document(&t, &DocumentId::new("a").expect("id"))
            .expect("delete");
        let r2 = rec("b", &[3]);
        primary
            .put_document(&t, &r2, &[chunk("b", 3)], &[])
            .expect("put b");
        let head = primary.log_head(&t).expect("head");
        assert_eq!(head.seq, 3);
        let entries = primary.read_log(&t, 0, 100).expect("log");
        for (seq, e) in &entries {
            replica
                .apply_log_entry(&t, head.epoch, *seq, e)
                .expect("apply");
        }
        // Re-applying is a no-op.
        assert!(matches!(
            replica
                .apply_log_entry(&t, head.epoch, 2, &entries[1].1)
                .expect("again"),
            Applied::Skipped
        ));
        assert!(
            replica
                .get_document(&t, &DocumentId::new("a").expect("id"))
                .expect("get")
                .is_none()
        );
        assert_eq!(
            replica
                .get_chunk(&t, ChunkId(3))
                .expect("chunk")
                .expect("b chunk")
                .text,
            "text 3"
        );
        assert_eq!(replica.applied_position(&t).expect("pos"), head);
        assert_eq!(replica.document_count(&t).expect("count"), 1);
        // A gap is refused, not silently skipped.
        assert!(
            replica
                .apply_log_entry(&t, head.epoch, 9, &entries[0].1)
                .is_err()
        );
        // Replica allocations continue above every replicated id.
        assert!(replica.allocate_chunk_ids(&t, 1).expect("alloc").start > 3);
    }

    #[test]
    fn log_reads_page_in_order_and_trim() {
        let d = tempfile::tempdir().expect("dir");
        let s = open(d.path(), true);
        let t = TenantId::new("p").expect("t");
        for i in 0..5 {
            s.put_document(&t, &rec(&format!("d{i}"), &[]), &[], &[])
                .expect("put");
        }
        let first = s.read_log(&t, 0, 2).expect("page");
        assert_eq!(
            first.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(s.read_log(&t, 2, 10).expect("rest").len(), 3);
        assert_eq!(s.trim_log(&t, 3).expect("trim"), 3);
        assert_eq!(
            s.read_log(&t, 0, 10).expect("left").first().map(|e| e.0),
            Some(4)
        );
    }

    #[test]
    fn standalone_storage_writes_no_log() {
        let d = tempfile::tempdir().expect("dir");
        let s = open(d.path(), false);
        let t = TenantId::new("q").expect("t");
        s.put_document(&t, &rec("x", &[]), &[], &[]).expect("put");
        assert_eq!(s.log_head(&t).expect("head").seq, 0);
    }
}
