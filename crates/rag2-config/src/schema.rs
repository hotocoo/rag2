//! The profile: everything an operator configures, as one declarative document. Unknown fields are
//! refused (a typo must not silently fall back to a default). The JSON Schema served to the admin
//! console and editors is generated from these types, so the schema can never drift from the code.

use crate::defaults as d;
use rag2_core::TenantId;
use rag2_core::model::RetrieverKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The only `api_version` this build reads. A future breaking schema change adds `rag2/v2` and a
/// migration (see `migrate.rs`), never a silent reinterpretation.
pub const API_VERSION: &str = "rag2/v1";

macro_rules! defaults {
    ($($name:ident: $ty:ty = $val:expr;)*) => {
        $(fn $name() -> $ty { $val })*
    };
}

defaults! {
    listen: String = d::LISTEN.into();
    request_timeout_ms: u64 = d::REQUEST_TIMEOUT_MS;
    max_body_bytes: usize = d::MAX_BODY_BYTES;
    max_concurrent_requests: usize = d::MAX_CONCURRENT_REQUESTS;
    shutdown_grace_ms: u64 = d::SHUTDOWN_GRACE_MS;
    data_dir: String = d::DATA_DIR.into();
    storage_cache_mb: u64 = d::STORAGE_CACHE_MB;
    max_top_k: usize = d::MAX_TOP_K;
    max_query_chars: usize = d::MAX_QUERY_CHARS;
    max_document_bytes: usize = d::MAX_DOCUMENT_BYTES;
    max_batch_documents: usize = d::MAX_BATCH_DOCUMENTS;
    ingest_queue_depth: usize = d::INGEST_QUEUE_DEPTH;
    ingest_workers: usize = d::INGEST_WORKERS;
    chunk_max_chars: usize = d::CHUNK_MAX_CHARS;
    chunk_overlap_chars: usize = d::CHUNK_OVERLAP_CHARS;
    chunk_min_chars: usize = d::CHUNK_MIN_CHARS;
    flat_below: usize = d::VECTOR_FLAT_BELOW;
    flat_chunk_rows: usize = d::FLAT_PARALLEL_CHUNK_ROWS;
    hnsw_m: usize = d::HNSW_M;
    hnsw_ef_construction: usize = d::HNSW_EF_CONSTRUCTION;
    hnsw_ef_search: usize = d::HNSW_EF_SEARCH;
    hnsw_tombstones: f32 = d::HNSW_MAX_TOMBSTONE_RATIO;
    lexical_heap_mb: usize = d::LEXICAL_WRITER_HEAP_MB;
    lexical_commit_ms: u64 = d::LEXICAL_COMMIT_INTERVAL_MS;
    hash_dim: usize = d::HASH_EMBEDDER_DIM;
    embed_batch: usize = d::EMBED_BATCH;
    embed_max_tokens: usize = d::EMBED_MAX_TOKENS;
    retriever_top_k: usize = d::RETRIEVER_TOP_K;
    retriever_weight: f32 = d::RETRIEVER_WEIGHT;
    rrf_k: f32 = d::RRF_K;
    pool_factor: usize = d::CANDIDATE_POOL_FACTOR;
    graph_seeds: usize = d::GRAPH_SEEDS;
    graph_neighbors: u32 = d::GRAPH_NEIGHBORS;
    graph_decay: f32 = d::GRAPH_DECAY;
    graph_link_field: String = d::GRAPH_LINK_FIELD.into();
    coverage_top_n: usize = d::COVERAGE_TOP_N;
    result_top_k: usize = d::RESULT_TOP_K;
    near_dup: f32 = d::NEAR_DUPLICATE_SIMILARITY;
    max_hops: u32 = d::MAX_HOPS;
    hop_terms: usize = d::HOP_EXPANSION_TERMS;
    decision_min_conf: f32 = d::DECISION_MIN_CONFIDENCE;
    cal_slope: f32 = d::CALIBRATION_SLOPE;
    cal_intercept: f32 = d::CALIBRATION_INTERCEPT;
    abstain_conf: f32 = d::ABSTAIN_MIN_CONFIDENCE;
    abstain_evidence: usize = d::ABSTAIN_MIN_EVIDENCE;
    contra_overlap: f32 = d::CONTRADICTION_MIN_OVERLAP;
    numeric_tol: f32 = d::NUMERIC_RELATIVE_TOLERANCE;
    context_chars: usize = d::CONTEXT_MAX_CHARS;
    context_chunks: usize = d::CONTEXT_MAX_CHUNKS;
    cache_entries: u64 = d::QUERY_CACHE_ENTRIES;
    cache_ttl_ms: u64 = d::QUERY_CACHE_TTL_MS;
    chunk_cache: u64 = d::CHUNK_CACHE_ENTRIES;
    doc_cache: u64 = d::DOCUMENT_CACHE_ENTRIES;
    decision_timeout_ms: u64 = d::DECISION_HTTP_TIMEOUT_MS;
    decision_concurrency: usize = d::DECISION_CONCURRENCY;
    tenant_rps: u32 = d::TENANT_REQUESTS_PER_SECOND;
    tenant_burst: u32 = d::TENANT_BURST;
    tenant_max_docs: u64 = d::TENANT_MAX_DOCUMENTS;
    node_id: String = d::NODE_ID.into();
    repl_poll_ms: u64 = d::REPLICATION_POLL_MS;
    repl_batch: usize = d::REPLICATION_BATCH;
    webhook_timeout_ms: u64 = d::WEBHOOK_TIMEOUT_MS;
    webhook_retries: u32 = d::WEBHOOK_MAX_RETRIES;
    plugin_fuel: u64 = d::PLUGIN_FUEL;
    plugin_memory_mb: u64 = d::PLUGIN_MEMORY_MB;
    plugin_timeout_ms: u64 = d::PLUGIN_TIMEOUT_MS;
    connector_interval_s: u64 = d::CONNECTOR_INTERVAL_S;
    connector_timeout_ms: u64 = d::CONNECTOR_TIMEOUT_MS;
    connector_extensions: Vec<String> = d::CONNECTOR_EXTENSIONS.iter().map(|s| s.to_string()).collect();
    yes: bool = true;
    default_name: String = "default".into();
}

/// A reference to a secret, never the secret itself: `env:NAME` or `file:/path`. Resolved at
/// activation; a profile can be shown, diffed and versioned without leaking credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SecretRef {
    Env(String),
    File(String),
}

impl JsonSchema for SecretRef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SecretRef".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^(env:[A-Za-z_][A-Za-z0-9_]*|file:/.+)$",
            "description": "Secret reference: env:NAME or file:/absolute/path. Never the secret itself."
        })
    }
}

impl TryFrom<String> for SecretRef {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        if let Some(name) = s.strip_prefix("env:") {
            let ok = !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit());
            return if ok {
                Ok(SecretRef::Env(name.into()))
            } else {
                Err(format!("bad env name in {s:?}"))
            };
        }
        if let Some(p) = s.strip_prefix("file:")
            && p.starts_with('/')
        {
            return Ok(SecretRef::File(p.into()));
        }
        Err("secrets must be referenced as env:NAME or file:/absolute/path, never inlined".into())
    }
}

impl From<SecretRef> for String {
    fn from(s: SecretRef) -> String {
        match s {
            SecretRef::Env(n) => format!("env:{n}"),
            SecretRef::File(p) => format!("file:{p}"),
        }
    }
}

impl SecretRef {
    /// Read the secret's value. Errors name the reference, never the value.
    pub fn resolve(&self) -> Result<String, String> {
        match self {
            SecretRef::Env(n) => {
                std::env::var(n).map_err(|_| format!("environment variable {n} is not set"))
            }
            SecretRef::File(p) => std::fs::read_to_string(p)
                .map(|s| s.trim_end_matches(['\n', '\r']).to_string())
                .map_err(|e| format!("secret file {p}: {e}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Must be `rag2/v1`.
    pub api_version: String,
    pub metadata: ProfileMetadata,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub chunking: Chunking,
    #[serde(default)]
    pub indexes: Indexes,
    /// Named embedders; pipelines refer to them by name.
    #[serde(default = "default_embedders")]
    pub embedders: BTreeMap<String, EmbedderConfig>,
    /// Named decision models (Laya-compatible); pipelines may ask them typed questions.
    #[serde(default)]
    pub decision_models: BTreeMap<String, DecisionModelConfig>,
    /// Named retrieval pipelines.
    #[serde(default = "default_pipelines")]
    pub pipelines: BTreeMap<String, Pipeline>,
    /// Pipeline used when neither the request nor the tenant names one.
    #[serde(default = "default_name")]
    pub default_pipeline: String,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    /// Per-tenant overrides. Tenants not listed get `tenant_defaults`.
    #[serde(default)]
    pub tenants: BTreeMap<TenantId, TenantConfig>,
    #[serde(default)]
    pub tenant_defaults: TenantConfig,
    #[serde(default)]
    pub cluster: ClusterConfig,
    /// Sandboxed Wasm plugins, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, PluginConfig>,
    /// `document_transform` plugins applied, in order, to every ingested document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ingest_plugins: Vec<String>,
    /// Data sources synchronized into tenants on a schedule.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connectors: BTreeMap<String, ConnectorConfig>,
    /// Outbound event notifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhooks: Vec<Webhook>,
    /// Environment overlays (RFC 7386 JSON merge patches), applied by name at activation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environments: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "listen")]
    pub listen: String,
    #[serde(default = "request_timeout_ms")]
    pub request_timeout_ms: u64,
    #[serde(default = "max_body_bytes")]
    pub max_body_bytes: usize,
    /// Requests in flight before new ones are shed (503 + Retry-After).
    #[serde(default = "max_concurrent_requests")]
    pub max_concurrent_requests: usize,
    #[serde(default = "shutdown_grace_ms")]
    pub shutdown_grace_ms: u64,
    /// Allowed browser origins for the REST API; empty = same-origin only.
    #[serde(default)]
    pub cors_origins: Vec<String>,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    /// Serve `/metrics` without authentication (for scrapers on a private network).
    #[serde(default)]
    pub public_metrics: bool,
    /// Address for the gRPC API (same TLS settings as REST); absent = gRPC disabled.
    #[serde(default)]
    pub grpc_listen: Option<String>,
    /// Directory of the built admin console to serve at `/`; absent = console disabled.
    #[serde(default)]
    pub console_dir: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
    /// When set, clients must present a certificate signed by this CA (mTLS).
    #[serde(default)]
    pub client_ca_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    #[default]
    Fjall,
    Redb,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default)]
    pub backend: StorageBackend,
    #[serde(default = "data_dir")]
    pub data_dir: String,
    #[serde(default = "storage_cache_mb")]
    pub cache_mb: u64,
    /// Key for encryption at rest (32 bytes, base64). Absent = plaintext at rest.
    #[serde(default)]
    pub encryption_key: Option<SecretRef>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "max_top_k")]
    pub max_top_k: usize,
    #[serde(default = "max_query_chars")]
    pub max_query_chars: usize,
    #[serde(default = "max_document_bytes")]
    pub max_document_bytes: usize,
    #[serde(default = "max_batch_documents")]
    pub max_batch_documents: usize,
    /// Pending ingest jobs before submissions are refused (backpressure).
    #[serde(default = "ingest_queue_depth")]
    pub ingest_queue_depth: usize,
    #[serde(default = "ingest_workers")]
    pub ingest_workers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Chunking {
    /// Upper bound per chunk; splits prefer paragraph, then sentence, then word boundaries.
    #[serde(default = "chunk_max_chars")]
    pub max_chars: usize,
    #[serde(default = "chunk_overlap_chars")]
    pub overlap_chars: usize,
    /// Trailing fragments shorter than this merge into the previous chunk.
    #[serde(default = "chunk_min_chars")]
    pub min_chars: usize,
}

impl Default for Chunking {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct Indexes {
    #[serde(default)]
    pub vector: VectorIndexConfig,
    #[serde(default)]
    pub lexical: LexicalIndexConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VectorIndexConfig {
    /// Tenants below this many vectors use the exact flat index; at or above it, HNSW.
    #[serde(default = "flat_below")]
    pub flat_below: usize,
    #[serde(default = "flat_chunk_rows")]
    pub flat_parallel_chunk_rows: usize,
    #[serde(default = "hnsw_m")]
    pub hnsw_m: usize,
    #[serde(default = "hnsw_ef_construction")]
    pub hnsw_ef_construction: usize,
    #[serde(default = "hnsw_ef_search")]
    pub hnsw_ef_search: usize,
    #[serde(default = "hnsw_tombstones")]
    pub hnsw_max_tombstone_ratio: f32,
}

impl Default for VectorIndexConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LexicalIndexConfig {
    #[serde(default = "lexical_heap_mb")]
    pub writer_heap_mb: usize,
    /// How often pending writes become searchable (near-real-time refresh). 0 = on every write.
    #[serde(default = "lexical_commit_ms")]
    pub commit_interval_ms: u64,
    #[serde(default)]
    pub tokenizer: LexicalTokenizer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum LexicalTokenizer {
    /// Split on non-alphanumerics, lowercase, drop tokens over 40 bytes. Language-neutral.
    #[default]
    Default,
    /// `default` plus English stemming.
    EnStem,
    /// Split on whitespace only (codes, identifiers).
    Whitespace,
}

impl Default for LexicalIndexConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmbedderConfig {
    /// Feature hashing of word unigrams and bigrams. No model, deterministic, CPU-cheap; a
    /// baseline and a fallback when no model is configured.
    Hash {
        #[serde(default = "hash_dim")]
        dim: usize,
    },
    /// A local BERT-family sentence encoder run with candle (safetensors + tokenizer.json).
    Candle {
        model_dir: String,
        #[serde(default)]
        device: Device,
        /// `cls` or `mean` pooling, as the model was trained.
        #[serde(default)]
        pooling: Pooling,
        #[serde(default = "embed_batch")]
        batch: usize,
        #[serde(default)]
        query_prefix: String,
        #[serde(default)]
        document_prefix: String,
        /// Tokens per input; longer inputs are truncated.
        #[serde(default = "embed_max_tokens")]
        max_tokens: usize,
    },
    /// Any OpenAI-compatible embeddings API: RAG2 posts to `{endpoint}/embeddings`.
    Http {
        endpoint: String,
        model: String,
        dim: usize,
        #[serde(default)]
        api_key: Option<SecretRef>,
        #[serde(default = "decision_timeout_ms")]
        timeout_ms: u64,
        #[serde(default = "embed_batch")]
        batch: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    #[default]
    Cpu,
    Metal,
    Cuda,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Pooling {
    Cls,
    #[default]
    Mean,
}

fn default_embedders() -> BTreeMap<String, EmbedderConfig> {
    BTreeMap::from([(default_name(), EmbedderConfig::Hash { dim: hash_dim() })])
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionModelConfig {
    /// A Laya checkpoint directory run in-process (`model.safetensors`, `rl_agent_config.json`,
    /// `encoder/`, `tokenizer/`).
    LayaLocal {
        checkpoint_dir: String,
        #[serde(default)]
        device: Device,
    },
    /// A Laya sidecar speaking the decision wire (`POST /v1/decide`).
    LayaHttp {
        endpoint: String,
        serve_id: String,
        #[serde(default = "decision_timeout_ms")]
        timeout_ms: u64,
    },
    /// A System One decision server (Liquid AI d1 on llama.cpp: `POST /v1/systemone`).
    /// `serve_id` must appear in the server's `GET /v1/models` list.
    SystemOne {
        endpoint: String,
        serve_id: String,
        #[serde(default = "decision_timeout_ms")]
        timeout_ms: u64,
        /// Requests in flight to this model at once, across all queries; more wait in a FIFO
        /// queue and the wait counts against `timeout_ms`. Match the server's `-np`.
        #[serde(default = "decision_concurrency")]
        concurrency: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Pipeline {
    #[serde(default)]
    pub description: String,
    pub retrievers: Vec<RetrieverConfig>,
    #[serde(default)]
    pub routing: Routing,
    #[serde(default)]
    pub fusion: Fusion,
    #[serde(default = "result_top_k")]
    pub top_k: usize,
    #[serde(default)]
    pub dedup: Dedup,
    #[serde(default)]
    pub multi_hop: MultiHop,
    #[serde(default)]
    pub contradiction: ContradictionConfig,
    #[serde(default)]
    pub calibration: Calibration,
    #[serde(default)]
    pub abstention: Abstention,
    #[serde(default)]
    pub context: ContextAssembly,
    /// Optional relevance gate: ask a decision model whether each top result answers the query.
    #[serde(default)]
    pub relevance_gate: Option<DecisionGate>,
    #[serde(default)]
    pub graph: GraphExpansion,
    /// Plugins used by this pipeline.
    #[serde(default)]
    pub plugins: PipelinePlugins,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct PipelinePlugins {
    /// `query_rewrite` plugins applied in order before routing.
    #[serde(default)]
    pub query_rewrite: Vec<String>,
    /// A `rerank` plugin that rescores the final candidates.
    #[serde(default)]
    pub rerank: Option<String>,
}

/// A Wasm plugin: a core module exporting the RAG2 plugin ABI. It runs with no WASI and no host
/// I/O, a fresh instance per call, and the quotas below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PluginConfig {
    /// `.wasm` (or `.wat`) file.
    pub path: String,
    /// Expected SHA-256 of the file (hex); loading fails on mismatch.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Hooks this plugin is allowed to serve (must be declared by its manifest).
    pub hooks: Vec<String>,
    /// Plugin-specific settings, passed to every call.
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default = "plugin_fuel")]
    pub fuel: u64,
    #[serde(default = "plugin_memory_mb")]
    pub memory_mb: u64,
    #[serde(default = "plugin_timeout_ms")]
    pub timeout_ms: u64,
    /// Allow the plugin to write log lines through the host.
    #[serde(default)]
    pub allow_log: bool,
}

/// Graph retrieval: expand the best results to their neighbours in the document graph, i.e. the
/// adjacent chunks of the same document and chunks of documents linked through metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphExpansion {
    /// Top fused results used as seeds.
    #[serde(default = "graph_seeds")]
    pub seeds: usize,
    /// Adjacent chunks taken on each side of a seed.
    #[serde(default = "graph_neighbors")]
    pub neighbors: u32,
    /// Score multiplier per step away from a seed.
    #[serde(default = "graph_decay")]
    pub decay: f32,
    /// Document metadata field listing linked document ids.
    #[serde(default = "graph_link_field")]
    pub link_field: String,
}

impl Default for GraphExpansion {
    fn default() -> Self {
        empty_object()
    }
}

fn default_pipelines() -> BTreeMap<String, Pipeline> {
    let r = |kind| RetrieverConfig {
        kind,
        embedder: Some(default_name()),
        top_k: retriever_top_k(),
        weight: retriever_weight(),
        ef_search: None,
        enabled: true,
    };
    BTreeMap::from([(
        default_name(),
        Pipeline {
            description: "Hybrid dense + lexical with reciprocal rank fusion".into(),
            retrievers: vec![
                r(RetrieverKind::Dense),
                RetrieverConfig {
                    embedder: None,
                    ..r(RetrieverKind::Lexical)
                },
            ],
            routing: Routing::default(),
            fusion: Fusion::default(),
            top_k: result_top_k(),
            dedup: Dedup::default(),
            multi_hop: MultiHop::default(),
            contradiction: ContradictionConfig::default(),
            calibration: Calibration::default(),
            abstention: Abstention::default(),
            context: ContextAssembly::default(),
            relevance_gate: None,
            graph: GraphExpansion::default(),
            plugins: PipelinePlugins::default(),
        },
    )])
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetrieverConfig {
    pub kind: RetrieverKind,
    /// Embedder name (dense only).
    #[serde(default)]
    pub embedder: Option<String>,
    #[serde(default = "retriever_top_k")]
    pub top_k: usize,
    /// Weight in weighted fusion; also scales this retriever's RRF contribution.
    #[serde(default = "retriever_weight")]
    pub weight: f32,
    /// HNSW search breadth for this retriever (dense only); absent = index default.
    #[serde(default)]
    pub ef_search: Option<usize>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Routing {
    /// Every enabled retriever runs on every query.
    #[default]
    All,
    /// Choose retrievers from query features with ordered rules; the first match wins.
    Rules {
        rules: Vec<RouteRule>,
        fallback: Vec<RetrieverKind>,
    },
    /// Ask a decision model to pick a route (a `choice` question over the configured routes).
    Decision {
        model: String,
        instructions: String,
        routes: BTreeMap<String, RouteChoice>,
        #[serde(default = "decision_min_conf")]
        min_confidence: f32,
        /// Used when the model is unavailable or below `min_confidence`.
        fallback: Vec<RetrieverKind>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteChoice {
    pub description: String,
    pub retrievers: Vec<RetrieverKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteRule {
    pub when: RouteCondition,
    pub retrievers: Vec<RetrieverKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteCondition {
    /// Query has at most this many words.
    MaxWords(usize),
    /// Query has at least this many words.
    MinWords(usize),
    /// Query contains a double-quoted phrase.
    HasQuotedPhrase,
    /// Query contains a token mixing letters and digits (codes, ids, versions).
    HasIdentifier,
    /// Query matches this regular expression.
    Matches(String),
    /// Request carries structured filters.
    HasFilters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum FusionMethod {
    /// Reciprocal rank fusion: robust to incomparable score scales.
    #[default]
    Rrf,
    /// Weighted sum of per-retriever min-max normalized scores.
    Weighted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Fusion {
    #[serde(default)]
    pub method: FusionMethod,
    #[serde(default = "rrf_k")]
    pub rrf_k: f32,
    /// Fused candidates kept per requested result before filtering and deduplication.
    #[serde(default = "pool_factor")]
    pub candidate_pool_factor: usize,
}

impl Default for Fusion {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dedup {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Results whose token-set Jaccard similarity reaches this are collapsed into the better one.
    #[serde(default = "near_dup")]
    pub near_duplicate_similarity: f32,
}

impl Default for Dedup {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MultiHop {
    /// Extra retrieval rounds after the first. 0 disables multi-hop.
    #[serde(default)]
    pub extra_hops: u32,
    /// Ceiling the request may raise `extra_hops` to.
    #[serde(default = "max_hops")]
    pub max_hops: u32,
    /// Salient terms taken from the previous hop's evidence to form the next query.
    #[serde(default = "hop_terms")]
    pub expansion_terms: usize,
}

impl Default for MultiHop {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContradictionConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Only pairs sharing at least this share of content words are compared.
    #[serde(default = "contra_overlap")]
    pub min_overlap: f32,
    /// Numbers differing by more than this relative amount in otherwise-similar claims conflict.
    #[serde(default = "numeric_tol")]
    pub numeric_tolerance: f32,
    /// Optionally confirm candidate pairs with a decision model (`yesno`).
    #[serde(default)]
    pub decision: Option<DecisionGate>,
}

impl Default for ContradictionConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionGate {
    pub model: String,
    pub instructions: String,
    #[serde(default = "decision_min_conf")]
    pub min_confidence: f32,
    #[serde(default)]
    pub mode: GateMode,
}

/// How a gate asks about several passages (or passage pairs).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// One call per passage or pair, run concurrently up to the model's `concurrency`.
    #[default]
    PerItem,
    /// One call: every passage numbered in one state, one question per passage or pair. One
    /// forward pass on models that read many questions at once (d1); the state grows with top_k.
    Joint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Calibration {
    /// Logistic calibration of the raw retrieval confidence: sigmoid(slope * logit(x) + intercept).
    /// Fit `slope` and `intercept` offline on labelled queries (`rag2 calibrate`).
    #[serde(default = "cal_slope")]
    pub slope: f32,
    #[serde(default = "cal_intercept")]
    pub intercept: f32,
}

impl Default for Calibration {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Abstention {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "abstain_conf")]
    pub min_confidence: f32,
    #[serde(default = "abstain_evidence")]
    pub min_evidence: usize,
    /// Abstain when evidence contradicts itself.
    #[serde(default)]
    pub on_contradiction: bool,
    /// Top evidence passages whose words count toward query term coverage.
    #[serde(default = "coverage_top_n")]
    pub coverage_top_n: usize,
}

impl Default for Abstention {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContextAssembly {
    #[serde(default = "context_chars")]
    pub max_chars: usize,
    #[serde(default = "context_chunks")]
    pub max_chunks: usize,
    /// Per-chunk template. Placeholders: {n} {text} {doc_id} {title} {source} {score}.
    #[serde(default = "context_template")]
    pub template: String,
    #[serde(default = "context_separator")]
    pub separator: String,
}

fn context_template() -> String {
    "[{n}] ({doc_id}) {text}".into()
}

fn context_separator() -> String {
    "\n\n".into()
}

impl Default for ContextAssembly {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    #[serde(default = "cache_entries")]
    pub query_entries: u64,
    #[serde(default = "cache_ttl_ms")]
    pub query_ttl_ms: u64,
    #[serde(default = "chunk_cache")]
    pub chunk_entries: u64,
    #[serde(default = "doc_cache")]
    pub document_entries: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Authentication. `none` is refused unless the server listens on loopback only.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Role definitions; principals map to roles via API keys or OIDC claims.
    #[serde(default = "default_roles")]
    pub roles: BTreeMap<String, Role>,
    #[serde(default)]
    pub audit: AuditConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthConfig {
    /// No authentication. Only valid with a loopback `server.listen`.
    #[default]
    None,
    /// Static API keys, each bound to roles and tenants.
    ApiKeys { keys: Vec<ApiKey> },
    /// OIDC bearer tokens validated against the issuer's JWKS, plus optional API keys.
    Oidc {
        issuer: String,
        audience: String,
        jwks_url: String,
        /// Claim holding role names.
        #[serde(default = "roles_claim")]
        roles_claim: String,
        /// Claim holding tenant ids the principal may access.
        #[serde(default = "tenants_claim")]
        tenants_claim: String,
        #[serde(default)]
        api_keys: Vec<ApiKey>,
    },
}

fn roles_claim() -> String {
    "roles".into()
}

fn tenants_claim() -> String {
    "tenants".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    /// Public identifier shown in audit logs.
    pub id: String,
    /// The key's value. Compared in constant time against the presented key's hash.
    pub secret: SecretRef,
    pub roles: Vec<String>,
    /// Tenants this key may act on; `["*"]` for all.
    pub tenants: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Query,
    Ingest,
    DeleteDocuments,
    ReadDocuments,
    ManageConfig,
    ManageTenants,
    ReadAudit,
    ReadMetrics,
    ManagePlugins,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub permissions: Vec<Permission>,
}

fn default_roles() -> BTreeMap<String, Role> {
    use Permission::*;
    BTreeMap::from([
        (
            "reader".into(),
            Role {
                permissions: vec![Query, ReadDocuments],
            },
        ),
        (
            "writer".into(),
            Role {
                permissions: vec![Query, ReadDocuments, Ingest, DeleteDocuments],
            },
        ),
        (
            "admin".into(),
            Role {
                permissions: vec![
                    Query,
                    Ingest,
                    DeleteDocuments,
                    ReadDocuments,
                    ManageConfig,
                    ManageTenants,
                    ReadAudit,
                    ReadMetrics,
                    ManagePlugins,
                ],
            },
        ),
    ])
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Also record queries (not only writes and admin actions).
    #[serde(default)]
    pub include_queries: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        empty_object()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TenantConfig {
    /// Pipeline for this tenant; absent = the profile's `default_pipeline`.
    #[serde(default)]
    pub pipeline: Option<String>,
    #[serde(default = "tenant_rps")]
    pub requests_per_second: u32,
    #[serde(default = "tenant_burst")]
    pub burst: u32,
    #[serde(default = "tenant_max_docs")]
    pub max_documents: u64,
    /// A disabled tenant's data is kept but every request is refused.
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl Default for TenantConfig {
    fn default() -> Self {
        empty_object()
    }
}

/// A scheduled source. Documents it produces carry metadata `_source: <connector name>`, so a
/// sync can delete what disappeared from the source without touching other documents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectorConfig {
    pub tenant: TenantId,
    pub source: SourceConfig,
    /// Seconds between syncs; 0 = only on demand (`POST /v1/admin/connectors/{name}/run`).
    #[serde(default = "connector_interval_s")]
    pub interval_s: u64,
    /// Delete documents that no longer exist in the source.
    #[serde(default)]
    pub delete_missing: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceConfig {
    /// Text files under a directory (recursive). Document id = relative path.
    Filesystem {
        path: String,
        #[serde(default = "connector_extensions")]
        extensions: Vec<String>,
    },
    /// Web pages; HTML is reduced to text. Document id = URL.
    Web {
        urls: Vec<String>,
        /// Extra request headers; values are secret references.
        #[serde(default)]
        headers: BTreeMap<String, SecretRef>,
        #[serde(default = "connector_timeout_ms")]
        timeout_ms: u64,
    },
    /// A JSON API returning a list of records, mapped with JSON Pointers (RFC 6901).
    HttpJson {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, SecretRef>,
        /// Pointer to the array of records ("" = the response is the array).
        #[serde(default)]
        items: String,
        id: String,
        text: String,
        #[serde(default)]
        title: Option<String>,
        /// Metadata field -> pointer within a record.
        #[serde(default)]
        metadata: BTreeMap<String, String>,
        #[serde(default = "connector_timeout_ms")]
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookEvent {
    DocumentsIngested,
    DocumentsDeleted,
    ConfigActivated,
    TenantDeleted,
}

/// POSTs a JSON event to `url`. With a secret, deliveries carry
/// `x-rag2-signature: sha256=<hex HMAC-SHA256 of "<x-rag2-timestamp>.<body>">`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Webhook {
    pub name: String,
    pub url: String,
    pub events: Vec<WebhookEvent>,
    #[serde(default)]
    pub secret: Option<SecretRef>,
    /// Tenants whose events are sent; empty = all.
    #[serde(default)]
    pub tenants: Vec<String>,
    #[serde(default = "webhook_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "webhook_retries")]
    pub max_retries: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClusterRole {
    /// Single node; no replication log.
    #[default]
    Standalone,
    /// Accepts writes and keeps a replication log for replicas.
    Primary,
    /// Read-only copy of a primary: serves queries, redirects writes.
    Replica,
}

/// Replication: a primary logs every write; replicas pull and apply the log (no re-embedding)
/// and serve queries. Promotion of a replica is an operator/orchestrator action (change the role
/// and restart). Restart-only settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterConfig {
    #[serde(default)]
    pub role: ClusterRole,
    #[serde(default = "node_id")]
    pub node_id: String,
    /// The primary's base URL (replicas).
    #[serde(default)]
    pub primary_url: Option<String>,
    /// Shared secret for the internal replication endpoint (primaries and replicas).
    #[serde(default)]
    pub replication_key: Option<SecretRef>,
    #[serde(default = "repl_poll_ms")]
    pub poll_interval_ms: u64,
    #[serde(default = "repl_batch")]
    pub batch: usize,
    /// This node group's public base URL (used with `tenant_routes`).
    #[serde(default)]
    pub self_url: Option<String>,
    /// Tenant partitioning: tenant id (or `*` for the rest) -> base URL of the group that owns
    /// it. Requests for tenants owned elsewhere are redirected there (307). Empty = no partitioning.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tenant_routes: BTreeMap<String, String>,
}

impl ClusterConfig {
    /// The owner's base URL when `tenant` belongs to another group.
    pub fn route_elsewhere(&self, tenant: &str) -> Option<&str> {
        let owner = self
            .tenant_routes
            .get(tenant)
            .or_else(|| self.tenant_routes.get("*"))?;
        let me = self.self_url.as_deref()?;
        (owner.trim_end_matches('/') != me.trim_end_matches('/')).then_some(owner.as_str())
    }
}

impl Default for ClusterConfig {
    fn default() -> Self {
        empty_object()
    }
}

/// Deserialize `{}` so a struct's `Default` is exactly its serde defaults (one source of truth).
fn empty_object<T: serde::de::DeserializeOwned>() -> T {
    match serde_json::from_value(serde_json::json!({})) {
        Ok(v) => v,
        // Reaching this is a schema bug (a required field in a struct marked defaultable); the
        // unit test `every_section_defaults_from_an_empty_object` catches it before release.
        Err(e) => unreachable!(
            "schema default for {} failed: {e}",
            std::any::type_name::<T>()
        ),
    }
}

impl Profile {
    /// A minimal valid profile: every section at its defaults.
    pub fn minimal(name: &str) -> Profile {
        Profile {
            api_version: API_VERSION.into(),
            metadata: ProfileMetadata {
                name: name.into(),
                description: String::new(),
                labels: BTreeMap::new(),
            },
            server: ServerConfig::default(),
            storage: StorageConfig::default(),
            limits: Limits::default(),
            chunking: Chunking::default(),
            indexes: Indexes::default(),
            embedders: default_embedders(),
            decision_models: BTreeMap::new(),
            pipelines: default_pipelines(),
            default_pipeline: default_name(),
            cache: CacheConfig::default(),
            security: SecurityConfig {
                auth: AuthConfig::None,
                roles: default_roles(),
                audit: AuditConfig::default(),
            },
            tenants: BTreeMap::new(),
            tenant_defaults: TenantConfig::default(),
            cluster: ClusterConfig::default(),
            webhooks: Vec::new(),
            plugins: BTreeMap::new(),
            ingest_plugins: Vec::new(),
            connectors: BTreeMap::new(),
            environments: BTreeMap::new(),
        }
    }

    /// The pipeline a tenant's request runs when the request names none.
    pub fn pipeline_for(&self, tenant: &TenantId) -> &str {
        self.tenants
            .get(tenant)
            .and_then(|t| t.pipeline.as_deref())
            .or(self.tenant_defaults.pipeline.as_deref())
            .unwrap_or(&self.default_pipeline)
    }

    pub fn tenant(&self, tenant: &TenantId) -> &TenantConfig {
        self.tenants.get(tenant).unwrap_or(&self.tenant_defaults)
    }
}

/// JSON Schema for profiles, for editors and the admin console.
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Profile)).unwrap_or_default()
}
