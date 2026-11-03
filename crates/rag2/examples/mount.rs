//! RAG2 mounted into an existing axum application under `/rag`:
//! `cargo run -p rag2 --example mount -- ./rag2-data 127.0.0.1:3000`, then
//! `curl 127.0.0.1:3000/rag/healthz`. Auth, limits and pipelines come from the profile in the
//! data directory (`rag2 config apply` or `rag2::open_with_profile`).

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "./rag2-data".into());
    let listen = args.next().unwrap_or_else(|| "127.0.0.1:3000".into());
    let rag = rag2::open(&dir)?;
    let app = axum::Router::new()
        .route("/", axum::routing::get(|| async { "my application" }))
        .nest("/rag", rag2::router(rag)?);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    println!("listening on http://{listen} (RAG2 under /rag)");
    axum::serve(listener, app).await?;
    Ok(())
}
