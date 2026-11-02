//! Connector scheduling and control: `GET /v1/admin/connectors`,
//! `POST /v1/admin/connectors/{name}/test`, `POST /v1/admin/connectors/{name}/run`.

use crate::error::{ApiError, ApiResult};
use crate::routes::principal;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use parking_lot::RwLock;
use rag2_config::schema::{ClusterRole, ConnectorConfig, Permission};
use rag2_config::store::now_ms;
use rag2_core::Error;
use rag2_engine::SyncReport;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// How often the scheduler checks whether a connector is due.
const TICK: Duration = Duration::from_secs(5);
const MS_PER_S: u64 = 1_000;

#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
pub struct ConnectorStatus {
    pub last_run_ms: Option<u64>,
    pub last_ok: Option<bool>,
    pub message: Option<String>,
    pub report: Option<SyncReport>,
}

#[derive(Default)]
pub struct Connectors {
    status: RwLock<HashMap<String, ConnectorStatus>>,
    running: parking_lot::Mutex<HashSet<String>>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ConnectorView {
    pub name: String,
    pub tenant: String,
    pub kind: String,
    pub enabled: bool,
    pub interval_s: u64,
    pub status: ConnectorStatus,
}

fn kind(c: &ConnectorConfig) -> String {
    serde_json::to_value(&c.source)
        .ok()
        .and_then(|v| v["kind"].as_str().map(String::from))
        .unwrap_or_default()
}

/// Fetch and sync one connector (blocking). Only one run per connector at a time.
async fn run_one(
    s: &Arc<AppState>,
    name: &str,
    cfg: ConnectorConfig,
) -> rag2_core::Result<SyncReport> {
    if !s.connectors.running.lock().insert(name.to_string()) {
        return Err(Error::Conflict(format!(
            "connector {name} is already running"
        )));
    }
    let e = s.engine.clone();
    let (n, c) = (name.to_string(), cfg.clone());
    let r = tokio::task::spawn_blocking(move || {
        let docs = rag2_connectors::fetch(&c.source)?;
        e.sync_source(&c.tenant, &n, docs, c.delete_missing)
    })
    .await
    .map_err(Error::internal)
    .and_then(|r| r);
    s.connectors.running.lock().remove(name);
    let st = ConnectorStatus {
        last_run_ms: Some(now_ms()),
        last_ok: Some(r.is_ok()),
        message: r.as_ref().err().map(|e| e.to_string()),
        report: r.as_ref().ok().cloned(),
    };
    metrics::counter!("rag2_connector_runs_total", "connector" => name.to_string(), "ok" => r.is_ok().to_string()).increment(1);
    s.connectors.status.write().insert(name.to_string(), st);
    r
}

/// Background scheduler. Reads connector definitions from the active profile on every tick.
pub fn spawn_scheduler(s: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            let p = s.engine.config().current().profile.clone();
            if p.cluster.role == ClusterRole::Replica {
                continue; // replicas receive data through replication
            }
            for (name, c) in &p.connectors {
                if !c.enabled || c.interval_s == 0 {
                    continue;
                }
                let last = s
                    .connectors
                    .status
                    .read()
                    .get(name)
                    .and_then(|x| x.last_run_ms);
                if last.is_some_and(|l| now_ms().saturating_sub(l) < c.interval_s * MS_PER_S) {
                    continue;
                }
                let (s2, n, c) = (s.clone(), name.clone(), c.clone());
                tokio::spawn(async move {
                    match run_one(&s2, &n, c).await {
                        Ok(r) => tracing::info!(connector = %n, ?r, "connector synced"),
                        Err(e) => {
                            tracing::warn!(connector = %n, error = %e, "connector sync failed")
                        }
                    }
                });
            }
        }
    });
}

pub async fn list(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<Vec<ConnectorView>>> {
    principal(&s, &h).await?.require(Permission::ManageConfig)?;
    let p = s.engine.config().current().profile.clone();
    let status = s.connectors.status.read();
    Ok(Json(
        p.connectors
            .iter()
            .map(|(name, c)| ConnectorView {
                name: name.clone(),
                tenant: c.tenant.to_string(),
                kind: kind(c),
                enabled: c.enabled,
                interval_s: c.interval_s,
                status: status.get(name).cloned().unwrap_or_default(),
            })
            .collect(),
    ))
}

fn configured(s: &AppState, name: &str) -> ApiResult<ConnectorConfig> {
    s.engine
        .config()
        .current()
        .profile
        .connectors
        .get(name)
        .cloned()
        .ok_or_else(|| ApiError(Error::NotFound(format!("connector {name}"))))
}

pub async fn test(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageConfig)?;
    let cfg = configured(&s, &name)?;
    let r = tokio::task::spawn_blocking(move || rag2_connectors::test(&cfg.source))
        .await
        .map_err(|e| ApiError(Error::internal(e)))?;
    s.audit(
        &who,
        "connector_test",
        None,
        &name,
        if r.is_ok() { "ok" } else { "failed" },
    );
    // A failed connection test is a successful API call reporting the failure.
    Ok(Json(match r {
        Ok(m) => serde_json::json!({ "ok": true, "message": m }),
        Err(e) => serde_json::json!({ "ok": false, "message": e.to_string() }),
    }))
}

pub async fn run(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Json<SyncReport>> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageConfig)?;
    let cfg = configured(&s, &name)?;
    who.require_tenant(Permission::Ingest, &cfg.tenant)?;
    let r = run_one(&s, &name, cfg).await;
    s.audit(
        &who,
        "connector_run",
        None,
        &name,
        if r.is_ok() { "ok" } else { "error" },
    );
    Ok(Json(r?))
}
