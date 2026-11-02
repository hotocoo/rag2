//! Replication over HTTP. Primaries expose their per-tenant logs on `/v1/internal/replication`
//! (shared-key authentication, constant-time comparison); replicas pull and apply them, and send
//! clients' writes to the primary with a 307 redirect.

use crate::error::{ApiError, ApiResult};
use crate::routes::blocking;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rag2_config::schema::ClusterRole;
use rag2_core::{Error, TenantId};
use rag2_engine::Engine;
use rag2_storage::replication::LogEntry;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;

pub const REPLICATION_KEY_HEADER: &str = "x-rag2-replication-key";
/// Ceiling on entries per log page served to a replica.
const MAX_LOG_PAGE: usize = 1_000;
/// Pause after a failed replication round before trying again.
const ERROR_BACKOFF: Duration = Duration::from_secs(2);
const PULL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize, Debug)]
pub struct TenantHead {
    pub tenant: String,
    pub epoch: u64,
    pub seq: u64,
}

#[derive(Serialize, Deserialize)]
pub struct LogPage {
    pub epoch: u64,
    pub entries: Vec<(u64, LogEntry)>,
}

fn resolve_key(engine: &Engine) -> rag2_core::Result<String> {
    let active = engine.config().current();
    let r = active
        .profile
        .cluster
        .replication_key
        .as_ref()
        .ok_or_else(|| Error::Forbidden("replication is not configured".into()))?;
    r.resolve().map_err(Error::InvalidInput)
}

fn check_key(s: &AppState, h: &HeaderMap) -> ApiResult<()> {
    if s.engine.role() != ClusterRole::Primary {
        return Err(ApiError(Error::NotFound("not a primary".into())));
    }
    let want = blake3::hash(resolve_key(&s.engine)?.as_bytes());
    let got = blake3::hash(
        h.get(REPLICATION_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .as_bytes(),
    );
    if bool::from(want.as_bytes().ct_eq(got.as_bytes())) {
        Ok(())
    } else {
        Err(ApiError(Error::Unauthenticated(
            "bad replication key".into(),
        )))
    }
}

pub async fn tenants(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<Vec<TenantHead>>> {
    check_key(&s, &h)?;
    let e = s.engine.clone();
    blocking(move || {
        e.tenants()?
            .into_iter()
            .map(|t| {
                let head = e.log_head(&t)?;
                Ok(TenantHead {
                    tenant: t.to_string(),
                    epoch: head.epoch,
                    seq: head.seq,
                })
            })
            .collect()
    })
    .await
    .map(Json)
}

#[derive(Deserialize)]
pub struct LogQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

pub async fn log(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
    Query(q): Query<LogQuery>,
) -> ApiResult<Json<LogPage>> {
    check_key(&s, &h)?;
    let t = TenantId::new(&raw)?;
    let e = s.engine.clone();
    let limit = q.limit.unwrap_or(MAX_LOG_PAGE).clamp(1, MAX_LOG_PAGE);
    let after = q.after.unwrap_or(0);
    blocking(move || {
        Ok(LogPage {
            epoch: e.log_head(&t)?.epoch,
            entries: e.read_log(&t, after, limit)?,
        })
    })
    .await
    .map(Json)
}

/// On replicas, send every write to the primary (307 keeps method and body). Queries and reads
/// are served locally.
pub async fn redirect_writes(State(s): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    // Tenant partitioning: send requests for tenants owned by another group to that group.
    if let Some(tenant) = req
        .uri()
        .path()
        .strip_prefix("/v1/tenants/")
        .and_then(|r| r.split('/').next())
    {
        let cluster = s.engine.config().current().profile.cluster.clone();
        if let Some(owner) = cluster.route_elsewhere(tenant) {
            let target = format!(
                "{}{}",
                owner.trim_end_matches('/'),
                req.uri()
                    .path_and_query()
                    .map(|p| p.as_str())
                    .unwrap_or("/")
            );
            metrics::counter!("rag2_partition_redirects_total").increment(1);
            return (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]).into_response();
        }
    }
    let is_write = req.method() != Method::GET
        && req.method() != Method::HEAD
        && req.method() != Method::OPTIONS
        && !req.uri().path().ends_with("/query")
        && req.uri().path() != "/mcp"
        && !req.uri().path().starts_with("/v1/internal/");
    if is_write && s.engine.role() == ClusterRole::Replica {
        let primary = s
            .engine
            .config()
            .current()
            .profile
            .cluster
            .primary_url
            .clone()
            .unwrap_or_default();
        let target = format!(
            "{}{}",
            primary.trim_end_matches('/'),
            req.uri()
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/")
        );
        metrics::counter!("rag2_replica_redirects_total").increment(1);
        return (StatusCode::TEMPORARY_REDIRECT, [(header::LOCATION, target)]).into_response();
    }
    next.run(req).await
}

/// Replica loop: poll the primary, apply new entries per tenant, drop tenants the primary no
/// longer has. Runs until the process exits.
pub fn spawn_replica(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let http = match reqwest::Client::builder().timeout(PULL_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "replication client could not start");
                return;
            }
        };
        loop {
            let cfg = engine.config().current().profile.cluster.clone();
            match round(&engine, &http, &cfg).await {
                Ok(applied) => {
                    if applied > 0 {
                        tracing::debug!(applied, "replication round");
                    }
                    tokio::time::sleep(Duration::from_millis(cfg.poll_interval_ms)).await;
                }
                Err(e) => {
                    metrics::counter!("rag2_replication_errors_total").increment(1);
                    tracing::warn!(error = %e, "replication round failed");
                    tokio::time::sleep(ERROR_BACKOFF).await;
                }
            }
        }
    });
}

async fn get<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    url: String,
    key: &str,
) -> rag2_core::Result<T> {
    http.get(url)
        .header(REPLICATION_KEY_HEADER, key)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Error::Unavailable(format!("primary: {e}")))?
        .json()
        .await
        .map_err(|e| Error::Unavailable(format!("primary sent an unreadable reply: {e}")))
}

async fn round(
    engine: &Arc<Engine>,
    http: &reqwest::Client,
    cfg: &rag2_config::schema::ClusterConfig,
) -> rag2_core::Result<usize> {
    let base = cfg
        .primary_url
        .clone()
        .ok_or_else(|| Error::InvalidInput("cluster.primary_url".into()))?;
    let base = base.trim_end_matches('/').to_string();
    let key = resolve_key(engine)?;
    let heads: Vec<TenantHead> = get(
        http,
        format!("{base}/v1/internal/replication/tenants"),
        &key,
    )
    .await?;
    let mut applied = 0;
    for h in &heads {
        let t = TenantId::new(&h.tenant)?;
        let local = {
            let (e, t) = (engine.clone(), t.clone());
            tokio::task::spawn_blocking(move || e.replication_position(&t))
                .await
                .map_err(Error::internal)??
        };
        let mut after = if local.epoch == h.epoch { local.seq } else { 0 };
        metrics::gauge!("rag2_replication_lag_entries", "tenant" => h.tenant.clone())
            .set(h.seq.saturating_sub(after) as f64);
        while after < h.seq {
            let page: LogPage = get(
                http,
                format!(
                    "{base}/v1/internal/replication/tenants/{}/log?after={after}&limit={}",
                    h.tenant, cfg.batch
                ),
                &key,
            )
            .await?;
            let Some(last) = page.entries.last().map(|(s, _)| *s) else {
                break;
            };
            let (e, t2) = (engine.clone(), t.clone());
            applied += tokio::task::spawn_blocking(move || {
                e.apply_replicated(&t2, page.epoch, &page.entries)
            })
            .await
            .map_err(Error::internal)??;
            after = last;
        }
        metrics::gauge!("rag2_replication_lag_entries", "tenant" => h.tenant.clone())
            .set(h.seq.saturating_sub(after) as f64);
    }
    // Tenants deleted on the primary.
    let primary: std::collections::HashSet<&str> =
        heads.iter().map(|h| h.tenant.as_str()).collect();
    let e = engine.clone();
    let local = tokio::task::spawn_blocking(move || e.tenants())
        .await
        .map_err(Error::internal)??;
    for t in local.into_iter().filter(|t| !primary.contains(t.as_str())) {
        tracing::info!(tenant = %t, "tenant gone from primary; removing local copy");
        let e = engine.clone();
        tokio::task::spawn_blocking(move || e.delete_tenant(&t))
            .await
            .map_err(Error::internal)??;
    }
    Ok(applied)
}
