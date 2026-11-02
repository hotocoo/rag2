//! The RAG2 HTTP service. [`router`] builds the axum application (usable in tests and when
//! embedding RAG2 in another axum service); [`serve`] runs it with TLS, backpressure, graceful
//! shutdown and background maintenance.

pub mod admin;
pub mod auth;
pub mod cluster;
pub mod connectors;
pub mod error;
#[cfg(feature = "grpc")]
pub mod grpc;
pub mod mcp;
pub mod openapi;
pub mod routes;
pub mod state;
pub mod webhooks;

use axum::Router;
use axum::error_handling::HandleErrorLayer;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use rag2_core::{Error, Result};
use rag2_engine::Engine;
use state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceBuilder;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

pub use state::AppState as State_;

const REQUEST_ID: &str = "x-request-id";
/// The console is same-origin wasm: scripts and connections only to this origin. trunk's loader is
/// an inline module script, hence 'unsafe-inline' for scripts; no third-party origins are allowed.
const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'self'; \
    img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'";
/// Fallback cadence for the lexical commit loop when the profile asks for synchronous commits.
const IDLE_COMMIT_CHECK: Duration = Duration::from_secs(1);

async fn track(req: Request, next: Next) -> Response {
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let method = req.method().to_string();
    let started = Instant::now();
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    metrics::counter!("rag2_http_requests_total", "method" => method.clone(), "path" => path.clone(), "status" => status)
        .increment(1);
    metrics::histogram!("rag2_http_request_duration_seconds", "method" => method, "path" => path)
        .record(started.elapsed().as_secs_f64());
    resp
}

async fn overloaded(err: tower::BoxError) -> Response {
    if err.is::<tower::load_shed::error::Overloaded>() {
        metrics::counter!("rag2_http_shed_total").increment(1);
        return error::ApiError(Error::Overloaded(
            "server at max_concurrent_requests".into(),
        ))
        .into_response();
    }
    if err.is::<tower::timeout::error::Elapsed>() {
        return (StatusCode::GATEWAY_TIMEOUT, "request timed out").into_response();
    }
    error::ApiError(Error::Internal(err.to_string())).into_response()
}

async fn openapi(State(s): State<Arc<AppState>>) -> axum::Json<serde_json::Value> {
    let _ = &s;
    axum::Json(openapi::spec(env!("CARGO_PKG_VERSION")))
}

/// The application with every route and middleware. Limits come from the active profile at
/// build time; changing `server.*` takes effect on restart.
pub fn router(state: Arc<AppState>) -> Router {
    let p = state.engine.config().current().profile.clone();
    let s = &p.server;
    let api = Router::new()
        .route("/v1/tenants/{tenant}/query", post(routes::query))
        .route(
            "/v1/tenants/{tenant}/documents",
            post(routes::ingest).get(routes::list_documents),
        )
        .route(
            "/v1/tenants/{tenant}/documents/{id}",
            get(routes::get_document).delete(routes::delete_document),
        )
        .route(
            "/v1/tenants/{tenant}/documents/{id}/chunks",
            get(routes::document_chunks),
        )
        .route("/v1/tenants/{tenant}/stats", get(routes::stats))
        .route("/v1/admin/config", get(admin::get_config))
        .route("/v1/admin/config/schema", get(admin::schema))
        .route(
            "/v1/admin/config/versions",
            get(admin::list_versions).post(admin::submit),
        )
        .route(
            "/v1/admin/config/versions/{version}",
            get(admin::get_version),
        )
        .route(
            "/v1/admin/config/versions/{version}/activate",
            post(admin::activate),
        )
        .route("/v1/admin/config/preview", post(admin::preview))
        .route("/v1/admin/config/rollback", post(admin::rollback))
        .route("/v1/admin/tenants", get(admin::list_tenants))
        .route("/v1/admin/tenants/{tenant}", delete(admin::delete_tenant))
        .route("/v1/admin/tenants/{tenant}/reindex", post(admin::reindex))
        .route("/v1/admin/audit", get(admin::audit))
        .route("/v1/admin/components", get(admin::components))
        .route("/v1/admin/connectors", get(connectors::list))
        .route("/v1/admin/connectors/{name}/test", post(connectors::test))
        .route("/v1/admin/connectors/{name}/run", post(connectors::run))
        .route("/v1/openapi.json", get(openapi))
        .route("/mcp", post(mcp::endpoint))
        .route("/v1/internal/replication/tenants", get(cluster::tenants))
        .route(
            "/v1/internal/replication/tenants/{tenant}/log",
            get(cluster::log),
        )
        .route("/metrics", get(admin::metrics))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(overloaded))
                .load_shed()
                .concurrency_limit(s.max_concurrent_requests)
                .timeout(Duration::from_millis(s.request_timeout_ms)),
        )
        .layer(RequestBodyLimitLayer::new(s.max_body_bytes));
    let probes = Router::new()
        .route("/healthz", get(admin::healthz))
        .route("/readyz", get(admin::readyz));
    let mut app = api
        .merge(probes)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            cluster::redirect_writes,
        ))
        .route_layer(middleware::from_fn(track));
    if let Some(dir) = &s.console_dir {
        let index = std::path::Path::new(dir).join("index.html");
        app = app.fallback_service(
            tower_http::services::ServeDir::new(dir)
                .fallback(tower_http::services::ServeFile::new(index)),
        );
    }
    let origins: Vec<HeaderValue> = s
        .cors_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    let cors = if s.cors_origins.iter().any(|o| o == "*") {
        CorsLayer::new().allow_origin(AllowOrigin::any())
    } else {
        CorsLayer::new().allow_origin(origins)
    }
    .allow_methods([Method::GET, Method::POST, Method::DELETE])
    .allow_headers([
        axum::http::header::AUTHORIZATION,
        axum::http::header::CONTENT_TYPE,
        HeaderName::from_static(auth::API_KEY_HEADER),
    ]);
    let rid = HeaderName::from_static(REQUEST_ID);
    use tower_http::set_header::SetResponseHeaderLayer;
    let hdr = |name: &'static str, value: &'static str| {
        SetResponseHeaderLayer::if_not_present(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        )
    };
    app.layer(hdr("content-security-policy", CSP))
        .layer(hdr("x-content-type-options", "nosniff"))
        .layer(hdr("x-frame-options", "DENY"))
        .layer(hdr("referrer-policy", "no-referrer"))
        .layer(hdr("cache-control", "no-store"))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(rid.clone()))
        .layer(SetRequestIdLayer::new(rid, MakeRequestUuid))
        .with_state(state)
}

fn tls_config(t: &rag2_config::schema::TlsConfig) -> Result<rustls::ServerConfig> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&t.cert_path)
        .and_then(|i| i.collect::<std::result::Result<_, _>>())
        .map_err(|e| Error::InvalidInput(format!("server.tls.cert_path: {e}")))?;
    let key = PrivateKeyDer::from_pem_file(&t.key_path)
        .map_err(|e| Error::InvalidInput(format!("server.tls.key_path: {e}")))?;
    let builder = rustls::ServerConfig::builder();
    let builder = match &t.client_ca_path {
        Some(ca) => {
            let mut roots = rustls::RootCertStore::empty();
            for c in CertificateDer::pem_file_iter(ca)
                .map_err(|e| Error::InvalidInput(format!("server.tls.client_ca_path: {e}")))?
            {
                roots
                    .add(c.map_err(|e| Error::InvalidInput(format!("client CA: {e}")))?)
                    .map_err(|e| Error::InvalidInput(format!("client CA: {e}")))?;
            }
            let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots))
                .build()
                .map_err(|e| Error::InvalidInput(format!("client verifier: {e}")))?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let mut cfg = builder
        .with_single_cert(certs, key)
        .map_err(|e| Error::InvalidInput(format!("TLS certificate: {e}")))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("shutdown signal received; draining");
}

/// Commit lexical writes on the configured interval (re-read every tick, so it follows reloads).
fn spawn_maintenance(engine: Arc<Engine>) {
    tokio::spawn(async move {
        loop {
            let ms = engine
                .config()
                .current()
                .profile
                .indexes
                .lexical
                .commit_interval_ms;
            tokio::time::sleep(if ms == 0 {
                IDLE_COMMIT_CHECK
            } else {
                Duration::from_millis(ms)
            })
            .await;
            let e = engine.clone();
            match tokio::task::spawn_blocking(move || e.commit_lexical()).await {
                Ok(Err(err)) => tracing::error!(error = %err, "lexical commit failed"),
                Err(err) => tracing::error!(error = %err, "lexical commit task panicked"),
                Ok(Ok(_)) => {}
            }
        }
    });
}

/// Run the service until SIGINT/SIGTERM, then drain in-flight requests within the grace period
/// and flush indexes.
pub async fn serve(
    engine: Arc<Engine>,
    metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
) -> Result<()> {
    let state = AppState::new(engine.clone(), metrics)?;
    let p = engine.config().current().profile.clone();
    let addr: SocketAddr = p
        .server
        .listen
        .parse()
        .map_err(|_| Error::InvalidInput("server.listen".into()))?;
    let app = router(state.clone());
    {
        let (e, s) = (engine.clone(), state.clone());
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || e.recover_all()).await {
                Ok(Ok(n)) => {
                    tracing::info!(rebuilt = n, "recovery complete; ready");
                    s.set_ready(true);
                }
                Ok(Err(err)) => tracing::error!(error = %err, "recovery failed; staying unready"),
                Err(err) => {
                    tracing::error!(error = %err, "recovery task panicked; staying unready")
                }
            }
        });
    }
    spawn_maintenance(engine.clone());
    connectors::spawn_scheduler(state.clone());
    if engine.role() == rag2_config::schema::ClusterRole::Replica {
        tracing::info!(primary = ?p.cluster.primary_url, "replica: pulling from primary");
        cluster::spawn_replica(engine.clone());
    }
    let grace = Duration::from_millis(p.server.shutdown_grace_ms);
    let handle = axum_server::Handle::new();
    {
        let h = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            h.graceful_shutdown(Some(grace));
        });
    }
    #[cfg(feature = "grpc")]
    if let Some(g) = &p.server.grpc_listen {
        let gaddr: SocketAddr = g
            .parse()
            .map_err(|_| Error::InvalidInput("server.grpc_listen".into()))?;
        let mut builder = tonic::transport::Server::builder();
        if let Some(t) = &p.server.tls {
            let read = |path: &str| {
                std::fs::read(path).map_err(|e| Error::InvalidInput(format!("{path}: {e}")))
            };
            let mut tls = tonic::transport::ServerTlsConfig::new().identity(
                tonic::transport::Identity::from_pem(read(&t.cert_path)?, read(&t.key_path)?),
            );
            if let Some(ca) = &t.client_ca_path {
                tls = tls.client_ca_root(tonic::transport::Certificate::from_pem(read(ca)?));
            }
            builder = builder
                .tls_config(tls)
                .map_err(|e| Error::InvalidInput(format!("grpc TLS: {e}")))?;
        }
        let router = builder
            .concurrency_limit_per_connection(p.server.max_concurrent_requests)
            .timeout(Duration::from_millis(p.server.request_timeout_ms))
            .add_service(grpc::service(state.clone()));
        tracing::info!(addr = %gaddr, "serving gRPC");
        tokio::spawn(async move {
            if let Err(e) = router.serve_with_shutdown(gaddr, shutdown_signal()).await {
                tracing::error!(error = %e, "gRPC server stopped");
            }
        });
    }
    let svc = app.into_make_service();
    tracing::info!(%addr, tls = p.server.tls.is_some(), "serving");
    let served = match &p.server.tls {
        Some(t) => {
            let cfg = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls_config(t)?));
            axum_server::bind_rustls(addr, cfg)
                .handle(handle)
                .serve(svc)
                .await
        }
        None => axum_server::bind(addr).handle(handle).serve(svc).await,
    };
    served.map_err(|e| Error::Unavailable(format!("server: {e}")))?;
    state.set_ready(false);
    tokio::task::spawn_blocking(move || engine.flush())
        .await
        .map_err(Error::internal)??;
    tracing::info!("stopped cleanly");
    Ok(())
}

/// A `file:` secret holding `value`, in a per-process temp directory. Tests use files rather than
/// environment variables: mutating the environment races with threads reading it.
#[cfg(test)]
pub(crate) fn test_secret(name: &str, value: &str) -> rag2_config::schema::SecretRef {
    let dir = std::env::temp_dir().join(format!("rag2-test-secrets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("secret dir");
    let path = dir.join(name);
    std::fs::write(&path, value).expect("secret file");
    rag2_config::schema::SecretRef::File(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests;
