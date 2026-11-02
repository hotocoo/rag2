use super::*;
use axum::body::Body;
use axum::http::{Request, header};
use rag2_config::schema::{ApiKey, AuthConfig, Profile, SecretRef, TenantConfig};
use rag2_config::{ConfigManager, FsConfigStore};
use tower::ServiceExt;

const ADMIN_KEY: &str = "admin-key-0123456789abcdef";
const READER_KEY: &str = "reader-key-0123456789abcdef";

fn set_env(k: &str, v: &str) {
    #[allow(unsafe_code)]
    // SAFETY: test-only variables with unique names, set before any thread reads them.
    unsafe {
        std::env::set_var(k, v)
    };
}

struct Harness {
    _dir: tempfile::TempDir,
    app: Router,
    state: Arc<AppState>,
}

fn harness(edit: impl FnOnce(&mut Profile)) -> Harness {
    set_env("RAG2_SRV_TEST_ADMIN", ADMIN_KEY);
    set_env("RAG2_SRV_TEST_READER", READER_KEY);
    let dir = tempfile::tempdir().expect("tempdir");
    let mut p = Profile::minimal("srv");
    p.storage.data_dir = dir.path().join("data").to_string_lossy().into_owned();
    p.indexes.lexical.commit_interval_ms = 0;
    p.indexes.lexical.writer_heap_mb = 16;
    p.security.auth = AuthConfig::ApiKeys {
        keys: vec![
            ApiKey {
                id: "admin".into(),
                secret: SecretRef::Env("RAG2_SRV_TEST_ADMIN".into()),
                roles: vec!["admin".into()],
                tenants: vec!["*".into()],
            },
            ApiKey {
                id: "reader".into(),
                secret: SecretRef::Env("RAG2_SRV_TEST_READER".into()),
                roles: vec!["reader".into()],
                tenants: vec!["acme".into()],
            },
        ],
    };
    edit(&mut p);
    let store = Arc::new(FsConfigStore::open(dir.path().join("config")).expect("store"));
    let cfg = Arc::new(ConfigManager::open(store, "srv", None, || p, 10).expect("config"));
    let engine = Engine::open(cfg, Vec::new()).expect("engine");
    let state = AppState::new(engine, None).expect("state");
    state.set_ready(true);
    Harness {
        _dir: dir,
        app: router(state.clone()),
        state,
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value, axum::http::HeaderMap) {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        b = b.header(header::AUTHORIZATION, format!("Bearer {k}"));
    }
    let req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .expect("request");
    let resp = app.clone().oneshot(req).await.expect("response");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json, headers)
}

fn docs() -> serde_json::Value {
    serde_json::json!({ "documents": [
        { "id": "pump", "text": "The coolant pump seals are inspected monthly by the maintenance crew." },
        { "id": "leave", "text": "Staff accrue twenty days of paid leave each year." }
    ]})
}

#[tokio::test]
async fn ingest_then_query_over_http_with_request_ids() {
    let h = harness(|_| {});
    let (s, r, _) = call(
        &h.app,
        "POST",
        "/v1/tenants/acme/documents",
        Some(ADMIN_KEY),
        Some(docs()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["documents"][0]["status"], "created");
    let (s, r, hdr) = call(
        &h.app,
        "POST",
        "/v1/tenants/acme/query",
        Some(READER_KEY),
        Some(serde_json::json!({"query": "pump seal inspection"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["evidence"][0]["provenance"]["doc_id"], "pump");
    assert!(hdr.contains_key(REQUEST_ID));
    let (s, r, _) = call(
        &h.app,
        "GET",
        "/v1/tenants/acme/documents/pump/chunks",
        Some(READER_KEY),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r.as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn authentication_permissions_and_tenant_scope_are_enforced() {
    let h = harness(|_| {});
    let q = Some(serde_json::json!({"query": "x"}));
    assert_eq!(
        call(&h.app, "POST", "/v1/tenants/acme/query", None, q.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &h.app,
            "POST",
            "/v1/tenants/acme/query",
            Some("not-a-real-key-at-all-xxxx"),
            q.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // Reader: other tenant forbidden, ingest forbidden, admin forbidden.
    assert_eq!(
        call(
            &h.app,
            "POST",
            "/v1/tenants/globex/query",
            Some(READER_KEY),
            q.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &h.app,
            "POST",
            "/v1/tenants/acme/documents",
            Some(READER_KEY),
            Some(docs())
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&h.app, "GET", "/v1/admin/config", Some(READER_KEY), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&h.app, "GET", "/v1/admin/config", Some(ADMIN_KEY), None)
            .await
            .0,
        StatusCode::OK
    );
    // Probes need no credentials.
    assert_eq!(
        call(&h.app, "GET", "/healthz", None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&h.app, "GET", "/readyz", None, None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn malformed_requests_get_the_error_envelope() {
    let h = harness(|_| {});
    let (s, r, _) = call(
        &h.app,
        "POST",
        "/v1/tenants/acme/query",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"qurey": "typo"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(r["error"]["code"], "invalid_input");
    let (s, r, _) = call(
        &h.app,
        "POST",
        "/v1/tenants/Bad..Tenant/query",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"query": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{r}");
}

#[tokio::test]
async fn tenants_over_their_rate_get_429_with_retry_after() {
    let h = harness(|p| {
        p.tenants.insert(
            rag2_core::TenantId::new("acme").expect("t"),
            TenantConfig {
                pipeline: None,
                requests_per_second: 1,
                burst: 2,
                max_documents: 100,
                enabled: true,
            },
        );
    });
    let q = Some(serde_json::json!({"query": "x"}));
    let mut statuses = Vec::new();
    for _ in 0..4 {
        let (s, _, hdr) = call(
            &h.app,
            "POST",
            "/v1/tenants/acme/query",
            Some(ADMIN_KEY),
            q.clone(),
        )
        .await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            assert!(hdr.contains_key(header::RETRY_AFTER));
        }
        statuses.push(s);
    }
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    // Another tenant is unaffected.
    assert_eq!(
        call(
            &h.app,
            "POST",
            "/v1/tenants/globex/query",
            Some(ADMIN_KEY),
            q
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn configuration_lifecycle_preview_submit_activate_rollback() {
    let h = harness(|_| {});
    let (_, cur, _) = call(&h.app, "GET", "/v1/admin/config", Some(ADMIN_KEY), None).await;
    let mut profile = cur["profile"].clone();
    profile["pipelines"]["default"]["top_k"] = serde_json::json!(3);
    let (s, prev, _) = call(
        &h.app,
        "POST",
        "/v1/admin/config/preview",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"profile": profile})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(prev["valid"], true);
    assert_eq!(prev["changes"][0]["path"], "pipelines.default.top_k");
    let (s, sub, _) = call(
        &h.app,
        "POST",
        "/v1/admin/config/versions",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"profile": profile, "comment": "narrow", "activate": true})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sub}");
    assert_eq!(
        h.state.engine.config().current().profile.pipelines["default"].top_k,
        3
    );
    // Invalid profile is rejected and changes nothing.
    profile["pipelines"]["default"]["top_k"] = serde_json::json!(0);
    let (s, _, _) = call(
        &h.app,
        "POST",
        "/v1/admin/config/versions",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"profile": profile, "activate": true})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Restart-only settings are refused rather than silently ignored.
    let mut port = cur["profile"].clone();
    port["server"]["listen"] = serde_json::json!("127.0.0.1:9999");
    let (s, r, _) = call(
        &h.app,
        "POST",
        "/v1/admin/config/versions",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"profile": port, "activate": true})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        r["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("restart")),
        "{r}"
    );
    let (s, _, _) = call(
        &h.app,
        "POST",
        "/v1/admin/config/rollback",
        Some(ADMIN_KEY),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h.state.engine.config().current().version, 1);
    let (_, audit, _) = call(&h.app, "GET", "/v1/admin/audit", Some(ADMIN_KEY), None).await;
    let actions: Vec<&str> = audit["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|e| e["action"].as_str())
        .collect();
    assert!(
        actions.contains(&"config_submit") && actions.contains(&"config_rollback"),
        "{actions:?}"
    );
}

#[tokio::test]
async fn deleting_a_tenant_requires_confirmation() {
    let h = harness(|_| {});
    call(
        &h.app,
        "POST",
        "/v1/tenants/acme/documents",
        Some(ADMIN_KEY),
        Some(docs()),
    )
    .await;
    assert_eq!(
        call(
            &h.app,
            "DELETE",
            "/v1/admin/tenants/acme",
            Some(ADMIN_KEY),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        call(
            &h.app,
            "DELETE",
            "/v1/admin/tenants/acme?confirm=acme",
            Some(ADMIN_KEY),
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, t, _) = call(&h.app, "GET", "/v1/admin/tenants", Some(ADMIN_KEY), None).await;
    assert_eq!(t, serde_json::json!([]));
}

#[tokio::test]
async fn openapi_and_metrics_are_served() {
    let h = harness(|_| {});
    let (s, spec, _) = call(&h.app, "GET", "/v1/openapi.json", Some(ADMIN_KEY), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(
        call(&h.app, "GET", "/metrics", None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&h.app, "GET", "/metrics", Some(ADMIN_KEY), None)
            .await
            .0,
        StatusCode::OK
    );
}

async fn rpc(app: &Router, key: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let (s, v, _) = call(app, "POST", "/mcp", Some(key), Some(body)).await;
    (s, v)
}

#[tokio::test]
async fn mcp_exposes_search_with_the_callers_tenant_scope() {
    let h = harness(|_| {});
    call(
        &h.app,
        "POST",
        "/v1/tenants/acme/documents",
        Some(ADMIN_KEY),
        Some(docs()),
    )
    .await;
    let (s, init) = rpc(
        &h.app,
        READER_KEY,
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(init["result"]["serverInfo"]["name"], "rag2");
    let (s, _) = rpc(
        &h.app,
        READER_KEY,
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (_, list) = rpc(
        &h.app,
        READER_KEY,
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    )
    .await;
    assert!(
        list["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|t| t["name"] == "search")
    );
    let (_, found) = rpc(
        &h.app,
        READER_KEY,
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
        "params":{"name":"search","arguments":{"tenant":"acme","query":"pump seal inspection"}}}),
    )
    .await;
    assert_eq!(found["result"]["isError"], false, "{found}");
    assert!(
        found["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .contains("pump")
    );
    let (_, denied) = rpc(
        &h.app,
        READER_KEY,
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
        "params":{"name":"search","arguments":{"tenant":"globex","query":"x"}}}),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true);
    let (s, _) = rpc(
        &h.app,
        "wrong-key-wrong-key-wrong-key",
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/list"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn webhooks_deliver_signed_events() {
    use std::sync::Mutex;
    type Deliveries = Arc<Mutex<Vec<(axum::http::HeaderMap, Vec<u8>)>>>;
    let seen: Deliveries = Arc::default();
    let sink = seen.clone();
    let receiver = Router::new().route(
        "/hook",
        post(move |h: axum::http::HeaderMap, body: axum::body::Bytes| {
            let sink = sink.clone();
            async move {
                sink.lock().expect("lock").push((h, body.to_vec()));
                StatusCode::OK
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = l.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(l, receiver).await });
    set_env("RAG2_SRV_TEST_HOOK", "hook-secret");
    let h = harness(|p| {
        p.webhooks.push(rag2_config::schema::Webhook {
            name: "audit-sink".into(),
            url: format!("http://{addr}/hook"),
            events: vec![rag2_config::schema::WebhookEvent::DocumentsIngested],
            secret: Some(SecretRef::Env("RAG2_SRV_TEST_HOOK".into())),
            tenants: vec![],
            timeout_ms: 2000,
            max_retries: 0,
        });
    });
    call(
        &h.app,
        "POST",
        "/v1/tenants/acme/documents",
        Some(ADMIN_KEY),
        Some(docs()),
    )
    .await;
    let mut got = None;
    for _ in 0..50 {
        if let Some(x) = seen.lock().expect("lock").first().cloned() {
            got = Some(x);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let (headers, body) = got.expect("webhook delivered");
    let ts: u64 = headers[webhooks::TIMESTAMP_HEADER]
        .to_str()
        .expect("ts")
        .parse()
        .expect("number");
    assert_eq!(
        headers[webhooks::SIGNATURE_HEADER].to_str().expect("sig"),
        webhooks::sign(b"hook-secret", ts, &body)
    );
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(v["event"], "documents_ingested");
    assert_eq!(v["tenant"], "acme");
}

#[cfg(feature = "grpc")]
#[tokio::test(flavor = "multi_thread")]
async fn grpc_ingest_and_query_with_metadata_auth() {
    use crate::grpc::pb;
    let h = harness(|_| {});
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().expect("addr");
    let svc = crate::grpc::service(h.state.clone());
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(svc)
            .serve(addr)
            .await
    });
    let mut client = None;
    for _ in 0..50 {
        if let Ok(c) = pb::rag2_client::Rag2Client::connect(format!("http://{addr}")).await {
            client = Some(c);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let mut c = client.expect("grpc client");
    fn authed<T>(msg: T) -> tonic::Request<T> {
        let mut r = tonic::Request::new(msg);
        r.metadata_mut().insert(
            "authorization",
            format!("Bearer {ADMIN_KEY}").parse().expect("meta"),
        );
        r
    }
    let ing = c
        .ingest(authed(pb::IngestRequest {
            tenant: "acme".into(),
            documents: vec![pb::Document {
                id: "pump".into(),
                title: None,
                text: "The coolant pump seals are inspected monthly.".into(),
                source: None,
                metadata_json: String::new(),
            }],
        }))
        .await
        .expect("ingest")
        .into_inner();
    assert_eq!(ing.documents[0].status, "created");
    let q = c
        .query(authed(pb::QueryRequest {
            tenant: "acme".into(),
            query: "pump seals".into(),
            top_k: Some(3),
            pipeline: None,
            filters_json: String::new(),
            hops: None,
            include_context: true,
        }))
        .await
        .expect("query")
        .into_inner();
    assert_eq!(q.evidence[0].doc_id, "pump");
    let denied = c
        .query(tonic::Request::new(pb::QueryRequest {
            tenant: "acme".into(),
            query: "x".into(),
            ..Default::default()
        }))
        .await;
    assert_eq!(
        denied.expect_err("no credentials").code(),
        tonic::Code::Unauthenticated
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connectors_test_and_sync_from_the_admin_api() {
    let src = tempfile::tempdir().expect("dir");
    std::fs::write(
        src.path().join("guide.md"),
        "The coolant pump seals are inspected monthly.",
    )
    .expect("write");
    let h = harness(|p| {
        p.connectors.insert(
            "handbook".into(),
            rag2_config::schema::ConnectorConfig {
                tenant: rag2_core::TenantId::new("acme").expect("t"),
                source: rag2_config::schema::SourceConfig::Filesystem {
                    path: src.path().to_string_lossy().into_owned(),
                    extensions: vec!["md".into()],
                },
                interval_s: 0,
                delete_missing: true,
                enabled: true,
            },
        );
    });
    let (s, t, _) = call(
        &h.app,
        "POST",
        "/v1/admin/connectors/handbook/test",
        Some(ADMIN_KEY),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(t["ok"], true, "{t}");
    let (s, r, _) = call(
        &h.app,
        "POST",
        "/v1/admin/connectors/handbook/run",
        Some(ADMIN_KEY),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["created"], 1);
    let (_, q, _) = call(
        &h.app,
        "POST",
        "/v1/tenants/acme/query",
        Some(ADMIN_KEY),
        Some(serde_json::json!({"query": "pump seals"})),
    )
    .await;
    assert_eq!(q["evidence"][0]["provenance"]["doc_id"], "guide.md");
    let (_, list, _) = call(&h.app, "GET", "/v1/admin/connectors", Some(ADMIN_KEY), None).await;
    assert_eq!(list[0]["status"]["last_ok"], true);
    assert_eq!(
        call(
            &h.app,
            "POST",
            "/v1/admin/connectors/handbook/run",
            Some(READER_KEY),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn tenants_owned_by_another_group_are_redirected() {
    let h = harness(|p| {
        p.cluster.self_url = Some("http://group-a:8080".into());
        p.cluster
            .tenant_routes
            .insert("acme".into(), "http://group-a:8080".into());
        p.cluster
            .tenant_routes
            .insert("*".into(), "http://group-b:8080".into());
    });
    let q = Some(serde_json::json!({"query": "x"}));
    let (s, _, _) = call(
        &h.app,
        "POST",
        "/v1/tenants/acme/query",
        Some(ADMIN_KEY),
        q.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "own tenant served locally");
    let (s, _, hdr) = call(
        &h.app,
        "POST",
        "/v1/tenants/globex/query?x=1",
        Some(ADMIN_KEY),
        q,
    )
    .await;
    assert_eq!(s, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        hdr[header::LOCATION],
        "http://group-b:8080/v1/tenants/globex/query?x=1"
    );
}
