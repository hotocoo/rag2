//! Tenant-scoped storage. Every method takes a [`TenantId`] and every key starts with
//! `tenant \0`; tenant ids cannot contain `\0`, so no key of one tenant is a prefix of another's and
//! no API can address another tenant's data.
//!
//! Values are JSON (records) or little-endian f32 (vectors), optionally sealed with
//! XChaCha20-Poly1305 ([`cipher`]). Keys are stored in plaintext; do not put secrets in document ids.

pub mod backend;
pub mod cipher;
pub mod fjall_backend;
pub mod redb_backend;
pub mod replication;

use backend::{KvBackend, Op, Space};
use cipher::Cipher;
use parking_lot::Mutex;
use rag2_core::model::{Chunk, DocumentRecord};
use rag2_core::{ChunkId, DocumentId, Error, Result, TenantId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Fjall,
    Redb,
}

/// One document's write in a [`Storage::put_documents`] batch.
pub struct DocumentWrite<'a> {
    pub record: &'a DocumentRecord,
    pub chunks: &'a [Chunk],
    pub vectors: &'a [(&'a str, ChunkId, Vec<f32>)],
}

pub struct StorageOptions {
    pub backend: BackendKind,
    pub path: PathBuf,
    pub cache_bytes: u64,
    /// fsync every write batch before acknowledging it.
    pub sync_writes: bool,
    /// 32-byte key; `None` stores plaintext.
    pub encryption_key: Option<Vec<u8>>,
    /// Append every write to the per-tenant replication log (primaries).
    pub replication_log: bool,
}

/// Registry of tenants, under a key no tenant prefix can produce (tenant ids never start with `\0`).
const TENANT_REGISTRY: &[u8] = b"\0tenants\0";
const CHUNK_COUNTER: &[u8] = b"chunk_seq";
const DOC_COUNT: &[u8] = b"doc_count";
/// Bumped in the same batch as every document write or delete. Derived indexes record the
/// generation they reflect; a mismatch at startup triggers a rebuild from storage.
const GENERATION: &[u8] = b"generation";

pub struct Storage {
    kv: Box<dyn KvBackend>,
    cipher: Option<Cipher>,
    /// Next chunk id per tenant (loaded lazily from the persisted counter).
    counters: Mutex<HashMap<TenantId, u64>>,
    /// Serializes read-modify-write sequences per tenant (document replace, counters).
    locks: Mutex<HashMap<TenantId, Arc<Mutex<()>>>>,
    audit_seq: AtomicU64,
    log_writes: bool,
}

fn tkey(t: &TenantId, rest: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(t.as_str().len() + 1 + rest.len());
    k.extend_from_slice(t.as_str().as_bytes());
    k.push(0);
    k.extend_from_slice(rest);
    k
}

fn tprefix(t: &TenantId) -> Vec<u8> {
    tkey(t, &[])
}

fn chunk_key(t: &TenantId, c: ChunkId) -> Vec<u8> {
    tkey(t, &c.0.to_be_bytes())
}

fn vector_key(t: &TenantId, embedder: &str, c: ChunkId) -> Vec<u8> {
    tkey(
        t,
        &[embedder.as_bytes(), b"\0", &c.0.to_be_bytes()].concat(),
    )
}

fn vector_prefix(t: &TenantId, embedder: &str) -> Vec<u8> {
    tkey(t, &[embedder.as_bytes(), b"\0"].concat())
}

/// Metadata index entry: `tenant \0 x \0 field \0 value-json \0 doc_id`.
fn meta_index_prefix(t: &TenantId, field: &str, value: &serde_json::Value) -> Vec<u8> {
    tkey(
        t,
        &[
            b"x\0",
            field.as_bytes(),
            b"\0",
            value.to_string().as_bytes(),
            b"\0",
        ]
        .concat(),
    )
}

/// Scalar values a metadata field is indexed under (array elements individually).
fn indexable(v: &serde_json::Value) -> Vec<&serde_json::Value> {
    match v {
        serde_json::Value::Array(items) => items
            .iter()
            .filter(|i| !i.is_object() && !i.is_array())
            .collect(),
        serde_json::Value::Object(_) | serde_json::Value::Null => Vec::new(),
        scalar => vec![scalar],
    }
}

fn meta_index_keys(t: &TenantId, rec: &DocumentRecord) -> Vec<Vec<u8>> {
    rec.metadata
        .iter()
        .filter(|(f, _)| !f.contains('\0'))
        .flat_map(|(f, v)| {
            indexable(v).into_iter().map(move |x| {
                [
                    meta_index_prefix(t, f, x),
                    rec.id.as_str().as_bytes().to_vec(),
                ]
                .concat()
            })
        })
        .collect()
}

fn registry_key(t: &TenantId) -> Vec<u8> {
    [TENANT_REGISTRY, t.as_str().as_bytes()].concat()
}

pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn decode_vector(b: &[u8]) -> Result<Vec<f32>> {
    let (chunks, rest) = b.as_chunks::<4>();
    if !rest.is_empty() {
        return Err(Error::Storage("vector bytes not a multiple of 4".into()));
    }
    Ok(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

fn be_u64(b: &[u8]) -> Result<u64> {
    b.try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| Error::Storage("corrupt counter".into()))
}

impl Storage {
    pub fn open(o: StorageOptions) -> Result<Self> {
        let kv: Box<dyn KvBackend> = match o.backend {
            BackendKind::Fjall => Box::new(fjall_backend::FjallBackend::open(
                &o.path,
                o.cache_bytes,
                o.sync_writes,
            )?),
            BackendKind::Redb => Box::new(redb_backend::RedbBackend::open(
                &o.path,
                o.cache_bytes,
                o.sync_writes,
            )?),
        };
        let cipher = o.encryption_key.as_deref().map(Cipher::new).transpose()?;
        tracing::info!(backend = kv.name(), path = %o.path.display(), encrypted = cipher.is_some(), "storage opened");
        Ok(Storage {
            kv,
            cipher,
            counters: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            audit_seq: AtomicU64::new(0),
            log_writes: o.replication_log,
        })
    }

    pub fn backend_name(&self) -> &'static str {
        self.kv.name()
    }

    fn seal(&self, space: Space, key: &[u8], plain: Vec<u8>) -> Result<Vec<u8>> {
        match &self.cipher {
            Some(c) => c.seal(&[space.name().as_bytes(), b"\0", key].concat(), &plain),
            None => Ok(plain),
        }
    }

    fn unseal(&self, space: Space, key: &[u8], stored: Vec<u8>) -> Result<Vec<u8>> {
        match &self.cipher {
            Some(c) => c.open(&[space.name().as_bytes(), b"\0", key].concat(), &stored),
            None => Ok(stored),
        }
    }

    fn put_json<T: serde::Serialize>(&self, space: Space, key: Vec<u8>, v: &T) -> Result<Op> {
        let bytes = serde_json::to_vec(v).map_err(Error::internal)?;
        let sealed = self.seal(space, &key, bytes)?;
        Ok(Op::Put(space, key, sealed))
    }

    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        space: Space,
        key: &[u8],
    ) -> Result<Option<T>> {
        let Some(raw) = self.kv.get(space, key)? else {
            return Ok(None);
        };
        let plain = self.unseal(space, key, raw)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|e| Error::Storage(format!("corrupt record: {e}")))
    }

    fn tenant_lock(&self, t: &TenantId) -> Arc<Mutex<()>> {
        self.locks.lock().entry(t.clone()).or_default().clone()
    }

    // ---- tenants ----

    pub fn register_tenant(&self, t: &TenantId) -> Result<()> {
        self.kv
            .write(vec![Op::Put(Space::Meta, registry_key(t), Vec::new())])
    }

    pub fn tenants(&self) -> Result<Vec<TenantId>> {
        self.kv
            .scan(Space::Meta, TENANT_REGISTRY, None, usize::MAX)?
            .into_iter()
            .map(|(k, _)| {
                let id =
                    std::str::from_utf8(&k[TENANT_REGISTRY.len()..]).map_err(Error::storage)?;
                TenantId::new(id)
            })
            .collect()
    }

    /// Remove every record of `t` in every space. Irreversible.
    pub fn delete_tenant(&self, t: &TenantId) -> Result<u64> {
        let lock = self.tenant_lock(t);
        let _g = lock.lock();
        let prefix = tprefix(t);
        let mut removed = 0u64;
        for space in [
            Space::Documents,
            Space::Chunks,
            Space::Vectors,
            Space::Meta,
            Space::Log,
        ] {
            loop {
                let page = self.kv.scan(space, &prefix, None, 1_000)?;
                if page.is_empty() {
                    break;
                }
                removed += page.len() as u64;
                self.kv.write(
                    page.into_iter()
                        .map(|(k, _)| Op::Delete(space, k))
                        .collect(),
                )?;
            }
        }
        self.kv
            .write(vec![Op::Delete(Space::Meta, registry_key(t))])?;
        self.counters.lock().remove(t);
        Ok(removed)
    }

    // ---- ids and counters ----

    /// Reserve `n` consecutive chunk ids for `t`. The high-water mark is persisted before the ids
    /// are handed out, so ids are never reused after a crash (gaps are possible and harmless).
    pub fn allocate_chunk_ids(&self, t: &TenantId, n: u64) -> Result<std::ops::Range<u64>> {
        let mut counters = self.counters.lock();
        let next = match counters.get(t) {
            Some(v) => *v,
            None => self
                .kv
                .get(Space::Meta, &tkey(t, CHUNK_COUNTER))?
                .map(|b| be_u64(&b))
                .transpose()?
                .unwrap_or(0),
        };
        let end = next
            .checked_add(n)
            .ok_or_else(|| Error::Internal("chunk id space exhausted".into()))?;
        self.kv.write(vec![Op::Put(
            Space::Meta,
            tkey(t, CHUNK_COUNTER),
            end.to_be_bytes().to_vec(),
        )])?;
        counters.insert(t.clone(), end);
        Ok(next..end)
    }

    /// The tenant's write generation (see [`GENERATION`]).
    pub fn generation(&self, t: &TenantId) -> Result<u64> {
        self.kv
            .get(Space::Meta, &tkey(t, GENERATION))?
            .map(|b| be_u64(&b))
            .transpose()
            .map(|v| v.unwrap_or(0))
    }

    fn bump_generation(&self, t: &TenantId) -> Result<(Op, u64)> {
        let g = self.generation(t)? + 1;
        Ok((
            Op::Put(Space::Meta, tkey(t, GENERATION), g.to_be_bytes().to_vec()),
            g,
        ))
    }

    pub fn document_count(&self, t: &TenantId) -> Result<u64> {
        self.kv
            .get(Space::Meta, &tkey(t, DOC_COUNT))?
            .map(|b| be_u64(&b))
            .transpose()
            .map(|v| v.unwrap_or(0))
    }

    // ---- documents ----

    /// Store a document with its chunks and vectors in one atomic batch, replacing any previous
    /// version (its chunks and vectors are deleted in the same batch). Returns the replaced record
    /// and the tenant's new generation.
    pub fn put_document(
        &self,
        t: &TenantId,
        rec: &DocumentRecord,
        chunks: &[Chunk],
        vectors: &[(&str, ChunkId, Vec<f32>)],
    ) -> Result<(Option<DocumentRecord>, u64)> {
        let (mut old, g) = self.put_documents(
            t,
            &[DocumentWrite {
                record: rec,
                chunks,
                vectors,
            }],
        )?;
        Ok((old.pop().flatten(), g))
    }

    /// Store several documents in ONE atomic batch (one fsync): all of them or none. Document ids
    /// must be distinct. Returns each replaced record (in order) and the new generation.
    pub fn put_documents(
        &self,
        t: &TenantId,
        writes: &[DocumentWrite<'_>],
    ) -> Result<(Vec<Option<DocumentRecord>>, u64)> {
        self.put_documents_with(t, writes, Vec::new(), self.log_writes)
    }

    /// [`put_documents`](Self::put_documents) with extra ops in the same batch, and an explicit
    /// choice of whether to append to the replication log.
    pub(crate) fn put_documents_with(
        &self,
        t: &TenantId,
        writes: &[DocumentWrite<'_>],
        extra: Vec<Op>,
        log: bool,
    ) -> Result<(Vec<Option<DocumentRecord>>, u64)> {
        let lock = self.tenant_lock(t);
        let _g = lock.lock();
        let mut ids = std::collections::HashSet::new();
        if !writes.iter().all(|w| ids.insert(&w.record.id)) {
            return Err(Error::InvalidInput(
                "duplicate document id in one batch".into(),
            ));
        }
        let (gen_op, generation) = self.bump_generation(t)?;
        let mut ops = Vec::new();
        let mut olds = Vec::with_capacity(writes.len());
        let mut new_docs = 0u64;
        for w in writes {
            let rec = w.record;
            let dkey = tkey(t, rec.id.as_str().as_bytes());
            let old: Option<DocumentRecord> = self.get_json(Space::Documents, &dkey)?;
            match &old {
                Some(o) => ops.extend(self.removal_ops(t, o)),
                None => new_docs += 1,
            }
            for c in w.chunks {
                if c.doc_id != rec.id {
                    return Err(Error::InvalidInput(
                        "chunk belongs to another document".into(),
                    ));
                }
                ops.push(self.put_json(Space::Chunks, chunk_key(t, c.id), c)?);
            }
            for (emb, id, v) in w.vectors {
                if !rec.embedders.iter().any(|e| e == emb) {
                    return Err(Error::InvalidInput(format!(
                        "vector for embedder {emb:?} not listed in the record"
                    )));
                }
                let k = vector_key(t, emb, *id);
                let sealed = self.seal(Space::Vectors, &k, encode_vector(v))?;
                ops.push(Op::Put(Space::Vectors, k, sealed));
            }
            for k in meta_index_keys(t, rec) {
                ops.push(Op::Put(Space::Meta, k, Vec::new()));
            }
            ops.push(self.put_json(Space::Documents, dkey, rec)?);
            olds.push(old);
        }
        if new_docs > 0 {
            let n = self.document_count(t)? + new_docs;
            ops.push(Op::Put(
                Space::Meta,
                tkey(t, DOC_COUNT),
                n.to_be_bytes().to_vec(),
            ));
            ops.push(Op::Put(Space::Meta, registry_key(t), Vec::new()));
        }
        ops.push(gen_op);
        // A key both deleted (old version) and written (new version) keeps only the write: the
        // order of same-key ops inside one batch is not something to depend on.
        let written: std::collections::HashSet<(Space, Vec<u8>)> = ops
            .iter()
            .filter_map(|o| match o {
                Op::Put(s, k, _) => Some((*s, k.clone())),
                Op::Delete(..) => None,
            })
            .collect();
        ops.retain(|o| !matches!(o, Op::Delete(s, k) if written.contains(&(*s, k.clone()))));
        ops.extend(extra);
        if log {
            ops.extend(self.log_ops(t, &replication::LogEntry::put(writes))?);
        }
        self.kv.write(ops)?;
        Ok((olds, generation))
    }

    /// Deletes for a stored document's chunks, vectors and metadata index entries.
    fn removal_ops(&self, t: &TenantId, rec: &DocumentRecord) -> Vec<Op> {
        let mut ops = Vec::new();
        for c in &rec.chunks {
            ops.push(Op::Delete(Space::Chunks, chunk_key(t, *c)));
            for e in &rec.embedders {
                ops.push(Op::Delete(Space::Vectors, vector_key(t, e, *c)));
            }
        }
        ops.extend(
            meta_index_keys(t, rec)
                .into_iter()
                .map(|k| Op::Delete(Space::Meta, k)),
        );
        ops
    }

    /// Ids of documents whose metadata `field` equals (or, for arrays, contains) `value`.
    pub fn documents_with(
        &self,
        t: &TenantId,
        field: &str,
        value: &serde_json::Value,
        limit: usize,
    ) -> Result<Vec<DocumentId>> {
        let prefix = meta_index_prefix(t, field, value);
        self.kv
            .scan(Space::Meta, &prefix, None, limit)?
            .into_iter()
            .map(|(k, _)| {
                DocumentId::new(std::str::from_utf8(&k[prefix.len()..]).map_err(Error::storage)?)
            })
            .collect()
    }

    pub fn get_document(&self, t: &TenantId, id: &DocumentId) -> Result<Option<DocumentRecord>> {
        self.get_json(Space::Documents, &tkey(t, id.as_str().as_bytes()))
    }

    /// Documents in id order, starting after `after`.
    pub fn list_documents(
        &self,
        t: &TenantId,
        after: Option<&DocumentId>,
        limit: usize,
    ) -> Result<Vec<DocumentRecord>> {
        let prefix = tprefix(t);
        let after_key = after.map(|a| tkey(t, a.as_str().as_bytes()));
        self.kv
            .scan(Space::Documents, &prefix, after_key.as_deref(), limit)?
            .into_iter()
            .map(|(k, v)| {
                let plain = self.unseal(Space::Documents, &k, v)?;
                serde_json::from_slice(&plain)
                    .map_err(|e| Error::Storage(format!("corrupt document: {e}")))
            })
            .collect()
    }

    /// Delete a document, its chunks and vectors atomically. Returns the deleted record and the
    /// new generation.
    pub fn delete_document(
        &self,
        t: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<(DocumentRecord, u64)>> {
        let (mut gone, g) =
            self.delete_documents_with(t, std::slice::from_ref(id), Vec::new(), self.log_writes)?;
        Ok(gone.pop().map(|r| (r, g)))
    }

    /// Delete documents in one atomic batch (plus `extra` ops). Returns the records that existed
    /// and the new generation; the generation is bumped only when something was deleted.
    pub(crate) fn delete_documents_with(
        &self,
        t: &TenantId,
        ids: &[DocumentId],
        extra: Vec<Op>,
        log: bool,
    ) -> Result<(Vec<DocumentRecord>, u64)> {
        let lock = self.tenant_lock(t);
        let _g = lock.lock();
        let mut ops = Vec::new();
        let mut gone = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for id in ids.iter().filter(|i| seen.insert(*i)) {
            let dkey = tkey(t, id.as_str().as_bytes());
            let Some(old): Option<DocumentRecord> = self.get_json(Space::Documents, &dkey)? else {
                continue;
            };
            ops.push(Op::Delete(Space::Documents, dkey));
            ops.extend(self.removal_ops(t, &old));
            gone.push(old);
        }
        if gone.is_empty() {
            if !extra.is_empty() {
                self.kv.write(extra)?;
            }
            return Ok((gone, self.generation(t)?));
        }
        let n = self.document_count(t)?.saturating_sub(gone.len() as u64);
        ops.push(Op::Put(
            Space::Meta,
            tkey(t, DOC_COUNT),
            n.to_be_bytes().to_vec(),
        ));
        let (gen_op, generation) = self.bump_generation(t)?;
        ops.push(gen_op);
        ops.extend(extra);
        if log {
            let ids: Vec<DocumentId> = gone.iter().map(|r| r.id.clone()).collect();
            ops.extend(self.log_ops(t, &replication::LogEntry::Delete { ids })?);
        }
        self.kv.write(ops)?;
        Ok((gone, generation))
    }

    // ---- chunks and vectors ----

    pub fn get_chunk(&self, t: &TenantId, id: ChunkId) -> Result<Option<Chunk>> {
        self.get_json(Space::Chunks, &chunk_key(t, id))
    }

    /// Chunks for `ids` in the same order; ids with no chunk (deleted since indexing) are `None`.
    pub fn get_chunks(&self, t: &TenantId, ids: &[ChunkId]) -> Result<Vec<Option<Chunk>>> {
        ids.iter().map(|id| self.get_chunk(t, *id)).collect()
    }

    pub fn get_vector(
        &self,
        t: &TenantId,
        embedder: &str,
        id: ChunkId,
    ) -> Result<Option<Vec<f32>>> {
        let k = vector_key(t, embedder, id);
        match self.kv.get(Space::Vectors, &k)? {
            Some(raw) => decode_vector(&self.unseal(Space::Vectors, &k, raw)?).map(Some),
            None => Ok(None),
        }
    }

    /// Page through a tenant's vectors in chunk-id order (index rebuild at startup).
    pub fn scan_vectors(
        &self,
        t: &TenantId,
        embedder: &str,
        after: Option<ChunkId>,
        limit: usize,
    ) -> Result<Vec<(ChunkId, Vec<f32>)>> {
        let after_key = after.map(|a| vector_key(t, embedder, a));
        self.kv
            .scan(
                Space::Vectors,
                &vector_prefix(t, embedder),
                after_key.as_deref(),
                limit,
            )?
            .into_iter()
            .map(|(k, v)| {
                let id = be_u64(&k[k.len() - 8..])?;
                Ok((
                    ChunkId(id),
                    decode_vector(&self.unseal(Space::Vectors, &k, v)?)?,
                ))
            })
            .collect()
    }

    /// Page through a tenant's chunks in id order (lexical index rebuild).
    pub fn scan_chunks(
        &self,
        t: &TenantId,
        after: Option<ChunkId>,
        limit: usize,
    ) -> Result<Vec<Chunk>> {
        let after_key = after.map(|a| chunk_key(t, a));
        self.kv
            .scan(Space::Chunks, &tprefix(t), after_key.as_deref(), limit)?
            .into_iter()
            .map(|(k, v)| {
                let plain = self.unseal(Space::Chunks, &k, v)?;
                serde_json::from_slice(&plain)
                    .map_err(|e| Error::Storage(format!("corrupt chunk: {e}")))
            })
            .collect()
    }

    // ---- per-tenant metadata ----

    pub fn put_meta(&self, t: &TenantId, name: &str, value: &serde_json::Value) -> Result<()> {
        let op = self.put_json(Space::Meta, tkey(t, format!("m:{name}").as_bytes()), value)?;
        self.kv.write(vec![op])
    }

    pub fn get_meta(&self, t: &TenantId, name: &str) -> Result<Option<serde_json::Value>> {
        self.get_json(Space::Meta, &tkey(t, format!("m:{name}").as_bytes()))
    }

    // ---- audit ----

    /// Append an audit event. Keys sort by time, so a scan reads the trail in order.
    pub fn append_audit(&self, at_ms: u64, event: &serde_json::Value) -> Result<()> {
        let seq = self.audit_seq.fetch_add(1, Ordering::Relaxed);
        let key = [at_ms.to_be_bytes(), seq.to_be_bytes()].concat();
        let op = self.put_json(Space::Audit, key, event)?;
        self.kv.write(vec![op])
    }

    /// Audit events after the cursor (an opaque key from a previous page), oldest first.
    pub fn scan_audit(
        &self,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, serde_json::Value)>> {
        self.kv
            .scan(Space::Audit, &[], after, limit)?
            .into_iter()
            .map(|(k, v)| {
                let plain = self.unseal(Space::Audit, &k, v)?;
                let ev = serde_json::from_slice(&plain)
                    .map_err(|e| Error::Storage(format!("corrupt audit event: {e}")))?;
                Ok((k, ev))
            })
            .collect()
    }

    pub fn persist(&self) -> Result<()> {
        self.kv.persist()
    }
}

#[cfg(test)]
mod tests;
