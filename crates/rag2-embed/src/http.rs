//! OpenAI-compatible embeddings over HTTP: `POST {endpoint}/embeddings` with
//! `{"model", "input": [...]}`, answered by `{"data": [{"index", "embedding"}]}`. Works with
//! OpenAI, Azure OpenAI-compatible gateways, vLLM, Ollama, LM Studio, TEI and llama.cpp servers.

use rag2_core::interfaces::Embedder;
use rag2_core::{Error, Result};
use std::time::Duration;

pub struct HttpEmbedder {
    id: String,
    url: String,
    model: String,
    dim: usize,
    key: Option<String>,
    batch: usize,
    http: reqwest::blocking::Client,
}

impl HttpEmbedder {
    pub fn new(
        name: &str,
        endpoint: &str,
        model: &str,
        dim: usize,
        key: Option<String>,
        timeout: Duration,
        batch: usize,
    ) -> Result<Self> {
        if dim == 0 || batch == 0 {
            return Err(Error::InvalidInput(
                "http embedder needs dim > 0 and batch > 0".into(),
            ));
        }
        Ok(HttpEmbedder {
            id: format!("http:{name}:{model}:{dim}"),
            url: format!("{}/embeddings", endpoint.trim_end_matches('/')),
            model: model.into(),
            dim,
            key,
            batch,
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()
                .map_err(Error::internal)?,
        })
    }

    fn call(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut rb = self
            .http
            .post(&self.url)
            .json(&serde_json::json!({ "model": self.model, "input": texts }));
        if let Some(k) = &self.key {
            rb = rb.bearer_auth(k);
        }
        let resp = rb
            .send()
            .map_err(|e| Error::Unavailable(format!("embedder {}: {e}", self.id)))?;
        let status = resp.status();
        let v: serde_json::Value = resp
            .json()
            .map_err(|e| Error::Unavailable(format!("embedder {}: {e}", self.id)))?;
        if !status.is_success() {
            // Provider error bodies can echo request content; keep only the status and message.
            let msg = v["error"]["message"]
                .as_str()
                .or(v["error"].as_str())
                .unwrap_or("request refused");
            return Err(Error::Unavailable(format!(
                "embedder {} returned {status}: {msg}",
                self.id
            )));
        }
        let data = v["data"]
            .as_array()
            .ok_or_else(|| Error::Unavailable(format!("embedder {}: no data", self.id)))?;
        let mut out = vec![Vec::new(); texts.len()];
        for (pos, item) in data.iter().enumerate() {
            let i = item["index"].as_u64().map_or(pos, |x| x as usize);
            let emb: Vec<f32> =
                serde_json::from_value(item["embedding"].clone()).map_err(|_| {
                    Error::Unavailable(format!("embedder {}: malformed embedding", self.id))
                })?;
            if i >= out.len() || emb.len() != self.dim {
                return Err(Error::Unavailable(format!(
                    "embedder {}: expected {} vectors of dim {}, got index {i} dim {}",
                    self.id,
                    texts.len(),
                    self.dim,
                    emb.len()
                )));
            }
            out[i] = emb;
        }
        if out.iter().any(Vec::is_empty) {
            return Err(Error::Unavailable(format!(
                "embedder {}: missing vectors",
                self.id
            )));
        }
        for v in out.iter_mut() {
            crate::normalize(v);
        }
        Ok(out)
    }
}

impl Embedder for HttpEmbedder {
    fn id(&self) -> &str {
        &self.id
    }
    fn dim(&self) -> usize {
        self.dim
    }
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for group in texts.chunks(self.batch) {
            out.extend(self.call(group)?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::http::HeaderMap;
    use axum::routing::post;

    async fn mock() -> String {
        let app = axum::Router::new().route(
            "/v1/embeddings",
            post(|h: HeaderMap, Json(v): Json<serde_json::Value>| async move {
                if h.get("authorization").and_then(|x| x.to_str().ok()) != Some("Bearer sekrit") {
                    return (axum::http::StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": {"message": "bad key"}})));
                }
                let inputs = v["input"].as_array().cloned().unwrap_or_default();
                // Return out of order to exercise index handling; vector = [len, 1, 0].
                let data: Vec<_> = inputs
                    .iter()
                    .enumerate()
                    .rev()
                    .map(|(i, t)| serde_json::json!({"index": i, "embedding": [t.as_str().map_or(0, str::len) as f32, 1.0, 0.0]}))
                    .collect();
                (axum::http::StatusCode::OK, Json(serde_json::json!({ "data": data })))
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(l, app).await });
        format!("http://{addr}/v1")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn batches_reorders_normalizes_and_authenticates() {
        let ep = mock().await;
        let r = tokio::task::spawn_blocking(move || {
            let e = HttpEmbedder::new(
                "t",
                &ep,
                "m",
                3,
                Some("sekrit".into()),
                Duration::from_secs(5),
                2,
            )
            .expect("embedder");
            let v = e.embed(&["a", "bbb", "cc"]).expect("embed");
            let bad = HttpEmbedder::new(
                "t",
                &ep,
                "m",
                3,
                Some("wrong".into()),
                Duration::from_secs(5),
                2,
            )
            .expect("embedder");
            let wrong_dim = HttpEmbedder::new(
                "t",
                &ep,
                "m",
                4,
                Some("sekrit".into()),
                Duration::from_secs(5),
                2,
            )
            .expect("embedder");
            (
                v,
                bad.embed(&["a"]).is_err(),
                wrong_dim.embed(&["a"]).is_err(),
            )
        })
        .await
        .expect("join");
        let (v, unauthorized, mismatched) = r;
        assert_eq!(v.len(), 3);
        assert!(
            (v[1][0] / v[1][1] - 3.0).abs() < 1e-5,
            "order preserved: {:?}",
            v[1]
        );
        assert!(
            v.iter()
                .all(|x| (x.iter().map(|a| a * a).sum::<f32>() - 1.0).abs() < 1e-5)
        );
        assert!(unauthorized && mismatched);
    }
}
