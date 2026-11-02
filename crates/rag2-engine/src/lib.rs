//! The RAG2 engine: ingestion and adaptive retrieval over tenant-scoped storage.
//!
//! Every tunable comes from the active profile ([`rag2_config::ConfigManager::current`]), read
//! once per request so a configuration activation never changes behaviour mid-request.

pub mod api;
pub mod chunker;
pub mod components;
pub mod ingest;
pub mod query;
mod replica;
pub mod text;
pub mod vectors;

use components::{ComponentFactory, Components, Registry};
use parking_lot::{Mutex, RwLock};
use rag2_config::schema::{LexicalTokenizer, Profile, StorageBackend};
use rag2_config::{Active, ConfigManager};
use rag2_core::model::{DocumentRecord, RetrieverKind};
use rag2_core::{DocumentId, Error, Result, TenantId};
use rag2_lexical::{LexicalIndexes, LexicalOptions, Tokenizer};
use rag2_storage::{BackendKind, Storage, StorageOptions};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use vectors::TenantVectors;

pub use api::*;
pub use ingest::{SOURCE_FIELD, SyncReport};

const BYTES_PER_MB: u64 = 1024 * 1024;

pub struct Engine {
    config: Arc<ConfigManager>,
    storage: Arc<Storage>,
    lexical: LexicalIndexes,
    vectors: RwLock<HashMap<TenantId, Arc<TenantVectors>>>,
    registry: Registry,
    components: RwLock<Arc<Components>>,
    /// Serializes ingest per tenant (index promotion, replace semantics).
    ingest_locks: Mutex<HashMap<TenantId, Arc<Mutex<()>>>>,
    cache: moka::sync::Cache<u64, Arc<QueryResponse>>,
    chunk_cache: moka::sync::Cache<(TenantId, u64), Arc<rag2_core::model::Chunk>>,
    doc_cache: moka::sync::Cache<(TenantId, DocumentId), Arc<DocumentRecord>>,
}

/// Secrets resolve at startup; the key must decode to 32 bytes (base64 or 64 hex chars).
fn decode_key(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return (0..32)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(Error::internal))
            .collect();
    }
    Err(Error::InvalidInput(
        "storage.encryption_key must resolve to 64 hex characters (32 bytes)".into(),
    ))
}

/// Embedders that ingestion must embed with: every embedder used by an enabled dense retriever.
pub fn ingest_embedders(p: &Profile) -> BTreeSet<String> {
    p.pipelines
        .values()
        .flat_map(|pl| pl.retrievers.iter())
        .filter(|r| r.enabled && r.kind == RetrieverKind::Dense)
        .filter_map(|r| r.embedder.clone())
        .collect()
}

impl Engine {
    /// Open storage and indexes as the active profile describes. Storage location, backend and
    /// encryption are fixed for the process lifetime; changing them is refused at activation and
    /// takes effect on restart.
    pub fn open(
        config: Arc<ConfigManager>,
        factories: Vec<Arc<dyn ComponentFactory>>,
    ) -> Result<Arc<Engine>> {
        let active = config.current();
        let p = &active.profile;
        let root = PathBuf::from(&p.storage.data_dir);
        let encryption_key = p
            .storage
            .encryption_key
            .as_ref()
            .map(|r| {
                r.resolve()
                    .map_err(Error::InvalidInput)
                    .and_then(|k| decode_key(&k))
            })
            .transpose()?;
        let storage = Arc::new(Storage::open(StorageOptions {
            backend: match p.storage.backend {
                StorageBackend::Fjall => BackendKind::Fjall,
                StorageBackend::Redb => BackendKind::Redb,
            },
            path: root.join("storage"),
            cache_bytes: p.storage.cache_mb * BYTES_PER_MB,
            sync_writes: true,
            encryption_key,
            replication_log: p.cluster.role == rag2_config::schema::ClusterRole::Primary,
        })?);
        let lexical = LexicalIndexes::new(
            root.join("lexical"),
            LexicalOptions {
                writer_heap_mb: p.indexes.lexical.writer_heap_mb,
                tokenizer: match p.indexes.lexical.tokenizer {
                    LexicalTokenizer::Default => Tokenizer::Default,
                    LexicalTokenizer::EnStem => Tokenizer::EnStem,
                    LexicalTokenizer::Whitespace => Tokenizer::Whitespace,
                },
            },
        );
        let mut all: Vec<Arc<dyn ComponentFactory>> = factories;
        all.push(Arc::new(components::BuiltinFactory));
        let registry = Registry::new(all);
        let comps = Arc::new(registry.build(p)?);
        let engine = Arc::new(Engine {
            cache: moka::sync::Cache::builder()
                .max_capacity(p.cache.query_entries)
                .time_to_live(std::time::Duration::from_millis(p.cache.query_ttl_ms))
                .build(),
            chunk_cache: moka::sync::Cache::new(p.cache.chunk_entries),
            doc_cache: moka::sync::Cache::new(p.cache.document_entries),
            config: config.clone(),
            storage,
            lexical,
            vectors: RwLock::new(HashMap::new()),
            registry,
            components: RwLock::new(comps),
            ingest_locks: Mutex::new(HashMap::new()),
        });
        engine.install_config_hooks();
        Ok(engine)
    }

    fn install_config_hooks(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let fixed = self.config.current().profile.storage.clone();
        let fixed_cluster = self.config.current().profile.cluster.clone();
        self.config.add_veto(move |p| {
            if p.storage != fixed {
                return Err("storage settings (backend, data_dir, cache, encryption) change only on restart".into());
            }
            if p.cluster != fixed_cluster {
                return Err("cluster settings change only on restart".into());
            }
            let Some(e) = weak.upgrade() else { return Ok(()) };
            e.registry.build(p).map(drop).map_err(|e| e.to_string())
        });
        let weak = Arc::downgrade(self);
        self.config.on_change(move |active| {
            let Some(e) = weak.upgrade() else { return };
            match e.registry.build(&active.profile) {
                Ok(c) => *e.components.write() = Arc::new(c),
                Err(err) => {
                    tracing::error!(error = %err, "component rebuild failed after activation")
                }
            }
            // Cached responses carry the config version in their key; nothing else to invalidate.
        });
    }

    pub fn config(&self) -> &Arc<ConfigManager> {
        &self.config
    }

    pub fn storage(&self) -> &Arc<Storage> {
        &self.storage
    }

    fn components(&self) -> Arc<Components> {
        self.components.read().clone()
    }

    /// The embedders and decision models of the active configuration.
    pub fn component_snapshot(&self) -> Arc<Components> {
        self.components()
    }

    fn check_tenant(&self, active: &Active, t: &TenantId) -> Result<()> {
        if !active.profile.tenant(t).enabled {
            return Err(Error::Forbidden(format!("tenant {t} is disabled")));
        }
        Ok(())
    }

    /// The tenant's vector indexes, loading them from storage on first use.
    fn tenant_vectors(&self, t: &TenantId) -> Result<Arc<TenantVectors>> {
        if let Some(v) = self.vectors.read().get(t) {
            return Ok(v.clone());
        }
        let active = self.config.current();
        let comps = self.components();
        let tv = Arc::new(TenantVectors::default());
        for name in ingest_embedders(&active.profile) {
            let Some(emb) = comps.embedders.get(&name) else {
                continue;
            };
            let ix = vectors::build_from_storage(
                &self.storage,
                t,
                &name,
                emb.dim(),
                &active.profile.indexes.vector,
            )?;
            tv.set(&name, ix);
        }
        let mut w = self.vectors.write();
        Ok(w.entry(t.clone()).or_insert(tv).clone())
    }

    fn ingest_lock(&self, t: &TenantId) -> Arc<Mutex<()>> {
        self.ingest_locks
            .lock()
            .entry(t.clone())
            .or_default()
            .clone()
    }

    /// Bring a tenant's derived indexes in line with storage: rebuild the lexical index when its
    /// committed generation differs from storage's, and load vector indexes. Called at startup
    /// for every known tenant.
    pub fn recover_tenant(&self, t: &TenantId) -> Result<bool> {
        let lex = self.lexical.get(t)?;
        let stored = self.storage.generation(t)?;
        let rebuilt = lex.generation() != stored;
        if rebuilt {
            tracing::warn!(tenant = %t, index = lex.generation(), storage = stored, "lexical index stale; rebuilding");
            lex.clear()?;
            let mut after = None;
            loop {
                let page = self.storage.scan_chunks(t, after, 1_024)?;
                let Some(last) = page.last() else { break };
                after = Some(last.id);
                lex.add(&page)?;
            }
            lex.set_generation(stored);
            // An empty rebuild must still record the generation.
            lex.mark_dirty();
            lex.commit()?;
        }
        self.tenant_vectors(t)?;
        Ok(rebuilt)
    }

    /// Recover every tenant in storage.
    pub fn recover_all(&self) -> Result<usize> {
        let mut n = 0;
        for t in self.storage.tenants()? {
            if self.recover_tenant(&t)? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Commit pending lexical writes (run on the configured interval).
    pub fn commit_lexical(&self) -> Result<usize> {
        self.lexical.commit_all()
    }

    /// A chunk through the chunk cache. Chunks are immutable and ids are never reused.
    pub(crate) fn chunk(
        &self,
        t: &TenantId,
        id: rag2_core::ChunkId,
    ) -> Result<Option<Arc<rag2_core::model::Chunk>>> {
        let key = (t.clone(), id.0);
        if let Some(c) = self.chunk_cache.get(&key) {
            return Ok(Some(c));
        }
        let c = self.storage.get_chunk(t, id)?.map(Arc::new);
        if let Some(c) = &c {
            self.chunk_cache.insert(key, c.clone());
        }
        Ok(c)
    }

    /// A document record through the document cache (writes invalidate it).
    pub(crate) fn record(
        &self,
        t: &TenantId,
        id: &DocumentId,
    ) -> Result<Option<Arc<DocumentRecord>>> {
        let key = (t.clone(), id.clone());
        if let Some(d) = self.doc_cache.get(&key) {
            return Ok(Some(d));
        }
        let d = self.storage.get_document(t, id)?.map(Arc::new);
        if let Some(d) = &d {
            self.doc_cache.insert(key, d.clone());
        }
        Ok(d)
    }

    /// Drop cached state of documents that were replaced or deleted.
    pub(crate) fn invalidate(&self, t: &TenantId, old: &DocumentRecord) {
        self.doc_cache.invalidate(&(t.clone(), old.id.clone()));
        for c in &old.chunks {
            self.chunk_cache.invalidate(&(t.clone(), c.0));
        }
    }

    pub fn get_document(&self, t: &TenantId, id: &DocumentId) -> Result<Option<DocumentRecord>> {
        self.check_tenant(&self.config.current(), t)?;
        self.storage.get_document(t, id)
    }

    pub fn list_documents(
        &self,
        t: &TenantId,
        after: Option<&DocumentId>,
        limit: usize,
    ) -> Result<Vec<DocumentRecord>> {
        self.check_tenant(&self.config.current(), t)?;
        self.storage.list_documents(t, after, limit)
    }

    pub fn document_chunks(&self, t: &TenantId, id: &DocumentId) -> Result<Vec<ChunkView>> {
        let Some(rec) = self.get_document(t, id)? else {
            return Err(Error::NotFound(format!("document {id}")));
        };
        Ok(self
            .storage
            .get_chunks(t, &rec.chunks)?
            .into_iter()
            .flatten()
            .map(|c| ChunkView {
                id: c.id,
                ordinal: c.ordinal,
                text: c.text,
            })
            .collect())
    }

    pub fn stats(&self, t: &TenantId) -> Result<TenantStats> {
        let tv = self.tenant_vectors(t)?;
        let mut vector_indexes: Vec<VectorIndexStats> = tv
            .embedders()
            .into_iter()
            .filter_map(|e| {
                tv.get(&e).map(|ix| VectorIndexStats {
                    embedder: e,
                    kind: ix.kind().into(),
                    vectors: ix.len(),
                    dim: ix.dim(),
                })
            })
            .collect();
        vector_indexes.sort_by(|a, b| a.embedder.cmp(&b.embedder));
        Ok(TenantStats {
            documents: self.storage.document_count(t)?,
            generation: self.storage.generation(t)?,
            lexical_docs: self.lexical.get(t)?.num_docs(),
            vector_indexes,
        })
    }

    pub fn tenants(&self) -> Result<Vec<TenantId>> {
        self.storage.tenants()
    }

    /// Delete a tenant and all its data. Irreversible.
    pub fn delete_tenant(&self, t: &TenantId) -> Result<u64> {
        let lock = self.ingest_lock(t);
        let _g = lock.lock();
        let n = self.storage.delete_tenant(t)?;
        self.chunk_cache.invalidate_all();
        self.doc_cache.invalidate_all();
        self.cache.invalidate_all();
        self.lexical.drop_tenant(t)?;
        self.vectors.write().remove(t);
        Ok(n)
    }

    /// Rebuild every derived index of a tenant from stored text (after an embedder change).
    pub fn reindex(&self, t: &TenantId) -> Result<usize> {
        let lock = self.ingest_lock(t);
        let _g = lock.lock();
        ingest::reembed_tenant(self, t)
    }

    pub fn flush(&self) -> Result<()> {
        self.lexical.commit_all()?;
        self.storage.persist()
    }
}

#[cfg(test)]
mod tests;
