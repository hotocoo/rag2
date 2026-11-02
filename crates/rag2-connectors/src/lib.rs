//! Prebuilt source connectors. Each one turns configuration into documents (`fetch`) and can
//! check its connection and credentials without ingesting anything (`test`). Scheduling and
//! ingestion are the server's job ([`rag2_engine::Engine::sync_source`]).

pub mod html;

use rag2_config::defaults::CONNECTOR_MAX_ITEM_BYTES;
use rag2_config::schema::{SecretRef, SourceConfig};
use rag2_core::model::{Document, Metadata};
use rag2_core::{DocumentId, Error, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

fn client(
    timeout_ms: u64,
    headers: &BTreeMap<String, SecretRef>,
) -> Result<reqwest::blocking::Client> {
    let mut h = reqwest::header::HeaderMap::new();
    for (k, secret) in headers {
        let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| Error::InvalidInput(format!("bad header name {k}")))?;
        let value = secret.resolve().map_err(Error::InvalidInput)?;
        let mut v = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|_| Error::InvalidInput(format!("bad value for header {k}")))?;
        v.set_sensitive(true);
        h.insert(name, v);
    }
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .default_headers(h)
        .build()
        .map_err(Error::internal)
}

fn get(c: &reqwest::blocking::Client, url: &str) -> Result<(String, String)> {
    let r = c
        .get(url)
        .send()
        .map_err(|e| Error::Unavailable(format!("{url}: {e}")))?;
    let status = r.status();
    if !status.is_success() {
        return Err(Error::Unavailable(format!("{url}: HTTP {status}")));
    }
    let ctype = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if r.content_length()
        .is_some_and(|n| n as usize > CONNECTOR_MAX_ITEM_BYTES)
    {
        return Err(Error::InvalidInput(format!(
            "{url}: larger than {CONNECTOR_MAX_ITEM_BYTES} bytes"
        )));
    }
    let body = r
        .text()
        .map_err(|e| Error::Unavailable(format!("{url}: {e}")))?;
    if body.len() > CONNECTOR_MAX_ITEM_BYTES {
        return Err(Error::InvalidInput(format!(
            "{url}: larger than {CONNECTOR_MAX_ITEM_BYTES} bytes"
        )));
    }
    Ok((ctype, body))
}

fn walk(dir: &Path, exts: &[String], out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| Error::Unavailable(format!("{}: {e}", dir.display())))?;
    for e in entries.flatten() {
        let p = e.path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
        // Never follow symlinks: a link could point outside the configured directory.
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(&p, exts, out)?;
        } else if ft.is_file()
            && p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| exts.iter().any(|e| e.eq_ignore_ascii_case(x)))
        {
            out.push(p);
        }
    }
    Ok(())
}

fn pointer_str(v: &serde_json::Value, ptr: &str) -> Option<String> {
    match v.pointer(ptr)? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Fetch every document the source currently holds.
pub fn fetch(src: &SourceConfig) -> Result<Vec<Document>> {
    match src {
        SourceConfig::Filesystem { path, extensions } => {
            let root = Path::new(path);
            let mut files = Vec::new();
            walk(root, extensions, &mut files)?;
            files.sort();
            let mut docs = Vec::with_capacity(files.len());
            for f in files {
                let meta = std::fs::metadata(&f).map_err(Error::storage)?;
                if meta.len() as usize > CONNECTOR_MAX_ITEM_BYTES {
                    tracing::warn!(file = %f.display(), "skipping file over the size limit");
                    continue;
                }
                let Ok(raw) = std::fs::read_to_string(&f) else {
                    tracing::warn!(file = %f.display(), "skipping non-UTF-8 file");
                    continue;
                };
                let is_html = f.extension().and_then(|x| x.to_str()).is_some_and(|x| {
                    x.eq_ignore_ascii_case("html") || x.eq_ignore_ascii_case("htm")
                });
                let rel = f
                    .strip_prefix(root)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .replace('\\', "/");
                docs.push(Document {
                    id: DocumentId::new(&rel)?,
                    title: f.file_stem().map(|s| s.to_string_lossy().into_owned()),
                    text: if is_html { html::to_text(&raw) } else { raw },
                    source: Some(f.to_string_lossy().into_owned()),
                    metadata: Metadata::new(),
                });
            }
            Ok(docs)
        }
        SourceConfig::Web {
            urls,
            headers,
            timeout_ms,
        } => {
            let c = client(*timeout_ms, headers)?;
            urls.iter()
                .map(|u| {
                    let (ctype, body) = get(&c, u)?;
                    let (title, text) =
                        if ctype.contains("html") || body.trim_start().starts_with('<') {
                            (html::title(&body), html::to_text(&body))
                        } else {
                            (None, body)
                        };
                    Ok(Document {
                        id: DocumentId::new(u)?,
                        title,
                        text,
                        source: Some(u.clone()),
                        metadata: Metadata::new(),
                    })
                })
                .collect()
        }
        SourceConfig::HttpJson {
            url,
            headers,
            items,
            id,
            text,
            title,
            metadata,
            timeout_ms,
        } => {
            let c = client(*timeout_ms, headers)?;
            let (_, body) = get(&c, url)?;
            let v: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| Error::Unavailable(format!("{url}: not JSON: {e}")))?;
            let list = v
                .pointer(items)
                .and_then(|x| x.as_array())
                .ok_or_else(|| Error::InvalidInput(format!("{url}: no array at {items:?}")))?;
            let mut docs = Vec::with_capacity(list.len());
            for (i, rec) in list.iter().enumerate() {
                let (Some(rid), Some(rtext)) = (pointer_str(rec, id), pointer_str(rec, text))
                else {
                    tracing::warn!(url, record = i, "record lacks id or text; skipped");
                    continue;
                };
                let md: Metadata = metadata
                    .iter()
                    .filter_map(|(k, p)| rec.pointer(p).map(|x| (k.clone(), x.clone())))
                    .collect();
                docs.push(Document {
                    id: DocumentId::new(&rid)?,
                    title: title.as_deref().and_then(|p| pointer_str(rec, p)),
                    text: rtext,
                    source: Some(url.clone()),
                    metadata: md,
                });
            }
            Ok(docs)
        }
    }
}

/// Check that the source is reachable with its credentials; returns a human-readable summary.
pub fn test(src: &SourceConfig) -> Result<String> {
    match src {
        SourceConfig::Filesystem { path, extensions } => {
            let mut files = Vec::new();
            walk(Path::new(path), extensions, &mut files)?;
            Ok(format!(
                "{path} is readable: {} matching files",
                files.len()
            ))
        }
        SourceConfig::Web {
            urls,
            headers,
            timeout_ms,
        } => {
            let c = client(*timeout_ms, headers)?;
            let first = urls
                .first()
                .ok_or_else(|| Error::InvalidInput("no URLs".into()))?;
            let (ctype, body) = get(&c, first)?;
            Ok(format!(
                "{first} answered ({ctype}, {} bytes); {} URLs configured",
                body.len(),
                urls.len()
            ))
        }
        SourceConfig::HttpJson { url, .. } => {
            let n = fetch(src)?.len();
            Ok(format!("{url} answered with {n} mappable records"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::routing::get as route_get;

    #[test]
    fn filesystem_reads_text_converts_html_and_skips_hidden_and_other_types() {
        let d = tempfile::tempdir().expect("dir");
        std::fs::create_dir_all(d.path().join("sub")).expect("mkdir");
        std::fs::write(d.path().join("a.md"), "# A\nalpha").expect("write");
        std::fs::write(
            d.path().join("sub/b.html"),
            "<html><head><title>B</title><style>x{}</style></head><body><p>bravo</p></body></html>",
        )
        .expect("write");
        std::fs::write(d.path().join(".hidden.md"), "secret").expect("write");
        std::fs::write(d.path().join("c.bin"), "binary").expect("write");
        let src = SourceConfig::Filesystem {
            path: d.path().to_string_lossy().into(),
            extensions: vec!["md".into(), "html".into()],
        };
        let docs = fetch(&src).expect("fetch");
        let ids: Vec<&str> = docs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["a.md", "sub/b.html"]);
        assert_eq!(docs[1].text.trim(), "bravo");
        assert!(test(&src).expect("test").contains("2 matching files"));
        assert!(
            test(&SourceConfig::Filesystem {
                path: "/nonexistent/rag2".into(),
                extensions: vec!["md".into()]
            })
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn json_apis_map_records_and_authenticate() {
        let app = axum::Router::new().route(
            "/tickets",
            route_get(|h: axum::http::HeaderMap| async move {
                if h.get("authorization").and_then(|v| v.to_str().ok()) != Some("Bearer t0ken") {
                    return (axum::http::StatusCode::UNAUTHORIZED, Json(serde_json::json!({})));
                }
                (
                    axum::http::StatusCode::OK,
                    Json(serde_json::json!({ "data": [
                        { "id": 7, "subject": "Pump noise", "body": "The pump is noisy.", "status": "open" },
                        { "id": 8, "subject": "No body" }
                    ]})),
                )
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(l, app).await });
        #[allow(unsafe_code)]
        // SAFETY: test-only variable with a unique name, set before any reader runs.
        unsafe {
            std::env::set_var("RAG2_CONN_TEST_TOKEN", "Bearer t0ken")
        };
        let src = |token: &str| SourceConfig::HttpJson {
            url: format!("http://{addr}/tickets"),
            headers: [("authorization".to_string(), SecretRef::Env(token.into()))].into(),
            items: "/data".into(),
            id: "/id".into(),
            text: "/body".into(),
            title: Some("/subject".into()),
            metadata: [("status".to_string(), "/status".to_string())].into(),
            timeout_ms: 5_000,
        };
        let good = src("RAG2_CONN_TEST_TOKEN");
        let docs = tokio::task::spawn_blocking(move || fetch(&good))
            .await
            .expect("join")
            .expect("fetch");
        assert_eq!(docs.len(), 1, "the record without a body is skipped");
        assert_eq!(docs[0].id.as_str(), "7");
        assert_eq!(docs[0].title.as_deref(), Some("Pump noise"));
        assert_eq!(docs[0].metadata["status"], "open");
        #[allow(unsafe_code)]
        // SAFETY: as above.
        unsafe {
            std::env::set_var("RAG2_CONN_TEST_BAD", "Bearer wrong")
        };
        let bad = src("RAG2_CONN_TEST_BAD");
        let err = tokio::task::spawn_blocking(move || test(&bad))
            .await
            .expect("join")
            .expect_err("unauthorized");
        assert!(err.to_string().contains("401"), "{err}");
    }
}
