# RAG2

A Rust-native adaptive retrieval and evidence platform: hybrid retrieval (dense, lexical,
structured, graph), routing, multi-hop, fusion, provenance, contradiction screening, calibrated
confidence, abstention and context assembly, behind one declarative configuration that operators
change without touching code.

One binary. No Python services, no external vector database, no framework runtime.

## Quick start

```sh
cargo build --release -p rag2-cli          # binary: target/release/rag2
rag2 init                                   # writes rag2.yaml + rag2.env (generated keys)
source rag2.env
rag2 serve --file rag2.yaml --watch         # hot-reloads rag2.yaml on save

rag2 ingest -t docs ./my-docs               # filesystem connector, incremental
rag2 query  -t docs "how often are pump seals inspected?"
```

REST, with the generated admin key:

```sh
curl -s localhost:8080/v1/tenants/docs/query \
  -H "authorization: Bearer $RAG2_API_KEY" -H 'content-type: application/json' \
  -d '{"query": "how often are pump seals inspected?", "top_k": 5}'
```

The OpenAPI 3.1 document is served at `/v1/openapi.json` (or `rag2 openapi`).

## Integrate

Pick the one that matches your application; each is one step.

| You have | Do this |
|---|---|
| A Rust program | `rag2 = { git = "https://github.com/hotocoo/rag2" }`, then `let rag = rag2::open("./rag2-data")?;` and `rag.ingest(..)` / `rag.query(..)` ([example](crates/rag2/examples/embed.rs)) |
| An axum / tower server | `.nest("/rag", rag2::router(rag2::open("./rag2-data")?)?)` adds the full API to your app ([example](crates/rag2/examples/mount.rs)) |
| Any other server or language | Run the sidecar (`rag2 serve`, `docker compose up`, Helm, systemd: [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md)) and call REST ([OpenAPI](docs/openapi.json)), gRPC ([proto](crates/rag2-server/proto/rag2.proto)) or the [TypeScript SDK](sdks/typescript) |
| An AI agent / IDE | Point it at the MCP endpoint (`POST /mcp`) or the stdio bridge ([docs/INTEGRATIONS.md](docs/INTEGRATIONS.md)) |

Cargo features keep the build to what you use: `decision` (d1 and Laya over HTTP, no candle),
`models` (`decision` plus candle and HTTP embedders and in-process Laya), `plugins` (Wasmtime), `server` (HTTP/gRPC API), `client` (Rust client),
`metal` / `cuda`. `default-features = false` gives the engine alone. Behavior (retrievers,
routing, gates, limits, auth) is set in the profile, never in code.

## What it does

| Capability | Where it is configured |
|---|---|
| Dense retrieval: exact flat index below 10k vectors, HNSW above (measured, see benchmarks) | `indexes.vector`, `pipelines.*.retrievers` |
| Lexical BM25 (Tantivy), language tokenizers | `indexes.lexical` |
| Structured retrieval and metadata filters | request `filters`, `structured` retriever |
| Graph expansion (adjacent chunks, linked documents) | `pipelines.*.graph` |
| Routing: all, rules over query features, or a decision model (d1, Laya) | `pipelines.*.routing` |
| Multi-hop retrieval with salient-term expansion | `pipelines.*.multi_hop` |
| Fusion: reciprocal rank or weighted normalized scores | `pipelines.*.fusion` |
| Near-duplicate collapse, contradiction screening (negation, numeric, optional model check) | `dedup`, `contradiction` |
| Calibrated confidence and abstention | `calibration`, `abstention` |
| Context assembly with citations and budgets | `context` |
| Embedders: hash baseline, local ModernBERT on candle, any OpenAI-compatible API | `embedders` |
| Decision models: Liquid AI d1 on llama.cpp (System One), Laya in-process or as a sidecar | `decision_models` |
| Incremental ingestion (content hash), atomic per request | automatic |

Every response carries provenance (document, version, chunk, byte span, source), per-retriever
signals, the route taken and why, stage timings, and warnings for any degraded component.

## Operating it

- Profiles are YAML/JSON, validated against a generated JSON Schema (`rag2 config schema`).
- Every change is a stored, immutable version: preview the diff, activate atomically, roll back
  (`rag2 config diff|apply|rollback|history`, or the admin API).
- Environment overlays (`environments.staging`) apply as JSON merge patches.
- Secrets are references (`env:NAME`, `file:/path`), never values.

See [docs/OPERATIONS.md](docs/OPERATIONS.md), [docs/CONFIGURATION.md](docs/CONFIGURATION.md),
[docs/SECURITY.md](docs/SECURITY.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md),
[docs/API.md](docs/API.md).

## Integrating it

- REST + OpenAPI: `/v1/...`
- Rust SDK: `rag2-client`
- Embedded library: `rag2-engine` (`Engine::open`, `ingest`, `query`), no server needed
- CLI: `rag2`
- Deployment: `deploy/Dockerfile`, `deploy/docker-compose.yml`, `deploy/helm/rag2`,
  `deploy/systemd/rag2.service`

## Measured performance

Numbers come only from the reproducible harness in `crates/rag2-bench`; see
[docs/benchmarks](docs/benchmarks). No figure here is a target or an estimate.

## Workspace

| Crate | Role |
|---|---|
| `rag2-core` | Types, tenant ids, versioned component interfaces |
| `rag2-config` | Profiles: schema, validation, versions, activation, rollback, hot reload |
| `rag2-storage` | Tenant-scoped storage on Fjall (default) or redb, encryption at rest |
| `rag2-lexical` | Tantivy BM25 per tenant |
| `rag2-vector` | Flat and HNSW vector indexes |
| `rag2-engine` | Ingestion and the retrieval pipeline |
| `rag2-embed` | HTTP and candle embedders |
| `rag2-decision` | Decision models over HTTP: d1 (System One on llama.cpp), Laya sidecars; fair admission |
| `rag2-laya` | In-process Laya checkpoints (candle) |
| `rag2-modernbert` | ModernBERT encoder on candle with fused CPU kernels |
| `rag2-server` | HTTP service, auth, RBAC, rate limits, audit, metrics |
| `rag2-client` | Rust SDK |
| `rag2` | One-dependency facade: `open`, `router`, feature-gated components |
| `rag2-cli` | The `rag2` binary |
| `rag2-bench` | Benchmarks |

Status and roadmap: [docs/STATUS.md](docs/STATUS.md).

## License

Apache-2.0. The ModernBERT encoder in `rag2-modernbert` is adapted from candle-transformers (MIT
OR Apache-2.0).
