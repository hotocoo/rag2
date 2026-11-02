# Plugins

Plugins add behaviour without forking RAG2: transform documents before indexing, rewrite queries,
rerank results. They are WebAssembly modules run by Wasmtime inside a sandbox.

## Hooks

| Hook | Input (`input` in the envelope) | Output | On plugin failure |
|---|---|---|---|
| `document_transform` | a document `{id, title?, text, source?, metadata?}` | the document, or `null` to drop it (reported as `dropped`) | the ingest request fails (untransformed data is never stored) |
| `query_rewrite` | `{"query": "..."}` | `{"query": "..."}` | rewrite skipped, warning in the response |
| `rerank` | `{"query": "...", "passages": ["..."]}` | `{"scores": [f32, one per passage]}` | fused order kept, warning in the response |

Any hook may return `{"error": "message"}`.

## ABI v1

A core Wasm module exporting:

| Export | Signature | Purpose |
|---|---|---|
| `memory` | memory | shared buffer space |
| `alloc` | `(len: i32) -> i32` | where the host writes the call envelope |
| `rag2_abi_version` | `() -> i32` | must return `1` |
| `rag2_manifest` | `() -> i64` | `(ptr << 32) \| len` of `{"name", "version", "hooks": [...]}` |
| `call` | `(ptr: i32, len: i32) -> i64` | envelope in, `(ptr << 32) \| len` of the JSON result out |

Envelope: `{"abi": 1, "hook": "...", "tenant": "...", "config": <plugin config>, "input": ...}`.

The only import allowed is `env.log(ptr: i32, len: i32)` and only with `allow_log: true`. A module
importing anything else (WASI included) fails to load: plugins have no files, network, clock or
environment.

## Configuration

```yaml
plugins:
  redact:
    path: /plugins/redact_emails.wasm
    sha256: 3f1c...e9          # optional integrity pin; load fails on mismatch
    hooks: [document_transform] # must be declared by the plugin's manifest
    config: { drop_if_contains: CONFIDENTIAL }
    fuel: 200000000             # instructions per call
    memory_mb: 64
    timeout_ms: 250             # wall clock per call
ingest_plugins: [redact]
pipelines:
  default:
    plugins: { query_rewrite: [], rerank: null }
```

Every call gets a fresh instance: no state carries over between calls, documents or tenants.
Exceeding fuel, memory or time stops the call with a clear error.

## Writing one in Rust

`examples/plugins/redact-emails` is a complete plugin (about 100 lines):

```sh
cd examples/plugins/redact-emails
cargo build --release --target wasm32-unknown-unknown
sha256sum target/wasm32-unknown-unknown/release/redact_emails.wasm   # for the sha256 pin
```

Any language that compiles to core Wasm and can read and write JSON in linear memory works.
