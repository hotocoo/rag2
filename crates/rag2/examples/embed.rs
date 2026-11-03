//! Embedded RAG2: `cargo run -p rag2 --example embed -- ./rag2-data "your question"`.
//! Ingests the text files named by `RAG2_EXAMPLE_FILES` (comma separated), then answers.

fn main() -> rag2::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "./rag2-data".into());
    let question = args
        .next()
        .unwrap_or_else(|| "what is in these documents?".into());
    let rag = rag2::open(&dir)?;
    let tenant = rag2::TenantId::new("example")?;
    let files = std::env::var("RAG2_EXAMPLE_FILES").unwrap_or_default();
    let docs = files
        .split(',')
        .filter(|f| !f.trim().is_empty())
        .map(|f| {
            let text = std::fs::read_to_string(f.trim())
                .map_err(|e| rag2::Error::InvalidInput(format!("{f}: {e}")))?;
            rag2::Document::new(f.trim(), text)
        })
        .collect::<rag2::Result<Vec<_>>>()?;
    if !docs.is_empty() {
        rag.ingest(&tenant, docs)?;
        rag.flush()?;
    }
    let r = rag.query(&tenant, rag2::QueryRequest::new(question))?;
    for e in &r.evidence {
        println!("[{}] {:.3} {}", e.provenance.doc_id, e.score, e.text);
    }
    println!(
        "confidence {:.2}, abstained: {:?}",
        r.confidence, r.abstained
    );
    Ok(())
}
