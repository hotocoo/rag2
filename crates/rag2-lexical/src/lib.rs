//! BM25 full-text retrieval: one Tantivy index per tenant under `<root>/<tenant>/`.
//!
//! The index is derived data. Each commit records the storage generation it reflects (commit
//! payload); [`LexicalIndex::generation`] lets the engine detect a stale index after a crash and
//! rebuild it from storage.

use parking_lot::{Mutex, RwLock};
use rag2_core::model::{Chunk, Hit};
use rag2_core::{ChunkId, Error, Result, TenantId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::QueryParser;
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{
    Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer, TextAnalyzer,
    WhitespaceTokenizer,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term, doc};

/// Tokens longer than this (bytes) are dropped: they are almost always noise (hashes, base64).
const MAX_TOKEN_BYTES: usize = 40;
const TOKENIZER_NAME: &str = "rag2";
const BYTES_PER_MB: usize = 1024 * 1024;
/// Tantivy's floor for the writer's memory arena.
const MIN_WRITER_HEAP_BYTES: usize = 15 * BYTES_PER_MB;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tokenizer {
    Default,
    EnStem,
    Whitespace,
}

#[derive(Debug, Clone, Copy)]
pub struct LexicalOptions {
    pub writer_heap_mb: usize,
    pub tokenizer: Tokenizer,
}

fn analyzer(t: Tokenizer) -> TextAnalyzer {
    match t {
        Tokenizer::Default => TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(MAX_TOKEN_BYTES))
            .filter(LowerCaser)
            .build(),
        Tokenizer::EnStem => TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(RemoveLongFilter::limit(MAX_TOKEN_BYTES))
            .filter(LowerCaser)
            .filter(Stemmer::new(Language::English))
            .build(),
        Tokenizer::Whitespace => TextAnalyzer::builder(WhitespaceTokenizer::default())
            .filter(RemoveLongFilter::limit(MAX_TOKEN_BYTES))
            .filter(LowerCaser)
            .build(),
    }
}

struct Fields {
    chunk_id: Field,
    doc_id: Field,
    text: Field,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let chunk_id = b.add_u64_field("chunk_id", INDEXED | FAST);
    let doc_id = b.add_text_field("doc_id", STRING);
    let text_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER_NAME)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let text = b.add_text_field("text", text_opts);
    (
        b.build(),
        Fields {
            chunk_id,
            doc_id,
            text,
        },
    )
}

fn tv(e: tantivy::TantivyError) -> Error {
    Error::Storage(format!("lexical index: {e}"))
}

pub struct LexicalIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    fields: Fields,
    parser: QueryParser,
    dirty: AtomicBool,
    pending_generation: Mutex<u64>,
    /// Generation of the last successful commit (what searches currently see).
    committed: AtomicU64,
}

impl LexicalIndex {
    pub fn open(path: &std::path::Path, o: LexicalOptions) -> Result<Self> {
        std::fs::create_dir_all(path).map_err(Error::storage)?;
        let (schema, fields) = schema();
        let dir =
            MmapDirectory::open(path).map_err(|e| Error::Storage(format!("lexical dir: {e}")))?;
        let index = Index::open_or_create(dir, schema).map_err(tv)?;
        index
            .tokenizers()
            .register(TOKENIZER_NAME, analyzer(o.tokenizer));
        let heap = (o.writer_heap_mb * BYTES_PER_MB).max(MIN_WRITER_HEAP_BYTES);
        let writer = index.writer(heap).map_err(tv)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(tv)?;
        let parser = QueryParser::for_index(&index, vec![fields.text]);
        let generation = Self::read_generation(&index);
        Ok(LexicalIndex {
            index,
            reader,
            writer: Mutex::new(writer),
            fields,
            parser,
            dirty: AtomicBool::new(false),
            pending_generation: Mutex::new(generation),
            committed: AtomicU64::new(generation),
        })
    }

    fn read_generation(index: &Index) -> u64 {
        index
            .load_metas()
            .ok()
            .and_then(|m| m.payload)
            .and_then(|p| p.parse().ok())
            .unwrap_or(0)
    }

    /// Storage generation the last commit reflects (and searches see).
    pub fn generation(&self) -> u64 {
        self.committed.load(Ordering::Acquire)
    }

    /// Add chunks (not yet searchable until [`commit`](Self::commit)).
    pub fn add(&self, chunks: &[Chunk]) -> Result<()> {
        let w = self.writer.lock();
        for c in chunks {
            w.add_document(doc!(
                self.fields.chunk_id => c.id.0,
                self.fields.doc_id => c.doc_id.as_str(),
                self.fields.text => c.text.as_str(),
            ))
            .map_err(tv)?;
        }
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    pub fn remove(&self, chunks: &[ChunkId]) -> Result<()> {
        let w = self.writer.lock();
        for c in chunks {
            w.delete_term(Term::from_field_u64(self.fields.chunk_id, c.0));
        }
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    /// Remove everything (before a rebuild).
    pub fn clear(&self) -> Result<()> {
        self.writer.lock().delete_all_documents().map_err(tv)?;
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    /// Force the next [`commit`](Self::commit) to write (e.g. to record a generation).
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Record that pending writes reflect storage generation `g`.
    pub fn set_generation(&self, g: u64) {
        let mut p = self.pending_generation.lock();
        *p = (*p).max(g);
    }

    /// Make pending writes durable and searchable. No-op when nothing changed.
    pub fn commit(&self) -> Result<bool> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(false);
        }
        let generation = *self.pending_generation.lock();
        let mut w = self.writer.lock();
        let mut prepared = w.prepare_commit().map_err(tv)?;
        prepared.set_payload(&generation.to_string());
        if let Err(e) = prepared.commit() {
            self.dirty.store(true, Ordering::Release);
            return Err(tv(e));
        }
        drop(w);
        self.reader.reload().map_err(tv)?;
        self.committed.store(generation, Ordering::Release);
        Ok(true)
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// BM25 top-k. Query syntax errors never fail the search: unparseable parts are treated as
    /// plain terms (lenient parsing), since queries come from end users.
    pub fn search(&self, query: &str, k: usize) -> Result<Vec<Hit>> {
        if k == 0 || query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let (q, _errors) = self.parser.parse_query_lenient(query);
        let searcher = self.reader.searcher();
        let top = searcher
            .search(&q, &TopDocs::with_limit(k).order_by_score())
            .map_err(tv)?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let col = searcher
                .segment_reader(addr.segment_ord)
                .fast_fields()
                .u64("chunk_id")
                .map_err(tv)?;
            if let Some(id) = col.first(addr.doc_id) {
                hits.push(Hit {
                    chunk: ChunkId(id),
                    score,
                });
            }
        }
        Ok(hits)
    }

    /// Document frequency of `term` after analysis, and total docs: for salience in multi-hop.
    pub fn doc_freq(&self, word: &str) -> Result<(u64, u64)> {
        let searcher = self.reader.searcher();
        let mut tokens = self
            .index
            .tokenizers()
            .get(TOKENIZER_NAME)
            .ok_or_else(|| Error::Internal("tokenizer missing".into()))?;
        let mut stream = tokens.token_stream(word);
        let Some(tok) = stream.next().map(|t| t.text.clone()) else {
            return Ok((0, searcher.num_docs()));
        };
        let term = Term::from_field_text(self.fields.text, &tok);
        Ok((searcher.doc_freq(&term).map_err(tv)?, searcher.num_docs()))
    }
}

/// All tenants' lexical indexes, opened on first use.
pub struct LexicalIndexes {
    root: PathBuf,
    opts: LexicalOptions,
    open: RwLock<HashMap<TenantId, Arc<LexicalIndex>>>,
}

impl LexicalIndexes {
    pub fn new(root: impl Into<PathBuf>, opts: LexicalOptions) -> Self {
        LexicalIndexes {
            root: root.into(),
            opts,
            open: RwLock::new(HashMap::new()),
        }
    }

    pub fn get(&self, t: &TenantId) -> Result<Arc<LexicalIndex>> {
        if let Some(ix) = self.open.read().get(t) {
            return Ok(ix.clone());
        }
        let mut w = self.open.write();
        if let Some(ix) = w.get(t) {
            return Ok(ix.clone());
        }
        // Tenant ids are validated to a path-safe charset, so this join cannot escape `root`.
        let ix = Arc::new(LexicalIndex::open(&self.root.join(t.as_str()), self.opts)?);
        w.insert(t.clone(), ix.clone());
        Ok(ix)
    }

    /// Commit every index with pending writes; returns how many committed.
    pub fn commit_all(&self) -> Result<usize> {
        let all: Vec<Arc<LexicalIndex>> = self.open.read().values().cloned().collect();
        let mut n = 0;
        for ix in all {
            if ix.commit()? {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Close and delete a tenant's index directory.
    pub fn drop_tenant(&self, t: &TenantId) -> Result<()> {
        self.open.write().remove(t);
        let p = self.root.join(t.as_str());
        if p.exists() {
            std::fs::remove_dir_all(p).map_err(Error::storage)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rag2_core::DocumentId;

    fn chunk(id: u64, text: &str) -> Chunk {
        Chunk {
            id: ChunkId(id),
            doc_id: DocumentId::new("d").expect("id"),
            ordinal: 0,
            text: text.into(),
            start: 0,
            end: 0,
            hash: 0,
        }
    }

    fn opts(t: Tokenizer) -> LexicalOptions {
        LexicalOptions {
            writer_heap_mb: 16,
            tokenizer: t,
        }
    }

    #[test]
    fn bm25_ranks_the_matching_chunk_first_and_commits_are_needed_for_visibility() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ix = LexicalIndex::open(dir.path(), opts(Tokenizer::Default)).expect("open");
        ix.add(&[
            chunk(1, "the reactor coolant pump failed"),
            chunk(2, "quarterly sales grew"),
            chunk(3, "pump maintenance schedule"),
        ])
        .expect("add");
        assert!(
            ix.search("pump", 5).expect("search").is_empty(),
            "uncommitted writes are invisible"
        );
        ix.set_generation(7);
        assert!(ix.commit().expect("commit"));
        let hits = ix.search("coolant pump", 5).expect("search");
        assert_eq!(hits[0].chunk, ChunkId(1));
        assert_eq!(hits.len(), 2);
        assert_eq!(ix.generation(), 7);
        assert!(!ix.commit().expect("noop commit"));
    }

    #[test]
    fn removed_chunks_disappear_and_bad_syntax_does_not_fail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ix = LexicalIndex::open(dir.path(), opts(Tokenizer::Default)).expect("open");
        ix.add(&[chunk(1, "alpha beta"), chunk(2, "alpha gamma")])
            .expect("add");
        ix.commit().expect("commit");
        ix.remove(&[ChunkId(1)]).expect("remove");
        ix.commit().expect("commit");
        let hits = ix.search("alpha", 5).expect("search");
        assert_eq!(hits.iter().map(|h| h.chunk.0).collect::<Vec<_>>(), vec![2]);
        assert!(ix.search("alpha AND (\"unclosed", 5).is_ok());
        assert!(ix.search("   ", 5).expect("empty").is_empty());
    }

    #[test]
    fn stemming_matches_inflections_when_configured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ix = LexicalIndex::open(dir.path(), opts(Tokenizer::EnStem)).expect("open");
        ix.add(&[chunk(1, "the engines were running")])
            .expect("add");
        ix.commit().expect("commit");
        assert_eq!(ix.search("engine run", 5).expect("search").len(), 1);
    }

    #[test]
    fn tenants_get_separate_indexes_and_generation_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let all = LexicalIndexes::new(dir.path(), opts(Tokenizer::Default));
        let (a, b) = (
            TenantId::new("a").expect("t"),
            TenantId::new("b").expect("t"),
        );
        let ia = all.get(&a).expect("a");
        ia.add(&[chunk(1, "only in tenant a")]).expect("add");
        ia.set_generation(3);
        all.get(&b).expect("b");
        assert_eq!(all.commit_all().expect("commit"), 1);
        assert!(
            all.get(&b)
                .expect("b")
                .search("tenant", 5)
                .expect("search")
                .is_empty()
        );
        drop((ia, all));
        let again = LexicalIndexes::new(dir.path(), opts(Tokenizer::Default));
        assert_eq!(again.get(&a).expect("a").generation(), 3);
        assert_eq!(again.get(&a).expect("a").num_docs(), 1);
    }
}
