# Security

## Controls

| Area | Control |
|---|---|
| Authentication | API keys (stored as BLAKE3 hashes, compared in constant time over every key), OIDC bearer tokens (JWKS fetched over TLS, `iss`/`aud`/`exp`/`sub` required, refetch rate-limited) |
| Authorization | Roles map to permissions (`query`, `ingest`, `delete_documents`, `read_documents`, `manage_config`, `manage_tenants`, `read_audit`, `read_metrics`, `manage_plugins`); every principal carries an explicit tenant scope |
| Tenant isolation | `TenantId` is a validated type required by every storage call; keys are `tenant\0...` with `\0` excluded from ids; per-tenant index directories; tests cover prefix collisions |
| Unauthenticated mode | Refused by validation unless `server.listen` is loopback |
| Transport | rustls; TLS 1.2/1.3; optional mTLS (`server.tls.client_ca_path`) |
| Encryption at rest | XChaCha20-Poly1305 per value, random 24-byte nonce, the record key bound as associated data (a ciphertext moved to another key or tenant fails authentication). Keys of records (document ids) are stored in plaintext |
| Secrets | Referenced (`env:`, `file:`), never stored in profiles, never logged; error messages name the reference, not the value |
| Input validation | Strict schemas (`deny_unknown_fields`), size limits on bodies, documents, queries, batches, `top_k`; lenient query parsing never fails on user syntax |
| Abuse | Per-tenant rate limits, global load shedding, timeouts, bounded ingest queue |
| Audit | Writes and admin actions always (queries optionally) recorded in storage with principal, action, tenant, target, outcome; readable with `read_audit` |
| Errors | 5xx bodies are generic; details go to server logs |
| Process | `unsafe_code = "deny"` workspace-wide; the only exceptions are two `mmap` calls for model weights, each with a SAFETY comment; release builds `panic = "abort"` |

## Supply chain

- `Cargo.lock` is committed; builds use `--locked`.
- `cargo audit --deny warnings` passes (2026-11-02). One reviewed exception in `.cargo/audit.toml`:
  `paste` (unmaintained build-time proc-macro via candle → gemm, no vulnerability).
- `deny.toml`: license allow-list, crates.io only, no wildcard versions; CI runs `cargo deny`.
- Native (C) code in the tree, all from established projects: aws-lc-rs (crypto provider for rustls
  and jsonwebtoken, chosen over `rsa`, which carries RUSTSEC-2023-0071), Oniguruma (tokenizer
  regexes, as Laya's runtime uses), Wasmtime's small runtime helpers. Everything else is Rust.
- Dependencies are the latest stable releases at addition time (policy in `Cargo.toml`).

## Known limitations

- Encryption-key rotation requires export and re-ingest; there is no in-place re-encryption tool yet.
- OIDC token revocation is bounded by token lifetime (no introspection endpoint support yet).
- Document ids and metadata index keys are not encrypted at rest.

## Reporting

Report vulnerabilities privately to the maintainers; do not open public issues for them.
