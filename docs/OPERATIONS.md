# Operations guide

## Install

| Target | How |
|---|---|
| Binary | `cargo build --release --locked -p rag2-cli` → `target/release/rag2` (features: `models` default; `metal`, `cuda`) |
| Container | `docker build -f deploy/Dockerfile -t rag2 .` (distroless, non-root) |
| Compose | `deploy/docker-compose.yml` |
| Kubernetes | `helm install rag2 deploy/helm/rag2 --set existingSecret=rag2-secrets` |
| systemd | `deploy/systemd/rag2.service` (hardened unit) |

First run: `rag2 init` writes `rag2.yaml` (a valid profile with API-key auth and encryption at
rest) and `rag2.env` (generated `RAG2_ADMIN_KEY`, `RAG2_ENCRYPTION_KEY`, mode 0600). Move those
values into your secret manager for production.

## Configuration lifecycle

```sh
rag2 config validate rag2.yaml [--env staging]   # schema + semantic checks, exit 1 on error
rag2 config diff rag2.yaml                        # what would change against the active version
rag2 config apply rag2.yaml --comment "raise top_k"
rag2 config history                               # * marks the active version
rag2 config rollback                              # re-activate the previous version
rag2 config export > current.yaml
rag2 config schema > rag2.schema.json             # for editors / CI
```

The same operations exist over HTTP (`/v1/admin/config/...`, permission `manage_config`) and are
audited. `rag2 serve --file rag2.yaml --watch` hot-reloads the file: a valid edit activates as a
new version, an invalid one is logged and ignored.

Settings applied without restart: pipelines, routing, fusion, thresholds, embedders, decision
models, tenants and quotas, roles, keys, audit options, cache TTL. Settings refused at runtime
(change them and restart): `storage.*`, `server.*`, `limits.ingest_queue_depth`,
`limits.ingest_workers`.

## Secrets

Profiles reference secrets: `env:NAME` or `file:/absolute/path` (Kubernetes/Docker secrets).
API keys must be at least 24 characters (`rag2 key api` generates them). The storage encryption
key is 32 bytes as 64 hex characters (`rag2 key encryption`).

## Health, readiness, metrics

| Endpoint | Meaning |
|---|---|
| `/healthz` | process is alive |
| `/readyz` | startup recovery finished (indexes consistent with storage); 503 before |
| `/metrics` | Prometheus text; requires `read_metrics` unless `server.public_metrics: true` |
| `/v1/admin/components` | loaded embedders, decision-model health |

Metrics: `rag2_http_requests_total{method,path,status}`, `rag2_http_request_duration_seconds`,
`rag2_http_shed_total`, `rag2_query_duration_seconds{outcome}`, `rag2_query_abstained_total`,
`rag2_query_cache_hits_total`, `rag2_ingest_duration_seconds`, `rag2_ingested_documents_total`.

Logs: `RAG2_LOG=info|debug|rag2_engine=debug`, `--log-format json`. Audit events are also logged
under target `rag2::audit`.

## Backpressure behaviour

| Condition | Response |
|---|---|
| More than `server.max_concurrent_requests` in flight | 503 + `Retry-After` |
| Tenant over `requests_per_second` / `burst` | 429 + `Retry-After` |
| Ingest queue full (`limits.ingest_queue_depth`) | 503 + `Retry-After` |
| Request slower than `server.request_timeout_ms` | 504 |
| Decision model or embedder down | query continues on the fallback route; `warnings` explain |

The SDK and CLI retry 429/503 with exponential backoff.

## Data, backup and restore

All state lives under `storage.data_dir` (`storage/` and `lexical/`) plus the config directory.
Lexical and vector indexes are derived: deleting `lexical/` is safe, it is rebuilt at startup.

Backup: stop the service (or snapshot the volume atomically), copy `data_dir` and the config
directory. Restore: put them back and start; `/readyz` turns green after recovery.

Tenant deletion is irreversible: `DELETE /v1/admin/tenants/{t}?confirm={t}`.

Re-embedding after an embedder change: `POST /v1/admin/tenants/{t}/reindex`.

## Upgrades

- Profiles carry `api_version: rag2/v1`. A future schema version ships with a migration; a
  binary refuses profiles it does not understand instead of guessing.
- Storage keys and records are versioned by layout; upgrade notes list any required reindex.
- Rolling back the binary: restore the previous binary; profiles are backward compatible within
  `rag2/v1`.

## Troubleshooting

| Symptom | Check |
|---|---|
| `/readyz` stays 503 | logs for `recovery failed`; storage path permissions |
| Dense results missing after changing an embedder | `warnings` in the response; run reindex |
| 401 with a valid key | key shorter than 24 chars is refused at activation; check `env:` resolves |
| Lexical results lag ingest | `indexes.lexical.commit_interval_ms` (0 = synchronous) |
| Activation refused | the error names the field path; `rag2 config validate` reproduces it |
