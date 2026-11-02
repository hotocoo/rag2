//! Control-plane endpoints (`/v1/admin/...`) and operational probes.

use crate::error::{ApiError, ApiResult};
use crate::routes::{blocking, body, principal};
use crate::state::AppState;
use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rag2_config::schema::Permission;
use rag2_config::store::{Activation, VersionMeta};
use rag2_config::{Preview, parse_profile_value};
use rag2_core::{Error, TenantId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Audit events per page when the caller does not say.
const DEFAULT_AUDIT_PAGE: usize = 100;
const MAX_AUDIT_PAGE: usize = 1_000;

#[derive(Serialize, schemars::JsonSchema)]
pub struct ActiveConfig {
    pub name: String,
    pub version: u64,
    pub environment: Option<String>,
    pub hash: String,
    pub profile: serde_json::Value,
}

pub async fn get_config(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<ActiveConfig>> {
    principal(&s, &h).await?.require(Permission::ManageConfig)?;
    let cfg = s.engine.config();
    let a = cfg.current();
    Ok(Json(ActiveConfig {
        name: cfg.name().into(),
        version: a.version,
        environment: a.environment.clone(),
        hash: a.hash.clone(),
        profile: serde_json::to_value(a.profile.as_ref())
            .map_err(|e| ApiError(Error::internal(e)))?,
    }))
}

pub async fn schema() -> Json<serde_json::Value> {
    Json(rag2_config::json_schema())
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct Versions {
    pub active: u64,
    pub versions: Vec<VersionMeta>,
    pub activations: Vec<Activation>,
}

pub async fn list_versions(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<Versions>> {
    principal(&s, &h).await?.require(Permission::ManageConfig)?;
    let cfg = s.engine.config().clone();
    blocking(move || {
        Ok(Versions {
            active: cfg.current().version,
            versions: cfg.store().list(cfg.name())?,
            activations: cfg.store().activations(cfg.name())?,
        })
    })
    .await
    .map(Json)
}

pub async fn get_version(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(v): Path<u64>,
) -> ApiResult<Json<serde_json::Value>> {
    principal(&s, &h).await?.require(Permission::ManageConfig)?;
    let cfg = s.engine.config().clone();
    let stored = blocking(move || cfg.store().get(cfg.name(), v)).await?;
    Ok(Json(
        serde_json::json!({ "meta": stored.meta, "profile": stored.profile }),
    ))
}

/// A profile submission: the document as YAML or JSON text, or as a JSON object.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProfileSubmission {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub profile: Option<serde_json::Value>,
    #[serde(default)]
    pub comment: String,
    /// Activate immediately after storing.
    #[serde(default)]
    pub activate: bool,
}

impl ProfileSubmission {
    fn value(&self) -> rag2_core::Result<serde_json::Value> {
        match (&self.text, &self.profile) {
            (Some(t), None) => parse_profile_value(t),
            (None, Some(v)) => Ok(v.clone()),
            _ => Err(Error::InvalidInput(
                "send exactly one of `text` or `profile`".into(),
            )),
        }
    }
}

pub async fn preview(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    req: Result<Json<ProfileSubmission>, JsonRejection>,
) -> ApiResult<Json<Preview>> {
    principal(&s, &h).await?.require(Permission::ManageConfig)?;
    let v = body(req)?.value()?;
    let cfg = s.engine.config().clone();
    blocking(move || Ok(cfg.preview(&v))).await.map(Json)
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct SubmitResult {
    pub version: VersionMeta,
    pub activated: bool,
}

pub async fn submit(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    req: Result<Json<ProfileSubmission>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<SubmitResult>)> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageConfig)?;
    let sub = body(req)?;
    let v = sub.value()?;
    let cfg = s.engine.config().clone();
    let author = who.id.clone();
    let (comment, activate) = (sub.comment.clone(), sub.activate);
    let result = blocking(move || {
        let meta = cfg.submit(&v, &author, &comment)?;
        if activate {
            cfg.activate(meta.version, &author)?;
        }
        Ok(SubmitResult {
            version: meta,
            activated: activate,
        })
    })
    .await;
    let target = result
        .as_ref()
        .map(|r| format!("v{}", r.version.version))
        .unwrap_or_default();
    s.audit(
        &who,
        "config_submit",
        None,
        &target,
        if result.is_ok() { "ok" } else { "rejected" },
    );
    result.map(|r| (StatusCode::CREATED, Json(r)))
}

pub async fn activate(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(v): Path<u64>,
) -> ApiResult<Json<serde_json::Value>> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageConfig)?;
    let cfg = s.engine.config().clone();
    let author = who.id.clone();
    let r = blocking(move || cfg.activate(v, &author).map(|a| a.version)).await;
    s.audit(
        &who,
        "config_activate",
        None,
        &format!("v{v}"),
        if r.is_ok() { "ok" } else { "rejected" },
    );
    r.map(|version| Json(serde_json::json!({ "active": version })))
}

pub async fn rollback(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageConfig)?;
    let cfg = s.engine.config().clone();
    let author = who.id.clone();
    let r = blocking(move || cfg.rollback(&author).map(|a| a.version)).await;
    s.audit(
        &who,
        "config_rollback",
        None,
        "",
        if r.is_ok() { "ok" } else { "rejected" },
    );
    r.map(|version| Json(serde_json::json!({ "active": version })))
}

pub async fn list_tenants(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<Vec<String>>> {
    principal(&s, &h)
        .await?
        .require(Permission::ManageTenants)?;
    let e = s.engine.clone();
    blocking(move || e.tenants())
        .await
        .map(|t| Json(t.into_iter().map(String::from).collect()))
}

#[derive(Deserialize)]
pub struct Confirm {
    confirm: Option<String>,
}

/// Deleting a tenant is irreversible: the request must repeat the tenant id in `?confirm=`.
pub async fn delete_tenant(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
    Query(c): Query<Confirm>,
) -> ApiResult<Json<serde_json::Value>> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageTenants)?;
    let t = TenantId::new(&raw)?;
    if c.confirm.as_deref() != Some(t.as_str()) {
        return Err(ApiError(Error::InvalidInput(format!(
            "repeat the tenant id: ?confirm={t}"
        ))));
    }
    let e = s.engine.clone();
    let tq = t.clone();
    let r = blocking(move || e.delete_tenant(&tq)).await;
    s.audit(
        &who,
        "tenant_delete",
        Some(&t),
        "",
        if r.is_ok() { "ok" } else { "error" },
    );
    if r.is_ok() {
        s.webhooks.emit(
            rag2_config::schema::WebhookEvent::TenantDeleted,
            Some(t.as_str()),
            serde_json::json!({}),
        );
    }
    r.map(|n| Json(serde_json::json!({ "deleted_records": n })))
}

pub async fn reindex(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Path(raw): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let who = principal(&s, &h).await?;
    who.require(Permission::ManageTenants)?;
    let t = TenantId::new(&raw)?;
    let e = s.engine.clone();
    let tq = t.clone();
    let r = blocking(move || e.reindex(&tq)).await;
    s.audit(
        &who,
        "tenant_reindex",
        Some(&t),
        "",
        if r.is_ok() { "ok" } else { "error" },
    );
    r.map(|n| Json(serde_json::json!({ "documents": n })))
}

#[derive(Deserialize)]
pub struct AuditPage {
    /// Hex cursor from a previous page's `next`.
    after: Option<String>,
    limit: Option<usize>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|p| u8::from_str_radix(p, 16).ok()))
        .collect()
}

pub async fn audit(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
    Query(p): Query<AuditPage>,
) -> ApiResult<Json<serde_json::Value>> {
    principal(&s, &h).await?.require(Permission::ReadAudit)?;
    let after = match p.after.as_deref() {
        Some(c) => {
            Some(unhex(c).ok_or_else(|| ApiError(Error::InvalidInput("bad cursor".into())))?)
        }
        None => None,
    };
    let limit = p
        .limit
        .unwrap_or(DEFAULT_AUDIT_PAGE)
        .clamp(1, MAX_AUDIT_PAGE);
    let e = s.engine.clone();
    let page = blocking(move || e.storage().scan_audit(after.as_deref(), limit)).await?;
    let next = page.last().map(|(k, _)| hex(k));
    Ok(Json(
        serde_json::json!({ "events": page.into_iter().map(|(_, v)| v).collect::<Vec<_>>(), "next": next }),
    ))
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ComponentHealth {
    pub embedders: Vec<String>,
    pub decision_models: Vec<(String, bool)>,
}

pub async fn components(
    State(s): State<Arc<AppState>>,
    h: HeaderMap,
) -> ApiResult<Json<ComponentHealth>> {
    principal(&s, &h).await?.require(Permission::ReadMetrics)?;
    let e = s.engine.clone();
    blocking(move || {
        let c = e.component_snapshot();
        let mut decision_models: Vec<(String, bool)> = c
            .decision_models
            .iter()
            .map(|(n, d)| (n.clone(), d.healthy()))
            .collect();
        decision_models.sort();
        let mut embedders: Vec<String> = c.embedders.keys().cloned().collect();
        embedders.sort();
        Ok(ComponentHealth {
            embedders,
            decision_models,
        })
    })
    .await
    .map(Json)
}

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn readyz(State(s): State<Arc<AppState>>) -> Response {
    if s.is_ready() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting").into_response()
    }
}

pub async fn metrics(State(s): State<Arc<AppState>>, h: HeaderMap) -> ApiResult<Response> {
    if !s.engine.config().current().profile.server.public_metrics {
        principal(&s, &h).await?.require(Permission::ReadMetrics)?;
    }
    let text = s.metrics.as_ref().map(|m| m.render()).unwrap_or_default();
    Ok(([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], text).into_response())
}
