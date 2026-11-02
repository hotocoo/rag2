//! Ingestion: validate, skip unchanged documents (content hash), chunk, embed, store atomically,
//! then update derived indexes. Storage is the source of truth; derived indexes carry the storage
//! generation so a crash between the two is repaired by [`Engine::recover_tenant`].

use crate::api::{IngestReport, IngestStatus, IngestedDocument};
use crate::chunker::chunk;
use crate::{Engine, ingest_embedders, vectors};
use rag2_config::store::now_ms as store_time_ms;
use rag2_core::interfaces::Embedder;
use rag2_core::model::{Chunk, Document, DocumentRecord};
use rag2_core::{ChunkId, DocumentId, Error, Result, TenantId};
use rag2_lexical::LexicalIndex;
use std::sync::Arc;
use vectors::TenantVectors;

/// Hash of everything that affects indexing; equal hashes mean the document is unchanged.
fn content_hash(d: &Document) -> u64 {
    let meta = serde_json::to_string(&d.metadata).unwrap_or_default();
    let parts = [
        d.title.as_deref().unwrap_or(""),
        d.source.as_deref().unwrap_or(""),
        &meta,
        &d.text,
    ];
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    for p in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    h.digest()
}

/// Hash for duplicate detection: case- and whitespace-insensitive.
pub fn normalized_hash(text: &str) -> u64 {
    let norm: String = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    xxhash_rust::xxh3::xxh3_64(norm.as_bytes())
}

fn embed_checked(e: &dyn Embedder, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let out = e.embed(texts)?;
    if out.len() != texts.len()
        || out
            .iter()
            .any(|v| v.len() != e.dim() || v.iter().any(|x| !x.is_finite()))
    {
        return Err(Error::Unavailable(format!(
            "embedder {} returned malformed vectors",
            e.id()
        )));
    }
    Ok(out)
}

impl Engine {
    /// Ingest documents for a tenant. The whole request is one atomic storage write (one fsync):
    /// every document is stored with its chunks and vectors, or none is. Unchanged documents
    /// (same content hash) are skipped.
    pub fn ingest(&self, t: &TenantId, docs: Vec<Document>) -> Result<IngestReport> {
        let active = self.config.current();
        self.check_tenant(&active, t)?;
        let p = &active.profile;
        if docs.is_empty() {
            return Err(Error::InvalidInput("no documents".into()));
        }
        if docs.len() > p.limits.max_batch_documents {
            return Err(Error::InvalidInput(format!(
                "at most {} documents per request",
                p.limits.max_batch_documents
            )));
        }
        for d in &docs {
            if d.text.len() > p.limits.max_document_bytes {
                return Err(Error::InvalidInput(format!(
                    "document {} exceeds limits.max_document_bytes ({})",
                    d.id, p.limits.max_document_bytes
                )));
            }
        }
        let comps = self.components();
        let mut dropped = Vec::new();
        let docs = self.transform_documents(t, &p.ingest_plugins, &comps, docs, &mut dropped)?;
        let lock = self.ingest_lock(t);
        let _g = lock.lock();
        let existing = self.storage.document_count(t)?;
        let quota = p.tenant(t).max_documents;
        if existing + docs.len() as u64 > quota {
            let new = docs
                .iter()
                .filter(|d| self.storage.get_document(t, &d.id).ok().flatten().is_none())
                .count() as u64;
            if existing + new > quota {
                return Err(Error::Forbidden(format!(
                    "tenant {t} document quota ({quota}) exceeded"
                )));
            }
        }
        let embedders: Vec<(String, Arc<dyn Embedder>)> = ingest_embedders(p)
            .into_iter()
            .filter_map(|n| comps.embedders.get(&n).cloned().map(|e| (n, e)))
            .collect();
        let tv = self.tenant_vectors(t)?;
        let lex = self.lexical.get(t)?;
        // The last occurrence of an id in one request wins.
        let mut seen = std::collections::HashSet::new();
        let mut docs: Vec<Document> = docs
            .into_iter()
            .rev()
            .filter(|d| seen.insert(d.id.clone()))
            .collect();
        docs.reverse();
        let mut report: Vec<Option<IngestedDocument>> = vec![None; docs.len()];
        // Phase 1: skip unchanged documents, chunk the rest.
        struct Pending {
            slot: usize,
            doc: Document,
            hash: u64,
            old_version: Option<u64>,
            spans: Vec<crate::chunker::Span>,
        }
        let mut pending = Vec::new();
        for (slot, d) in docs.into_iter().enumerate() {
            let hash = content_hash(&d);
            let old = self.storage.get_document(t, &d.id)?;
            if let Some(o) = &old
                && o.content_hash == hash
            {
                report[slot] = Some(IngestedDocument {
                    id: d.id,
                    status: IngestStatus::Unchanged,
                    chunks: o.chunks.len(),
                    version: o.version,
                });
                continue;
            }
            let spans = chunk(&d.text, &p.chunking);
            pending.push(Pending {
                slot,
                old_version: old.map(|o| o.version),
                doc: d,
                hash,
                spans,
            });
        }
        // Phase 2: ids for every new chunk in one reservation, then build the chunks.
        let total: u64 = pending.iter().map(|x| x.spans.len() as u64).sum();
        let mut next_id = self.storage.allocate_chunk_ids(t, total)?.start;
        let mut chunked: Vec<(Pending, Vec<Chunk>)> = Vec::with_capacity(pending.len());
        for x in pending {
            let chunks: Vec<Chunk> = x
                .spans
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let text = x.doc.text[s.start..s.end].to_string();
                    let id = ChunkId(next_id);
                    next_id += 1;
                    Chunk {
                        id,
                        doc_id: x.doc.id.clone(),
                        ordinal: i as u32,
                        hash: normalized_hash(&text),
                        text,
                        start: s.start as u32,
                        end: s.end as u32,
                    }
                })
                .collect();
            chunked.push((x, chunks));
        }
        // Phase 3: embed every chunk of the request in one call per embedder (models batch it).
        let texts: Vec<&str> = chunked
            .iter()
            .flat_map(|(_, cs)| cs.iter().map(|c| c.text.as_str()))
            .collect();
        let mut by_embedder: Vec<Vec<Vec<f32>>> = Vec::with_capacity(embedders.len());
        for (_, e) in &embedders {
            by_embedder.push(embed_checked(e.as_ref(), &texts)?);
        }
        let now = store_time_ms();
        let mut records = Vec::with_capacity(chunked.len());
        let mut vecs_per_doc: Vec<Vec<(&str, ChunkId, Vec<f32>)>> =
            Vec::with_capacity(chunked.len());
        let mut offset = 0;
        for (x, chunks) in &chunked {
            let mut vecs = Vec::with_capacity(chunks.len() * embedders.len());
            for (ei, (name, _)) in embedders.iter().enumerate() {
                for (j, c) in chunks.iter().enumerate() {
                    vecs.push((
                        name.as_str(),
                        c.id,
                        std::mem::take(&mut by_embedder[ei][offset + j]),
                    ));
                }
            }
            offset += chunks.len();
            records.push(DocumentRecord {
                id: x.doc.id.clone(),
                title: x.doc.title.clone(),
                source: x.doc.source.clone(),
                metadata: x.doc.metadata.clone(),
                version: x.old_version.map_or(1, |v| v + 1),
                content_hash: x.hash,
                chunks: chunks.iter().map(|c| c.id).collect(),
                embedders: embedders.iter().map(|(n, _)| n.clone()).collect(),
                ingested_at_ms: now,
            });
            vecs_per_doc.push(vecs);
        }
        // Phase 4: one atomic storage write for the whole request.
        let writes: Vec<rag2_storage::DocumentWrite<'_>> = records
            .iter()
            .zip(&chunked)
            .zip(&vecs_per_doc)
            .map(
                |((record, (_, chunks)), vectors)| rag2_storage::DocumentWrite {
                    record,
                    chunks,
                    vectors,
                },
            )
            .collect();
        let generation = if writes.is_empty() {
            self.storage.generation(t)?
        } else {
            let (replaced, g) = self.storage.put_documents(t, &writes)?;
            // Phase 5: derived indexes and caches.
            let docs: Vec<IndexedDoc<'_>> = records
                .iter()
                .zip(&chunked)
                .zip(&vecs_per_doc)
                .map(|((r, (_, cs)), vs)| (r, cs.as_slice(), vs.as_slice()))
                .collect();
            self.index_put(t, &tv, &lex, &docs, &replaced, g)?;
            for ((x, chunks), (rec, replaced)) in chunked.iter().zip(records.iter().zip(&replaced))
            {
                report[x.slot] = Some(IngestedDocument {
                    id: rec.id.clone(),
                    status: if replaced.is_some() {
                        IngestStatus::Updated
                    } else {
                        IngestStatus::Created
                    },
                    chunks: chunks.len(),
                    version: rec.version,
                });
            }
            g
        };
        let report: Vec<IngestedDocument> = report
            .into_iter()
            .flatten()
            .chain(dropped.into_iter().map(|id| IngestedDocument {
                id,
                status: IngestStatus::Dropped,
                chunks: 0,
                version: 0,
            }))
            .collect();
        let interval = p.indexes.lexical.commit_interval_ms;
        if interval == 0 {
            lex.commit()?;
        }
        self.maybe_rebuild_vectors(t, &tv)?;
        Ok(IngestReport {
            documents: report,
            generation,
            searchable_within_ms: interval,
        })
    }

    /// Delete documents; returns how many existed.
    pub fn delete_documents(&self, t: &TenantId, ids: &[DocumentId]) -> Result<usize> {
        let active = self.config.current();
        self.check_tenant(&active, t)?;
        let lock = self.ingest_lock(t);
        let _g = lock.lock();
        let tv = self.tenant_vectors(t)?;
        let lex = self.lexical.get(t)?;
        let mut n = 0;
        for id in ids {
            let Some((old, g)) = self.storage.delete_document(t, id)? else {
                continue;
            };
            n += 1;
            self.index_delete(t, &tv, &lex, std::slice::from_ref(&old), g)?;
        }
        if active.profile.indexes.lexical.commit_interval_ms == 0 {
            lex.commit()?;
        }
        self.maybe_rebuild_vectors(t, &tv)?;
        Ok(n)
    }

    pub(crate) fn maybe_rebuild_vectors(
        &self,
        t: &TenantId,
        tv: &vectors::TenantVectors,
    ) -> Result<()> {
        let cfg = &self.config.current().profile.indexes.vector;
        for name in tv.embedders() {
            let Some(ix) = tv.get(&name) else { continue };
            if vectors::TenantVectors::needs_rebuild(ix.as_ref(), cfg) {
                tracing::info!(tenant = %t, embedder = %name, kind = ix.kind(), vectors = ix.len(), "rebuilding vector index");
                tv.set(
                    &name,
                    vectors::build_from_storage(&self.storage, t, &name, ix.dim(), cfg)?,
                );
            }
        }
        Ok(())
    }
}

/// Re-embed every chunk of a tenant with the current embedders and rebuild all derived indexes.
pub(crate) fn reembed_tenant(e: &Engine, t: &TenantId) -> Result<usize> {
    let active = e.config.current();
    let p = &active.profile;
    let comps = e.components();
    let embedders: Vec<(String, Arc<dyn Embedder>)> = ingest_embedders(p)
        .into_iter()
        .filter_map(|n| comps.embedders.get(&n).cloned().map(|x| (n, x)))
        .collect();
    let lex = e.lexical.get(t)?;
    lex.clear()?;
    let mut after: Option<DocumentId> = None;
    let mut docs = 0;
    loop {
        let page = e.storage.list_documents(t, after.as_ref(), 256)?;
        let Some(last) = page.last() else { break };
        after = Some(last.id.clone());
        for mut rec in page {
            let chunks: Vec<Chunk> = e
                .storage
                .get_chunks(t, &rec.chunks)?
                .into_iter()
                .flatten()
                .collect();
            let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
            let mut vecs: Vec<(&str, ChunkId, Vec<f32>)> = Vec::new();
            for (name, emb) in &embedders {
                for (c, v) in chunks.iter().zip(embed_checked(emb.as_ref(), &texts)?) {
                    vecs.push((name.as_str(), c.id, v));
                }
            }
            rec.embedders = embedders.iter().map(|(n, _)| n.clone()).collect();
            let (_, g) = e.storage.put_document(t, &rec, &chunks, &vecs)?;
            lex.add(&chunks)?;
            lex.set_generation(g);
            docs += 1;
        }
    }
    lex.mark_dirty();
    lex.commit()?;
    let tv = Arc::new(vectors::TenantVectors::default());
    for (name, emb) in &embedders {
        tv.set(
            name,
            vectors::build_from_storage(&e.storage, t, name, emb.dim(), &p.indexes.vector)?,
        );
    }
    e.vectors.write().insert(t.clone(), tv);
    Ok(docs)
}

impl Engine {
    /// Reflect a committed storage write in the derived indexes and caches.
    pub(crate) fn index_put(
        &self,
        t: &TenantId,
        tv: &TenantVectors,
        lex: &LexicalIndex,
        docs: &[IndexedDoc<'_>],
        replaced: &[Option<DocumentRecord>],
        generation: u64,
    ) -> Result<()> {
        for (rec, _, _) in docs {
            // Absence is never cached, so only previously present records can be stale.
            self.doc_cache.invalidate(&(t.clone(), rec.id.clone()));
        }
        let gone: Vec<DocumentRecord> = replaced.iter().flatten().cloned().collect();
        self.unindex(t, tv, lex, &gone)?;
        let vcfg = self.config.current().profile.indexes.vector.clone();
        for (_, chunks, vecs) in docs {
            lex.add(chunks)?;
            for (name, id, v) in vecs.iter() {
                match tv.get(name) {
                    Some(ix) => ix.upsert(id.0, v)?,
                    None => tv.set(
                        name,
                        vectors::build_from_storage(&self.storage, t, name, v.len(), &vcfg)?,
                    ),
                }
            }
        }
        lex.set_generation(generation);
        Ok(())
    }

    /// Reflect committed deletions in the derived indexes and caches.
    pub(crate) fn index_delete(
        &self,
        t: &TenantId,
        tv: &TenantVectors,
        lex: &LexicalIndex,
        gone: &[DocumentRecord],
        generation: u64,
    ) -> Result<()> {
        self.unindex(t, tv, lex, gone)?;
        lex.set_generation(generation);
        Ok(())
    }

    fn unindex(
        &self,
        t: &TenantId,
        tv: &TenantVectors,
        lex: &LexicalIndex,
        gone: &[DocumentRecord],
    ) -> Result<()> {
        for old in gone {
            self.invalidate(t, old);
            lex.remove(&old.chunks)?;
            for name in tv.embedders() {
                if let Some(ix) = tv.get(&name) {
                    for c in &old.chunks {
                        ix.remove(c.0)?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl Engine {
    /// Run `document_transform` plugins in order. A plugin returning `null` drops the document;
    /// a failing plugin fails the request (ingesting untransformed data could leak what the plugin
    /// exists to remove).
    fn transform_documents(
        &self,
        t: &TenantId,
        names: &[String],
        comps: &crate::components::Components,
        docs: Vec<Document>,
        dropped: &mut Vec<DocumentId>,
    ) -> Result<Vec<Document>> {
        if names.is_empty() {
            return Ok(docs);
        }
        let mut out = Vec::with_capacity(docs.len());
        'doc: for d in docs {
            let id = d.id.clone();
            let mut v = serde_json::to_value(&d).map_err(Error::internal)?;
            for name in names {
                let p = comps.plugins.get(name).ok_or_else(|| {
                    Error::Unavailable(format!("ingest plugin {name} not loaded"))
                })?;
                v = p.call(
                    rag2_core::interfaces::hooks::DOCUMENT_TRANSFORM,
                    t.as_str(),
                    &v,
                )?;
                if v.is_null() {
                    dropped.push(id);
                    continue 'doc;
                }
            }
            let d: Document = serde_json::from_value(v).map_err(|e| {
                Error::InvalidInput(format!("ingest plugin produced an invalid document: {e}"))
            })?;
            out.push(d);
        }
        Ok(out)
    }
}

/// A stored document with its chunks and vectors, as the derived indexes receive it.
pub(crate) type IndexedDoc<'a> = (
    &'a DocumentRecord,
    &'a [Chunk],
    &'a [(&'a str, ChunkId, Vec<f32>)],
);

/// Metadata field marking which connector produced a document.
pub const SOURCE_FIELD: &str = "_source";

/// Outcome of a connector sync.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, schemars::JsonSchema)]
pub struct SyncReport {
    pub fetched: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub dropped: usize,
    pub deleted: usize,
}

impl Engine {
    /// Make the tenant hold exactly `docs` for `source`: ingest them (incrementally, tagged with
    /// `_source`) and, when `delete_missing`, delete that source's documents not in `docs`.
    pub fn sync_source(
        &self,
        t: &TenantId,
        source: &str,
        docs: Vec<Document>,
        delete_missing: bool,
    ) -> Result<SyncReport> {
        let batch = self
            .config
            .current()
            .profile
            .limits
            .max_batch_documents
            .max(1);
        let mut report = SyncReport {
            fetched: docs.len(),
            ..Default::default()
        };
        let mut seen = std::collections::HashSet::new();
        let mut tagged = Vec::with_capacity(docs.len());
        for mut d in docs {
            d.metadata.insert(
                SOURCE_FIELD.into(),
                serde_json::Value::String(source.into()),
            );
            seen.insert(d.id.clone());
            tagged.push(d);
        }
        for group in tagged.chunks(batch) {
            for d in self.ingest(t, group.to_vec())?.documents {
                match d.status {
                    IngestStatus::Created => report.created += 1,
                    IngestStatus::Updated => report.updated += 1,
                    IngestStatus::Unchanged => report.unchanged += 1,
                    IngestStatus::Dropped => report.dropped += 1,
                }
            }
        }
        if delete_missing {
            let tag = serde_json::Value::String(source.into());
            let gone: Vec<DocumentId> = self
                .storage
                .documents_with(t, SOURCE_FIELD, &tag, usize::MAX)?
                .into_iter()
                .filter(|id| !seen.contains(id))
                .collect();
            if !gone.is_empty() {
                report.deleted = self.delete_documents(t, &gone)?;
            }
        }
        Ok(report)
    }
}
