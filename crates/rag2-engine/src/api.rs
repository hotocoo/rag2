//! Request and response types of the engine, shared verbatim by REST, gRPC, MCP, the CLI and the
//! embedded library. Versioned with the REST API (`/v1`).

use rag2_core::model::{
    Abstention, Contradiction, Document, Evidence, Metadata, RetrieverKind, StageTimings,
};
use rag2_core::{ChunkId, DocumentId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub query: String,
    /// Results to return; defaults to the pipeline's `top_k`, capped by `limits.max_top_k`.
    #[serde(default)]
    pub top_k: Option<usize>,
    /// Pipeline name; defaults to the tenant's pipeline.
    #[serde(default)]
    pub pipeline: Option<String>,
    /// Metadata equality filters. A list value matches documents having any of its values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filters: Metadata,
    /// Extra retrieval hops, up to the pipeline's `multi_hop.max_hops`.
    #[serde(default)]
    pub hops: Option<u32>,
    /// Include the assembled context string (default true).
    #[serde(default)]
    pub include_context: Option<bool>,
    /// Bypass the query cache.
    #[serde(default)]
    pub no_cache: bool,
}

impl QueryRequest {
    pub fn new(query: impl Into<String>) -> Self {
        QueryRequest {
            query: query.into(),
            top_k: None,
            pipeline: None,
            filters: Metadata::new(),
            hops: None,
            include_context: None,
            no_cache: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct QueryResponse {
    pub evidence: Vec<Evidence>,
    /// Calibrated confidence that the evidence answers the query, in [0, 1].
    pub confidence: f32,
    /// Raw (pre-calibration) confidence and its components, for calibration and debugging.
    pub confidence_detail: ConfidenceDetail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstained: Option<Abstention>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contradictions: Vec<Contradiction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub pipeline: String,
    /// Retrievers the router chose, and why.
    pub route: Route,
    pub hops: u32,
    pub config_version: u64,
    pub timings: StageTimings,
    pub cached: bool,
    /// Non-fatal problems (a decision model was unavailable, an embedder has no vectors yet).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// The query after `query_rewrite` plugins, when they changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_rewritten: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Route {
    pub retrievers: Vec<RetrieverKind>,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfidenceDetail {
    pub raw: f32,
    /// Best dense cosine similarity among results (absent when dense did not run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dense_similarity: Option<f32>,
    /// Share of the query's content words found in the top evidence.
    pub term_coverage: f32,
    /// Share of retrievers that ran and placed the top result in their own lists.
    pub agreement: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IngestRequest {
    pub documents: Vec<Document>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    Created,
    Updated,
    /// Content hash unchanged: nothing re-indexed.
    Unchanged,
    /// An ingest plugin dropped the document.
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IngestedDocument {
    pub id: DocumentId,
    pub status: IngestStatus,
    pub chunks: usize,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IngestReport {
    pub documents: Vec<IngestedDocument>,
    pub generation: u64,
    /// When the documents become searchable lexically: immediately, or within the commit interval.
    pub searchable_within_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TenantStats {
    pub documents: u64,
    pub generation: u64,
    pub lexical_docs: u64,
    pub vector_indexes: Vec<VectorIndexStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VectorIndexStats {
    pub embedder: String,
    pub kind: String,
    pub vectors: usize,
    pub dim: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChunkView {
    pub id: ChunkId,
    pub ordinal: u32,
    pub text: String,
}
