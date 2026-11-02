//! Shared service state: the engine, the live authenticator and rate limiters (both rebuilt on
//! configuration activation), ingest backpressure, audit, metrics.

use crate::auth::{Authenticator, Principal};
use arc_swap::ArcSwap;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use metrics_exporter_prometheus::PrometheusHandle;
use parking_lot::RwLock;
use rag2_config::store::now_ms;
use rag2_core::{Error, Result, TenantId};
use rag2_engine::Engine;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Semaphore;

struct Limiter {
    rps: u32,
    burst: u32,
    inner: DefaultDirectRateLimiter,
}

pub struct AppState {
    pub engine: Arc<Engine>,
    pub auth: ArcSwap<Authenticator>,
    limiters: RwLock<HashMap<TenantId, Arc<Limiter>>>,
    /// Ingest requests admitted (running or waiting): `limits.ingest_queue_depth`.
    pub ingest_queue: Arc<Semaphore>,
    /// Ingest requests running: `limits.ingest_workers`.
    pub ingest_workers: Arc<Semaphore>,
    pub metrics: Option<PrometheusHandle>,
    pub ready: AtomicBool,
    pub webhooks: crate::webhooks::Dispatcher,
    pub connectors: crate::connectors::Connectors,
}

impl AppState {
    pub fn new(engine: Arc<Engine>, metrics: Option<PrometheusHandle>) -> Result<Arc<Self>> {
        let active = engine.config().current();
        let auth = Authenticator::from_profile(active.profile.clone())?;
        let l = &active.profile.limits;
        let state = Arc::new(AppState {
            ingest_queue: Arc::new(Semaphore::new(l.ingest_queue_depth)),
            ingest_workers: Arc::new(Semaphore::new(l.ingest_workers)),
            engine: engine.clone(),
            auth: ArcSwap::from_pointee(auth),
            limiters: RwLock::new(HashMap::new()),
            metrics,
            ready: AtomicBool::new(false),
            webhooks: crate::webhooks::Dispatcher::start(engine.config().clone()),
            connectors: crate::connectors::Connectors::default(),
        });
        // The listener, TLS, middleware limits and ingest pools are built once at startup;
        // activating a change to them would silently do nothing, so it is refused instead.
        let fixed_server = active.profile.server.clone();
        let fixed_ingest = (l.ingest_queue_depth, l.ingest_workers);
        engine.config().add_veto(move |p| {
            if p.server != fixed_server {
                return Err(
                    "server settings (listen, TLS, limits, CORS, console) change only on restart"
                        .into(),
                );
            }
            if (p.limits.ingest_queue_depth, p.limits.ingest_workers) != fixed_ingest {
                return Err(
                    "limits.ingest_queue_depth and limits.ingest_workers change only on restart"
                        .into(),
                );
            }
            Ok(())
        });
        // Credentials must resolve before a profile goes live; then swap them in after.
        engine.config().add_veto(|p| {
            Authenticator::from_profile(Arc::new(p.clone()))
                .map(drop)
                .map_err(|e| e.to_string())
        });
        let weak = Arc::downgrade(&state);
        engine.config().on_change(move |a| {
            let Some(s) = weak.upgrade() else { return };
            s.webhooks.emit(
                rag2_config::schema::WebhookEvent::ConfigActivated,
                None,
                serde_json::json!({ "version": a.version, "hash": a.hash }),
            );
            match Authenticator::from_profile(a.profile.clone()) {
                Ok(auth) => s.auth.store(Arc::new(auth)),
                Err(e) => tracing::error!(error = %e, "authenticator rebuild failed; keeping the previous one"),
            }
            // Limiters pick up new quotas lazily (parameters are compared on use).
        });
        Ok(state)
    }

    /// Admit one request for `t` under its configured rate, or refuse with `RateLimited`.
    pub fn admit(&self, t: &TenantId) -> Result<()> {
        let active = self.engine.config().current();
        let cfg = active.profile.tenant(t);
        let (rps, burst) = (
            cfg.requests_per_second,
            cfg.burst.max(cfg.requests_per_second),
        );
        let existing = self.limiters.read().get(t).cloned();
        let lim = match existing {
            Some(l) if l.rps == rps && l.burst == burst => l,
            _ => {
                let quota = Quota::per_second(
                    NonZeroU32::new(rps).ok_or_else(|| Error::Internal("zero rate".into()))?,
                )
                .allow_burst(
                    NonZeroU32::new(burst).ok_or_else(|| Error::Internal("zero burst".into()))?,
                );
                let l = Arc::new(Limiter {
                    rps,
                    burst,
                    inner: RateLimiter::direct(quota),
                });
                self.limiters.write().insert(t.clone(), l.clone());
                l
            }
        };
        lim.inner
            .check()
            .map_err(|_| Error::RateLimited(format!("tenant {t} is over {rps} requests/s")))
    }

    /// Record an audit event (writes and admin actions always; queries when configured).
    pub fn audit(
        &self,
        who: &Principal,
        action: &str,
        tenant: Option<&TenantId>,
        target: &str,
        outcome: &str,
    ) {
        let active = self.engine.config().current();
        if !active.profile.security.audit.enabled {
            return;
        }
        let ev = serde_json::json!({
            "at_ms": now_ms(),
            "principal": who.id,
            "action": action,
            "tenant": tenant.map(|t| t.as_str()),
            "target": target,
            "outcome": outcome,
        });
        tracing::info!(target: "rag2::audit", principal = %who.id, action, tenant = tenant.map(|t| t.as_str()), target, outcome, "audit");
        if let Err(e) = self.engine.storage().append_audit(now_ms(), &ev) {
            tracing::error!(error = %e, "audit write failed");
        }
    }

    pub fn audit_queries(&self) -> bool {
        self.engine
            .config()
            .current()
            .profile
            .security
            .audit
            .include_queries
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}
