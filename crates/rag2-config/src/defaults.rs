//! Every default value in one place. A profile that omits a field gets the value named here;
//! operators override any of them in YAML/JSON without touching code.

// Server
pub const LISTEN: &str = "127.0.0.1:8080";
pub const REQUEST_TIMEOUT_MS: u64 = 10_000;
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Requests in flight before new ones are shed with 503 (backpressure).
pub const MAX_CONCURRENT_REQUESTS: usize = 1_024;
pub const SHUTDOWN_GRACE_MS: u64 = 15_000;

// Storage
pub const DATA_DIR: &str = "./data";
pub const STORAGE_CACHE_MB: u64 = 256;

// Limits
pub const MAX_TOP_K: usize = 100;
pub const MAX_QUERY_CHARS: usize = 4_096;
pub const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BATCH_DOCUMENTS: usize = 1_000;
pub const INGEST_QUEUE_DEPTH: usize = 10_000;
pub const INGEST_WORKERS: usize = 2;

// Chunking
pub const CHUNK_MAX_CHARS: usize = 1_200;
pub const CHUNK_OVERLAP_CHARS: usize = 150;
pub const CHUNK_MIN_CHARS: usize = 40;

// Vector index
/// Tenants with fewer vectors than this use the exact flat index (a scan beats a graph there).
pub const VECTOR_FLAT_BELOW: usize = 10_000;
pub const FLAT_PARALLEL_CHUNK_ROWS: usize = 8_192;
pub const HNSW_M: usize = 16;
pub const HNSW_EF_CONSTRUCTION: usize = 200;
pub const HNSW_EF_SEARCH: usize = 128;
pub const HNSW_MAX_TOMBSTONE_RATIO: f32 = 0.2;

// Lexical index
pub const LEXICAL_WRITER_HEAP_MB: usize = 64;
pub const LEXICAL_COMMIT_INTERVAL_MS: u64 = 1_000;

// Embedding
pub const HASH_EMBEDDER_DIM: usize = 384;
pub const EMBED_BATCH: usize = 32;
pub const EMBED_MAX_TOKENS: usize = 512;

// Retrieval
pub const RETRIEVER_TOP_K: usize = 50;
pub const RETRIEVER_WEIGHT: f32 = 1.0;
pub const RRF_K: f32 = 60.0;
/// Fused candidates kept per requested result, before filtering and deduplication.
pub const CANDIDATE_POOL_FACTOR: usize = 3;
pub const GRAPH_SEEDS: usize = 5;
pub const GRAPH_NEIGHBORS: u32 = 1;
pub const GRAPH_DECAY: f32 = 0.5;
pub const GRAPH_LINK_FIELD: &str = "links";
pub const RESULT_TOP_K: usize = 10;
pub const NEAR_DUPLICATE_SIMILARITY: f32 = 0.92;
pub const MAX_HOPS: u32 = 1;
pub const HOP_EXPANSION_TERMS: usize = 8;
pub const DECISION_MIN_CONFIDENCE: f32 = 0.9;

// Calibration and abstention
pub const CALIBRATION_SLOPE: f32 = 1.0;
pub const CALIBRATION_INTERCEPT: f32 = 0.0;
pub const ABSTAIN_MIN_CONFIDENCE: f32 = 0.25;
pub const ABSTAIN_MIN_EVIDENCE: usize = 1;
/// Evidence passages whose words count toward query term coverage.
pub const COVERAGE_TOP_N: usize = 3;

// Contradiction detection
pub const CONTRADICTION_MIN_OVERLAP: f32 = 0.5;
pub const NUMERIC_RELATIVE_TOLERANCE: f32 = 0.01;

// Context assembly
pub const CONTEXT_MAX_CHARS: usize = 12_000;
pub const CONTEXT_MAX_CHUNKS: usize = 8;

// Caching
pub const QUERY_CACHE_ENTRIES: u64 = 10_000;
pub const QUERY_CACHE_TTL_MS: u64 = 30_000;
/// Chunks kept decoded in memory for the query path (immutable, so never stale).
pub const CHUNK_CACHE_ENTRIES: u64 = 100_000;
/// Document records kept decoded in memory (invalidated on every write).
pub const DOCUMENT_CACHE_ENTRIES: u64 = 50_000;

// Decision model transport
pub const DECISION_HTTP_TIMEOUT_MS: u64 = 5_000;
/// Decision requests in flight per model, across all queries. llama.cpp 0.8.0 runs d1 calls one
/// at a time, so more in flight adds no throughput and queues inside the server, where requests
/// starve. Measured, 346-token states: 1 in flight p99 0.38 s at 7.6 calls/s; 2: 0.70 s at
/// 5.1/s; 8: 49 s (docs/benchmarks/decision-d1.md). RAG2 queues the rest in FIFO order.
pub const DECISION_CONCURRENCY: usize = 1;

// Tenancy
pub const TENANT_REQUESTS_PER_SECOND: u32 = 200;
pub const TENANT_BURST: u32 = 400;
pub const TENANT_MAX_DOCUMENTS: u64 = 10_000_000;

// Config store
/// Versions kept per profile before the oldest inactive ones are pruned.
pub const CONFIG_HISTORY: usize = 100;

// Cluster
pub const NODE_ID: &str = "node-1";
/// How often a replica polls its primary for new log entries.
pub const REPLICATION_POLL_MS: u64 = 500;
/// Log entries fetched per replication request.
pub const REPLICATION_BATCH: usize = 64;

// Webhooks
pub const WEBHOOK_TIMEOUT_MS: u64 = 5_000;
pub const WEBHOOK_MAX_RETRIES: u32 = 5;

// Plugins
/// Wasm instructions (fuel) a plugin may spend per call.
pub const PLUGIN_FUEL: u64 = 200_000_000;
pub const PLUGIN_MEMORY_MB: u64 = 64;
pub const PLUGIN_TIMEOUT_MS: u64 = 250;

// Connectors
pub const CONNECTOR_INTERVAL_S: u64 = 900;
pub const CONNECTOR_TIMEOUT_MS: u64 = 30_000;
/// Largest single item a connector will fetch.
pub const CONNECTOR_MAX_ITEM_BYTES: usize = 4 * 1024 * 1024;
pub const CONNECTOR_EXTENSIONS: &[&str] =
    &["md", "txt", "rst", "adoc", "html", "htm", "csv", "json"];
