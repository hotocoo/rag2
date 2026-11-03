# RAG2 status

Resume point for any new working session. Update the checkboxes as work lands.

## Decision models

- Primary: Liquid AI d1-2B (Q8_0 GGUF) on llama.cpp 0.8.0 (`/v1/systemone`), adapter
  `SystemOneHttp` (`kind: system_one`). Verified against the live model on 2026-11-03.
- Laya: in-process candle runtime + HTTP adapter. No Laya checkpoint on this machine, so the
  in-process path is untested end to end (`RAG2_LAYA_CHECKPOINT`); no accuracy claim is made.
- embedvec 0.10 default features pull fjall 2.x (conflicts with fjall 3); used with
  `default-features = false`.

## Decisions so far

- Vector: hnsw_rs behind `HnswIndex` (tombstones, clamped `UnitCosine`), exact `FlatIndex` below
  10k vectors. embedvec rejected: recall@10 0.31-0.39 at 100k. Numbers: docs/benchmarks/vector-index.md
- Storage: `KvBackend` trait, Fjall default, redb alternative (both pass the same tests). Keys
  `tenant\0...`; optional XChaCha20-Poly1305 per value with key-bound AAD. Per-tenant write
  `generation` bumped atomically with every write; derived indexes compare it at startup.
- Config: profiles in `FsConfigStore` (immutable versions, ACTIVE pointer, activations.jsonl),
  `ConfigManager` with ArcSwap snapshot, vetoes, preview diff, rollback, env overlays, hot reload.
- Lexical: Tantivy per tenant, commit payload = storage generation, lenient parsing.
- Engine: chunker (boundary-aware, strict end progress), hash embedder baseline, per-(tenant,
  embedder) vector index with flat->HNSW promotion, RRF/weighted fusion, rules/decision routing,
  metadata filters + structured retriever, graph expansion (adjacent chunks + `links`), multi-hop
  (tf-idf expansion), dedup, negation/numeric contradiction screening (+ optional decision-model
  confirmation), logistic calibration, abstention, context templates, generation-keyed cache.
- Server: axum, API keys (blake3 + constant-time) and OIDC (JWKS, aws-lc-rs), RBAC + tenant scope,
  per-tenant governor limits, load shedding, timeouts, body limits, ingest queue backpressure,
  audit trail in storage, Prometheus metrics, OpenAPI 3.1 generated from schemars types, TLS/mTLS
  via rustls. utoipa NOT used (schemas come from the same schemars derives).
- SDK: `rag2-client` (reqwest). CLI `rag2`: serve, init (profile + keys), config validate/diff/
  apply/rollback/history/export/schema, key api/encryption, openapi, query, ingest (filesystem
  connector with retry/backoff), status. Live E2E verified 2026-11-02.
- Laya: in-process runtime on candle 0.11.1 (only a scoped `#[allow(unsafe_code)]` on the mmap);
  10 unit tests pass; adapter = LayaLocal/LayaHttp + LayaFactory, answers validated.
- Embedders: `rag2-embed` (HTTP OpenAI-compatible, mock-tested; candle ModernBERT via Laya's
  encoder, verified with nomic-ai/modernbert-embed-base forward pass).
- Replication: per-tenant log written atomically with data on primaries (epoch + seq),
  `/v1/internal/replication/*` (shared key), replicas pull/apply exactly-once with chunk ids and
  vectors preserved, epoch change resets the copy, writes on replicas 307 to the primary.
  Verified live with two processes. No consensus: promotion = change role + restart.
- Supply chain: cargo audit clean (464 crates, 0 advisories) on 2026-11-02.
- Known limits: structured retriever scans at most top_k docs per filter value before
  intersecting; filters post-filter fused candidates (no vector prefiltering yet); log trimming
  is manual (`trim_log`) and a replica behind a trimmed log needs a data copy.
- Plugins: `rag2-plugins` Wasmtime host, core-wasm JSON ABI v1, hooks document_transform /
  query_rewrite / rerank, no WASI, env.log only when granted, fuel + memory + epoch deadline,
  sha256 pin, fresh instance per call. Tested with WAT plugins incl. abuse cases.
- Interfaces: gRPC (`proto/rag2.proto`, tonic 0.14, same auth), MCP (`POST /mcp`, JSON-RPC,
  tools search/get_document_chunks/list_documents), webhooks (HMAC-SHA256, bounded queue, retry).
- Console: Leptos 0.8 CSR (`console/`, trunk), verified in Chrome: login, overview, pipeline
  edit -> server preview -> apply (new version authored by the key), playground, docs, audit.
  Toolchain pinned to rustup 1.99.1 (`rust-toolchain.toml`) for wasm32.
- Calibration: rag2-bench fits Platt (Newton + ridge) on half the queries; held-out ECE
  0.31-0.37 -> 0.07-0.11 on SciFact (hash embedder runs).
- OIDC verified with mock JWKS; alg/key-type allow-list prevents algorithm confusion.
- Repository: github.com/hotocoo/rag2 (private), branch `main`.
- Supply chain: `cargo deny check licenses bans sources` passes (0BSD allowed for varint-rs via
  fjall; internal path deps exempt from wildcard rule); `cargo audit --deny warnings` passes with
  one reviewed exception (`paste`, unmaintained, build-time only).
- Tests never mutate the process environment (secrets via `file:` references).

## Known gaps (unmeasured or unbuilt, not hidden)

- Coverage (cargo llvm-cov, 2026-11-03, 171 tests incl. live d1 and ModernBERT tests via
  `RAG2_SYSTEMONE` / `RAG2_EMBED_MODEL_DIR`): 84.46% lines, 82.43% regions for the shipped crates;
  75.77% lines including `rag2-bench` (exercised by benchmark runs, not unit tests). Lowest:
  `rag2-laya/src/model.rs` 0% (needs a Laya checkpoint), `rag2-server/src/lib.rs` 50% (TLS and
  gRPC serving paths).
- The console has no automated tests; verified in Chrome (login, pipeline edit -> preview ->
  apply, playground, connectors build) and served with d1 on 2026-11-03.
- Laya numerical parity on candle 0.11 unmeasured (no checkpoint on this machine).
- No automatic failover; promotion is orchestrator/operator driven by design.
- llama.cpp 0.8.0 serializes d1 calls and re-reads the state per question: decision throughput
  is about 1 / (0.39 ms x input tokens) per server. Scale d1 by running more servers (one
  decision model entry each), not more slots.
- CI cold builds compile candle + wasmtime: expect 10+ minutes for the test job.

## Integration (2026-11-03)

- `rag2` facade crate: `rag2::open(dir)`, `open_with_profile(_text)`, `rag2::router(engine)`
  (mount into any axum/tower app), `rag2::factories()`; features `decision`, `models`,
  `plugins`, `server`, `client`, `metal`, `cuda`. Examples: crates/rag2/examples.
- Crates split for modular builds: `rag2-decision` (d1 System One + Laya HTTP, FIFO admission,
  no candle), `rag2-modernbert` (encoder + fused kernels), `rag2-laya` (in-process Laya only).
- One-command d1 stack: `docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.d1.yml up`.
- Evaluation of d1-2B Q8_0 under extreme load: docs/benchmarks/decision-d1.md.

## Phases

### Phase 1: vertical slice
- [x] Workspace, lints, dependency policy
- [x] rag2-core: types, tenant id, versioned traits
- [x] Vector index benchmark (embedvec vs hnsw_rs vs flat SIMD) -> docs/benchmarks/vector-index.md
- [x] rag2-vector (choice from the benchmark)
- [x] rag2-config: schema, validation, versioned profiles, atomic activation, rollback, hot reload
- [x] rag2-storage: Fjall, tenant-scoped API, isolation test
- [x] rag2-lexical: Tantivy per tenant
- [x] rag2-engine: routing, hybrid retrieval, fusion, dedup, calibration, abstention, context assembly
- [x] rag2-server: REST + OpenAPI, health, readiness, metrics
- [x] rag2-cli
- [x] End-to-end ingest -> query test

### Phase 2: platform
- [x] Decision models: d1 (System One, llama.cpp), Laya in-process and HTTP
- [x] Embedding providers (candle local, HTTP), config-selected
- [x] Security: OIDC/JWT, API keys, RBAC/policy, audit, rate limit, encrypted secrets, TLS/mTLS
- [x] Plugins: Wasmtime/WASI host, capabilities, quotas
- [x] Connectors: filesystem, web, http_json (scheduled, test/run endpoints, console page)
- [x] gRPC (tonic), MCP, webhooks
- [x] Admin console (Leptos CSR)
- [x] SDKs: Rust client, TypeScript client, OpenAPI 3.1 export (docs/openapi.json), MCP stdio bridge

### Phase 3: production
- [x] Replication: primary log + pulling replicas (read scaling, hot standby), write redirect
- [x] Tenant partitioning across primary groups (cluster.tenant_routes, 307 redirect)
- Not planned: automatic failover / consensus (orchestrator-driven promotion, documented)
- [x] Benchmarks: vector index, retrieval quality/latency/throughput (SciFact), calibration
- [x] Benchmarks: ingest/query interference, recovery (cold start, index loss, kill -9 crash test)
- [x] Deployment: Dockerfile, compose, Helm, systemd
- [x] Docs: operator guide, security, upgrade/migration, API, deployment, configuration
- [x] Supply chain: cargo-audit, cargo-deny, SBOM
