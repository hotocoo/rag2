# Configuration reference

A profile is one YAML or JSON document. The authoritative field reference is the JSON Schema
generated from the code (`rag2 config schema`, `GET /v1/admin/config/schema`); every default
lives in `crates/rag2-config/src/defaults.rs`. Unknown fields are errors.

Working examples, all validated in CI (`crates/rag2-config/tests/examples.rs`):

| File | Shows |
|---|---|
| `examples/profiles/local-hybrid.yaml` | loopback dev, rule routing, multi-hop, graph |
| `examples/profiles/production-oidc.yaml` | OIDC + API keys, mTLS, encryption, quotas, overlays |
| `examples/profiles/laya-routing.yaml` | Laya decision routing and relevance gate |
| `examples/profiles/openai-embeddings.yaml` | OpenAI-compatible embeddings |
| `examples/profiles/modernbert-local.yaml` | local ModernBERT embeddings on candle |

## Sections

| Section | Purpose | Runtime change |
|---|---|---|
| `server` | listen address, TLS/mTLS, timeouts, concurrency, body size, CORS, console dir | restart |
| `storage` | backend (`fjall`, `redb`), data dir, cache, encryption key | restart |
| `limits` | max top_k, query length, document size, batch size, ingest queue and workers | live (ingest pool: restart) |
| `chunking` | max/overlap/min characters | live (new documents) |
| `indexes.vector` | flat/HNSW threshold, HNSW `m`, `ef_construction`, `ef_search`, tombstone ratio | live |
| `indexes.lexical` | tokenizer (`default`, `en_stem`, `whitespace`), writer heap, commit interval | live |
| `embedders.<name>` | `hash`, `candle`, `http` | live (reindex if dimension changes) |
| `decision_models.<name>` | `laya_local`, `laya_http` | live |
| `pipelines.<name>` | retrievers, routing, fusion, dedup, multi-hop, contradiction, calibration, abstention, context, graph, relevance gate | live |
| `default_pipeline` | pipeline when none is named | live |
| `cache` | query cache size/TTL, chunk and document caches | live (TTL) |
| `security` | auth mode, roles, audit | live |
| `tenants`, `tenant_defaults` | per-tenant pipeline, rate, burst, document quota, enabled | live |
| `environments.<env>` | JSON merge patch applied with `--env <env>` | at activation |

## Pipelines in brief

```yaml
pipelines:
  default:
    retrievers:            # each kind at most once; weight scales its fusion contribution
      - { kind: dense, embedder: default, top_k: 50, ef_search: 128 }
      - { kind: lexical, top_k: 50 }
      - { kind: structured, top_k: 20 }   # driven by request filters
      - { kind: graph, top_k: 10, weight: 0.5 }
    routing: { mode: all }                 # or rules / decision
    fusion: { method: rrf, rrf_k: 60, candidate_pool_factor: 3 }
    top_k: 10
    dedup: { enabled: true, near_duplicate_similarity: 0.92 }
    multi_hop: { extra_hops: 0, max_hops: 1, expansion_terms: 8 }
    contradiction: { enabled: true, min_overlap: 0.5, numeric_tolerance: 0.01 }
    calibration: { slope: 1.0, intercept: 0.0 }      # fit on labelled queries
    abstention: { enabled: true, min_confidence: 0.25, min_evidence: 1, on_contradiction: false }
    context: { max_chars: 12000, max_chunks: 8, template: "[{n}] ({doc_id}) {text}", separator: "\n\n" }
    graph: { seeds: 5, neighbors: 1, decay: 0.5, link_field: links }
```

Routing conditions for `mode: rules`: `max_words`, `min_words`, `has_quoted_phrase`,
`has_identifier`, `matches` (regex), `has_filters`. Template placeholders: `{n}`, `{text}`,
`{doc_id}`, `{title}`, `{source}`, `{score}`.

## Confidence

`raw` is the mean of the available signals: best dense similarity (when dense ran), share of the
query's content words found in the top `abstention.coverage_top_n` passages, and the share of
retrievers that agree on the top result. It is a heuristic score until `calibration.slope` and
`intercept` are fitted on labelled queries (Platt scaling).
