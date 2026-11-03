//! The two one-step integrations, end to end with the real engine: embedded, and mounted into an
//! existing axum application.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

const SEALS: &str = "Mechanical pump seals are inspected every 500 operating hours.";
const CAFETERIA: &str = "The staff cafeteria serves lunch from twelve to two.";

#[test]
fn embedded_ingest_query_and_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let t = rag2::TenantId::new("docs").expect("tenant");
    {
        let rag = rag2::open(dir.path()).expect("open");
        rag.ingest(
            &t,
            vec![
                rag2::Document::new("seals", SEALS).expect("doc"),
                rag2::Document::new("cafeteria", CAFETERIA).expect("doc"),
            ],
        )
        .expect("ingest");
        rag.flush().expect("flush");
    }
    // Reopening the directory resumes the same data and configuration.
    let rag = rag2::open(dir.path()).expect("reopen");
    rag.recover_all().expect("recover");
    let r = rag
        .query(
            &t,
            rag2::QueryRequest::new("how often are pump seals inspected"),
        )
        .expect("query");
    assert_eq!(
        r.evidence.first().map(|e| e.provenance.doc_id.to_string()),
        Some("seals".into())
    );
}

#[test]
fn profile_text_is_validated_and_activated() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = "api_version: rag2/v1\nmetadata: {name: main}\ndefault_pipeline: missing\n";
    assert!(rag2::open_with_profile_text(dir.path(), bad).is_err());
    let good = "api_version: rag2/v1\nmetadata: {name: main}\nchunking: {max_chars: 400}\n";
    let rag = rag2::open_with_profile_text(dir.path(), good).expect("open");
    assert_eq!(rag.config().current().profile.chunking.max_chars, 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn mounts_into_an_existing_axum_app() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rag = rag2::open(dir.path()).expect("open");
    let app = axum::Router::new()
        .route("/", axum::routing::get(|| async { "my app" }))
        .nest("/rag", rag2::router(rag).expect("router"));
    let res = app
        .clone()
        .oneshot(
            Request::get("/rag/healthz")
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("call");
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .oneshot(Request::get("/").body(Body::empty()).expect("req"))
        .await
        .expect("call");
    assert_eq!(res.status(), StatusCode::OK);
}
