//! Data-plane endpoints: `/v1/tenants/{tenant}/...`.

use crate::auth::Principal;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use rag2_config::schema::Permission;
use rag2_core::model::DocumentRecord;
use rag2_core::{DocumentId, Error, TenantId};
use rag2_engine::{
    ChunkView, IngestReport, IngestRequest, QueryRequest, QueryResponse, TenantStats,
};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;

/// Documents per page when the caller does not say.
const DEFAULT_PAGE: usize = 100;
/// Largest page a caller may ask for.
const MAX_PAGE: usize = 1_000;

pub fn body<T>(r: Result<Json<T>, JsonRejection>) -> ApiResult<T> {
    r.map(|Json(v)| v).map_err(|e| {
        ApiError(Error::InvalidInput(format!(
            "request body: {}",
            e.body_text()
        )))
    })
}

pub async fn principal(s: &AppState, h: &HeaderMap) -> ApiResult<Principal> {
    Ok(s.auth.load_full().authenticate(h).await?)
}

fn tenant(raw: &str) -> ApiResult<TenantId> {
    Ok(TenantId::new(raw)?)
}

/// Run blocking engine work off the async runtime.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> rag2_core::Result<T> + Send + 'static,
) -> ApiResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError(Error::Internal(format!("worker: {e}"))))?
        .map_err(ApiError)
}

async fn authorize(
    s: &AppState,
    h: &HeaderMap,
    raw: &str,
    p: Permission,
) -> ApiResult<(Principal, TenantId)> {
    let who = principal(s, h).await?;
    let t = tenant(raw)?;
    who.require_tenant(p, &t)?;
    s.admit(&t)?;
    Ok((who, t))
}

pub async fn query(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
    req: Result<Json<QueryRequest>, JsonRejection>,
) -> ApiResult<Json<QueryResponse>> {
    let req = body(req)?;
    let (who, t) = authorize(&s, &h, &raw, Permission::Query).await?;
    let started = Instant::now();
    let e = s.engine.clone();
    let tq = t.clone();
    let result = blocking(move || e.query(&tq, req)).await;
    let outcome = if result.is_ok() { "ok" } else { "error" };
    metrics::histogram!("rag2_query_duration_seconds", "outcome" => outcome)
        .record(started.elapsed().as_secs_f64());
    if let Ok(r) = &result {
        if r.abstained.is_some() {
            metrics::counter!("rag2_query_abstained_total").increment(1);
        }
        if r.cached {
            metrics::counter!("rag2_query_cache_hits_total").increment(1);
        }
    }
    if s.audit_queries() {
        s.audit(&who, "query", Some(&t), "", outcome);
    }
    result.map(Json)
}

pub async fn ingest(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
    req: Result<Json<IngestRequest>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<IngestReport>)> {
    let req = body(req)?;
    let (who, t) = authorize(&s, &h, &raw, Permission::Ingest).await?;
    // Backpressure: a full queue refuses immediately (503 + Retry-After) instead of piling up.
    let queued = s
        .ingest_queue
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(Error::Overloaded("ingest queue is full".into())))?;
    let worker = s
        .ingest_workers
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| ApiError(Error::internal(e)))?;
    let started = Instant::now();
    let n = req.documents.len();
    let e = s.engine.clone();
    let tq = t.clone();
    let result = blocking(move || {
        let _held = (queued, worker);
        e.ingest(&tq, req.documents)
    })
    .await;
    metrics::histogram!("rag2_ingest_duration_seconds").record(started.elapsed().as_secs_f64());
    metrics::counter!("rag2_ingested_documents_total").increment(n as u64);
    s.audit(
        &who,
        "ingest",
        Some(&t),
        &format!("{n} documents"),
        if result.is_ok() { "ok" } else { "error" },
    );
    if let Ok(r) = &result {
        let changed: Vec<_> = r
            .documents
            .iter()
            .filter(|d| d.status != rag2_engine::IngestStatus::Unchanged)
            .map(|d| serde_json::json!({ "id": d.id, "status": d.status, "version": d.version }))
            .collect();
        if !changed.is_empty() {
            s.webhooks.emit(
                rag2_config::schema::WebhookEvent::DocumentsIngested,
                Some(t.as_str()),
                serde_json::json!({ "documents": changed, "generation": r.generation }),
            );
        }
    }
    result.map(|r| (StatusCode::OK, Json(r)))
}

#[derive(Deserialize)]
pub struct Page {
    after: Option<String>,
    limit: Option<usize>,
}

pub async fn list_documents(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
    Query(page): Query<Page>,
) -> ApiResult<Json<Vec<DocumentRecord>>> {
    let (_, t) = authorize(&s, &h, &raw, Permission::ReadDocuments).await?;
    let after = page.after.map(DocumentId::new).transpose()?;
    let limit = page.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let e = s.engine.clone();
    blocking(move || e.list_documents(&t, after.as_ref(), limit))
        .await
        .map(Json)
}

pub async fn get_document(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((raw, id)): Path<(String, String)>,
) -> ApiResult<Json<DocumentRecord>> {
    let (_, t) = authorize(&s, &h, &raw, Permission::ReadDocuments).await?;
    let id = DocumentId::new(id)?;
    let e = s.engine.clone();
    let idc = id.clone();
    blocking(move || e.get_document(&t, &idc))
        .await?
        .map(Json)
        .ok_or_else(|| ApiError(Error::NotFound(format!("document {id}"))))
}

pub async fn document_chunks(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((raw, id)): Path<(String, String)>,
) -> ApiResult<Json<Vec<ChunkView>>> {
    let (_, t) = authorize(&s, &h, &raw, Permission::ReadDocuments).await?;
    let id = DocumentId::new(id)?;
    let e = s.engine.clone();
    blocking(move || e.document_chunks(&t, &id)).await.map(Json)
}

pub async fn delete_document(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path((raw, id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let (who, t) = authorize(&s, &h, &raw, Permission::DeleteDocuments).await?;
    let id = DocumentId::new(id)?;
    let e = s.engine.clone();
    let (tq, idq) = (t.clone(), id.clone());
    let n = blocking(move || e.delete_documents(&tq, &[idq])).await?;
    s.audit(
        &who,
        "delete_document",
        Some(&t),
        id.as_str(),
        if n > 0 { "ok" } else { "not_found" },
    );
    if n == 0 {
        return Err(ApiError(Error::NotFound(format!("document {id}"))));
    }
    s.webhooks.emit(
        rag2_config::schema::WebhookEvent::DocumentsDeleted,
        Some(t.as_str()),
        serde_json::json!({ "documents": [id.as_str()] }),
    );
    Ok(StatusCode::NO_CONTENT)
}

pub async fn stats(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<TenantStats>> {
    let (_, t) = authorize(&s, &h, &raw, Permission::ReadDocuments).await?;
    let e = s.engine.clone();
    blocking(move || e.stats(&t)).await.map(Json)
}
