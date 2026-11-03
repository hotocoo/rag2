//! The Rust SDK against a real RAG2 served over TCP with API-key auth: every call, plus the
//! errors a caller must be able to tell apart.

use rag2_client::{Client, ClientError};

const ADMIN: &str = "admin-key-0123456789abcdefghijklmnopqrstuv";
const READER: &str = "reader-key-0123456789abcdefghijklmnopqrstu";
const TENANT: &str = "plant";

fn profile(dir: &std::path::Path) -> String {
    let secret = |name: &str, value: &str| {
        let p = dir.join(name);
        std::fs::write(&p, value).expect("secret");
        format!("file:{}", p.display())
    };
    format!(
        "api_version: rag2/v1\nmetadata: {{name: main}}\nstorage: {{data_dir: \"{}\"}}\nsecurity:\n  auth:\n    mode: api_keys\n    keys:\n      - {{id: admin, secret: \"{}\", roles: [admin], tenants: [\"*\"]}}\n      - {{id: reader, secret: \"{}\", roles: [reader], tenants: [{TENANT}]}}\n",
        dir.join("rag").join(rag2::DATA_DIR).display(),
        secret("admin", ADMIN),
        secret("reader", READER)
    )
}

async fn serve(dir: &std::path::Path) -> String {
    let rag = rag2::open_with_profile_text(dir.join("rag"), &profile(dir)).expect("open");
    let app = rag2::router(rag).expect("router");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
}

#[tokio::test(flavor = "multi_thread")]
async fn every_sdk_call_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let url = serve(dir.path()).await;
    let admin = Client::new(&url, Some(ADMIN.into())).expect("client");
    let reader = Client::new(&url, Some(READER.into())).expect("client");
    let anon = Client::new(&url, None).expect("client");

    // Readiness turns true once startup recovery finishes.
    for _ in 0..100 {
        if admin.ready().await.expect("ready") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(admin.ready().await.expect("ready"));

    let docs = vec![
        rag2::Document::new(
            "seals",
            "Mechanical pump seals are inspected every 500 hours.",
        )
        .expect("doc"),
        rag2::Document::new("lunch", "The cafeteria serves lunch from twelve to two.")
            .expect("doc"),
    ];
    let report = admin.ingest(TENANT, docs).await.expect("ingest");
    assert_eq!(report.documents.len(), 2);

    let r = reader
        .query(
            TENANT,
            &rag2::QueryRequest::new("how often are pump seals inspected"),
        )
        .await
        .expect("query");
    assert_eq!(r.evidence[0].provenance.doc_id.as_str(), "seals");

    let listed = admin.list_documents(TENANT, None, 10).await.expect("list");
    assert_eq!(listed.len(), 2);
    let doc = admin.get_document(TENANT, "seals").await.expect("get");
    assert_eq!(doc.id.as_str(), "seals");
    assert!(
        !admin
            .document_chunks(TENANT, "seals")
            .await
            .expect("chunks")
            .is_empty()
    );
    assert_eq!(admin.stats(TENANT).await.expect("stats").documents, 2);
    assert_eq!(
        admin.tenants().await.expect("tenants"),
        vec![TENANT.to_string()]
    );

    // Configuration: preview, submit without activating, activate, roll back.
    let active = admin.active_config().await.expect("config");
    let version = active["version"].as_u64().expect("version");
    let text = profile(dir.path()).replace(
        "metadata: {name: main}",
        "metadata: {name: main, description: tuned}",
    );
    let preview = admin.preview_config(&text).await.expect("preview");
    assert!(preview.to_string().contains("tuned"));
    let submitted = admin
        .submit_config(&text, "sdk test", false)
        .await
        .expect("submit");
    let new_version = submitted["version"]["version"]
        .as_u64()
        .expect("new version");
    assert_eq!(submitted["activated"], false);
    assert!(new_version > version);
    admin.activate_config(new_version).await.expect("activate");
    admin.rollback_config().await.expect("rollback");

    admin
        .delete_document(TENANT, "lunch")
        .await
        .expect("delete");
    let missing = admin
        .get_document(TENANT, "lunch")
        .await
        .expect_err("deleted");
    assert!(
        matches!(missing, ClientError::Api { status: 404, .. }),
        "{missing:?}"
    );

    // Errors a caller must tell apart: no credentials, insufficient role.
    let e = anon.stats(TENANT).await.expect_err("anonymous");
    assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e:?}");
    let e = reader
        .delete_document(TENANT, "seals")
        .await
        .expect_err("reader cannot delete");
    assert!(matches!(e, ClientError::Api { status: 403, .. }), "{e:?}");
    assert!(!e.is_retryable());
}
