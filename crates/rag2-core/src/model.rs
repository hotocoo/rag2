//! Data model: documents in, evidence out.

use crate::ids::{ChunkId, DocumentId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Free-form, caller-supplied attributes. Used by structured retrieval filters.
pub type Metadata = BTreeMap<String, serde_json::Value>;

/// A document as submitted for ingestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Document {
    pub id: DocumentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub text: String,
    /// Where the document came from (URL, path, connector record id). Carried into provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: Metadata,
}

impl Document {
    /// A document with an id and text; set `title`, `source` and `metadata` as needed.
    pub fn new(id: &str, text: impl Into<String>) -> crate::Result<Self> {
        Ok(Document {
            id: DocumentId::new(id)?,
            title: None,
            text: text.into(),
            source: None,
            metadata: Metadata::new(),
        })
    }
}

/// A retrievable span of a document. Offsets are byte offsets into the document text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Chunk {
    pub id: ChunkId,
    pub doc_id: DocumentId,
    pub ordinal: u32,
    pub text: String,
    pub start: u32,
    pub end: u32,
    /// xxh3 of the normalized text; equal hashes within a tenant are deduplicated.
    pub hash: u64,
}

/// Stored per-document record: what was ingested and which chunks it produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentRecord {
    pub id: DocumentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: Metadata,
    /// Monotonic per document; bumped on every re-ingest that changed content.
    pub version: u64,
    /// xxh3 of the full text, so unchanged re-ingests are skipped (incremental indexing).
    pub content_hash: u64,
    pub chunks: Vec<ChunkId>,
    /// Embedders that stored vectors for this document's chunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedders: Vec<String>,
    pub ingested_at_ms: u64,
}

/// The retrieval families RAG2 can route a query to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RetrieverKind {
    /// Embedding similarity over the vector index.
    Dense,
    /// BM25 over the full-text index.
    Lexical,
    /// Metadata predicates (exact match / ranges) over stored documents.
    Structured,
    /// Neighbour expansion over chunks that share entities or documents.
    Graph,
}

/// One index's opinion of a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Hit {
    pub chunk: ChunkId,
    pub score: f32,
}

/// Where a piece of evidence came from, precise enough to cite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Provenance {
    pub doc_id: DocumentId,
    pub doc_version: u64,
    pub chunk_ordinal: u32,
    pub start: u32,
    pub end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// How one retriever ranked a piece of evidence.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Signal {
    pub retriever: RetrieverKind,
    /// 1-based rank in that retriever's list.
    pub rank: u32,
    pub score: f32,
    /// Which hop of a multi-hop query found it (0 = the original query).
    pub hop: u32,
}

/// A ranked, attributed chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub chunk_id: ChunkId,
    pub text: String,
    /// Fused score after ranking policy.
    pub score: f32,
    pub signals: Vec<Signal>,
    pub provenance: Provenance,
}

/// Two pieces of evidence that disagree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Contradiction {
    pub a: ChunkId,
    pub b: ChunkId,
    /// Which detector flagged it (configured by name).
    pub detector: String,
    pub reason: String,
    pub confidence: f32,
}

/// Why the engine declined to answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Abstention {
    pub reason: AbstainReason,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AbstainReason {
    NoEvidence,
    LowConfidence,
    Contradictory,
    DecisionModel,
}

/// Time spent per stage, microseconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StageTimings {
    pub route_us: u64,
    pub retrieve_us: u64,
    pub fuse_us: u64,
    pub analyze_us: u64,
    pub assemble_us: u64,
    pub total_us: u64,
}
