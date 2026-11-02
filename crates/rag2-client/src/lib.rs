//! Rust SDK for the RAG2 REST API. Request and response types are the engine's own, so the SDK
//! cannot drift from the server.
//!
//! ```no_run
//! # async fn demo() -> Result<(), rag2_client::ClientError> {
//! let c = rag2_client::Client::new("http://127.0.0.1:8080", Some("my-api-key".into()))?;
//! let r = c.query("acme", &rag2_client::QueryRequest::new("how often are pump seals inspected")).await?;
//! for e in r.evidence {
//!     println!("{:.3} {} {}", e.score, e.provenance.doc_id, e.text);
//! }
//! # Ok(()) }
//! ```

use rag2_core::model::{Document, DocumentRecord};
pub use rag2_engine::{
    ChunkView, IngestReport, IngestRequest, IngestStatus, QueryRequest, QueryResponse, TenantStats,
};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Default per-request timeout; override with [`Client::with_timeout`].
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("{status} {code}: {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("invalid argument: {0}")]
    Invalid(String),
}

impl ClientError {
    /// True for errors worth retrying with backoff (rate limited, overloaded, unavailable).
    pub fn is_retryable(&self) -> bool {
        match self {
            ClientError::Api { status, .. } => *status == 429 || *status == 503,
            ClientError::Transport(e) => e.is_timeout() || e.is_connect(),
            ClientError::Invalid(_) => false,
        }
    }
}

#[derive(Clone)]
pub struct Client {
    base: String,
    key: Option<String>,
    http: reqwest::Client,
}

/// Percent-encode one path segment (document ids may contain `/`, spaces, `#`...).
fn segment(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Client {
    pub fn new(base_url: &str, api_key: Option<String>) -> Result<Self, ClientError> {
        Self::with_timeout(base_url, api_key, DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(
        base_url: &str,
        api_key: Option<String>,
        timeout: Duration,
    ) -> Result<Self, ClientError> {
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(ClientError::Invalid(
                "base URL must start with http:// or https://".into(),
            ));
        }
        Ok(Client {
            base: base_url.trim_end_matches('/').to_string(),
            key: api_key,
            http: reqwest::Client::builder().timeout(timeout).build()?,
        })
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, ClientError> {
        let mut rb = self.http.request(method, format!("{}{path}", self.base));
        if let Some(k) = &self.key {
            rb = rb.bearer_auth(k);
        }
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let resp = rb.send().await?;
        let status = resp.status();
        if status == StatusCode::NO_CONTENT {
            return serde_json::from_value(serde_json::Value::Null)
                .map_err(|e| ClientError::Invalid(format!("unexpected empty response: {e}")));
        }
        let bytes = resp.bytes().await?;
        if !status.is_success() {
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            return Err(ClientError::Api {
                status: status.as_u16(),
                code: v["error"]["code"].as_str().unwrap_or("unknown").into(),
                message: v["error"]["message"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| String::from_utf8_lossy(&bytes).into()),
            });
        }
        serde_json::from_slice(&bytes)
            .map_err(|e| ClientError::Invalid(format!("response did not parse: {e}")))
    }

    fn json<T: serde::Serialize>(v: &T) -> Result<serde_json::Value, ClientError> {
        serde_json::to_value(v).map_err(|e| ClientError::Invalid(e.to_string()))
    }

    pub async fn query(
        &self,
        tenant: &str,
        req: &QueryRequest,
    ) -> Result<QueryResponse, ClientError> {
        self.send(
            Method::POST,
            &format!("/v1/tenants/{}/query", segment(tenant)),
            Some(Self::json(req)?),
        )
        .await
    }

    pub async fn ingest(
        &self,
        tenant: &str,
        documents: Vec<Document>,
    ) -> Result<IngestReport, ClientError> {
        let req = IngestRequest { documents };
        self.send(
            Method::POST,
            &format!("/v1/tenants/{}/documents", segment(tenant)),
            Some(Self::json(&req)?),
        )
        .await
    }

    pub async fn list_documents(
        &self,
        tenant: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DocumentRecord>, ClientError> {
        let mut path = format!("/v1/tenants/{}/documents?limit={limit}", segment(tenant));
        if let Some(a) = after {
            path.push_str(&format!("&after={}", segment(a)));
        }
        self.send(Method::GET, &path, None).await
    }

    pub async fn get_document(
        &self,
        tenant: &str,
        id: &str,
    ) -> Result<DocumentRecord, ClientError> {
        self.send(
            Method::GET,
            &format!("/v1/tenants/{}/documents/{}", segment(tenant), segment(id)),
            None,
        )
        .await
    }

    pub async fn document_chunks(
        &self,
        tenant: &str,
        id: &str,
    ) -> Result<Vec<ChunkView>, ClientError> {
        self.send(
            Method::GET,
            &format!(
                "/v1/tenants/{}/documents/{}/chunks",
                segment(tenant),
                segment(id)
            ),
            None,
        )
        .await
    }

    pub async fn delete_document(&self, tenant: &str, id: &str) -> Result<(), ClientError> {
        let _: serde_json::Value = self
            .send(
                Method::DELETE,
                &format!("/v1/tenants/{}/documents/{}", segment(tenant), segment(id)),
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn stats(&self, tenant: &str) -> Result<TenantStats, ClientError> {
        self.send(
            Method::GET,
            &format!("/v1/tenants/{}/stats", segment(tenant)),
            None,
        )
        .await
    }

    pub async fn active_config(&self) -> Result<serde_json::Value, ClientError> {
        self.send(Method::GET, "/v1/admin/config", None).await
    }

    /// Validate and diff a profile (YAML or JSON text) against the running one.
    pub async fn preview_config(&self, text: &str) -> Result<serde_json::Value, ClientError> {
        self.send(
            Method::POST,
            "/v1/admin/config/preview",
            Some(serde_json::json!({ "text": text })),
        )
        .await
    }

    /// Store a profile version, optionally activating it.
    pub async fn submit_config(
        &self,
        text: &str,
        comment: &str,
        activate: bool,
    ) -> Result<serde_json::Value, ClientError> {
        self.send(
            Method::POST,
            "/v1/admin/config/versions",
            Some(serde_json::json!({ "text": text, "comment": comment, "activate": activate })),
        )
        .await
    }

    pub async fn activate_config(&self, version: u64) -> Result<serde_json::Value, ClientError> {
        self.send(
            Method::POST,
            &format!("/v1/admin/config/versions/{version}/activate"),
            None,
        )
        .await
    }

    pub async fn rollback_config(&self) -> Result<serde_json::Value, ClientError> {
        self.send(Method::POST, "/v1/admin/config/rollback", None)
            .await
    }

    pub async fn tenants(&self) -> Result<Vec<String>, ClientError> {
        self.send(Method::GET, "/v1/admin/tenants", None).await
    }

    /// True when `/readyz` answers 200.
    pub async fn ready(&self) -> Result<bool, ClientError> {
        Ok(self
            .http
            .get(format!("{}/readyz", self.base))
            .send()
            .await?
            .status()
            .is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(segment("docs/a b#1"), "docs%2Fa%20b%231");
        assert_eq!(segment("plain-id_1.md"), "plain-id_1.md");
    }

    #[test]
    fn base_urls_must_be_http() {
        assert!(Client::new("ftp://x", None).is_err());
        assert!(Client::new("http://127.0.0.1:8080/", None).is_ok());
    }

    #[test]
    fn retryable_errors_are_classified() {
        let e = |status| ClientError::Api {
            status,
            code: String::new(),
            message: String::new(),
        };
        assert!(e(429).is_retryable() && e(503).is_retryable());
        assert!(!e(400).is_retryable() && !e(403).is_retryable());
    }
}
