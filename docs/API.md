# Integration

## REST

Base path `/v1`. Authenticate with `Authorization: Bearer <api key or OIDC token>` or
`x-api-key: <key>`. Errors share one envelope:

```json
{ "error": { "code": "invalid_input", "message": "top_k must be in 1..=100" } }
```

Codes: `invalid_input` 400, `unauthenticated` 401, `forbidden` 403, `not_found` 404,
`conflict` 409, `rate_limited` 429, `overloaded`/`unavailable` 503 (both with `Retry-After`).
Every response carries `x-request-id` (echoed if the client sends one).

| Method | Path | Permission |
|---|---|---|
| POST | `/v1/tenants/{t}/query` | query |
| POST | `/v1/tenants/{t}/documents` | ingest |
| GET | `/v1/tenants/{t}/documents?after=&limit=` | read_documents |
| GET/DELETE | `/v1/tenants/{t}/documents/{id}` (percent-encode `id`) | read_documents / delete_documents |
| GET | `/v1/tenants/{t}/documents/{id}/chunks` | read_documents |
| GET | `/v1/tenants/{t}/stats` | read_documents |
| GET | `/v1/admin/config`, `/schema`, `/versions`, `/versions/{v}` | manage_config |
| POST | `/v1/admin/config/preview`, `/versions`, `/versions/{v}/activate`, `/rollback` | manage_config |
| GET/DELETE/POST | `/v1/admin/tenants`, `/tenants/{t}?confirm={t}`, `/tenants/{t}/reindex` | manage_tenants |
| GET | `/v1/admin/audit?after=&limit=` | read_audit |
| GET | `/v1/admin/components`, `/metrics` | read_metrics |
| GET | `/healthz`, `/readyz`, `/v1/openapi.json` | none |

### Query

```json
POST /v1/tenants/docs/query
{ "query": "maximum pump pressure", "top_k": 5, "filters": { "dept": ["ops", "safety"] }, "hops": 1 }
```

Response (abridged):

```json
{
  "evidence": [{
    "chunk_id": 42, "score": 0.0328, "text": "...",
    "signals": [{ "retriever": "lexical", "rank": 1, "score": 7.1, "hop": 0 },
                { "retriever": "dense", "rank": 2, "score": 0.81, "hop": 0 }],
    "provenance": { "doc_id": "manual.md", "doc_version": 3, "chunk_ordinal": 4,
                    "start": 1830, "end": 2950, "source": "/docs/manual.md", "title": "manual" }
  }],
  "confidence": 0.74,
  "confidence_detail": { "raw": 0.74, "dense_similarity": 0.81, "term_coverage": 0.67, "agreement": 1.0 },
  "contradictions": [{ "a": 42, "b": 57, "detector": "numeric", "reason": "120 vs 150 bar", "confidence": 0.7 }],
  "context": "[1] (manual.md) ...",
  "route": { "retrievers": ["dense", "lexical"], "reason": "all enabled retrievers" },
  "pipeline": "default", "hops": 1, "config_version": 7, "cached": false,
  "timings": { "route_us": 3, "retrieve_us": 410, "fuse_us": 12, "analyze_us": 95, "assemble_us": 4, "total_us": 530 }
}
```

`abstained` appears (and `context` is omitted) when the engine declines: `no_evidence`,
`low_confidence`, `contradictory` or `decision_model`.

### Ingest

```json
POST /v1/tenants/docs/documents
{ "documents": [{ "id": "manual.md", "title": "Manual", "text": "...", "source": "s3://bucket/manual.md",
                  "metadata": { "dept": "ops", "links": ["appendix.md"] } }] }
```

A request is atomic: every document is stored or none is. Unchanged documents (same content
hash) report `unchanged` and cost nothing. `searchable_within_ms` says when lexical search sees
them (0 = immediately).

## Rust SDK (`rag2-client`)

```rust
let c = rag2_client::Client::new("https://rag2.internal", Some(std::env::var("RAG2_API_KEY")?))?;
let r = c.query("docs", &rag2_client::QueryRequest::new("maximum pump pressure")).await?;
```

`ClientError::is_retryable()` identifies 429/503/timeouts.

## Embedded library (`rag2-engine`)

No server, same behaviour:

```rust
let store = Arc::new(rag2_config::FsConfigStore::open("./config")?);
let cfg = Arc::new(rag2_config::ConfigManager::open(store, "main", None, || rag2_config::Profile::minimal("main"), 100)?);
let engine = rag2_engine::Engine::open(cfg, vec![Arc::new(rag2_embed::EmbedFactory)])?;
engine.recover_all()?;
let t = rag2_core::TenantId::new("docs")?;
engine.ingest(&t, docs)?;
let answer = engine.query(&t, rag2_engine::QueryRequest::new("maximum pump pressure"))?;
```

## Other languages

Generate a client from `/v1/openapi.json` (OpenAPI 3.1) with any generator, e.g.
`openapi-generator-cli generate -i openapi.json -g typescript-fetch -o sdk-ts`.
