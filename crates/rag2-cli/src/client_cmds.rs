//! Commands that talk to a running server through the Rust SDK.

use crate::Remote;
use anyhow::{Context, Result};
use rag2_client::{Client, ClientError, QueryRequest};
use rag2_core::DocumentId;
use rag2_core::model::{Document, Metadata};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Retries for 429/503 with exponential backoff starting here.
const RETRY_BASE: Duration = Duration::from_millis(250);
const MAX_RETRIES: u32 = 6;

fn client(r: &Remote) -> Result<Client> {
    Ok(Client::new(&r.url, r.api_key.clone())?)
}

pub async fn query(
    r: &Remote,
    tenant: &str,
    q: String,
    top_k: Option<usize>,
    pipeline: Option<String>,
    json: bool,
) -> Result<()> {
    let mut req = QueryRequest::new(q);
    req.top_k = top_k;
    req.pipeline = pipeline;
    let resp = client(r)?.query(tenant, &req).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    println!(
        "confidence {:.3}  route {:?}  {} µs",
        resp.confidence, resp.route.retrievers, resp.timings.total_us
    );
    if let Some(a) = &resp.abstained {
        println!("abstained: {:?} ({})", a.reason, a.detail);
    }
    for (i, e) in resp.evidence.iter().enumerate() {
        let snippet: String = e.text.chars().take(160).collect();
        println!(
            "[{}] {:.4} {}#{}  {}",
            i + 1,
            e.score,
            e.provenance.doc_id,
            e.provenance.chunk_ordinal,
            snippet.replace('\n', " ")
        );
    }
    for c in &resp.contradictions {
        println!(
            "contradiction ({}) chunk {} vs {}: {}",
            c.detector, c.a.0, c.b.0, c.reason
        );
    }
    for w in &resp.warnings {
        println!("warning: {w}");
    }
    Ok(())
}

fn collect(path: &Path, ext: &[String], out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let p = entry?.path();
        let hidden = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        if p.is_dir() {
            collect(&p, ext, out)?;
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ext.iter().any(|x| x.eq_ignore_ascii_case(e)))
        {
            out.push(p);
        }
    }
    Ok(())
}

async fn with_retry<T, F, Fut>(mut f: F) -> Result<T, ClientError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Err(e) if e.is_retryable() && attempt < MAX_RETRIES => {
                tokio::time::sleep(RETRY_BASE * 2u32.pow(attempt)).await;
                attempt += 1;
            }
            other => return other,
        }
    }
}

pub async fn ingest(
    r: &Remote,
    tenant: &str,
    path: &Path,
    ext: &[String],
    batch: usize,
) -> Result<()> {
    let c = client(r)?;
    let mut files = Vec::new();
    collect(path, ext, &mut files)?;
    files.sort();
    let root = if path.is_file() {
        path.parent().unwrap_or(Path::new("."))
    } else {
        path
    };
    let (mut created, mut updated, mut unchanged, mut skipped, mut dropped) = (0, 0, 0, 0, 0);
    for group in files.chunks(batch.max(1)) {
        let mut docs = Vec::with_capacity(group.len());
        for f in group {
            let Ok(text) = std::fs::read_to_string(f) else {
                eprintln!("skipping {} (not UTF-8 text)", f.display());
                skipped += 1;
                continue;
            };
            let rel = f
                .strip_prefix(root)
                .unwrap_or(f)
                .to_string_lossy()
                .replace('\\', "/");
            docs.push(Document {
                id: DocumentId::new(&rel)?,
                title: f.file_stem().map(|s| s.to_string_lossy().into_owned()),
                text,
                source: Some(f.to_string_lossy().into_owned()),
                metadata: Metadata::new(),
            });
        }
        if docs.is_empty() {
            continue;
        }
        let report = with_retry(|| c.ingest(tenant, docs.clone())).await?;
        for d in report.documents {
            match d.status {
                rag2_client::IngestStatus::Created => created += 1,
                rag2_client::IngestStatus::Updated => updated += 1,
                rag2_client::IngestStatus::Unchanged => unchanged += 1,
                rag2_client::IngestStatus::Dropped => dropped += 1,
            }
        }
    }
    println!(
        "{created} created, {updated} updated, {unchanged} unchanged, {dropped} dropped by plugins, {skipped} skipped"
    );
    Ok(())
}

pub async fn status(r: &Remote) -> Result<()> {
    let c = client(r)?;
    println!("ready: {}", c.ready().await?);
    match c.tenants().await {
        Ok(t) => println!("tenants: {}", t.join(", ")),
        Err(e) => println!("tenants: unavailable ({e})"),
    }
    Ok(())
}

/// Forward newline-delimited JSON-RPC from stdin to `{url}/mcp`; write replies to stdout.
/// Diagnostics go to stderr so stdout stays a clean protocol stream.
pub async fn mcp_stdio(r: &Remote) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let http = reqwest::Client::new();
    let url = format!("{}/mcp", r.url.trim_end_matches('/'));
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let mut req = http
            .post(&url)
            .header("content-type", "application/json")
            .body(line);
        if let Some(k) = &r.api_key {
            req = req.bearer_auth(k);
        }
        match req.send().await {
            Ok(resp) if resp.status() == reqwest::StatusCode::ACCEPTED => {}
            Ok(resp) => {
                let body = resp.text().await.unwrap_or_default();
                out.write_all(body.trim().as_bytes()).await?;
                out.write_all(b"\n").await?;
                out.flush().await?;
            }
            Err(e) => eprintln!("rag2 mcp: {e}"),
        }
    }
    Ok(())
}
