//! A primary and a pulling replica, both real servers on TCP, plus tenant partitioning: the
//! replica converges on the primary's data, redirects writes to it, and a group sends tenants it
//! does not own to their owner.

use rag2_client::Client;
use std::time::Duration;

const ADMIN: &str = "admin-key-0123456789abcdefghijklmnopqrstuv";
const REPL: &str = "replication-key-0123456789abcdefghijklmnop";
const TENANT: &str = "plant";
const POLL_MS: u64 = 50;
const CONVERGE_TRIES: usize = 200;

fn secret(dir: &std::path::Path, name: &str, value: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, value).expect("secret");
    format!("file:{}", p.display())
}

fn profile(dir: &std::path::Path, cluster: &str) -> String {
    format!(
        "api_version: rag2/v1\nmetadata: {{name: main}}\nsecurity:\n  auth:\n    mode: api_keys\n    keys:\n      - {{id: admin, secret: \"{}\", roles: [admin], tenants: [\"*\"]}}\ncluster:\n{cluster}",
        secret(dir, "admin", ADMIN)
    )
}

async fn bind() -> (tokio::net::TcpListener, String) {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}", l.local_addr().expect("addr"));
    (l, url)
}

fn serve(dir: std::path::PathBuf, text: String, listener: tokio::net::TcpListener) {
    let rag = rag2::open_with_profile_text(dir, &text).expect("open");
    let app = rag2::router(rag).expect("router");
    tokio::spawn(async move { axum::serve(listener, app).await });
}

async fn ready(c: &Client) {
    for _ in 0..CONVERGE_TRIES {
        if c.ready().await.unwrap_or(false) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    panic!("server never became ready");
}

#[tokio::test(flavor = "multi_thread")]
async fn replica_converges_and_redirects_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let key = secret(dir.path(), "repl", REPL);
    let (pl, purl) = bind().await;
    let (rl, rurl) = bind().await;
    serve(
        dir.path().join("primary"),
        profile(
            dir.path(),
            &format!("  role: primary\n  replication_key: \"{key}\"\n"),
        ),
        pl,
    );
    serve(
        dir.path().join("replica"),
        profile(
            dir.path(),
            &format!(
                "  role: replica\n  node_id: r1\n  primary_url: {purl}\n  replication_key: \"{key}\"\n  poll_interval_ms: {POLL_MS}\n"
            ),
        ),
        rl,
    );
    let primary = Client::new(&purl, Some(ADMIN.into())).expect("client");
    let replica = Client::new(&rurl, Some(ADMIN.into())).expect("client");
    ready(&primary).await;
    ready(&replica).await;

    let docs = vec![
        rag2::Document::new(
            "seals",
            "Mechanical pump seals are inspected every 500 hours.",
        )
        .expect("doc"),
    ];
    primary.ingest(TENANT, docs).await.expect("ingest");
    primary
        .delete_document(TENANT, "seals")
        .await
        .expect("delete");
    let docs = vec![
        rag2::Document::new("bearings", "Pump bearings are greased every 2,000 hours.")
            .expect("doc"),
    ];
    primary.ingest(TENANT, docs).await.expect("ingest");

    let mut converged = false;
    for _ in 0..CONVERGE_TRIES {
        if let Ok(s) = replica.stats(TENANT).await
            && s.documents == 1
            && replica.get_document(TENANT, "bearings").await.is_ok()
        {
            converged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(POLL_MS)).await;
    }
    assert!(converged, "replica never caught up");
    assert!(
        replica.get_document(TENANT, "seals").await.is_err(),
        "delete replicated"
    );
    let r = replica
        .query(
            TENANT,
            &rag2::QueryRequest::new("how often are bearings greased"),
        )
        .await
        .expect("replica serves queries");
    assert_eq!(r.evidence[0].provenance.doc_id.as_str(), "bearings");

    // Writes on the replica go to the primary.
    let raw = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http");
    let res = raw
        .delete(format!("{rurl}/v1/tenants/{TENANT}/documents/bearings"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("send");
    assert_eq!(res.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        res.headers()["location"],
        format!("{purl}/v1/tenants/{TENANT}/documents/bearings").as_str()
    );
    // The replication endpoint refuses callers without the shared key.
    let res = raw
        .get(format!("{purl}/v1/internal/replication/tenants"))
        .send()
        .await
        .expect("send");
    assert!(res.status().is_client_error(), "{}", res.status());
}

#[tokio::test(flavor = "multi_thread")]
async fn partitioned_tenants_redirect_to_their_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (l, url) = bind().await;
    serve(
        dir.path().join("a"),
        profile(
            dir.path(),
            &format!(
                "  self_url: {url}\n  tenant_routes:\n    plant: {url}\n    \"*\": http://group-b.internal:8080\n"
            ),
        ),
        l,
    );
    let raw = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http");
    ready(&Client::new(&url, Some(ADMIN.into())).expect("client")).await;
    let res = raw
        .get(format!("{url}/v1/tenants/office/stats"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("send");
    assert_eq!(res.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        res.headers()["location"],
        "http://group-b.internal:8080/v1/tenants/office/stats"
    );
    let res = raw
        .get(format!("{url}/v1/tenants/{TENANT}/stats"))
        .bearer_auth(ADMIN)
        .send()
        .await
        .expect("send");
    assert_ne!(res.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
}
