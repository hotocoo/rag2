# Deployment topologies

| Topology | When | How |
|---|---|---|
| Embedded | inside a Rust service, no network hop | `rag2-engine` as a library (see API.md) |
| Standalone | one node | `rag2 serve` (`cluster.role: standalone`, the default) |
| Primary + replicas | read scaling, hot standby | one `primary`, N `replica` nodes |
| Partitioned | tenants beyond one node | several primary groups, ingress routes by tenant path |

## Primary and replicas

```yaml
# primary
cluster: { role: primary, node_id: p1, replication_key: env:RAG2_REPL_KEY }
# each replica
cluster: { role: replica, node_id: r1, primary_url: https://p1.internal:8443,
           replication_key: env:RAG2_REPL_KEY, poll_interval_ms: 500, batch: 64 }
```

- The primary appends every write (ingest, delete) to a per-tenant log in the same atomic batch as
  the data. Replicas pull `/v1/internal/replication/...` with the shared key and apply entries
  exactly once, chunk ids and vectors included (no re-embedding on replicas).
- Replicas serve queries and reads. Writes sent to a replica get `307 Temporary Redirect` to the
  primary (method and body preserved), so a load balancer can spread all traffic across all nodes.
- Lag per tenant: `rag2_replication_lag_entries{tenant}`; failures:
  `rag2_replication_errors_total`; redirects: `rag2_replica_redirects_total`.
- Replicas are rebuildable: wipe a replica's data directory and it replays the log from the start.
- Every node runs the same profile except `cluster.*` (deploy it with your GitOps tooling).
  Configuration is per node; activate the same version on each.

### Failover

There is no built-in consensus or automatic failover (deliberately: split-brain safety is
left to the orchestrator that owns fencing). Promotion is an operator or orchestrator action:

1. Stop writes to the failed primary (fence it).
2. Pick the replica with the lowest `rag2_replication_lag_entries`.
3. Change its `cluster.role` to `primary` and restart it.
4. Point the other replicas' `cluster.primary_url` at it and restart them.

A replica keeps its applied position and chunk-id counter, so it continues the log without id
collisions. Writes acknowledged by the old primary but not yet pulled are lost; keep the poll
interval short and monitor lag if that matters.

### Log retention

The log grows with writes until trimmed (`Storage::trim_log`, used after all replicas pass a
sequence number). A replica that falls behind a trimmed log needs a data copy from a healthy node.

## Partitioning by tenant

Run one primary group (primary + replicas) per tenant group. Two ways to route:

Built in: give every node the same routing map; a node redirects (307) requests for tenants that
another group owns, so any load balancer can front all groups.

```yaml
cluster:
  self_url: https://group-a.internal:8443      # differs per group
  tenant_routes:
    acme: https://group-a.internal:8443
    globex: https://group-b.internal:8443
    "*": https://group-b.internal:8443          # everyone else
```

At the ingress, by path:

```yaml
# Kubernetes Ingress (excerpt)
- path: /v1/tenants/acme
  backend: { service: { name: rag2-group-a, port: { number: 8080 } } }
- path: /v1/tenants/globex
  backend: { service: { name: rag2-group-b, port: { number: 8080 } } }
```

Tenants never share data across groups, so groups scale independently.

## Sizing notes

Measure on your own data with `rag2-bench retrieval` before sizing (results in
`docs/benchmarks`). Vector indexes live in memory: about `vectors x dim x 4` bytes plus graph
overhead for HNSW (see the vector benchmark for measured figures).
