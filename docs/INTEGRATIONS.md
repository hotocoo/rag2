# Integration recipes

Copy, set `RAG2_URL` and `RAG2_API_KEY`, run. Generate keys with `rag2 key api`; bind them to roles
and tenants in the profile.

## curl

```sh
curl -s "$RAG2_URL/v1/tenants/docs/documents" -H "authorization: Bearer $RAG2_API_KEY" \
  -H 'content-type: application/json' \
  -d '{"documents":[{"id":"faq.md","text":"Refunds are processed within 5 business days."}]}'

curl -s "$RAG2_URL/v1/tenants/docs/query" -H "authorization: Bearer $RAG2_API_KEY" \
  -H 'content-type: application/json' -d '{"query":"how long do refunds take","top_k":3}'
```

## Rust

```rust
let c = rag2_client::Client::new(&std::env::var("RAG2_URL")?, std::env::var("RAG2_API_KEY").ok())?;
let r = c.query("docs", &rag2_client::QueryRequest::new("how long do refunds take")).await?;
if r.abstained.is_none() { println!("{}", r.context.unwrap_or_default()); }
```

Embedded (no server): see [API.md](API.md#embedded-library-rag2-engine).

## TypeScript / JavaScript

`sdks/typescript` (zero dependencies, uses `fetch`):

```ts
import { Rag2Client } from "@rag2/client";
const rag = new Rag2Client(process.env.RAG2_URL!, process.env.RAG2_API_KEY);
const r = await rag.query("docs", { query: "how long do refunds take", top_k: 3 });
if (!r.abstained) console.log(r.context);
```

## Python (no SDK needed)

```python
import os, requests
r = requests.post(f"{os.environ['RAG2_URL']}/v1/tenants/docs/query",
                  headers={"authorization": f"Bearer {os.environ['RAG2_API_KEY']}"},
                  json={"query": "how long do refunds take", "top_k": 3}, timeout=30)
r.raise_for_status()
print(r.json()["context"])
```

## Other languages

`docs/openapi.json` (also served at `/v1/openapi.json`) is OpenAPI 3.1, e.g.
`openapi-generator-cli generate -i docs/openapi.json -g go -o sdk-go`.

## gRPC

Enable with `server.grpc_listen: 0.0.0.0:9090`; schema in `crates/rag2-server/proto/rag2.proto`.

```sh
grpcurl -import-path crates/rag2-server/proto -proto rag2.proto \
  -H "authorization: Bearer $RAG2_API_KEY" -plaintext \
  -d '{"tenant":"docs","query":"how long do refunds take"}' localhost:9090 rag2.v1.Rag2/Query
```

## MCP (AI assistants)

HTTP clients: `POST $RAG2_URL/mcp` with the bearer key. Desktop clients that launch tools over
stdio:

```json
{
  "mcpServers": {
    "rag2": {
      "command": "rag2",
      "args": ["mcp", "--url", "https://rag2.internal"],
      "env": { "RAG2_API_KEY": "<key with the query permission for the tenants it may search>" }
    }
  }
}
```

Tools: `search` (cited evidence, confidence, abstention), `get_document_chunks`,
`list_documents`. The key's roles and tenants bound what the assistant can see.

## Webhooks (RAG2 calls you)

```yaml
webhooks:
  - name: search-index-sync
    url: https://hooks.example.com/rag2
    events: [documents_ingested, documents_deleted]
    secret: env:RAG2_WEBHOOK_SECRET
```

Verify a delivery: `x-rag2-signature` must equal
`"sha256=" + hex(HMAC_SHA256(secret, x-rag2-timestamp + "." + raw_body))`. Reject timestamps older
than a few minutes to stop replays. `x-rag2-delivery` is unique per event (use it to deduplicate).

## Connectors (RAG2 pulls from you)

```yaml
connectors:
  handbook:
    tenant: docs
    source: { kind: filesystem, path: /data/handbook, extensions: [md, html] }
    interval_s: 600
    delete_missing: true
  tickets:
    tenant: support
    source:
      kind: http_json
      url: https://api.example.com/v2/tickets?status=solved
      headers: { authorization: env:TICKETS_TOKEN }   # the variable holds "Bearer <token>"
      items: /data
      id: /id
      text: /description
      title: /subject
      metadata: { product: /product, priority: /priority }
    interval_s: 900
  status-pages:
    tenant: support
    source: { kind: web, urls: [https://status.example.com/history] }
```

`POST /v1/admin/connectors/{name}/test` checks reachability and credentials without ingesting;
`.../run` syncs now. Status and last result: `GET /v1/admin/connectors` or the console.

## Plugins (extend without forking)

See [PLUGINS.md](PLUGINS.md).
